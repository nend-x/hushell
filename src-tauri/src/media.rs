use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use base64::Engine as _;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use crate::util::log;

static WANTS_LEVELS: AtomicBool = AtomicBool::new(false);

/// The last media snapshot (also the boot seed for a fresh media widget).
static LAST: OnceLock<Mutex<Option<MediaNow>>> = OnceLock::new();
/// The SMTC session id the snapshot came from (empty = none seen yet).
static WATCH: OnceLock<Mutex<WatchState>> = OnceLock::new();

struct WatchState {
    manager: Option<GlobalSystemMediaTransportControlsSessionManager>,
    session_id: String,
    /// signature of the properties already rendered (title + artist + playing)
    props_sig: String,
    /// signature of the artwork already decoded
    art_sig: String,
}

impl Default for WatchState {
    fn default() -> Self {
        WatchState {
            manager: None,
            session_id: String::new(),
            props_sig: String::new(),
            art_sig: String::new(),
        }
    }
}

fn last_slot() -> &'static Mutex<Option<MediaNow>> {
    LAST.get_or_init(|| Mutex::new(None))
}

fn watch_slot() -> &'static Mutex<WatchState> {
    WATCH.get_or_init(|| Mutex::new(WatchState::default()))
}

/// One media snapshot as the media widget consumes it.
#[derive(Serialize, Clone, Default)]
pub struct MediaNow {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub cover: Option<String>,
}

fn com_mta() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

pub fn start(app: AppHandle) {
    com_mta();
    // Seed the demand flag: the visualizer runs from boot only when a media
    // widget survived the restart in the config.
    set_wants_levels(&app);
    let la = app.clone();
    std::thread::Builder::new()
        .name("media-watch".into())
        .spawn(move || {
            crate::util::register_thread("media-watch");
            watch_loop(app);
        })
        .expect("media watch thread");
    std::thread::Builder::new()
        .name("media-levels".into())
        .spawn(move || {
            crate::util::register_thread("media-levels");
            levels_loop(la);
        })
        .expect("media levels thread");
    log("media: engines started (smtc watcher + visualizer capture)");
}

/// Recompute the demand flag from a state reference the caller already
/// holds. Callers that own the shared lock MUST use this — `set_wants_levels`
/// would try to re-lock the same non-reentrant Mutex on the calling thread
/// and deadlock it (the dropped-widget freeze + crash).
pub fn set_wants_levels_sync(s: &crate::shared::Shared) {
    let want = s
        .cfg
        .spaces
        .iter()
        .any(|sp| sp.widgets.iter().any(|w| w.kind == "media"));
    WANTS_LEVELS.store(want, Ordering::Relaxed);
}

pub fn set_wants_levels(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    set_wants_levels_sync(&s);
}

// ---------------------------------------------------------------------------
// SMTC watcher (1 Hz poll — cheap, robust against missed WinRT events)
// ---------------------------------------------------------------------------

fn watch_loop(app: AppHandle) {
    com_mta();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let snap = poll_once();
        if let Some(now) = snap {
            let changed = {
                let mut g = watch_slot().lock().unwrap_or_else(|e| e.into_inner());
                let sig = format!("{}|{}|{}", now.title, now.artist, now.playing);
                let changed = sig != g.props_sig;
                g.props_sig = sig;
                changed
            };
            if changed {
                *last_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(now.clone());
                let _ = app.emit("hush:media", &now);
                log(&format!(
                    "media: \"{}\" — {}{}",
                    now.title,
                    now.artist,
                    if now.playing { " (playing)" } else { " (paused)" }
                ));
            }
        } else {
            let cleared = {
                let mut g = watch_slot().lock().unwrap_or_else(|e| e.into_inner());
                let had = !g.props_sig.is_empty() || !g.session_id.is_empty();
                g.session_id.clear();
                g.props_sig.clear();
                had
            };
            if cleared {
                let mut now = last_slot()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_default();
                now.playing = false;
                *last_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(now.clone());
                let _ = app.emit("hush:media", &now);
                log("media: session gone — nothing playing");
            }
        }
    }
}

fn poll_once() -> Option<MediaNow> {
    unsafe {
        // The manager is created once (RequestAsync is expensive).
        let manager = {
            let mut g = watch_slot().lock().unwrap_or_else(|e| e.into_inner());
            if g.manager.is_none() {
                let m = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                    .ok()?
                    .get()
                    .ok()?;
                g.manager = Some(m);
            }
            g.manager.clone()?
        };

        let sessions = manager.GetSessions().ok()?;
        let n = sessions.Size().ok()? as u32;
        if n == 0 {
            return None;
        }

        let mut all: Vec<GlobalSystemMediaTransportControlsSession> = Vec::new();
        for i in 0..n {
            if let Ok(s) = sessions.GetAt(i) {
                all.push(s);
            }
        }
        let playing_of = |s: &GlobalSystemMediaTransportControlsSession| {
            s.GetPlaybackInfo()
                .ok()
                .and_then(|p| p.PlaybackStatus().ok())
                .map(|st| st == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
                .unwrap_or(false)
        };
        let prev_id = watch_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session_id
            .clone();
        let chosen = all
            .iter()
            .find(|s| playing_of(s) && session_key(s) == prev_id)
            .or_else(|| all.iter().find(|s| playing_of(s)))
            .or_else(|| all.iter().find(|s| session_key(s) == prev_id))
            .or_else(|| all.first())
            .cloned()?;

        let key = session_key(&chosen);
        watch_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session_id = key;

        let playing = playing_of(&chosen);
        let props = chosen.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
        let title = props
            .Title()
            .map(|t| t.to_string())
            .unwrap_or_default();
        let artist = props
            .Artist()
            .map(|t| t.to_string())
            .unwrap_or_default();

        // Artwork: decode only when the identity changed (the WinRT property
        // fetch is cheap, the decode is not).
        let art_key = format!("{}|{}|{}", title, artist, playing);
        let need_art = {
            let g = watch_slot().lock().unwrap_or_else(|e| e.into_inner());
            g.art_sig != art_key
        };
        let cover = if need_art {
            let cover = decode_cover(&props);
            watch_slot()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .art_sig = art_key;
            cover
        } else {
            let g = last_slot().lock().unwrap_or_else(|e| e.into_inner());
            g.as_ref().and_then(|p| p.cover.clone())
        };

        Some(MediaNow { title, artist, playing, cover })
    }
}

fn session_key(s: &GlobalSystemMediaTransportControlsSession) -> String {
    // The WinRT object has no cheap handle; SourceAppUserModelId +
    // identity string is stable enough within a poll loop.
    s.SourceAppUserModelId()
        .map(|a| a.to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Cover decode: bytes -> 128px square PNG b64
// ---------------------------------------------------------------------------

fn decode_cover(
    props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Option<String> {
    let bytes = unsafe {
        let thumb = match props.Thumbnail() {
            Ok(t) => t,
            Err(_) => return None,
        };
        let stream = match thumb.OpenReadAsync().and_then(|op| op.get()) {
            Ok(s) => s,
            Err(_) => return None,
        };
        let size = match stream.Size() {
            Ok(s) if s > 0 && s <= 16_000_000 => s as u32,
            _ => return None,
        };
        let reader = match DataReader::CreateDataReader(&stream) {
            Ok(r) => r,
            Err(_) => return None,
        };
        if reader.LoadAsync(size).and_then(|op| op.get()).is_err() {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if reader.ReadBytes(&mut buf).is_err() {
            return None;
        }
        buf
    };

    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i,
        Err(_) => return None,
    };
    let small = img.resize_to_fill(128, 128, image::imageops::FilterType::Triangle);
    let mut png_bytes: Vec<u8> = Vec::new();
    if small
        .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .is_err()
    {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
}

// ---------------------------------------------------------------------------
// Commands (IPC surface)
// ---------------------------------------------------------------------------

/// Boot seed / widget body refresh.
pub fn media_now() -> MediaNow {
    last_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

pub fn media_ctl(action: &str) -> Result<bool, String> {
    com_mta();
    let manager = unsafe {
        GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|e| format!("smtc request failed: {e}"))?
            .get()
            .map_err(|e| format!("smtc wait failed: {e}"))?
    };
    let sessions = unsafe {
        manager
            .GetSessions()
            .map_err(|e| format!("smtc sessions failed: {e}"))?
    };
    let n = unsafe { sessions.Size().map_err(|e| e.to_string())? } as u32;
    if n == 0 {
        return Err("no media session".into());
    }
    // Prefer the playing session, else the watcher's pick, else first.
    let mut all = Vec::new();
    for i in 0..n {
        if let Ok(s) = unsafe { sessions.GetAt(i) } {
            all.push(s);
        }
    }
    let playing_of = |s: &GlobalSystemMediaTransportControlsSession| {
        unsafe {
            s.GetPlaybackInfo()
                .ok()
                .and_then(|p| p.PlaybackStatus().ok())
                .map(|st| st == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
                .unwrap_or(false)
        }
    };
    let prev_id = watch_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .session_id
        .clone();
    let s = all
        .iter()
        .find(|s| playing_of(s))
        .or_else(|| all.iter().find(|s| session_key(s) == prev_id))
        .or_else(|| all.first())
        .ok_or("no media session")?;
    let r = match action {
        "play" => unsafe { s.TryPlayAsync() },
        "pause" => unsafe { s.TryPauseAsync() },
        "toggle" => {
            if playing_of(s) {
                unsafe { s.TryPauseAsync() }
            } else {
                unsafe { s.TryPlayAsync() }
            }
        }
        "next" => unsafe { s.TrySkipNextAsync() },
        "prev" => unsafe { s.TrySkipPreviousAsync() },
        other => return Err(format!("unknown media action '{other}'")),
    };
    r.map_err(|e| format!("{action} failed: {e}"))?
        .get()
        .map_err(|e| format!("{action} failed: {e}"))
}

const FFT_SIZE: usize = 2048;
const N_BANDS: usize = 12;
/// Emit interval: ~45 ms (~22 fps) — fluid enough to feel alive, cheap
/// enough to never show up in the shell's CPU.
const LEVEL_TICK_MS: u64 = 45;
/// Band edges (Hz), log-spaced 55..15000 — the bass band sits where the
/// kick lives, the top band where the hats live.
const BAND_EDGES: [f32; N_BANDS + 1] = [
    55.0, 100.0, 180.0, 320.0, 560.0, 1000.0, 1750.0, 3000.0, 5000.0, 7800.0, 11000.0, 14000.0,
    15500.0,
];

const SILENCE_FLOOR: f32 = 0.0025;

fn levels_loop(app: AppHandle) {
    com_mta();
    let mut ring: Vec<f32> = Vec::with_capacity(FFT_SIZE);
    let mut re = vec![0f32; FFT_SIZE];
    let mut im = vec![0f32; FFT_SIZE];
    let mut bands = vec![0f32; N_BANDS];
    let mut client: Option<IAudioClient> = None;
    let mut capture: Option<IAudioCaptureClient> = None;
    let mut rate = 48000f32;
    let mut block_align = 8usize; // stereo f32
    let mut last_emit = std::time::Instant::now();

    loop {
        let ambient = WANTS_LEVELS.load(Ordering::Relaxed);
        if !ambient {
            // Park: drop the capture device entirely so an idle shell owns
            // zero audio machinery.
            if let Some(c) = client.take() {
                unsafe {
                    let _ = c.Stop();
                }
            }
            capture = None;
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }

        // (Re)open the loopback on demand: endpoint changes / first run.
        if client.is_none() {
            match open_loopback() {
                Ok((c, cap, r, ba)) => {
                    rate = r;
                    block_align = ba;
                    client = Some(c);
                    capture = Some(cap);
                    log(&format!(
                        "ambient: loopback open ({rate} Hz, {block_align} B/frame)"
                    ));
                }
                Err(e) => {
                    log(&format!("ambient: loopback open failed: {e}"));
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    continue;
                }
            }
        }

        let Some(cap) = &capture else { continue };
        let mut got_any = false;
        unsafe {
            // Drain every buffered packet into the ring.
            loop {
                let mut frames = match cap.GetNextPacketSize() {
                    Ok(f) if f > 0 => f,
                    _ => break,
                };
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut fl = 0u32;
                let mut _dp = 0u64;
                let mut _qp = 0u64;
                if cap
                    .GetBuffer(&mut data, &mut frames, &mut fl, Some(&mut _dp), Some(&mut _qp))
                    .is_err()
                {
                    break;
                }
                let silent = (fl & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                let bytes =
                    std::slice::from_raw_parts(data, (frames as usize) * block_align);
                downmix_append(&mut ring, bytes, frames as usize, silent);
                let _ = cap.ReleaseBuffer(frames);
                got_any = true;
            }
        }
        if !got_any {
            std::thread::sleep(std::time::Duration::from_millis(LEVEL_TICK_MS / 2));
        }

        if last_emit.elapsed() >= std::time::Duration::from_millis(LEVEL_TICK_MS) {
            last_emit = std::time::Instant::now();
            let tail = if ring.len() > 512 { 512 } else { ring.len() };
            let sum_sq: f32 = ring[ring.len() - tail..]
                .iter()
                .map(|v| v * v)
                .sum();
            let rms = if tail > 0 { (sum_sq / tail as f32).sqrt() } else { 0.0 };
            if rms < SILENCE_FLOOR {
                for b in bands.iter_mut() {
                    *b *= 0.6;
                }
            } else {
                compute_bands(&ring, &mut re, &mut im, rate, &mut bands);
            }
            let payload: Vec<f32> =
                bands.iter().map(|v| (v * 100.0).round() / 100.0).collect();
            let _ = app.emit_to("hushtop", "hush:levels", &payload);
        }
    }
}

/// Open the default render endpoint in loopback. Returns (client, capture,
/// sample rate, block align bytes per frame).
fn open_loopback() -> windows::core::Result<(IAudioClient, IAudioCaptureClient, f32, usize)> {
    unsafe {
        let enumr: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = enumr.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = dev.Activate::<IAudioClient>(CLSCTX_ALL, None)?;
        let fmt = client.GetMixFormat()?;
        let tag = (*fmt).wFormatTag;
        let rate = (*fmt).nSamplesPerSec as f32;
        let ba = (*fmt).nBlockAlign as usize;
        let float_ok = tag == 3 || tag == 0xFFFE; // WAVE_FORMAT_IEEE_FLOAT | EXTENSIBLE
        if !float_ok || ba == 0 {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-1)));
        }
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            2_000_000, // hundreds of ns -> 200 ms buffer
            0,
            fmt,
            None,
        )?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok((client, capture, rate, ba))
    }
}

unsafe fn downmix_append(ring: &mut Vec<f32>, bytes: &[u8], frames: usize, silent: bool) {
    if silent {
        for _ in 0..frames {
            push_mono(ring, 0.0);
        }
        return;
    }
    // Mix format: float32, channels from the format; assume stereo (the
    // mix is at least stereo on every real endpoint).
    let floats: &[f32] = std::slice::from_raw_parts(bytes.as_ptr() as *const f32, bytes.len() / 4);
    let ch = if frames == 0 {
        2
    } else {
        (floats.len() / frames).max(1)
    };
    for f in floats.chunks(ch) {
        let mut acc = 0f32;
        for v in f {
            acc += *v;
        }
        push_mono(ring, acc / f.len() as f32);
    }
}

fn push_mono(ring: &mut Vec<f32>, v: f32) {
    if ring.len() >= FFT_SIZE {
        let drop = ring.len() + 1 - FFT_SIZE;
        ring.drain(..drop.min(ring.len()));
    }
    ring.push(v);
}

/// Window + FFT + band fold. Output 0..1 per band, with a fast-attack /
/// slow-release envelope so the bars dance on transients and land softly.
fn compute_bands(
    ring: &Vec<f32>,
    re: &mut [f32],
    im: &mut [f32],
    rate: f32,
    bands: &mut [f32],
) {
    let n = FFT_SIZE;
    if ring.len() < n {
        return;
    }
    let start = ring.len() - n;
    for i in 0..n {
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos();
        re[i] = ring[start + i] * w;
        im[i] = 0.0;
    }
    fft_inplace(re, im);

    let bin_hz = rate / n as f32;
    for (bi, band) in bands.iter_mut().enumerate() {
        let lo = (BAND_EDGES[bi] / bin_hz).max(1.0) as usize;
        let hi = ((BAND_EDGES[bi + 1] / bin_hz) as usize).clamp(lo + 1, n / 2);
        let mut peak = 0f32;
        for b in lo..hi {
            let m = (re[b] * re[b] + im[b] * im[b]).sqrt();
            if m > peak {
                peak = m;
            }
        }
        // Amplitude -> 0..1: normalize by n/8 (a full-scale sine peaks at 1),
        // then a dB-shaped curve so quiet details stay quiet.
        let a = (peak / (n as f32 / 8.0)).clamp(0.0, 1.0);
        let v = a.powf(0.55);
        // fast attack, slow release
        *band = if v > *band { v } else { *band * 0.82 + v * 0.18 };
    }
}

/// Iterative radix-2 FFT (no external deps; n is a power of two).
fn fft_inplace(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // bit-reversal permutation
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cwr, mut cwi) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let ur = re[i + k];
                let ui = im[i + k];
                let vr = re[i + k + len / 2] * cwr - im[i + k + len / 2] * cwi;
                let vi = re[i + k + len / 2] * cwi + im[i + k + len / 2] * cwr;
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + len / 2] = ur - vr;
                im[i + k + len / 2] = ui - vi;
                let nwr = cwr * wr - cwi * wi;
                cwi = cwr * wi + cwi * wr;
                cwr = nwr;
            }
            i += len;
        }
        len <<= 1;
    }
}
