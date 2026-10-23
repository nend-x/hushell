//! Small shared helpers.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, HDC,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, PeekMessageW, MSG, MONITORINFOF_PRIMARY, PM_NOREMOVE,
    PM_QS_SENDMESSAGE, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::shared::{BAR_H, Shared};

/// UTF-16, NUL-terminated copy of a &str (for dynamic strings passed to Win32).
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn pump_sent_messages() {
    let mut msg = MSG::default();
    let _ = PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
}

// --- thread identity for diagnostics --------------------------------------
// Every thread that matters registers a name (register_thread); lock
// contention reports then say WHO holds the state lock and WHO is waiting,
// instead of the old guessy one-liner.

static NEXT_TID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static THREAD_TAGS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, &'static str>>> =
    std::sync::OnceLock::new();

fn tid() -> u64 {
    thread_local! {
        static TID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    TID.with(|c| {
        if c.get() == 0 {
            c.set(NEXT_TID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1);
        }
        c.get()
    })
}

/// Name the calling thread for later diagnostic lines (lock contention).
pub fn register_thread(tag: &'static str) {
    let map = THREAD_TAGS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut m) = map.lock() {
        m.insert(tid(), tag);
    }
}

fn thread_tag(id: u64) -> String {
    if let Some(map) = THREAD_TAGS.get() {
        if let Ok(m) = map.lock() {
            if let Some(t) = m.get(&id) {
                return (*t).to_string();
            }
        }
    }
    format!("anon-{id}")
}

// --- the state lock, instrumented -----------------------------------------

static LOCK_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_SINCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn ms_since_t0() -> u64 {
    let t0 = T0.get_or_init(std::time::Instant::now);
    t0.elapsed().as_millis() as u64
}

pub fn lock_shared(m: &std::sync::Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    let start = std::time::Instant::now();
    let mut last_log = start;
    loop {
        match m.try_lock() {
            Ok(g) => {
                let waited = start.elapsed().as_millis();
                if waited >= 200 {
                    log(&format!(
                        "shared: '{}' got the lock after waiting {waited} ms",
                        thread_tag(tid())
                    ));
                }
                LOCK_OWNER.store(tid(), std::sync::atomic::Ordering::Relaxed);
                LOCK_SINCE.store(ms_since_t0(), std::sync::atomic::Ordering::Relaxed);
                return g;
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                unsafe { pump_sent_messages() };
                std::thread::sleep(std::time::Duration::from_millis(1));
                let now = std::time::Instant::now();
                if now.duration_since(last_log) >= std::time::Duration::from_secs(5) {
                    last_log = now;
                    let owner = LOCK_OWNER.load(std::sync::atomic::Ordering::Relaxed);
                    let held_ms = ms_since_t0().saturating_sub(
                        LOCK_SINCE.load(std::sync::atomic::Ordering::Relaxed),
                    );
                    log(&format!(
                        "shared: '{}' has waited {} ms for the state lock — last taken by '{}' (thread {owner}) {held_ms} ms ago and not released since",
                        thread_tag(tid()),
                        (now - start).as_millis(),
                        thread_tag(owner)
                    ));
                }
            }
            Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
        }
    }
}

pub fn screen_width() -> i32 {
    unsafe { GetSystemMetrics(SM_CXSCREEN) }
}

pub fn screen_height() -> i32 {
    unsafe { GetSystemMetrics(SM_CYSCREEN) }
}

/// Physical rect of the monitor a window is on (falls back to the primary
/// monitor metrics). Using the monitor rect (instead of raw GetSystemMetrics)
/// makes the bar scale to its screen regardless of DPI or monitor layout.
pub fn monitor_rect(hwnd: HWND) -> (i32, i32, i32, i32) {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let rc = mi.rcMonitor;
            return (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
        }
    }
    (0, 0, screen_width(), screen_height())
}

/// DPI of the window (96 = 100%). Falls back to the system DPI then 96.
pub fn dpi_for(hwnd: HWND) -> u32 {
    unsafe {
        let d = GetDpiForWindow(hwnd);
        if d != 0 {
            d
        } else {
            96
        }
    }
}

/// Effective DPI of the monitor containing the point (physical screen
/// coords). Mixed-DPI rigs render one hushtop webview across monitors of
/// different scale factors — every per-monitor CSS conversion needs THIS,
/// not the window DPI. None = the point sits on no monitor (or the API is
/// unavailable) — callers fall back to the window DPI.
pub fn monitor_dpi_at(x: i32, y: i32) -> Option<u32> {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut dx = 0u32;
        let mut dy = 0u32;
        if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() && dx > 0 {
            Some(dx)
        } else {
            None
        }
    }
}

/// One attached monitor: physical rect + primary flag.
#[derive(Clone, Copy, Debug)]
pub struct Mon {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub primary: bool,
}

impl Mon {
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        (self.x, self.y, self.w, self.h)
    }
}

/// Every attached monitor, primary first (stable order: the secondary bars
/// index off this list).
pub fn monitors() -> Vec<Mon> {
    unsafe {
        let mut out: Vec<Mon> = Vec::new();
        unsafe extern "system" fn cb(
            hmon: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            let out = &mut *(data.0 as *mut Vec<Mon>);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                let rc = mi.rcMonitor;
                out.push(Mon {
                    x: rc.left,
                    y: rc.top,
                    w: rc.right - rc.left,
                    h: rc.bottom - rc.top,
                    primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
            BOOL(1)
        }
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(cb),
            LPARAM(&mut out as *mut Vec<Mon> as isize),
        );
        out.sort_by(|a, b| b.primary.cmp(&a.primary));
        out
    }
}

/// Physical rect of the whole virtual screen — the union of every monitor.
/// The custom desktop spans exactly this: the desktop band it parents into
/// (Progman / the icon layer's host) covers the virtual screen, so the paper
/// must too.
pub fn virtual_screen_rect() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Monitor under the cursor (nearest fallback) — dialogs belong where the
/// user is actually working, not where a window happens to be anchored.
pub fn monitor_rect_at_cursor() -> (i32, i32, i32, i32) {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let rc = mi.rcMonitor;
            return (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
        }
    }
    (0, 0, screen_width(), screen_height())
}

/// Bar height in physical pixels for the window's DPI (BAR_H is logical).
pub fn bar_h_phys(hwnd: HWND) -> i32 {
    (BAR_H as u32 * dpi_for(hwnd) / 96) as i32
}

pub fn del_gdi<T: Into<windows::Win32::Graphics::Gdi::HGDIOBJ>>(h: T) {
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(h.into());
    }
}

/// Local timestamp for log lines.
fn stamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// Start a FRESH log for this session — the file is replaced, not appended,
/// so every shell start begins from a blank page (owner request).
pub fn log_reset() {
    if let Some(dir) = crate::config::data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hushell.log");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
        {
            let _ = writeln!(f, "[{}] log opened (fresh session)", stamp());
        }
    }
}

/// Append a line to %APPDATA%\hushell\hushell.log (best effort, never panics).
pub fn log(s: &str) {
    if let Some(dir) = crate::config::data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hushell.log");
        let stamp = stamp();
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[{}] {}", stamp, s);
        }
    }
}
