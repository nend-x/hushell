use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    SHGetKnownFolderPath, FOLDERID_CommonStartMenu, FOLDERID_Desktop, FOLDERID_Documents,
    FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_PublicDesktop,
    FOLDERID_StartMenu, FOLDERID_Videos, KNOWN_FOLDER_FLAG,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindow, IsWindowVisible, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_SHOWWINDOW, SW_HIDE,
};

use crate::shared::{Shared, BAR_H};
use crate::util::{dpi_for, log, screen_height};

// Logical size of the menu window (CSS px at 96 dpi). The webview sheet is
// designed against these numbers; the native place dance scales by DPI.
const SM_W: f64 = 640.0;
const SM_H: f64 = 560.0;

fn hwnd(v: isize) -> HWND {
    HWND(v as _)
}

static MENU_OPEN: AtomicBool = AtomicBool::new(false);

static LAST_RECLAIM_MS: AtomicU64 = AtomicU64::new(0);

static LAST_RECLAIM_LOG_MS: AtomicU64 = AtomicU64::new(0);

static BELT_STOOD_DOWN: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Window toggle / show-hide handshake (Hush_UI launcher pattern)
// ---------------------------------------------------------------------------

/// Win-key tap entry point: open when closed, close when open. Logical
/// state, not window visibility — during the close animation the window is
/// still visible, so is_visible() would mis-route a rapid toggle.
pub fn toggle(app: &AppHandle) {
    if MENU_OPEN.load(Ordering::SeqCst) {
        hide(app);
    } else {
        show(app);
    }
}

pub fn show(app: &AppHandle) {
    if crate::lockscreen::is_locked() {
        log("startmenu: show refused — shell locked");
        return;
    }
    // Cancel any pending delayed hide from an interrupted close.
    MENU_OPEN.store(true, Ordering::SeqCst);
    BELT_STOOD_DOWN.store(false, Ordering::SeqCst);
    let st = app.state::<crate::AppShared>();
    {
        let mut s = crate::util::lock_shared(&st.0);
        s.startmenu_visible = true;
        s.startmenu_opened_at = Some(Instant::now());
        s.startmenu_prev_fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }
            .0 as isize;
        s.startmenu_ever_fg = false;
    }
    log("startmenu: show queued (place + paired show on main thread)");
    let a2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if !place_inner(&a2) {
            return;
        }
        show_window_now(&a2);
        let _ = a2.emit("startmenu:show", ());
        log("startmenu: shown (window on screen, show event broadcast)");
    });
}

/// Main-thread dance: bottom-left of the PRIMARY display, just above the
/// bar, DPI-scaled. Deliberately screen metrics (primary), never
/// MonitorFromWindow — the start menu fires ONLY on main, no matter which
/// display the Win key was pressed on or which bar sits under the cursor.
/// Returns false when the menu closed before the dispatch landed.
fn place_inner(app: &AppHandle) -> bool {
    let st = app.state::<crate::AppShared>();
    let hm = {
        let s = crate::util::lock_shared(&st.0);
        if !s.startmenu_visible {
            return false;
        }
        s.hwnd_startmenu
    };
    if hm == 0 {
        log("startmenu: place SKIPPED — no hwnd (window creation failed?)");
        return false;
    }
    unsafe {
        let h = hwnd(hm);
        if !IsWindow(h).as_bool() {
            log("startmenu: place SKIPPED — hwnd dead");
            return false;
        }
        let dpi = dpi_for(h) as i32;
        let w = (SM_W * dpi as f64 / 96.0).round() as i32;
        let ht = (SM_H * dpi as f64 / 96.0).round() as i32;
        let bar_h = BAR_H * dpi / 96;
        let m = 10 * dpi / 96;
        let x = m;
        let y = screen_height() - bar_h - ht - m;
        let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, ht, SWP_SHOWWINDOW);
    }
    true
}

/// The paired show (Hush_UI order): resume the WEBVIEW first — the
/// controller re-presents its last frame instantly, so the reveal never
/// shows a dead surface — then reveal the window, then take focus.
fn show_window_now(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("startmenu") {
        let _ = w.as_ref().show();
        let _ = w.show();
        let _ = w.eval("if(window.__hushMenuOpen)window.__hushMenuOpen();");
        let _ = w.set_focus();
    }
    let st = app.state::<crate::AppShared>();
    let hm = crate::util::lock_shared(&st.0).hwnd_startmenu;
    if hm != 0 {
        let ok = crate::native::activate_hwnd(hm);
        if ok {
            if let Some(w) = app.get_webview_window("startmenu") {
                let _ = w.set_focus(); // keyboard focus into the webview too
            }
            log("startmenu: focused (ladder rung stuck)");
        } else {
            log("startmenu: activation denied at show — belt will reclaim");
        }
        // The keyboard must live in the BROWSER CHILD, not just on the host
        // window — MoveFocus is the only primitive that puts it there, and
        // without it the menu ignores every keystroke until the user clicks
        // inside it (the “click empty space before typing” bug).
        crate::native::webview_keys(app, "startmenu");
        if !ok {
            log("startmenu: activation did not stick — focus hammer at 120 ms (2 s budget)");
            let a3 = app.clone();
            std::thread::Builder::new()
                .name("startmenu-focus-hammer".into())
                .spawn(move || {
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_millis(2000);
                    let mut tries = 0u32;
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        if !MENU_OPEN.load(Ordering::SeqCst) {
                            return; // closed again — nothing to hold
                        }
                        let fg = unsafe {
                            windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
                                .0 as isize
                        };
                        if fg == hm {
                            if tries > 0 {
                                log(&format!(
                                    "startmenu: activation stuck on hammer try {}",
                                    tries + 1
                                ));
                            }
                            return;
                        }
                        let fighting = crate::startmenu_killer::foreground_is_flyout_host()
                            || crate::startmenu_killer::ms_since_last_kill() < 1500;
                        if !fighting {
                            return;
                        }
                        tries += 1;
                        crate::native::activate_hwnd(hm);
                        crate::native::webview_keys(&a3, "startmenu");
                        if std::time::Instant::now() >= deadline {
                            log("startmenu: activation hammer exhausted — belt hold decides");
                            return;
                        }
                    }
                })
                .ok();
        }
    }
}

fn hide_window_now(app: &AppHandle) {
    if MENU_OPEN.load(Ordering::SeqCst) {
        log("startmenu: late hide VETOED — menu was re-opened");
        return;
    }
    let st = app.state::<crate::AppShared>();
    let (hm, prev_fg) = {
        let s = crate::util::lock_shared(&st.0);
        (s.hwnd_startmenu, s.startmenu_prev_fg)
    };
    unsafe {
        if hm != 0 && IsWindow(hwnd(hm)).as_bool() {
            let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize;
            let menu_has_focus = fg == hm;
            if let Some(w) = app.get_webview_window("startmenu") {
                let _ = w.hide();
                let _ = w.as_ref().hide();
            } else if hm != 0 {
                let _ = ShowWindow(hwnd(hm), SW_HIDE);
            }
            if menu_has_focus && prev_fg != 0 && prev_fg != hm {
                crate::native::activate_hwnd(prev_fg);
            }
        }
    }
}

pub fn hide(app: &AppHandle) {
    if !MENU_OPEN.swap(false, Ordering::SeqCst) {
        return; // already closing or closed
    }
    let st = app.state::<crate::AppShared>();
    {
        let mut s = crate::util::lock_shared(&st.0);
        s.startmenu_visible = false;
        s.startmenu_opened_at = None;
    }
    log("startmenu: hide (instant native close, then the handshake)");
    hide_window_now(app);
    let _ = app.emit("startmenu:hide", ());

    // Fallback: if the native hide above failed for any reason, hide
    // anyway. A new show (MENU_OPEN true again) supersedes this.
    let a2 = app.clone();
    std::thread::Builder::new()
        .name("startmenu-hide-fallback".into())
        .spawn(move || {
            crate::util::register_thread("sm-hide-fb");
            std::thread::sleep(std::time::Duration::from_millis(400));
            if !MENU_OPEN.load(Ordering::SeqCst) {
                hide_window_now(&a2);
            }
        })
        .ok();
}

pub fn close_finished(app: &AppHandle) {
    if MENU_OPEN.load(Ordering::SeqCst) {
        return; // a new show arrived while the close report was in flight
    }
    hide_window_now(app);
}

/// Heartbeat close-belt (native.rs beat_flyouts): the foreground moved to a
/// foreign window — the menu closes. First 400 ms ignored (focus dance).
/// Runs on the pump thread WITH the state lock held (`s` is borrowed) — so
/// this must never call hide()/hide_window_now() paths that lock; it does
/// its own short, lock-free window work after the flag flips.
pub unsafe fn tick(app: &AppHandle, s: &mut Shared) {
    if s.startmenu_visible {
        let hm = s.hwnd_startmenu;
        let dead = hm == 0 || !IsWindow(hwnd(hm)).as_bool();
        if dead {
            // Self-heal a lost window: flag cleared, nothing else to do.
            s.startmenu_visible = false;
            s.startmenu_opened_at = None;
            MENU_OPEN.store(false, Ordering::SeqCst);
        } else {
            let age = s
                .startmenu_opened_at
                .map(|t| t.elapsed())
                .unwrap_or(std::time::Duration::from_secs(10));
            let shown = IsWindowVisible(hwnd(hm)).as_bool();
            if !shown {
                if age >= std::time::Duration::from_millis(1500) {
                    log("startmenu: belt self-heal — show never completed (1.5 s)");
                    s.startmenu_visible = false;
                    s.startmenu_opened_at = None;
                    MENU_OPEN.store(false, Ordering::SeqCst);
                }
            } else if age >= std::time::Duration::from_millis(400) {
                let fg =
                    windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize;
                if fg == hm {
                    s.startmenu_ever_fg = true;
                } else if fg != 0 && !s.startmenu_ever_fg {
                    if BELT_STOOD_DOWN.load(Ordering::SeqCst) {
                    } else if age < std::time::Duration::from_millis(8000) {
                        reclaim_focus(app, hm);
                    } else {
                        BELT_STOOD_DOWN.store(true, Ordering::SeqCst);
                        log("startmenu: belt stands down — 8 s never-focused, menu stays until dismissed");
                    }
                } else if fg != 0 {
                    let host_fg = crate::startmenu_killer::foreground_is_flyout_host();
                    let fight =
                        host_fg || crate::startmenu_killer::ms_since_last_kill() < 2500;
                    if fight {
                        reclaim_focus(app, hm);
                    } else {
                        s.startmenu_visible = false;
                        s.startmenu_opened_at = None;
                        MENU_OPEN.store(false, Ordering::SeqCst);
                        let why = crate::startmenu_killer::image_of_window(fg)
                            .unwrap_or_else(|| format!("hwnd {fg:#x}"));
                        log(&format!("startmenu: belt close — foreground = {why}"));
                        belt_close_emit(app);
                    }
                }
            }
        }
    }
}

/// The pop-out plays (same event as a Win-tap close); 170 ms later the
/// paired hide runs, lock-free. Shared by every belt-side close path.
fn belt_close_emit(app: &AppHandle) {
    let _ = app.emit("startmenu:hide", ());
    let a2 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(170));
        hide_window_now(&a2);
    });
}

fn reclaim_focus(app: &AppHandle, hm: isize) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_RECLAIM_MS.load(Ordering::SeqCst);
    if now.saturating_sub(last) < 150 {
        return;
    }
    LAST_RECLAIM_MS.store(now, Ordering::SeqCst);
    let last_log = LAST_RECLAIM_LOG_MS.load(Ordering::SeqCst);
    if now.saturating_sub(last_log) >= 1500 {
        LAST_RECLAIM_LOG_MS.store(now, Ordering::SeqCst);
        log("startmenu: reclaiming focus (menu never/still not foreground)");
    }
    let a2 = app.clone();
    std::thread::spawn(move || {
        if !MENU_OPEN.load(Ordering::SeqCst) {
            return;
        }
        crate::native::activate_hwnd(hm);
        // every re-activation re-arms the keyboard into the browser child
        crate::native::webview_keys(&a2, "startmenu");
    });
}

// ---------------------------------------------------------------------------
// Search index
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct Hit {
    pub name: String,
    pub sub: String,
    /// "verb" | "app" | "file"
    pub kind: String,
    /// "@cmd" / "@powershell" / "@control" / "@settings" or a path
    pub target: String,
    pub icon: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct PinDto {
    pub name: String,
    pub target: String,
    pub user_pin: bool,
    pub icon: Option<String>,
}

#[derive(Clone)]
struct IndexEntry {
    display: String,
    lower: String,
    path: String,
    /// "app" (a .lnk) | "file"
    kind: &'static str,
}

static INDEX: Mutex<Option<(Instant, Vec<IndexEntry>)>> = Mutex::new(None);
static ICONS: std::sync::LazyLock<Mutex<HashMap<String, (u64, String, Option<String>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

const INDEX_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(600);

fn known_folder(fid: &windows::core::GUID) -> Option<PathBuf> {
    unsafe {
        let pwstr = SHGetKnownFolderPath(fid, KNOWN_FOLDER_FLAG(0), None).ok()?;
        if pwstr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *pwstr.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(pwstr.0, len);
        let s = String::from_utf16_lossy(slice);
        CoTaskMemFree(Some(pwstr.0 as *mut core::ffi::c_void));
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

fn display_of(file_name: &str) -> String {
    let mut n = file_name.to_string();
    if n.to_lowercase().ends_with(".lnk") {
        n.truncate(n.len() - 4);
    }
    n
}

fn walk(dir: &Path, depth: u32, max_depth: u32, out: &mut Vec<IndexEntry>, budget: &mut usize) {
    if *budget == 0 || depth > max_depth {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        let p = e.path();
        let name = match p.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if p.is_dir() {
            walk(&p, depth + 1, max_depth, out, budget);
        } else {
            if name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let kind = if name.to_lowercase().ends_with(".lnk") {
                "app"
            } else {
                "file"
            };
            let display = display_of(&name);
            out.push(IndexEntry {
                lower: display.to_lowercase(),
                display,
                path: p.to_string_lossy().to_string(),
                kind,
            });
            *budget -= 1;
        }
    }
}

/// Build (or refresh) the search index on the CALLING thread — expected to
/// run on a blocking-pool thread or the prewarm thread, never on the UI path
/// with a cold index.
fn build_index() -> Vec<IndexEntry> {
    let mut out = Vec::new();
    let mut budget = 4000usize;
    for fid in [&FOLDERID_StartMenu, &FOLDERID_CommonStartMenu] {
        if let Some(d) = known_folder(fid) {
            walk(&d, 0, 8, &mut out, &mut budget);
        }
    }
    for fid in [&FOLDERID_Desktop, &FOLDERID_PublicDesktop] {
        if let Some(d) = known_folder(fid) {
            walk(&d, 0, 8, &mut out, &mut budget);
        }
    }
    let mut seen_apps: std::collections::HashSet<String> = std::collections::HashSet::new();
    out.retain(|e| {
        if e.kind != "app" {
            return true;
        }
        seen_apps.insert(e.lower.clone())
    });
    let mut small = 1500usize;
    for fid in [
        &FOLDERID_Downloads,
        &FOLDERID_Documents,
        &FOLDERID_Pictures,
        &FOLDERID_Music,
        &FOLDERID_Videos,
    ] {
        if let Some(d) = known_folder(fid) {
            walk(&d, 0, 2, &mut out, &mut small);
        }
    }
    out
}

fn ensure_index() -> Vec<IndexEntry> {
    let mut g = INDEX.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = g
        .as_ref()
        .map(|(t, _)| t.elapsed() < INDEX_MAX_AGE)
        .unwrap_or(false);
    if !fresh {
        let idx = build_index();
        log(&format!("startmenu: search index built ({} entries)", idx.len()));
        *g = Some((Instant::now(), idx));
    }
    match g.as_ref() {
        Some((_, i)) => (*i).clone(),
        None => Vec::new(),
    }
}

/// Boot prewarm: build the index + extract the pinned/app icons in the
/// background so the FIRST menu open is already warm.
pub fn prewarm() {
    std::thread::spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let theme = "dark".to_string(); // dark-only shell; icon cache key
        let idx = ensure_index();
        let pins = read_pins();
        for p in pins.iter().take(60) {
            icon_for_target_cached(p, &theme);
        }
        let mut apps = 0usize;
        for e in &idx {
            if e.kind == "app" && apps < 150 {
                icon_for_target_cached(&e.path, &theme);
                apps += 1;
            }
        }
        log(&format!(
            "startmenu: prewarm done ({apps} app icons cached)"
        ));
    });
}

fn read_cfg() -> crate::config::Config {
    // No AppHandle here (prewarm thread) — read the config file directly.
    if let Some(dir) = crate::config::data_dir() {
        let p = dir.join("config.json");
        if let Ok(txt) = std::fs::read_to_string(&p) {
            if let Ok(c) = serde_json::from_str::<crate::config::Config>(&txt) {
                return c;
            }
        }
    }
    crate::config::Config::default()
}

fn read_pins() -> Vec<String> {
    read_cfg().pinned
}

fn mtime_sig(p: &Path) -> u64 {
    std::fs::metadata(p)
        .map(|m| {
            (m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0)
                << 8)
                ^ m.len()
        })
        .unwrap_or(0)
}

pub fn icon_for_target_cached(path: &str, theme: &str) -> Option<String> {
    let p = PathBuf::from(path);
    if !p.exists() {
        return None;
    }
    let sig = mtime_sig(&p);
    {
        let g = ICONS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, t, icon)) = g.get(path) {
            if *s == sig && t == theme {
                return icon.clone();
            }
        }
    }
    let icon = unsafe { crate::hushtop::shell_icon_dark(&p, theme) };
    {
        let mut g = ICONS.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(path.to_string(), (sig, theme.to_string(), icon.clone()));
        if g.len() > 800 {
            g.clear(); // crude cap; the cache refills lazily
        }
    }
    icon
}

fn icon_cached_only(path: &str, sig: u64, theme: &str) -> Option<String> {
    let g = ICONS.lock().unwrap_or_else(|e| e.into_inner());
    g.get(path)
        .and_then(|(s, t, i)| if *s == sig && t == theme { i.clone() } else { None })
}

const BUILTINS: &[(&str, &str, &[&str])] = &[
    // (display, target, keywords)
    ("Command Prompt", "@cmd", &["cmd", "command prompt", "terminal"]),
    ("PowerShell", "@powershell", &["powershell", "pwsh"]),
    ("Control Panel", "@control", &["control panel", "control", "cpl"]),
    ("Settings", "@settings", &["settings", "ms-settings", "ms:settings", "system settings"]),
];

fn builtin_hits(ql: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    for (disp, target, kws) in BUILTINS {
        let matched = ql.is_empty()
            || kws.iter().any(|kw| {
                kw.starts_with(ql) || ql.split_whitespace().any(|w| kw.starts_with(w))
            });
        if matched {
            out.push(Hit {
                name: disp.to_string(),
                sub: match *target {
                    "@cmd" => "command interpreter",
                    "@powershell" => "windows powershell",
                    "@control" => "all control panel items",
                    _ => "windows settings",
                }
                .to_string(),
                kind: "verb".into(),
                target: target.to_string(),
                icon: None,
            });
        }
    }
    out
}

/// One search turn. Runs on the blocking pool (the command wraps it) — the
/// first call may build the index (~tens of ms), afterwards it is pure
/// matching over a few thousand strings.
pub fn search(q: &str) -> Vec<Hit> {
    let ql = q.trim().to_lowercase();
    let theme = "dark".to_string(); // dark-only shell; icon cache key
    let mut scored: Vec<(u8, u8, usize, Hit)> = Vec::new();
    for h in builtin_hits(&ql) {
        scored.push((0, 0, h.name.len(), h));
    }
    if ql.is_empty() {
        scored.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
        return scored.into_iter().map(|(_, _, _, h)| h).collect();
    }
    let idx = ensure_index();
    for e in &idx {
        let m: u8 = if e.lower.starts_with(&ql) {
            0
        } else if e
            .lower
            .split(|c: char| c == ' ' || c == '\\' || c == '/' || c == '-' || c == '_')
            .any(|w| w.starts_with(&ql))
        {
            1
        } else if e.lower.contains(&ql) {
            2
        } else {
            continue;
        };
        let kind_rank: u8 = if e.kind == "app" { 1 } else { 2 };
        let sig = mtime_sig(Path::new(&e.path));
        scored.push((
            m,
            kind_rank,
            e.display.len(),
            Hit {
                name: e.display.clone(),
                sub: e.path.clone(),
                kind: e.kind.into(),
                target: e.path.clone(),
                icon: icon_cached_only(&e.path, sig, &theme),
            },
        ));
    }
    scored.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    scored.truncate(40);
    scored.into_iter().map(|(_, _, _, h)| h).collect()
}

pub fn open_target(_app: &AppHandle, target: &str) -> Result<(), String> {
    match target {
        "@cmd" | "@powershell" => {
            let home = std::env::var("USERPROFILE").unwrap_or_default();
            let exe = if target == "@cmd" { "cmd.exe" } else { "powershell.exe" };
            let wide = windows::core::HSTRING::from(exe);
            let dir = windows::core::HSTRING::from(home);
            unsafe {
                let r = windows::Win32::UI::Shell::ShellExecuteW(
                    None,
                    windows::core::w!("open"),
                    &wide,
                    windows::core::PCWSTR::null(),
                    &dir,
                    windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                );
                if r.0 as isize <= 32 {
                    return Err(format!("failed to launch {exe}"));
                }
            }
            Ok(())
        }
        "@control" => crate::winhook::shell_open("control.exe", ""),
        "@settings" => crate::winhook::shell_open("ms-settings:", ""),
        t => {
            // A bare name (no separator) = a desktop item: resolve it through
            // the hushtop item resolver first.
            let resolved = if !t.contains('\\') && !t.contains('/') {
                crate::hushtop::item_path(t)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| t.to_string())
            } else {
                t.to_string()
            };
            crate::winhook::shell_open(&resolved, "")
        }
    }
}

// ---------------------------------------------------------------------------
// Pins
// ---------------------------------------------------------------------------

pub fn pin(app: &AppHandle, target: &str) {
    let st = app.state::<crate::AppShared>();
    {
        let mut s = crate::util::lock_shared(&st.0);
        if s.cfg.pinned.iter().any(|p| p == target) {
            return;
        }
        s.cfg.pinned.push(target.to_string());
        if s.cfg.pinned.len() > 60 {
            s.cfg.pinned.remove(0);
        }
        s.cfg.save();
        log(&format!("startmenu: pinned '{target}'"));
    }
    let _ = app.emit("startmenu:pins", pinned_names(app));
}

pub fn unpin(app: &AppHandle, target: &str) {
    let st = app.state::<crate::AppShared>();
    {
        let mut s = crate::util::lock_shared(&st.0);
        let before = s.cfg.pinned.len();
        s.cfg.pinned.retain(|p| p != target);
        if s.cfg.pinned.len() == before {
            return;
        }
        s.cfg.save();
        log(&format!("startmenu: unpinned '{target}'"));
    }
    let _ = app.emit("startmenu:pins", pinned_names(app));
}

/// The raw pin list (bare names + absolute paths). The hushtop icon context
/// menu asks for membership per RMB.
pub fn pinned_names(app: &AppHandle) -> Vec<String> {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    s.cfg.pinned.clone()
}

pub fn is_pinned(app: &AppHandle, target: &str) -> bool {
    pinned_names(app).iter().any(|p| p == target)
}

pub fn pinned_view(app: &AppHandle) -> Vec<PinDto> {
    let theme = "dark".to_string(); // dark-only shell; icon cache key
    let mut out: Vec<PinDto> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in read_pins_cfg(app) {
        let target = t.clone();
        let (path, name) = if !t.contains('\\') && !t.contains('/') {
            match crate::hushtop::item_path(&t) {
                Ok(p) => (p, display_of(&t)),
                Err(_) => continue, // dangling desktop pin — hide, keep
            }
        } else {
            let p = PathBuf::from(&t);
            if !p.exists() {
                continue;
            }
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .map(display_of)
                .unwrap_or_else(|| t.clone());
            (p, name)
        };
        if !seen_names.insert(name.to_lowercase()) {
            continue; // a later app-grid copy of the same name is a duplicate
        }
        let icon = icon_for_target_cached(&path.to_string_lossy(), &theme);
        out.push(PinDto {
            name,
            target,
            user_pin: true,
            icon,
        });
    }
    let idx = ensure_index();
    let mut apps: Vec<&IndexEntry> = idx.iter().filter(|e| e.kind == "app").collect();
    apps.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    for e in apps.into_iter().take(150) {
        if !seen_names.insert(e.lower.clone()) {
            continue;
        }
        out.push(PinDto {
            name: e.display.clone(),
            target: e.path.clone(),
            user_pin: false,
            icon: icon_for_target_cached(&e.path, &theme),
        });
    }
    out
}

fn read_pins_cfg(app: &AppHandle) -> Vec<String> {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    s.cfg.pinned.clone()
}

// ---------------------------------------------------------------------------
// Controls view backend
// ---------------------------------------------------------------------------

/// Auto-place: a count-based cascade down the paper, in GRID CELLS —
/// columns cycle 2..6, one row further down per existing card. Cells, not
/// pixels: the webview derives the position at render time, so the spawn
/// spot is stable no matter the monitor or DPI. Counts the TARGET space.
fn auto_widget_pos(app: &AppHandle, space: usize) -> (i32, i32) {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    let sp = &s.cfg.spaces[space.min(1)];
    let n = sp.notepads.len() + sp.widgets.len();
    (2 + (n % 5) as i32, 4 + n as i32)
}

pub fn add_widget(app: &AppHandle, kind: &str) -> Result<u32, String> {
    let a = {
        let st = app.state::<crate::AppShared>();
        let s = crate::util::lock_shared(&st.0);
        s.cfg.active_space.min(1)
    };
    let (col, row) = auto_widget_pos(app, a);
    match kind {
        "notes" => crate::hushtop::notepad_add(app, col, row, a),
        "volume" | "clipboard" | "stats" | "lock" | "media" | "lyrics" | "spaces"
        | "brightness" | "effects" => crate::hushtop::widget_add(app, kind, col, row, a),
        other => Err(format!("unknown widget kind '{other}'")),
    }
}
