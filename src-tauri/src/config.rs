//! Config persistence (%APPDATA%\hushell\config.json).
//!
//! Schema rules (owner directive — NO backward compatibility):
//!   * the desktop has TWO spaces (the second-desktop widget swaps between
//!     them); every space owns its own cards and its own ICON SET — desktop 2
//!     starts EMPTY and only holds what is dropped/created/moved onto it
//!     (the icons-per-space split; the one-time migration wipes the old
//!     duplicated desktop-2 map);
//!   * a card's spot is its GRID CELL (col, row) — col steps 36px, row steps
//!     18px, both anchored at 14px (the same cadence hushtop.js snaps to);
//!   * a card's SIZE is whole cells too: w is a multiple of 36, h of 18, so
//!     every card fits exactly box-to-box on the ruled sheet. load() snaps
//!     fractional sizes up (7.7 boxes -> 8) — the card gets bigger, never
//!     smaller, and content can never clip;
//!   * an icon's spot is a CELL too — free placing: cells are written only
//!     by drops, adds and explicit sorts, never by a refresh;
//!   * legacy pixel fields are gone; a config that does not parse under the
//!     current schema, or carries invalid records, resets to the default
//!     config (which is then written to disk).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_RoamingAppData, KNOWN_FOLDER_FLAG};

use crate::util::log;

/// Start Fresh armed — from the moment this flips, `Config::save()` is a
/// no-op: no call site (teardown restore, DnD commit, clipboard write,
/// watcher) may re-persist the settings the wipe just removed, or the
/// "clean sheet" boots with the user's old settings intact — exactly the
/// bug the owner reported ("start fresh … i still see my settings kept":
/// the exit-path teardown re-saved config.json behind the wipe).
pub static FRESH_WIPE: AtomicBool = AtomicBool::new(false);

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

/// One desktop space. The shell carries exactly two (the second-desktop
/// widget swaps between them); each space arranges the SAME desktop files
/// independently and holds its own cards.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Space {
    #[serde(default)]
    pub notepads: Vec<Notepad>,
    #[serde(default)]
    pub widgets: Vec<HushWidget>,
    /// Free-placed icon spots: name -> grid cell (col, row) within the
    /// space's region. Written by drops / new files / explicit sorts only.
    #[serde(default)]
    pub icon_cells: HashMap<String, (i32, i32)>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub recolor_icons: bool,
    pub hide_native_taskbar: bool,
    #[serde(default)]
    pub hushtop_hid_icons: bool,
    /// The two desktop spaces. Index 0 = space 1 (the switcher's left
    /// button), index 1 = space 2.
    #[serde(default = "default_spaces")]
    pub spaces: [Space; 2],
    /// The space a single-display desktop shows (0 or 1). Meaningless while
    /// two displays are connected — each monitor then hosts its own space.
    #[serde(default)]
    pub active_space: usize,
    /// Icons are PER-SPACE (desktop 2 starts empty). A config from before
    /// the split carries the same names mapped in BOTH spaces — load()
    /// clears desktop 2's map once and flips this to true.
    #[serde(default)]
    pub icons_split: bool,
    /// The desktop effects widget's current layer:
    /// "none" | "snow" | "leaves" | "star" (wind lives on the lock screen only).
    #[serde(default)]
    pub effect: String,
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
    /// The lock screen's OLED burn shield (full-black frame after a minute
    /// of lock-screen silence; keyboard wakes, mouse does not). The options
    /// switch turns it off entirely.
    #[serde(default = "default_true")]
    pub lock_burn: bool,
    /// Run at startup — the shell drops a .lnk for itself into the user's
    /// Startup folder (shell:startup) when on, removes it when off.
    #[serde(default = "default_true")]
    pub startup: bool,
}

fn default_true() -> bool {
    true
}

fn default_spaces() -> [Space; 2] {
    [Space::default(), Space::default()]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recolor_icons: true,
            hide_native_taskbar: true,
            hushtop_hid_icons: false,
            spaces: default_spaces(),
            active_space: 0,
            icons_split: false,
            effect: String::new(),
            clipboard_history: Vec::new(),
            pinned: Vec::new(),
            lock_salt: String::new(),
            lock_hash: String::new(),
            lock_hotkey: true,
            lock_burn: true,
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

/// Start Fresh — EVERY saved byte goes: the whole data dir is removed
/// (config.json — settings, clipboard history, pinned apps, lock hash — and
/// hushell.log, which the tray bridge writes here too). The next boot
/// recreates the dir and persists the default config right away (load()).
pub fn wipe_all() {
    if let Some(d) = data_dir() {
        match std::fs::remove_dir_all(&d) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log(&format!("fresh: data dir remove failed: {e}")),
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

/// Widgets with a FIXED canvas never take a caller's size. The media card is
/// exactly 7 x 14 grid boxes (252 x 252 — the CD, transport and info row were
/// measured to fill it), the lyrics card is 7 x 16 (252 x 288); every holder
/// of the record — the config on disk, the spaces payload the webview
/// renders, the switch-wipe placeholder that paints it verbatim — must agree
/// on the same dimensions. The frontend carries the same normalizer
/// (renderWidgets); this is the backend's half.
pub fn canonical_widget_dims(kind: &str, w: i32, h: i32) -> (i32, i32) {
    match kind {
        "media" => (252, 252),
        "lyrics" => (252, 288),
        _ => (w, h),
    }
}

impl Config {
    pub fn load() -> Config {
        if let Some(p) = config_path() {
            if let Ok(txt) = std::fs::read_to_string(&p) {
                match serde_json::from_str::<Config>(&txt) {
                    Ok(mut c) => {
                        if c.normalize() {
                            c.migrate_icons_per_space();
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
        // on disk always matches the current schema. A fresh default IS the
        // per-space schema — mark it migrated so no wipe logic ever runs on
        // it later.
        let mut d = Config::default();
        d.icons_split = true;
        d.save();
        d
    }

    /// Validate every card record and snap its size to whole grid boxes.
    /// Returns false when ANY record is broken — the caller resets the whole
    /// config to default (owner rule: an incorrect config never limps along).
    fn normalize(&mut self) -> bool {
        let mut ok = true;
        for sp in &mut self.spaces {
            for n in &mut sp.notepads {
                ok &= valid_cell(n.col, n.row) && snap_dims(&mut n.w, &mut n.h);
            }
            for w in &mut sp.widgets {
                ok &= valid_cell(w.col, w.row) && snap_dims(&mut w.w, &mut w.h);
                // Fixed-canvas widgets snap to their canonical size, not
                // just to whole boxes: a card saved with an older canvas
                // used to ride the spaces payload oversized — and the
                // generic snap even rounded the stale height UP to the next
                // row, so the switch-wipe placeholder (which paints the
                // payload verbatim) hung past its grid slot.
                let (cw, ch) = canonical_widget_dims(&w.kind, w.w, w.h);
                w.w = cw;
                w.h = ch;
            }
            for (_, &(c, r)) in sp.icon_cells.iter() {
                ok &= valid_cell(c, r);
            }
        }
        ok &= self.active_space <= 1;
        ok &= matches!(
            self.effect.as_str(),
            // "rain" still parses (a legacy stored value) — it renders as
            // none; dropping it from the whitelist would reset the WHOLE
            // config over a retired string. Same for "wind": the farm moved
            // to the lock screen, a stale stored value renders as none.
            "" | "none" | "snow" | "leaves" | "star" | "wind" | "rain"
        );
        ok
    }

    /// One-time migration to per-space icons. A PRE-SPLIT config mapped
    /// EVERY desktop name in BOTH spaces (the old build_spaces resolved all
    /// names twice) — that duplicated desktop-2 map is wiped once; desktop 1
    /// keeps everything. A config written by the PER-SPACE build already
    /// holds single ownership, so its desktop-2 map must survive: the wipe
    /// only fires when every desktop-2 name is also mapped on desktop 1
    /// (the duplication signature). Runs right after a valid parse, before
    /// the desktop is ever built.
    fn migrate_icons_per_space(&mut self) {
        if self.icons_split {
            return;
        }
        // Every desktop-2 name ALSO on desktop 1 = the old duplicated map.
        let duplicated = !self.spaces[1].icon_cells.is_empty()
            && self
                .spaces[1]
                .icon_cells
                .keys()
                .all(|n| self.spaces[0].icon_cells.contains_key(n));
        let wiped = if duplicated {
            self.spaces[1].icon_cells.len()
        } else {
            0
        };
        if duplicated {
            self.spaces[1].icon_cells.clear();
        }
        self.icons_split = true;
        self.save();
        if duplicated {
            log(&format!(
                "config: icons are per-space now — desktop 2 starts empty ({} duplicated map entr{} dropped)",
                wiped,
                if wiped == 1 { "y" } else { "ies" }
            ));
        }
    }

    pub fn save(&self) {
        // Start Fresh in flight — every save call site stays silent, so the
        // wipe cannot be undone between the arm and the process exit.
        if FRESH_WIPE.load(Ordering::Relaxed) {
            return;
        }
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
