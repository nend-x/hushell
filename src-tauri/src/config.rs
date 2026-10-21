//! Config persistence (%APPDATA%\hushell\config.json).
//!
//! Schema rules (owner directive — NO backward compatibility):
//!   * a card's spot is its GRID CELL (col, row) — col steps 36px, row steps
//!     18px, both anchored at 14px (the same cadence hushtop.js snaps to);
//!   * a card's SIZE is whole cells too: w is a multiple of 36, h of 18, so
//!     every card fits exactly box-to-box on the ruled sheet. load() snaps
//!     fractional sizes up (7.7 boxes -> 8) — the card gets bigger, never
//!     smaller, and content can never clip;
//!   * legacy pixel fields are gone; a config that does not parse under the
//!     current schema, or carries invalid records, resets to the default
//!     config (which is then written to disk).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_RoamingAppData, KNOWN_FOLDER_FLAG};

use crate::util::log;

// The card grid cadence — MUST stay in sync with hushtop.js
// (CARD_GRID / CARD_GRID_Y / CARD_ANCHOR).
pub const CARD_GRID_X: i32 = 36;
pub const CARD_GRID_Y: i32 = 18;
const CARD_MAX: i32 = 4096;
const CELL_MAX: i32 = 1000; // ±1000 sanity bound — out of bounds is allowed by design

#[derive(Serialize, Deserialize, Clone)]
pub struct Notepad {
    pub id: u32,
    pub col: i32,
    pub row: i32,
    pub w: i32,
    pub h: i32,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HushWidget {
    pub id: u32,
    pub kind: String,
    pub col: i32,
    pub row: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub recolor_icons: bool,
    pub hide_native_taskbar: bool,
    #[serde(default)]
    pub hushtop_positions: HashMap<String, (i32, i32)>,
    #[serde(default)]
    pub hushtop_hid_icons: bool,
    #[serde(default)]
    pub notepads: Vec<Notepad>,
    #[serde(default)]
    pub widgets: Vec<HushWidget>,
    #[serde(default)]
    pub clipboard_history: Vec<String>,
    #[serde(default)]
    pub pinned: Vec<String>,
    #[serde(default)]
    pub lock_salt: String,
    #[serde(default)]
    pub lock_hash: String,
    #[serde(default = "default_true")]
    pub lock_hotkey: bool,
    /// Run at startup — the shell drops a .lnk for itself into the user's
    /// Startup folder (shell:startup) when on, removes it when off.
    #[serde(default = "default_true")]
    pub startup: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recolor_icons: true,
            hide_native_taskbar: true,
            hushtop_positions: HashMap::new(),
            hushtop_hid_icons: false,
            notepads: Vec::new(),
            widgets: Vec::new(),
            clipboard_history: Vec::new(),
            pinned: Vec::new(),
            lock_salt: String::new(),
            lock_hash: String::new(),
            lock_hotkey: true,
            startup: true,
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    data_dir().map(|d| d.join("config.json"))
}

/// %APPDATA%\hushell
pub fn data_dir() -> Option<std::path::PathBuf> {
    unsafe {
        let pwstr = SHGetKnownFolderPath(
            &FOLDERID_RoamingAppData,
            KNOWN_FOLDER_FLAG(0),
            HANDLE(std::ptr::null_mut()),
        )
        .ok()?;
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
            Some(std::path::PathBuf::from(s).join("hushell"))
        }
    }
}

/// Round a card dimension UP to whole grid boxes (7.7 boxes -> 8): the card
/// gets slightly bigger and lands exactly box-to-box on the ruled sheet.
/// Every caller clamps v >= 1 first, so the classic positive-integer
/// formula is exact (int_roundings/div_ceil is still unstable here).
pub fn snap_up(v: i32, grid: i32) -> i32 {
    ((v + grid - 1) / grid) * grid
}

/// Snap a width to whole cells, clamped to the sanity cap. The floor is one
/// box — smaller values are garbage, not layouts.
pub fn snap_card_w(v: i32) -> i32 {
    snap_up(v.clamp(1, CARD_MAX), CARD_GRID_X)
}

/// Snap a height to whole cells (the finer 18px cadence), same rules.
pub fn snap_card_h(v: i32) -> i32 {
    snap_up(v.clamp(1, CARD_MAX), CARD_GRID_Y)
}

impl Config {
    pub fn load() -> Config {
        if let Some(p) = config_path() {
            if let Ok(txt) = std::fs::read_to_string(&p) {
                match serde_json::from_str::<Config>(&txt) {
                    Ok(mut c) => {
                        if c.normalize() {
                            return c;
                        }
                        log("config: invalid record(s) — resetting to the default config");
                    }
                    Err(e) => log(&format!(
                        "config: parse failed ({e}) — resetting to the default config"
                    )),
                }
            }
        }
        // No backward compatibility: an unparsable or invalid config resets
        // to defaults, and the default is persisted right away so the file
        // on disk always matches the current schema.
        let d = Config::default();
        d.save();
        d
    }

    /// Validate every card record and snap its size to whole grid boxes.
    /// Returns false when ANY record is broken — the caller resets the whole
    /// config to default (owner rule: an incorrect config never limps along).
    fn normalize(&mut self) -> bool {
        let mut ok = true;
        for n in &mut self.notepads {
            ok &= valid_cell(n.col, n.row) && snap_dims(&mut n.w, &mut n.h);
        }
        for w in &mut self.widgets {
            ok &= valid_cell(w.col, w.row) && snap_dims(&mut w.w, &mut w.h);
        }
        ok
    }

    pub fn save(&self) {
        if let Some(p) = config_path() {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(txt) = serde_json::to_string_pretty(self) {
                // Atomic write: temp file + rename. A crash mid-write used to
                // leave a truncated config.json — the next boot then wiped the
                // arrangement back to defaults. The rename replaces the target
                // on the same volume (std::fs::rename uses MOVEFILE_REPLACE_EXISTING).
                let tmp = p.with_extension("json.tmp");
                if std::fs::write(&tmp, &txt).is_ok() {
                    if std::fs::rename(&tmp, &p).is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
        }
    }
}

fn valid_cell(col: i32, row: i32) -> bool {
    col.abs() <= CELL_MAX && row.abs() <= CELL_MAX
}

fn snap_dims(w: &mut i32, h: &mut i32) -> bool {
    if *w <= 0 || *h <= 0 || *w > CARD_MAX || *h > CARD_MAX {
        return false;
    }
    *w = snap_up(*w, CARD_GRID_X);
    *h = snap_up(*h, CARD_GRID_Y);
    true
}
