//! The lock keyboard guard: a system-wide low-level keyboard hook that is
//! installed for the WHOLE session and swallows keys ONLY while the shell
//! is locked (`lockscreen::is_locked()` is one atomic load per event).
//!
//! While locked the keyboard belongs to the password field and nothing
//! else:
//! - The function row dies, media/launch/browser/volume keys die,
//!   PrintScreen dies (no clipboard screen grabs of the lock), the Win row
//!   dies so no OS chord opens anything on the lock, and the menu key,
//!   Pause, Scroll Lock and Sleep die with them.
//! - Everything the password field needs passes untouched: letters,
//!   digits, punctuation, Space, Backspace, Enter, Shift/Ctrl/Alt
//!   (Ctrl+A/C/V in the field), Caps Lock, Num Lock and the numpad.
//!
//! The hook lives on its own thread with its own message loop — it never
//! depends on the main thread staying responsive, so a stalled shell cannot
//! silently drop the guard (Windows would time a non-pumping hook out and
//! pass keys through). Injected events are NOT exempted: a key event that
//! matches the swallow list dies whether a human or SendInput produced it.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, MSG,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::util::log;

static INSTALLED: AtomicBool = AtomicBool::new(false);
static HOOK: AtomicIsize = AtomicIsize::new(0);

/// The swallow list, by virtual key — lock-time only, keydown AND keyup.
/// Tab is deliberately NOT on it: the field needs Tab for focus cycling,
/// and the hook has no way to tell a bare Tab from the Win+Tab chord's
/// tail key anyway (the Win VKs themselves are swallowed, so the chord
/// never assembles — a plain Tab must keep working).
fn swallowed(vk: u32) -> bool {
    matches!(vk,
        0x13          // VK_PAUSE
        | 0x2C          // VK_SNAPSHOT — PrintScreen
        | 0x5B | 0x5C   // VK_LWIN / VK_RWIN — no OS chords on the lock
        | 0x5D          // VK_APPS — the menu key
        | 0x5F          // VK_SLEEP
        | 0x70..=0x87   // VK_F1 .. VK_F24
        | 0x91          // VK_SCROLL
        | 0xA6..=0xB7   // VK_BROWSER_* + VK_VOLUME_* + VK_MEDIA_* + VK_LAUNCH_*
    )
}

unsafe extern "system" fn guard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        let key = msg == WM_KEYDOWN
            || msg == WM_KEYUP
            || msg == WM_SYSKEYDOWN
            || msg == WM_SYSKEYUP;
        if key && crate::lockscreen::is_locked() {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if swallowed(kb.vkCode) {
                return LRESULT(1); // nil — the key never reaches anyone
            }
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

/// Install the session-long guard. Called once from Tauri setup.
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        log("lockguard: already installed");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("lock-guard".into())
        .spawn(|| unsafe {
            let hmod = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(guard_proc), HINSTANCE(hmod.0), 0) {
                Ok(h) => {
                    HOOK.store(h.0 as isize, Ordering::SeqCst);
                    log("lockguard: LL keyboard hook up — F-row/media/volume/Win swallowed while locked");
                }
                Err(e) => {
                    INSTALLED.store(false, Ordering::SeqCst);
                    log(&format!("lockguard: hook install FAILED — locked keys unguarded: {e}"));
                    return;
                }
            }
            // The hook is serviced by THIS thread's message pump; the loop
            // runs for the whole session (nothing posts WM_QUIT to it).
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(HHOOK(HOOK.load(Ordering::SeqCst) as *mut _));
            log("lockguard: message loop ended — hook removed");
        });
    if spawned.is_err() {
        INSTALLED.store(false, Ordering::SeqCst);
        log("lockguard: guard thread spawn failed");
    }
}
