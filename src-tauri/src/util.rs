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

pub fn lock_shared(m: &std::sync::Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    let start = std::time::Instant::now();
    let mut last_log = start;
    loop {
        match m.try_lock() {
            Ok(g) => return g,
            Err(std::sync::TryLockError::WouldBlock) => {
                unsafe { pump_sent_messages() };
                std::thread::sleep(std::time::Duration::from_millis(1));
                let now = std::time::Instant::now();
                if now.duration_since(last_log) >= std::time::Duration::from_secs(5) {
                    last_log = now;
                    log(&format!(
                        "shared: state lock contended for {} ms (main thread pumping sent messages)",
                        (now - start).as_millis()
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
/// The custom desktop spans exactly this: WorkerW (the wallpaper host)
/// covers the virtual screen, so the paper must too.
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

/// Append a line to %APPDATA%\hushell\hushell.log (best effort, never panics).
pub fn log(s: &str) {
    if let Some(dir) = crate::config::data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hushell.log");
        let stamp = {
            use windows::Win32::System::SystemInformation::GetLocalTime;
            let st = unsafe { GetLocalTime() };
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
            )
        };
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[{}] {}", stamp, s);
        }
    }
}
