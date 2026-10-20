#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(windows)]
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use crate::util::log;

/// Whether the monitor should keep running.
#[cfg(windows)]
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Millis (unix epoch) until which the monitor stays ARMED. Updated by
/// arm() whenever a real Win-key down is seen by the hook or the poller.
#[cfg(windows)]
static ARMED_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
static LAST_KILL_MS: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
const ARMED_WINDOW_MS: u64 = 1200;

/// Idle-mode poll interval for the cheap window walk.
#[cfg(windows)]
const IDLE_POLL_MS: u64 = 250;

#[cfg(windows)]
const ARMED_POLL_MS: u64 = 40;

/// The processes to kill on sight: the UWP shell surfaces the Win key (or
/// a leaked chord) can trigger. SearchHost.exe is Win+S's flyout.
#[cfg(windows)]
const KILL_TARGETS: &[&str] = &["StartMenuExperienceHost.exe", "SearchHost.exe"];

/// Class of the UWP shell flyout windows (Start menu, search host).
#[cfg(windows)]
const CORE_WINDOW_CLASS: windows::core::PCWSTR = windows::core::w!("Windows.UI.Core.CoreWindow");

#[cfg(windows)]
pub fn arm() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ARMED_UNTIL_MS.store(now + ARMED_WINDOW_MS, Ordering::SeqCst);
    kill_targets_snapshot();
    kill_visible_core_windows();
}

#[cfg(not(windows))]
pub fn arm() {}

/// Start the monitor on a background thread. Runs forever (until the
/// process exits). Called once at app startup.
#[cfg(windows)]
pub fn start() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("start-menu-killer".into())
        .spawn(monitor_loop)
        .ok();
}

#[cfg(not(windows))]
pub fn start() {}

/// One idle-mode pass, for the global heartbeat (beat_start_mute): find and
/// kill VISIBLE CoreWindows owned by the targets. 1-3 syscalls when no
/// flyout exists — the common case.
pub fn sweep() {
    #[cfg(windows)]
    kill_visible_core_windows();
}

#[cfg(windows)]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn monitor_loop() {
    log("smkiller: monitor started (StartMenuExperienceHost + SearchHost, armed 100ms / idle 250ms)");
    loop {
        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }
        let armed = now_ms() < ARMED_UNTIL_MS.load(Ordering::SeqCst);
        if armed {
            // Gesture window — full snapshot protection.
            kill_targets_snapshot();
            kill_visible_core_windows();
            std::thread::sleep(std::time::Duration::from_millis(ARMED_POLL_MS));
        } else {
            // Idle — near-zero cost: look for visible CoreWindows only.
            kill_visible_core_windows();
            std::thread::sleep(std::time::Duration::from_millis(IDLE_POLL_MS));
        }
    }
    log("smkiller: monitor exiting");
}

#[cfg(windows)]
fn kill_visible_core_windows() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId, IsWindowVisible};

    unsafe {
        let mut prev = HWND::default();
        loop {
            let hwnd = match FindWindowExW(HWND::default(), prev, CORE_WINDOW_CLASS, PCWSTR::null())
            {
                Ok(h) => h,
                Err(_) => break,
            };
            if hwnd.is_invalid() {
                break;
            }
            prev = hwnd;

            // Only act on windows that are actually shown. One syscall;
            // avoids opening processes for cached/pre-created (hidden)
            // CoreWindows.
            if !IsWindowVisible(hwnd).as_bool() {
                continue;
            }

            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                continue;
            }
            if let Some(image) = process_image_name(pid) {
                if KILL_TARGETS.iter().any(|t| image.eq_ignore_ascii_case(t)) {
                    kill_pid(pid, &image);
                }
            }
        }
    }
}

/// Resolve a pid to its full image name (no snapshot needed).
#[cfg(windows)]
fn process_image_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(
            handle,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if ok.is_err() {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/']).next().map(|s| s.to_string())
    }
}

#[cfg(windows)]
fn kill_pid(pid: u32, image: &str) {
    unsafe {
        if let Ok(handle) =
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid)
        {
            let _ = TerminateProcess(handle, 1);
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            LAST_KILL_MS.store(now_ms(), Ordering::SeqCst);
            log(&format!("smkiller: killed {} (pid {})", image, pid));
        }
    }
}

#[cfg(windows)]
pub fn ms_since_last_kill() -> u64 {
    let last = LAST_KILL_MS.load(Ordering::SeqCst);
    if last == 0 {
        return u64::MAX;
    }
    now_ms().saturating_sub(last)
}

#[cfg(not(windows))]
pub fn ms_since_last_kill() -> u64 {
    u64::MAX
}

#[cfg(windows)]
pub fn foreground_is_flyout_host() -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == HWND::default() {
            return false;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == 0 {
            return false;
        }
        match process_image_name(pid) {
            Some(image) => KILL_TARGETS.iter().any(|t| image.eq_ignore_ascii_case(t)),
            None => false,
        }
    }
}

#[cfg(not(windows))]
pub fn foreground_is_flyout_host() -> bool {
    false
}

#[cfg(windows)]
pub fn image_of_window(id: isize) -> Option<String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    unsafe {
        let h = HWND(id as _);
        if h.is_invalid() || h == HWND::default() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        process_image_name(pid)
    }
}

#[cfg(not(windows))]
pub fn image_of_window(_id: isize) -> Option<String> {
    None
}

/// Snapshot sweep — used only while ARMED (shortly after a real Win-key
/// press). Kills target processes even before they create a window.
#[cfg(windows)]
fn kill_targets_snapshot() {
    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(s) => s,
            Err(e) => {
                log(&format!("smkiller: snapshot failed: {e}"));
                return;
            }
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_err() {
            let _ = windows::Win32::Foundation::CloseHandle(snapshot);
            return;
        }

        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);

            if KILL_TARGETS.iter().any(|t| name.eq_ignore_ascii_case(t)) {
                let pid = entry.th32ProcessID;
                kill_pid(pid, &name);
            }

            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }

        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }
}
