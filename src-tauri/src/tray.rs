use serde::Serialize;
use std::io::Write as IoWrite;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use systray_util::{ImageFormat, Systray, SystrayIcon, SystrayEvent};
use tauri::{Emitter, Manager};
use windows::core::w;
use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::NIN_SELECT;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumWindows, FindWindowW, GetClassNameW,
    GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible,
    RegisterWindowMessageW, SendNotifyMessageW, SetForegroundWindow,
    HWND_BROADCAST, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use crate::shared::Shared;
use crate::util;

// ---------------------------------------------------------------------------
// State (mirrors what the frontend needs; insertion-ordered)
// ---------------------------------------------------------------------------

/// One tray icon as stored natively.
#[derive(Clone, PartialEq)]
pub struct TrayIconState {
    /// systray-util stable id ("{hwnd}:{uid}" or a guid string). Echoed
    /// back by the frontend on click.
    pub key: String,
    pub hwnd: isize,
    pub uid: u32,
    pub callback: u32,
    pub version: Option<u32>,
    pub tip: String,
    /// base64 PNG extracted from the icon image.
    pub icon: Option<String>,
    pub process: String,
    pub icon_handle: isize,
    pub visible: bool,
    pub dying: Option<std::time::Instant>,
    pub seq: u64,
    pub exe: String,
}

/// What the frontend receives.
#[derive(Serialize, Clone, PartialEq)]
pub struct TrayDto {
    pub key: String,
    pub tip: String,
    pub icon: Option<String>,
    /// Owner process name — rendered as the text cell when `icon` is None.
    pub process: String,
}

pub fn forward_click_sync(app: &tauri::AppHandle, key: &str, button: &str) -> bool {
    if !matches!(button, "left" | "right" | "middle" | "dbl") {
        util::log(&format!("tray: click dropped — unknown button '{button}'"));
        return false;
    }
    util::log(&format!("tray: click request {button} {key} (sync)"));

    let (icons, ht, hb) = {
        let st = app.state::<crate::AppShared>();
        let s = crate::util::lock_shared(&st.0);
        (s.tray.clone(), s.hwnd_trayfly, s.hwnd_bar)
    };

    // Foreground truth + repair BEFORE anything is sent. Only a process
    // that can take the foreground may grant it (AllowSetForegroundWindow
    // inside send_packed_click below).
    let fg = unsafe { GetForegroundWindow().0 as isize };
    if hwnd_is_ours(fg) {
        util::log(&format!("tray: fg ok ({fg:#x} is ours)"));
    } else {
        let repaired = repair_foreground(ht, hb);
        util::log(&format!(
            "tray: fg was {fg:#x} (not ours) — repair {}",
            if repaired { "succeeded" } else { "FAILED (grant may not deliver)" }
        ));
    }

    let Some(t) = icons.iter().find(|t| t.key == key) else {
        util::log(&format!(
            "tray: {key} not tracked ({} icon(s) listed) — click dropped",
            icons.len()
        ));
        return false;
    };
    if t.hwnd == 0 || t.callback == 0 {
        util::log(&format!(
            "tray: {key} missing owner data (hwnd {} cb {}) — click dropped",
            t.hwnd, t.callback
        ));
        return false;
    }
    let (pid, exe) = hwnd_owner_info(t.hwnd);
    util::log(&format!(
        "tray: target hwnd={:#x} uid={} cb={} v={:?} pid={pid} app={exe}",
        t.hwnd, t.uid, t.callback, t.version
    ));

    send_packed_click(t, button);
    let fg_after = unsafe { GetForegroundWindow().0 as isize };
    util::log(&format!(
        "tray: forward done fg_after={fg_after:#x} ours={} (grant needs ours)",
        hwnd_is_ours(fg_after)
    ));
    true
}

static REFRESH_PENDING: AtomicBool = AtomicBool::new(false);

/// Called from the heartbeat beat (any thread): schedule one refresh cycle.
pub fn request_refresh() {
    REFRESH_PENDING.store(true, Ordering::Relaxed);
}

static RESEED_PENDING: AtomicBool = AtomicBool::new(false);

pub fn request_reseed() {
    RESEED_PENDING.store(true, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Tracing bridge — the crate logs everything through `tracing`; without a
// subscriber every spy failure is completely silent. Route INFO+ into
// hushell.log (same file util::log uses).
// ---------------------------------------------------------------------------

struct LogBridge;

impl IoWrite for LogBridge {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(dir) = crate::config::data_dir() {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("hushell.log"))
            {
                let _ = f.write_all(buf);
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBridge {
    type Writer = LogBridge;
    fn make_writer(&'a self) -> Self::Writer {
        LogBridge
    }
}

fn install_tracing_bridge() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::INFO)
        .with_ansi(false)
        .with_writer(LogBridge)
        .try_init();
}

// ---------------------------------------------------------------------------
// Engine thread
// ---------------------------------------------------------------------------

pub fn spawn_engine(app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("tray-engine".into())
        .spawn(move || engine_main(app))
        .map_err(|e| util::log(&format!("tray: spawn failed: {e}")))
        .ok();
}

static RULES_POKE: std::sync::OnceLock<tokio::sync::mpsc::Sender<()>> = std::sync::OnceLock::new();

fn engine_main(app: tauri::AppHandle) {
    util::log("tray: v0.2.20 engine starting (systray-util Shell_TrayWnd stream; silence reap REMOVED)");
    install_tracing_bridge();

    let mut systray = match Systray::new() {
        Ok(s) => s,
        Err(e) => {
            util::log(&format!("tray: Systray::new failed: {e:?}"));
            return;
        }
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            util::log(&format!("tray: tokio runtime failed: {e}"));
            return;
        }
    };

    util::log("tray: spy created — TaskbarCreated broadcast sent, waiting for stream events (clicks forward synchronously on the IPC thread since v0.2.17)");

    let mut icons: Vec<TrayIconState> = Vec::new();
    let mut logged_first = false;
    let mut events: u64 = 0;
    // 1s tick slots without any stream event (drives status/watchdog/reseed).
    let mut idle: u64 = 0;
    let mut reseeds: u32 = 0;
    let mut squat_missing = false;
    let (poke_tx, mut poke_rx) = tokio::sync::mpsc::channel::<()>(1);
    let _ = RULES_POKE.set(poke_tx);
    let mut poked = false;
    let mut recolor = current_recolor(&app);
    let mut recolor_check = std::time::Instant::now();
    let mut last_reseed: Option<std::time::Instant> = None;

    // catch_unwind: a panic inside the loop must still be observable — the
    // panic hook logs the PANIC line itself.
    let engine_body = std::panic::AssertUnwindSafe(|| {
        rt.block_on(async move {
            loop {
                if poked || recolor_check.elapsed() >= Duration::from_secs(1) {
                    poked = false;
                    recolor_check = std::time::Instant::now();

                    let want_recolor = current_recolor(&app);
                    if want_recolor != recolor {
                        recolor = want_recolor;
                        util::log(&format!(
                            "tray: render rules -> recolor={recolor}; re-rendering {} icon(s) from crate registry",
                            icons.len()
                        ));
                        let registry = systray.icons();
                        let mut changed = false;
                        for t in icons.iter_mut() {
                            if let Some(cr) = registry
                                .iter()
                                .find(|i| i.stable_id.to_string() == t.key)
                            {
                                if let Some(png) = render_icon_png(cr, recolor) {
                                    if t.icon != Some(png.clone()) {
                                        t.icon = Some(png);
                                        changed = true;
                                    }
                                }
                            }
                        }
                        if changed {
                            emit(&app, &icons);
                        }
                    }

                    if REFRESH_PENDING.swap(false, Ordering::Relaxed) {
                        if squat_alive() {
                            let registry = systray.icons();
                            let mut changed = false;
                            for t in icons.iter_mut() {
                                if t.icon.is_some() {
                                    continue;
                                }
                                if let Some(cr) = registry
                                    .iter()
                                    .find(|i| i.stable_id.to_string() == t.key)
                                {
                                    if let Some(png) = render_icon_png(cr, recolor) {
                                        t.icon = Some(png);
                                        changed = true;
                                        util::log(&format!(
                                            "tray: resurrected image for {} ({})",
                                            t.key, t.process
                                        ));
                                    }
                                }
                            }
                            if changed {
                                emit(&app, &icons);
                            }
                        } else {
                            util::log(
                                "tray: refresh skipped — spy window gone (stream cannot recover)",
                            );
                        }
                    }

                    if RESEED_PENDING.swap(false, Ordering::Relaxed) {
                        let ok_time = last_reseed
                            .map_or(true, |t| t.elapsed() >= Duration::from_secs(3));
                        if ok_time && !icons.is_empty() {
                            // Icons arrived between the open and this tick —
                            // nothing to seed anymore.
                            util::log("tray: open-path reseed skipped — icons already present");
                        } else if ok_time && squat_alive() {
                            util::log(
                                "tray: reseed on open — TaskbarCreated broadcast (tray was empty)",
                            );
                            reseed_taskbar_created();
                            last_reseed = Some(std::time::Instant::now());
                            idle = 0;
                        }
                    }
                }

                tokio::select! {
                    ev = systray.events() => match ev {
                        Some(event) => {
                            events += 1;
                            idle = 0;
                            if !logged_first {
                                logged_first = true;
                                util::log("tray: first stream event received — spy is live");
                            }
                            if apply_event(&mut icons, event, recolor) {
                                emit(&app, &icons);
                            }
                        }
                        None => {
                            util::log("tray: stream closed (spy thread ended)");
                            break;
                        }
                    },
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        idle += 1;
                        let mut changed = sweep_dead(&mut icons);
                        changed |= finalize_dying(&mut icons);
                        if changed {
                            emit(&app, &icons);
                        }
                        if idle % 10 == 0 {
                            status_line(&icons, events);
                            if !squat_alive() && !squat_missing {
                                squat_missing = true;
                                util::log(
                                    "tray: FATAL spy window gone — the icon stream cannot recover; restart hushell",
                                );
                            }
                        }
                        if icons.is_empty() {
                            let wait: u64 = if reseeds == 0 { 6 } else { 30 };
                            if idle >= wait {
                                reseeds += 1;
                                util::log(&format!(
                                    "tray: no icons after {}s — re-broadcasting TaskbarCreated (attempt {})",
                                    idle, reseeds
                                ));
                                reseed_taskbar_created();
                                last_reseed = Some(std::time::Instant::now());
                                idle = 0;
                            }
                        }
                    }
                    _ = poke_rx.recv() => {
                        poked = true;
                    }
                }
            }
        })
    });

    let result = std::panic::catch_unwind(engine_body);
    if result.is_err() {
        util::log("tray: engine PANICKED — icon updates stopped until hushell restarts (see PANIC line above)");
    }

    util::log("tray: engine exited");
}

// ---------------------------------------------------------------------------
// Event application
// ---------------------------------------------------------------------------

/// Applies one stream event; returns true when the display list changed.
fn apply_event(
    icons: &mut Vec<TrayIconState>,
    ev: SystrayEvent,
    recolor: bool,
) -> bool {
    match ev {
        SystrayEvent::IconAdd(ic) | SystrayEvent::IconUpdate(ic) => {
            let key = ic.stable_id.to_string();
            let png = render_icon_png(&ic, recolor);
            let hwnd = ic.window_handle.unwrap_or(0);
            let (_pid, exe_full) = hwnd_owner_info(hwnd);
            let entry = TrayIconState {
                key: key.clone(),
                hwnd,
                uid: ic.uid.unwrap_or(0),
                callback: ic.callback_message.unwrap_or(0),
                version: ic.version,
                tip: ic.tooltip.clone(),
                icon: png,
                process: process_from_owner(&exe_full, &ic.tooltip),
                icon_handle: ic.icon_handle.unwrap_or(0),
                visible: ic.is_visible,
                dying: None,
                seq: ICON_SEQ.fetch_add(1, Ordering::Relaxed),
                exe: exe_full.to_lowercase(),
            };
            if let Some(slot) = icons.iter_mut().find(|t| t.key == key) {
                let mut e = entry;
                if e.icon.is_none() {
                    e.icon = slot.icon.clone();
                }
                if *slot != e {
                    *slot = e;
                    return true;
                }
                false
            } else {
                let mut replaced: Option<String> = None;
                if entry.hwnd != 0 {
                    if let Some(pos) = icons
                        .iter()
                        .position(|t| t.key != key && t.hwnd == entry.hwnd && t.uid == entry.uid)
                    {
                        replaced = Some(icons.remove(pos).key);
                    }
                } else if entry.icon_handle != 0 {
                    if let Some(pos) = icons.iter().position(|t| {
                        t.key != key && t.hwnd == 0 && t.icon_handle == entry.icon_handle
                    }) {
                        replaced = Some(icons.remove(pos).key);
                    }
                }
                if let Some(old) = replaced {
                    util::log(&format!(
                        "tray: identity flip {old} -> {key} (same icon re-keyed, replaced)"
                    ));
                }
                util::log(&format!(
                    "tray: add {} uid {} cb {} v{:?} vis {} img {} ({} icon(s) tracked)",
                    key,
                    entry.uid,
                    entry.callback,
                    entry.version,
                    entry.visible,
                    if entry.icon.is_some() { "yes" } else { "no" },
                    icons.len() + 1
                ));
                icons.push(entry);
                true
            }
        }
        SystrayEvent::IconRemove(id) => {
            let key = id.to_string();
            if let Some(slot) = icons.iter_mut().find(|t| t.key == key) {
                if slot.dying.is_none() {
                    slot.dying = Some(std::time::Instant::now());
                    util::log(&format!(
                        "tray: remove {} — marked, 1s grace (re-add dance suppression)",
                        key
                    ));
                }
            }
            false
        }
    }
}

fn finalize_dying(icons: &mut Vec<TrayIconState>) -> bool {
    let now = std::time::Instant::now();
    let before = icons.len();
    icons.retain(|t| match t.dying {
        Some(d) => now.duration_since(d) < Duration::from_secs(1),
        None => true,
    });
    if icons.len() != before {
        util::log(&format!(
            "tray: grace expired for {} icon(s) — finalized removal",
            before - icons.len()
        ));
        true
    } else {
        false
    }
}

fn sweep_dead(icons: &mut Vec<TrayIconState>) -> bool {
    let before = icons.len();
    icons.retain(|t| {
        t.hwnd == 0 || unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(HWND(t.hwnd as _)) }.as_bool()
    });
    icons.len() != before
}

// ---------------------------------------------------------------------------
// Emission + click forwarding
// ---------------------------------------------------------------------------

const HIDDEN_PROCESSES: &[&str] = &["taskmgr.exe"];

fn render_list(icons: &[TrayIconState]) -> Vec<TrayDto> {
    let eligible: Vec<usize> = icons
        .iter()
        .enumerate()
        .filter(|(_i, t)| t.icon.is_some() || t.hwnd != 0)
        .filter(|(_i, t)| !HIDDEN_PROCESSES.contains(&t.exe.as_str()))
        .map(|(i, _)| i)
        .collect();
    let mut best: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for &i in &eligible {
        let t = &icons[i];
        if t.exe.is_empty() {
            continue; // unknown owner: cannot dedupe, keep as-is
        }
        match best.get(t.exe.as_str()) {
            Some(&j) if icons[j].seq >= t.seq => {}
            _ => {
                best.insert(t.exe.as_str(), i);
            }
        }
    }
    eligible
        .into_iter()
        .filter(|&i| {
            let t = &icons[i];
            t.exe.is_empty() || best.get(t.exe.as_str()) == Some(&i)
        })
        .map(|i| {
            let t = &icons[i];
            TrayDto {
                key: t.key.clone(),
                tip: t.tip.clone(),
                icon: t.icon.clone(),
                process: t.process.clone(),
            }
        })
        .collect()
}

fn emit(app: &tauri::AppHandle, icons: &[TrayIconState]) {
    let list: Vec<TrayDto> = render_list(icons);

    let st = app.state::<crate::AppShared>();
    let mut size_changed = false;
    {
        let mut s = st.0.lock().unwrap_or_else(|e| e.into_inner());
        s.tray = icons.to_vec();
        if list == s.tray_last {
            return;
        }
        s.tray_last = list.clone();
        if s.trayfly_visible && list.len() != s.trayfly_geo_n {
            s.trayfly_geo_n = list.len();
            size_changed = true;
        }
    }
    // Only the tray flyout renders tray icons — a broadcast used to push
    // the full base64 list to every webview (hushtop, bars, prompt...) on
    // each change for nothing.
    let _ = app.emit_to("trayfly", "tray_icons", &list);
    if size_changed {
        crate::thumbs::trayfly_sync_size(app);
    }
}

fn send_packed_click(t: &TrayIconState, button: &str) {
    let down_up: &[(u32, u32)] = match button {
        "left" => &[(WM_LBUTTONDOWN, WM_LBUTTONUP)],
        "right" => &[(WM_RBUTTONDOWN, WM_RBUTTONUP)],
        "middle" => &[(WM_MBUTTONDOWN, WM_MBUTTONUP)],
        "dbl" => &[(WM_LBUTTONDBLCLK, WM_LBUTTONUP)],
        _ => return,
    };
    let target = HWND(t.hwnd as _);
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
    }
    let v4 = t.version.is_some_and(|v| v > 3);
    let v3 = t.version.is_some_and(|v| v >= 3);
    let mut cursor = (0i32, 0i32);
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_ok() {
            cursor = (pt.x, pt.y);
        }
    }
    // The EVENT rides in wParam/lParam; the WINDOW MESSAGE is the callback id.
    let send_event = |event: u32| {
        let (wp, lp) = if v4 {
            (
                ((cursor.0 as u16 as usize) | ((cursor.1 as u16 as usize) << 16)),
                (((event as u16 as isize) & 0xFFFF)
                    | (((t.uid as u16 as isize) & 0xFFFF) << 16)),
            )
        } else {
            (t.uid as usize, (event as i16 as isize) & 0xFFFF)
        };
        unsafe {
            let _ = SendNotifyMessageW(
                target,
                t.callback,
                WPARAM(wp),
                LPARAM(lp),
            );
        }
    };
    for (down, up) in down_up {
        send_event(*down);
        send_event(*up);
    }
    // Version 3+: the "modern" notification (ref: Shell_NotifyIcon remarks).
    if v3 {
        match button {
            "left" | "dbl" => send_event(NIN_SELECT),
            "right" => send_event(WM_CONTEXTMENU),
            _ => {}
        }
    }
    match button {
        "left" | "dbl" if t.version.is_none() => {
            let wp = (cursor.0 as u16 as usize) | ((cursor.1 as u16 as usize) << 16);
            let lp = ((NIN_SELECT as u16 as isize) & 0xFFFF)
                | (((t.uid as u16 as isize) & 0xFFFF) << 16);
            unsafe {
                let _ = SendNotifyMessageW(target, t.callback, WPARAM(wp), LPARAM(lp));
            }
            util::log("tray: complement NIN_SELECT (v4 packing) sent");
        }
        "right" if t.version.is_none() => {
            unsafe {
                let _ = SendNotifyMessageW(
                    target,
                    t.callback,
                    WPARAM(t.uid as usize),
                    LPARAM(WM_CONTEXTMENU as isize),
                );
            }
            util::log("tray: complement WM_CONTEXTMENU (v3 packing) sent");
        }
        _ => {}
    }
    util::log(&format!(
        "tray: forwarded {button} (sync) to hwnd={:#x} cb={} uid={} v={:?}",
        t.hwnd, t.callback, t.uid, t.version
    ));
}

// ---------------------------------------------------------------------------
// Click-support helpers: foreground truth + owner identity + recolor render
// ---------------------------------------------------------------------------

fn hwnd_is_ours(h: isize) -> bool {
    if h == 0 {
        return false;
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(HWND(h as _), Some(&mut pid));
    }
    pid != 0 && pid == unsafe { GetCurrentProcessId() }
}

static ICON_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn process_from_owner(exe: &str, tip: &str) -> String {
    let stem = exe
        .strip_suffix(".exe")
        .or_else(|| exe.strip_suffix(".EXE"))
        .unwrap_or(exe);
    if stem != "?" && !stem.is_empty() {
        return stem.to_string();
    }
    let tip = tip.trim();
    if !tip.is_empty() {
        return tip.to_string();
    }
    "app".to_string()
}

/// (pid, exe-name) of the process owning the hwnd — click diagnostics.
fn hwnd_owner_info(h: isize) -> (u32, String) {
    if h == 0 {
        return (0, "?".into());
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(HWND(h as _), Some(&mut pid)) };
    let mut exe = String::from("?");
    if pid != 0 {
        unsafe {
            if let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 1024];
                let mut len = buf.len() as u32;
                if QueryFullProcessImageNameW(
                    p,
                    PROCESS_NAME_WIN32,
                    windows::core::PWSTR(buf.as_mut_ptr()),
                    &mut len,
                )
                .is_ok()
                {
                    let full = String::from_utf16_lossy(&buf[..len as usize]);
                    if let Some(name) = full.rsplit(['\\', '/']).next() {
                        exe = name.to_string();
                    }
                }
                let _ = CloseHandle(p);
            }
        }
    }
    (pid, exe)
}

fn repair_foreground(ht: isize, hb: isize) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let my_thread = GetCurrentThreadId();
        let _ = AttachThreadInput(fg_thread, my_thread, true);
        for h in [ht, hb] {
            if h == 0 || !IsWindowVisible(HWND(h as _)).as_bool() {
                continue;
            }
            let _ = SetForegroundWindow(HWND(h as _));
            if hwnd_is_ours(GetForegroundWindow().0 as isize) {
                break;
            }
        }
        let _ = AttachThreadInput(fg_thread, my_thread, false);
    }
    hwnd_is_ours(unsafe { GetForegroundWindow().0 as isize })
}

/// Current recolor setting from shared config (engine-side poll).
fn current_recolor(app: &tauri::AppHandle) -> bool {
    let st = app.state::<crate::AppShared>();
    let on = st
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .cfg
        .recolor_icons;
    on
}

fn render_icon_png(ic: &SystrayIcon, recolor: bool) -> Option<String> {
    let (rgba, w, h) = icon_rgba_any(ic)?;
    if recolor {
        let (mut rgba, w, h) =
            crate::icons::resize_box(rgba, w, h, crate::icons::CANONICAL);
        // Dark-only shell: recolored tray icons always print in chalk tone.
        crate::icons::to_chalk_tone(&mut rgba);
        return crate::icons::png_b64(&rgba, w, h);
    }
    crate::icons::png_b64(&rgba, w, h)
}

/// Best-effort RGBA extraction for a tray icon: crate image first, own HICON
/// extraction as fallback. Recolor applies to BOTH paths (in render_icon_png).
fn icon_rgba_any(ic: &SystrayIcon) -> Option<(Vec<u8>, i32, i32)> {
    if let Ok(bytes) = ic.to_image_format(ImageFormat::Png) {
        if let Some(rgba) = decode_png_rgba(&bytes) {
            return Some(rgba);
        }
    }
    let h = ic.icon_handle.filter(|h| *h != 0)?;
    crate::icons::hicon_rgba_fallback(h)
}

fn decode_png_rgba(bytes: &[u8]) -> Option<(Vec<u8>, i32, i32)> {
    let dec = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some((buf, info.width as i32, info.height as i32))
}

/// Which window currently wins the FindWindow race that Shell_NotifyIcon
/// plays for every icon registration. `ours` = the squat is winning and
/// traffic should reach us; `explorer` = traffic bypasses us.
fn find_window_winner() -> &'static str {
    unsafe {
        match FindWindowW(w!("Shell_TrayWnd"), None) {
            Ok(h) => {
                let mut pid = 0u32;
                let _ = GetWindowThreadProcessId(h, Some(&mut pid));
                if pid != 0 && pid == GetCurrentProcessId() {
                    "ours"
                } else {
                    "explorer"
                }
            }
            Err(_) => "none",
        }
    }
}

fn squat_alive() -> bool {
    struct Ctx {
        me: u32,
        found: bool,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let ctx = &mut *(l.0 as *mut Ctx);
        let mut buf = [0u16; 16];
        let n = GetClassNameW(h, &mut buf);
        if n > 0 && String::from_utf16_lossy(&buf[..n.max(0) as usize]) == "Shell_TrayWnd" {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid != 0 && pid == ctx.me {
                ctx.found = true;
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut ctx = Ctx {
        me: unsafe { GetCurrentProcessId() },
        found: false,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.found
}

fn status_line(icons: &[TrayIconState], events: u64) {
    let hidden = icons.iter().filter(|t| !t.visible).count();
    let iconless = icons.iter().filter(|t| t.icon.is_none()).count();
    let render = render_list(icons).len();
    util::log(&format!(
        "tray: status tracked={} render={} hidden={} iconless={} events={} fw={}",
        icons.len(),
        render,
        hidden,
        iconless,
        events,
        find_window_winner()
    ));
}

fn reseed_taskbar_created() {
    unsafe {
        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        if msg != 0 {
            let _ = SendNotifyMessageW(HWND_BROADCAST, msg, None, None);
        }
    }
}

// ---------------------------------------------------------------------------
// Real Shell_TrayWnd lookup (spy-aware) — used by native.rs
// ---------------------------------------------------------------------------

struct EnumCtx {
    me: u32,
    found: isize,
}

unsafe extern "system" fn real_tray_cb(h: HWND, l: LPARAM) -> BOOL {
    let ctx = &mut *(l.0 as *mut EnumCtx);
    let mut buf = [0u16; 16];
    let n = GetClassNameW(h, &mut buf);
    let cls = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if cls == "Shell_TrayWnd" {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != 0 && pid != ctx.me {
            ctx.found = h.0 as isize;
            return BOOL(0);
        }
    }
    BOOL(1)
}

pub fn find_real_shell_tray() -> Option<isize> {
    let mut ctx = EnumCtx {
        me: unsafe { GetCurrentProcessId() },
        found: 0,
    };
    unsafe {
        let _ = EnumWindows(Some(real_tray_cb), LPARAM(
            &mut ctx as *mut EnumCtx as isize,
        ));
    }
    (ctx.found != 0).then_some(ctx.found)
}

/// Keep the shared-state shape referenced (sync_state pulls tray_last).
#[allow(dead_code)]
fn _shape_check(s: &Shared) -> Vec<TrayDto> {
    s.tray_last.clone()
}
