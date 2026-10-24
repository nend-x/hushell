use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use base64::Engine as _;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::util::log;

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
    /// The playback clock projector (see poll_once): the last ACCEPTED raw
    /// SMTC position, when it was accepted, and what the clock reads now.
    pos: PosClock,
    /// The session the clock was built for (a track/session change restarts it).
    pos_key: String,
}

/// The local playback clock. SMTC's Position is a LAZY snapshot — plenty of
/// players refresh it only every few seconds (or on events), and blindly
/// accepting it every poll made the widget's highlight drag backward (the
/// "line only switches when the previous one is finished" bug), while the
/// old LastUpdatedTime projection made it OVERSHOOT on players that leave
/// that stamp stale (the clock flying through the lyrics). The cure is the
/// one other lyric apps use: trust raw snapshots only when they move like
/// playback (progress or a real seek), and advance the clock LOCALLY in
/// between.
#[derive(Clone, Copy)]
struct PosClock {
    raw: f64,
    at: std::time::Instant,
    playing: bool,
    out: f64,
}

impl Default for PosClock {
    fn default() -> Self {
        PosClock {
            raw: 0.0,
            at: std::time::Instant::now(),
            playing: false,
            out: 0.0,
        }
    }
}

impl Default for WatchState {
    fn default() -> Self {
        WatchState {
            manager: None,
            session_id: String::new(),
            props_sig: String::new(),
            art_sig: String::new(),
            pos: PosClock::default(),
            pos_key: String::new(),
        }
    }
}

fn last_slot() -> &'static Mutex<Option<MediaNow>> {
    LAST.get_or_init(|| Mutex::new(None))
}

fn watch_slot() -> &'static Mutex<WatchState> {
    WATCH.get_or_init(|| Mutex::new(WatchState::default()))
}

/// One media snapshot as the media widget consumes it. The position block
/// (duration/position) feeds the lyrics widget's line highlighter — it
/// interpolates locally between the 1 Hz `hush:media_pos` anchors.
#[derive(Serialize, Clone, Default)]
pub struct MediaNow {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub cover: Option<String>,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub position: f64,
}

/// The 1 Hz playback anchor for the lyrics widget (no artwork round-trip).
#[derive(Serialize, Clone, Default)]
pub struct MediaPos {
    pub position: f64,
    pub duration: f64,
    pub playing: bool,
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
    std::thread::Builder::new()
        .name("media-watch".into())
        .spawn(move || {
            crate::util::register_thread("media-watch");
            watch_loop(app);
        })
        .expect("media watch thread");
    log("media: engine started (smtc watcher)");
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
                let _ = app.emit("hush:media", &now);
                log(&format!(
                    "media: \"{}\" — {}{}",
                    now.title,
                    now.artist,
                    if now.playing { " (playing)" } else { " (paused)" }
                ));
            }
            // The boot-seed snapshot stays LIVE regardless of identity:
            // media_now() consumers (a fresh CD widget's first paint, the
            // lyrics fetch's duration) must read the current clock, not the
            // one frozen at the last track change.
            *last_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(now.clone());
            // The lyrics widget's anchor: fresh EVERY poll while a session
            // exists (1 Hz) — it interpolates between these stamps locally,
            // so a stable position with no event would stall the highlight.
            let _ = app.emit(
                "hush:media_pos",
                &MediaPos {
                    position: now.position,
                    duration: now.duration,
                    playing: now.playing,
                },
            );
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
                let _ = app.emit(
                    "hush:media_pos",
                    &MediaPos {
                        position: now.position,
                        duration: now.duration,
                        playing: false,
                    },
                );
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
            .session_id = key.clone();

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
        let album = props
            .AlbumTitle()
            .map(|t| t.to_string())
            .unwrap_or_default();

        // Playback clock for the lyrics highlighter. Sessions that do not
        // publish a timeline (some browser streams) leave zeros — the widget
        // then falls back to unsynced display instead of a frozen line.
        // DURATION is the timeline's span (End − Start), not the raw End:
        // players that publish a non-zero StartTime (stream progress baked
        // into the timeline) made the raw End read long by exactly Start —
        // and the lyrics sources then matched a 30-minute “song”.
        let (mut position, mut duration) = (0f64, 0f64);
        if let Ok(tl) = chosen.GetTimelineProperties() {
            // WinRT TimeSpan: 100 ns ticks.
            let raw = tl.Position().map(|t| t.Duration).unwrap_or(0) as f64 / 10_000_000.0;
            let end = tl.EndTime().map(|t| t.Duration).unwrap_or(0) as f64 / 10_000_000.0;
            let start = tl.StartTime().map(|t| t.Duration).unwrap_or(0) as f64 / 10_000_000.0;
            duration = if end > start { end - start } else { end };
            // THE PLAYBACK CLOCK — raw snapshots + local projection:
            //   * raw moved like playback (caught up with the projection, or
            //     ran ahead of it)  -> trust it, re-stamp;
            //   * raw jumped BACKWARD  -> a real seek, trust it;
            //   * raw is the same stale snapshot -> keep advancing locally.
            // The projection is capped at the track's end. No LastUpdatedTime
            // math: players that refresh that stamp lazily made the old drift
            // projection overshoot by seconds-to-minutes.
            let now = std::time::Instant::now();
            let mut g = watch_slot().lock().unwrap_or_else(|e| e.into_inner());
            if !playing {
                g.pos = PosClock {
                    raw,
                    at: now,
                    playing: false,
                    out: raw,
                };
            } else if !g.pos.playing || g.pos_key != key {
                // resume or track/session change: start from the snapshot
                g.pos = PosClock {
                    raw,
                    at: now,
                    playing: true,
                    out: raw,
                };
            } else {
                let expected = g.pos.raw + now.duration_since(g.pos.at).as_secs_f64();
                if raw >= expected - 0.45 || raw < g.pos.raw - 1.5 {
                    g.pos = PosClock {
                        raw,
                        at: now,
                        playing: true,
                        out: raw,
                    };
                } else {
                    let cap = if duration > 0.0 { duration } else { f64::INFINITY };
                    g.pos.out = expected.min(cap);
                }
            }
            // The clock's READING is g.pos.out in EVERY branch: an accepted
            // snapshot re-stamps at `raw`, a stale one reads the projection,
            // a pause holds at its raw. (The first draft assigned `position`
            // only inside the stale-snapshot arm — an accepted snapshot or a
            // resume read 0.0, and the widget's 1 Hz anchor treated that as a
            // seek and snapped the karaoke clock to the top of the song.)
            position = g.pos.out;
            g.pos_key = key.clone();
        }

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

        Some(MediaNow {
            title,
            artist,
            playing,
            cover,
            album,
            duration,
            position,
        })
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
    // 384 px: the CD widget spins the art at ~148 CSS px — 128 was soft on
    // a 150 % DPI display and mushy at 200 %. Still one small PNG per track
    // change, never a stream.
    let small = img.resize_to_fill(384, 384, image::imageops::FilterType::Triangle);
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

