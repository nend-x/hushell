//! Config persistence (%APPDATA%\hushell\config.json).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_RoamingAppData, KNOWN_FOLDER_FLAG};

// The widget layout map: a card's spot is its GRID CELL (col, row), never
// pixels. col steps 36px, row steps 18px, both anchored at 14px — the same
// cadence hushtop.js snaps to (CARD_ANCHOR / CARD_GRID / CARD_GRID_Y). The
// webview derives pixels at render time, so a save→reload round trip lands
// EXACTLY on the arranged cell regardless of DPI, monitor size, or when the
// webview boots. Rust stores the map verbatim and never touches pixels.

#[derive(Serialize, Deserialize, Clone)]
pub struct Notepad {
    pub id: u32,
    #[serde(default)]
    pub col: i32,
    #[serde(default)]
    pub row: i32,
    #[serde(default)]
    pub w: i32,
    #[serde(default)]
    pub h: i32,
    pub text: String,
    /// Legacy pixel position (pre layout-map). Migrated into col/row at
    /// load, then left as None forever.
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HushWidget {
    pub id: u32,
    pub kind: String,
    #[serde(default)]
    pub col: i32,
    #[serde(default)]
    pub row: i32,
    #[serde(default)]
    pub w: i32,
    #[serde(default)]
    pub h: i32,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
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

impl Config {
    pub fn load() -> Config {
        if let Some(p) = config_path() {
            if let Ok(txt) = std::fs::read_to_string(&p) {
                if let Ok(mut c) = serde_json::from_str::<Config>(&txt) {
                    c.migrate_layout();
                    return c;
                }
            }
        }
        Config::default()
    }

    /// One-time migration: legacy pixel positions → layout-map cells.
    /// x = 14 + col*36, y = 14 + row*18 — inverted with a rounded divide.
    /// Only acts on records that still carry pixels; runs on every load so
    /// older config.json files upgrade silently on first boot.
    fn migrate_layout(&mut self) {
        const ANCHOR: f32 = 14.0;
        const GRID_X: f32 = 36.0;
        const GRID_Y: f32 = 18.0;
        let cell = |px: i32, grid: f32| -> i32 {
            (((px as f32 - ANCHOR) / grid).round() as i32).clamp(0, 1000)
        };
        for n in &mut self.notepads {
            if let (Some(px), Some(py)) = (n.x, n.y) {
                n.col = cell(px, GRID_X);
                n.row = cell(py, GRID_Y);
                n.x = None;
                n.y = None;
            }
            n.col = n.col.clamp(0, 1000);
            n.row = n.row.clamp(0, 1000);
        }
        for w in &mut self.widgets {
            if let (Some(px), Some(py)) = (w.x, w.y) {
                w.col = cell(px, GRID_X);
                w.row = cell(py, GRID_Y);
                w.x = None;
                w.y = None;
            }
            w.col = w.col.clamp(0, 1000);
            w.row = w.row.clamp(0, 1000);
        }
    }

    pub fn save(&self) {
        if let Some(p) = config_path() {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(txt) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(&p, txt);
            }
        }
    }
}
