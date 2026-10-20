use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    STYLESTRUCT, WM_CLOSE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCMBUTTONDOWN, WM_NCLBUTTONDOWN,
    WM_NCPAINT, WM_NCRBUTTONDOWN, WM_NCXBUTTONDOWN, WM_SYSCOMMAND, WM_STYLECHANGING, GWL_STYLE,
};

const WM_NCUAHDRAWCAPTION: u32 = 0x00AE;
const WM_NCUAHDRAWFRAME: u32 = 0x00AF;
/// HTCAPTION = 2 (Win32 hit-test enum). Kept as a literal because only the
/// comparison matters here and importing the HT newtype for one check
/// forces type juggling at the wparam boundary.
const HTCAPTION_USIZE: usize = 2;

const SUBCLASS_ID: usize = 0x4853_4631;

unsafe fn sanitize_style_change(w: WPARAM, l: LPARAM) {
    if w.0 != GWL_STYLE.0 as usize {
        return; // ex-style changes carry no caption bits
    }
    let ss = l.0 as *mut STYLESTRUCT;
    if ss.is_null() {
        return;
    }
    let mask = crate::native::NC_FRAME_STYLES as u32;
    let dropped = (*ss).styleNew & mask;
    if dropped != 0 {
        (*ss).styleNew &= !mask;
        crate::util::log(&format!(
            "frame: style change sanitized — dropped {dropped:#x} (tao style walk)"
        ));
    }
}

unsafe extern "system" fn frameless_proc(
    h: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        // Client area = the whole window. Returning 0 with the suggested
        // rect untouched tells Windows there is NO non-client area at all.
        WM_NCCALCSIZE if w.0 != 0 => LRESULT(0),
        // Never paint a non-client frame (classic caption lives here).
        WM_NCPAINT => LRESULT(0),
        WM_NCUAHDRAWCAPTION | WM_NCUAHDRAWFRAME => LRESULT(0),
        WM_NCACTIVATE => DefSubclassProc(h, msg, w, LPARAM(-1)),
        WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN | WM_NCMBUTTONDOWN | WM_NCXBUTTONDOWN
            if w.0 == HTCAPTION_USIZE => LRESULT(0),
        // The caption family never gets applied, ever — not by tao, not by
        // a theme walk, not by anyone.
        WM_STYLECHANGING => {
            sanitize_style_change(w, l);
            DefSubclassProc(h, msg, w, l)
        }
        _ => DefSubclassProc(h, msg, w, l),
    }
}

pub unsafe fn guard_frameless(h: isize) -> bool {
    if h == 0 {
        return false;
    }
    SetWindowSubclass(
        HWND(h as *mut core::ffi::c_void),
        Some(frameless_proc),
        SUBCLASS_ID,
        0,
    )
    .as_bool()
}

// ---------------------------------------------------------------------------
// The no-close guard: the lockscreen must not be closable. Alt+F4 arrives as
// WM_SYSCOMMAND (SC_CLOSE) and dies here; a direct WM_CLOSE dies too. The
// shell's own exits go through request_exit → app.exit(0), which never
// sends WM_CLOSE — so denying close here cannot wedge a legit shutdown.
// ---------------------------------------------------------------------------

/// SC_CLOSE, masked the way WM_SYSCOMMAND requires (low 4 bits are internal).
const SC_CLOSE_MASKED: usize = 0xF060;

unsafe extern "system" fn no_close_proc(
    h: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        // Alt+F4's SC_CLOSE (and the system-menu Close) — swallowed.
        WM_SYSCOMMAND if (w.0 & 0xFFF0) == SC_CLOSE_MASKED => LRESULT(0),
        // A direct WM_CLOSE — swallowed as well.
        WM_CLOSE => LRESULT(0),
        _ => DefSubclassProc(h, msg, w, l),
    }
}

pub unsafe fn guard_no_close(h: isize) -> bool {
    if h == 0 {
        return false;
    }
    SetWindowSubclass(
        HWND(h as *mut core::ffi::c_void),
        Some(no_close_proc),
        SUBCLASS_ID + 1,
        0,
    )
    .as_bool()
}
