use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Memory::GMEM_MOVEABLE;

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;

use crate::util::log;

/// WM_CLIPBOARDUPDATE (WinUser.h) — the clipboard-changed broadcast the
/// host window receives after start_listener.
pub const WM_CLIPBOARDUPDATE: u32 = 0x031D;

pub unsafe fn start_listener(host: HWND) {
    if let Err(e) = AddClipboardFormatListener(host) {
        log(&format!("clipboard: listener registration failed: {e}"));
        return;
    }
    log("clipboard: listening (WM_CLIPBOARDUPDATE)");
}

/// Read CF_UNICODETEXT from the clipboard, if any (text only, always).
unsafe fn read_text() -> Option<String> {
    if OpenClipboard(HWND::default()).is_err() {
        return None;
    }
    let out = (|| {
        let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let hg = HGLOBAL(h.0);
        let ptr = GlobalLock(hg) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 && len < 1 << 20 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(hg);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    })();
    let _ = CloseClipboard();
    out
}

/// The cap. Old entries fall off the end.
const HISTORY_MAX: usize = 20;

/// WM_CLIPBOARDUPDATE handler: capture text, dedupe (front or anywhere),
/// cap, persist, emit to the hushtop webview. Runs on the native thread —
/// keep it cheap and never panic.
pub fn on_clipboard_update(app: &AppHandle) {
    let Some(text) = (unsafe { read_text() }) else {
        return;
    };
    let (list, changed) = {
        let st = app.state::<crate::AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        let trimmed = text.trim_end().to_string();
        if trimmed.is_empty() {
            (Vec::new(), false)
        } else {
            if s.cfg.clipboard_history.first().is_some_and(|t| *t == trimmed) {
                (s.cfg.clipboard_history.clone(), false)
            } else {
                s.cfg.clipboard_history.retain(|t| *t != trimmed);
                s.cfg.clipboard_history.insert(0, trimmed);
                s.cfg.clipboard_history.truncate(HISTORY_MAX);
                s.cfg.save();
                (s.cfg.clipboard_history.clone(), true)
            }
        }
    };
    if changed {
        log(&format!("clipboard: history {} entries", list.len()));
        let _ = app.emit_to("hushtop", "hushtop:clipboard", list);
    }
}

pub fn history(app: &AppHandle) -> Vec<String> {
    let st = app.state::<crate::AppShared>();
    { let g = crate::util::lock_shared(&st.0); g.cfg.clipboard_history.clone() }
}

pub fn clear(app: &AppHandle) {
    {
        let st = app.state::<crate::AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        if s.cfg.clipboard_history.is_empty() {
            return; // already empty — no event churn
        }
        s.cfg.clipboard_history.clear();
        s.cfg.save();
    }
    let _ = app.emit_to("hushtop", "hushtop:clipboard", Vec::<String>::new());
    log("clipboard: history cleared (widget button)");
}

pub fn set_text(app: &AppHandle, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("nothing to copy".into());
    }
    let st = app.state::<crate::AppShared>();
    let host = crate::util::lock_shared(&st.0).hwnd_host;
    let host = HWND(host as _);
    unsafe {
        OpenClipboard(host).map_err(|e| format!("OpenClipboard: {e}"))?;
        let ok = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| format!("EmptyClipboard: {e}"))?;
            let mut wide: Vec<u16> = text.encode_utf16().collect();
            wide.push(0);
            let bytes = wide.len() * 2;
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|e| format!("GlobalAlloc: {e}"))?;
            let ptr = GlobalLock(h) as *mut u16;
            if ptr.is_null() {
                return Err("GlobalLock failed".into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(h);
            if SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0)).is_err() {
                return Err("SetClipboardData failed".into());
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        if let Err(e) = ok {
            log(&format!("clipboard: copy failed: {e}"));
            return Err(e);
        }
    }
    log(&format!("clipboard: copied {} chars back", text.chars().count()));
    Ok(())
}
