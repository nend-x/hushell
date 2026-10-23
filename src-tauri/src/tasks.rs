//! Top-level window enumeration & filtering (Alt-Tab-like logic).
//!
//! Two-tier design for the 100 ms heartbeat:
//! * `enumerate_light` — filtered hwnds + minimized flags, NO titles. Cheap
//!   enough to run every beat.
//! * `window_title` — hang-proof title fetch (SendMessageTimeoutW), only
//!   called when the structural signature changed or on title re-check beats.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetWindowLongPtrW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, SendMessageTimeoutW, ShowWindow, GA_ROOT, GWL_EXSTYLE,
    SMTO_ABORTIFHUNG, SW_MINIMIZE, SW_RESTORE, WM_GETTEXT, WS_EX_TOOLWINDOW,
};

const EXCLUDED_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "TaskListThumbnailWnd",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "hushell_strip",
];

pub fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n > 0 {
        String::from_utf16_lossy(&buf[..n as usize])
    } else {
        String::new()
    }
}

/// Hang-proof title fetch: GetWindowTextW can block forever on a hung
/// window's WM_GETTEXT; SendMessageTimeoutW caps the wait.
pub fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let mut copied: usize = 0;
    let r = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETTEXT,
            WPARAM(buf.len() as _),
            LPARAM(buf.as_mut_ptr() as _),
            SMTO_ABORTIFHUNG,
            100,
            Some(&mut copied),
        )
    };
    if r.0 != 0 && copied > 0 {
        String::from_utf16_lossy(&buf[..copied.min(buf.len())])
    } else {
        String::new()
    }
}

pub fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let r = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    r.is_ok() && cloaked != 0
}

unsafe extern "system" fn enum_light_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<(isize, bool)>);
    list.push((hwnd.0 as isize, IsIconic(hwnd).as_bool()));
    BOOL(1)
}

/// Lightweight pass for the heartbeat: top-level windows that pass all the
/// Alt-Tab-like filters, in z-order, as `(hwnd, minimized)` pairs. Titles are
/// NOT fetched here (cross-process WM_GETTEXT is the expensive part).
pub fn enumerate_light(ours: &[isize]) -> Vec<(isize, bool)> {
    let our_pid = unsafe { GetCurrentProcessId() };
    let mut list: Vec<(isize, bool)> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_light_proc),
            LPARAM(&mut list as *mut Vec<(isize, bool)> as isize),
        );
    }
    list.retain(|(hw, _)| {
        let hwnd = HWND(*hw as _);
        if !unsafe { IsWindowVisible(hwnd).as_bool() } {
            return false;
        }
        if EXCLUDED_CLASSES.iter().any(|c| *c == unsafe { window_class(hwnd) }) {
            return false;
        }
        if unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } & WS_EX_TOOLWINDOW.0 as isize != 0 {
            return false;
        }
        // Root windows only (skip owned popups whose root is another window).
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        if root != hwnd {
            return false;
        }
        if is_cloaked(hwnd) {
            return false;
        }
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == our_pid {
            return false;
        }
        if ours.iter().any(|o| *o == *hw) {
            return false;
        }
        true
    });
    list
}

// ---------------------------------------------------------------------------
// Minimize-all (lock + boot choreography)
// ---------------------------------------------------------------------------

/// Minimize every visible foreign top-level window — the exact Alt-Tab
/// filter of enumerate_light (own process, shell strips, wallpaper hosts,
/// cloaked UWP hosts and tool windows are all skipped), so hushell's own
/// windows and the desktop itself are untouched. Returns the hwnds this
/// call MINIMIZED (already-iconic windows are left alone and not recorded).
pub fn minimize_all_except_shell() -> Vec<isize> {
    let mut done: Vec<isize> = Vec::new();
    for (hw, iconic) in enumerate_light(&[]) {
        if iconic {
            continue;
        }
        let h = HWND(hw as _);
        let ok = unsafe { ShowWindow(h, SW_MINIMIZE) };
        if ok.as_bool() {
            done.push(hw);
        }
    }
    done
}

/// Restore what a lock parked: only windows that are STILL iconic (anything
/// the user re-raised themselves stays as it is). Returns how many came back.
pub fn restore_windows(list: &[isize]) -> usize {
    let mut n = 0usize;
    for &hw in list {
        let h = HWND(hw as _);
        if unsafe { IsWindow(h).as_bool() } && unsafe { IsIconic(h).as_bool() } {
            if unsafe { ShowWindow(h, SW_RESTORE) }.as_bool() {
                n += 1;
            }
        }
    }
    n
}
