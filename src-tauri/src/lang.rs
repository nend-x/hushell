use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::Win32::Globalization::{
    GetLocaleInfoW, LOCALE_SISO639LANGNAME, LOCALE_SLOCALIZEDDISPLAYNAME,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::shared::Shared;

/// One layout reading as the bar renders it: `code` is the short uppercase
/// tag ("EN", "RO"), `name` the localized display name for the tooltip.
#[derive(Serialize, Clone, PartialEq)]
pub struct LangDto {
    pub code: String,
    pub name: String,
}

fn last_slot() -> &'static Mutex<Option<LangDto>> {
    static LAST: OnceLock<Mutex<Option<LangDto>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// The last LANGID seen, 0 = none yet. The atomic IS the dirty check —
/// the expensive locale resolution only runs when this changes.
fn last_langid() -> &'static AtomicU32 {
    static L: OnceLock<AtomicU32> = OnceLock::new();
    L.get_or_init(|| AtomicU32::new(0))
}

/// The beat window that covers the bar's listen() registration race: the
/// first 10 beats (~1 s) re-emit the current reading even when unchanged.
/// The bar's setLang dedupes by textContent, so a duplicate costs one
/// no-op write; a MISSED first reading costs an invisible chip until the
/// next layout switch — far worse.
const SEED_BEATS: u64 = 10;

/// Current reading for `sync_state` (the bar's boot safety net).
pub fn snapshot() -> Option<LangDto> {
    last_slot().lock().ok().and_then(|m| m.clone())
}

fn locale_str(lcid: u32, lctype: u32) -> String {
    unsafe {
        let mut buf = [0u16; 80];
        let n = GetLocaleInfoW(lcid, lctype, Some(&mut buf));
        if n > 0 {
            let len = (n as usize).min(buf.len()) - 1;
            String::from_utf16_lossy(&buf[..len])
        } else {
            String::new()
        }
    }
}

fn current_langid() -> Option<u32> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        let tid = GetWindowThreadProcessId(fg, Some(&mut pid));
        if tid == 0 {
            return None;
        }
        let hkl = GetKeyboardLayout(tid);
        // Low word of the HKL is the language identifier. MAKELCID(langid,
        // SORT_DEFAULT) is just the LANGID itself (SORT_DEFAULT == 0).
        let langid = (hkl.0 as usize & 0xFFFF) as u32;
        if langid == 0 {
            None
        } else {
            Some(langid)
        }
    }
}

fn resolve(langid: u32) -> Option<LangDto> {
    let code = locale_str(langid, LOCALE_SISO639LANGNAME);
    if code.is_empty() {
        return None;
    }
    Some(LangDto {
        code: code.to_uppercase(),
        name: locale_str(langid, LOCALE_SLOCALIZEDDISPLAYNAME),
    })
}

/// The heartbeat beat (registered in native.rs build_cycle). Every 100 ms:
/// read the foreground thread's LANGID, and only when it changed — or
/// during the seed window — resolve + emit to the bar.
pub unsafe fn beat(app: &AppHandle, _s: &mut Shared, beats: u64) {
    let Some(langid) = current_langid() else { return };
    let changed = last_langid().swap(langid, Ordering::Relaxed) != langid;
    if !changed && beats > SEED_BEATS {
        return;
    }
    let Some(dto) = resolve(langid) else { return };
    if let Ok(mut m) = last_slot().lock() {
        // Only emit when the RESOLVED reading actually moved (the langid
        // can change between two layouts that locale_str renders alike).
        if m.as_ref() != Some(&dto) || beats <= SEED_BEATS {
            *m = Some(dto.clone());
            let _ = app.emit_to("taskbar", "lang", &dto);
        }
    }
}
