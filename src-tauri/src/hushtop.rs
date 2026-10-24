use std::collections::{HashMap, HashSet};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HANDLE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
    REG_VALUE_TYPE,
};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHGetImageList, SHGetKnownFolderPath, FOLDERID_Desktop,
    FOLDERID_PublicDesktop, KNOWN_FOLDER_FLAG, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
    SHGFI_SYSICONINDEX,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, EnumWindows, FindWindowExW, FindWindowW, GetAncestor, GetForegroundWindow,
    GetParent, GetWindowLongPtrW, IsIconic, IsWindow, PostMessageW,
    SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, GA_ROOT, GWL_STYLE, HWND_BOTTOM,
    HWND_TOP, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE,
    SW_SHOW, SW_SHOWNOACTIVATE, WM_COMMAND, WS_CAPTION, WS_CHILD, WS_MAXIMIZEBOX,
    WS_MINIMIZEBOX, WS_POPUP, WS_THICKFRAME, WS_VISIBLE, HICON,
};

use crate::icons;
use crate::shared::HushtopItem;
use crate::shared::Shared;
use crate::util::{log, to_wide, monitor_rect_at_cursor, virtual_screen_rect, dpi_for};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PROGMAN_TOGGLE_ICONS: usize = 0x7402;

/// Icon grid (CSS px): a cell spans two ruled lines. Keep in sync with
/// ui/hushtop.css (.cell) — the JS snap math uses the same numbers.
pub const CELL_W: i32 = 92;
pub const CELL_H: i32 = 72;
pub const GRID_MARGIN: i32 = 14;

fn hwnd(v: isize) -> HWND {
    HWND(v as _)
}

// ---------------------------------------------------------------------------
// COM (shell icon extraction wants an initialized apartment)
// ---------------------------------------------------------------------------

/// Initialize COM on the CALLING thread — every call, no once-flag. The old
/// static meant only the FIRST caller's thread got an apartment: cmd_open
/// runs ShellExecuteW on an IPC pool thread, and a later pool thread (or a
/// different command) silently ran with no COM at all. CoInitializeEx on an
/// already-initialized thread is a cheap S_FALSE.
fn com_init() {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if hr.is_err() {
            // RPC_E_CHANGED_MODE: the thread already runs the other
            // apartment — COM is available either way; keep going.
            log("hushtop: CoInitializeEx failed (continuing)");
        }
    }
}

// ---------------------------------------------------------------------------
// Known folders + desktop scan
// ---------------------------------------------------------------------------

fn known_folder(fid: &windows::core::GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(fid, KNOWN_FOLDER_FLAG(0), HANDLE(std::ptr::null_mut())).ok()?;
        if p.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *p.0.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len));
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *mut core::ffi::c_void));
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

pub fn desk_paths() -> Option<(PathBuf, PathBuf)> {
    let user = known_folder(&FOLDERID_Desktop)?;
    let public = known_folder(&FOLDERID_PublicDesktop).unwrap_or_else(|| user.clone());
    Some((user, public))
}

fn scan_signature() -> u64 {
    let Some((user, public)) = desk_paths() else {
        return 0;
    };
    let mut h: u64 = 0xcbf29ce484222325;
    fn mix(h: &mut u64, b: &[u8]) {
        for &x in b {
            *h ^= x as u64;
            *h = h.wrapping_mul(0x100000001b3);
        }
    }
    for dir in [&user, &public] {
        let mut names: Vec<(String, bool, u64)> = Vec::new();
        collect_dir(dir, &mut names);
        names.sort();
        for (n, d, m) in names {
            mix(&mut h, n.as_bytes());
            mix(&mut h, &[d as u8]);
            mix(&mut h, &m.to_le_bytes());
        }
        mix(&mut h, b"|hushtop|");
    }
    h
}

/// Enumerate one desktop folder. Returns false when the directory could not
/// be READ AT ALL (transient shell/AV lock) — the caller must not treat that
/// as "the folder is empty" or it would wipe every saved icon cell.
fn collect_dir(dir: &Path, out: &mut Vec<(String, bool, u64)>) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(md) = e.metadata() else { continue };
        let attr = md.file_attributes(); // std::os::windows::fs::MetadataExt
        if attr & 0x2 != 0 || attr & 0x4 != 0 {
            continue; // hidden / system (desktop.ini and friends)
        }
        if name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push((name, md.is_dir(), mtime));
    }
    true
}

/// Stable per-file signature for the icon cache (mtime+size hash): a
/// re-extracted icon only happens when the file actually changed.
fn fs_sig(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(md) => {
            let m = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let l = md.len();
            let mut h = m ^ l.wrapping_mul(0x9e3779b97f4a7c15);
            h ^= h >> 33;
            h = h.wrapping_mul(0xff51afd7ed558ccd);
            h ^= h >> 33;
            h
        }
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Shell icon extraction — the exact taskbar recolor pipeline
// ---------------------------------------------------------------------------

/// Highest-fidelity icon for a desktop item: jumbo (256px) system image
/// list first (that's what the real desktop draws at large sizes), then
/// the classic 32px extraction. Returns (HICON, owned).
unsafe fn shell_icon_hq(path: &Path) -> Option<(HICON, bool)> {
    let wide = to_wide(&path.as_os_str().to_string_lossy());
    let pc = PCWSTR(wide.as_ptr());

    // 1) jumbo system image list via the item's system icon index
    let mut sfi: SHFILEINFOW = std::mem::zeroed();
    let idx = SHGetFileInfoW(
        pc,
        FILE_ATTRIBUTE_NORMAL,
        Some(&mut sfi),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_SYSICONINDEX,
    );
    if idx != 0 && sfi.iIcon >= 0 {
        if let Ok(list) = unsafe { SHGetImageList::<IImageList>(4 /* SHIL_JUMBO */) } {
            if let Ok(hi) = list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0) {
                if !hi.is_invalid() {
                    return Some((hi, true));
                }
            }
        }
    }

    // 2) classic large icon (32px) — every icon the jumbo list lacks
    let mut sfi2: SHFILEINFOW = std::mem::zeroed();
    let r = SHGetFileInfoW(
        pc,
        FILE_ATTRIBUTE_NORMAL,
        Some(&mut sfi2),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON,
    );
    if r != 0 && !sfi2.hIcon.is_invalid() {
        return Some((sfi2.hIcon, true));
    }
    None
}

pub(crate) unsafe fn shell_icon_dark(path: &Path, _theme: &str) -> Option<String> {
    let (hicon, owned) = shell_icon_hq(path)?;
    let out = icons::icon_rgba(hicon).and_then(|(rgba, w, h)| {
        let (mut rgba, w, h) = icons::icon_canonical(rgba, w, h, icons::CANONICAL);
        icons::to_ink_tone(&mut rgba);
        icons::png_b64(&rgba, w, h)
    });
    if owned {
        let _ = DestroyIcon(hicon);
    }
    out
}

// ---------------------------------------------------------------------------
// Space regions — the two desktops and where each one lives
// ---------------------------------------------------------------------------

/// One space's placement region: origin + size in viewport CSS px. On a
/// single display every space spans the whole viewport; on two displays
/// each space fills its own monitor (the second-desktop widget's reason
/// to exist).
#[derive(Clone, Copy)]
struct Region {
    ox: i32,
    oy: i32,
    w: i32,
    h: i32,
}

impl Region {
    fn cols(&self) -> i32 {
        ((self.w - GRID_MARGIN * 2) / CELL_W).max(1)
    }
    fn rows(&self) -> i32 {
        ((self.h - GRID_MARGIN * 2) / CELL_H).max(1)
    }
    fn px_of(&self, c: i32, r: i32) -> (i32, i32) {
        (
            self.ox + GRID_MARGIN + c * CELL_W,
            self.oy + GRID_MARGIN + r * CELL_H,
        )
    }
    fn cell_of(&self, x: i32, y: i32) -> (i32, i32) {
        let c = (((x - self.ox - GRID_MARGIN) as f32 / CELL_W as f32).round() as i32)
            .clamp(0, self.cols() - 1);
        let r = (((y - self.oy - GRID_MARGIN) as f32 / CELL_H as f32).round() as i32)
            .clamp(0, self.rows() - 1);
        (c, r)
    }

    // CARD-grid extents: the card sheet runs on a 36 x 18 cadence anchored
    // at the same 14px margin (config.rs CARD_GRID_X / CARD_GRID_Y; the
    // webview's placeByCell uses exactly this math).
    fn card_cols(&self) -> i32 {
        ((self.w - GRID_MARGIN * 2) / crate::config::CARD_GRID_X).max(1)
    }
    fn card_rows(&self) -> i32 {
        ((self.h - GRID_MARGIN * 2) / crate::config::CARD_GRID_Y).max(1)
    }
    /// Viewport point -> CARD cell. The cell a card dropped here RENDERS at:
    /// cellPx(c, r) = ox + 14 + c*36 / oy + 14 + r*18. The icon-grid cell_of
    /// must NEVER be used for cards — its 92x72 cadence re-homes every drop
    /// (the widget landed wherever it pleased).
    fn cell_of_card(&self, x: i32, y: i32) -> (i32, i32) {
        let c = (((x - self.ox - GRID_MARGIN) as f32 / crate::config::CARD_GRID_X as f32).round()
            as i32)
            .clamp(0, self.card_cols() - 1);
        let r = (((y - self.oy - GRID_MARGIN) as f32 / crate::config::CARD_GRID_Y as f32).round()
            as i32)
            .clamp(0, self.card_rows() - 1);
        (c, r)
    }
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.ox && x < self.ox + self.w && y >= self.oy && y < self.oy + self.h
    }
}

/// Full-viewport region = the union of every monitor region.
fn full_region(view: &crate::shared::HushtopView) -> Region {
    let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for m in &view.monitors {
        l = l.min(m.x);
        t = t.min(m.y);
        r = r.max(m.x + m.w);
        b = b.max(m.y + m.h);
    }
    if l == i32::MAX {
        return Region { ox: 0, oy: 0, w: 1600, h: 900 };
    }
    Region { ox: l, oy: t, w: r - l, h: b - t }
}

/// Per-monitor regions in viewport CSS px, primary first. Each monitor's
/// physical rect divides by ITS OWN effective DPI: one webview spans
/// mixed-DPI monitors, so every region must land on the same CSS canvas
/// the webview draws.
fn monitor_regions_css(hh: HWND) -> Vec<crate::shared::MonCss> {
    let (vx, vy, _, _) = virtual_screen_rect();
    let fallback = (dpi_for(hh) as f64 / 96.0).max(1.0);
    crate::util::monitors()
        .iter()
        .map(|m| {
            let dpr = crate::util::monitor_dpi_at(m.x + m.w / 2, m.y + m.h / 2)
                .map(|d| (d as f64 / 96.0).max(1.0))
                .unwrap_or(fallback);
            crate::shared::MonCss {
                x: ((m.x - vx) as f64 / dpr).round() as i32,
                y: ((m.y - vy) as f64 / dpr).round() as i32,
                w: (m.w as f64 / dpr).round() as i32,
                h: (m.h as f64 / dpr).round() as i32,
            }
        })
        .collect()
}

/// The region a space occupies: dual mode pins space i to monitor i; a
/// single display gives every space the whole viewport.
fn region_for(view: &crate::shared::HushtopView, space: usize) -> Region {
    if view.dual {
        if let Some(m) = view.monitors.get(space) {
            return Region { ox: m.x, oy: m.y, w: m.w, h: m.h };
        }
    }
    full_region(view)
}

/// The space a viewport point belongs to: dual mode picks the monitor the
/// point sits on (fallback: the active space), single mode is always the
/// active one.
fn space_at(view: &crate::shared::HushtopView, x: i32, y: i32) -> usize {
    if view.dual {
        for (i, m) in view.monitors.iter().enumerate().take(2) {
            if (Region { ox: m.x, oy: m.y, w: m.w, h: m.h }).contains(x, y) {
                return i;
            }
        }
        return view.active.min(1);
    }
    view.active.min(1)
}

// ---------------------------------------------------------------------------
// Icon placement — free placing, cells written only by drops / new files /
// explicit sorts. A refresh NEVER rearranges anything.
// ---------------------------------------------------------------------------

fn nearest_free_cell(
    taken: &HashSet<(i32, i32)>,
    rows: i32,
    cols: i32,
    c0: i32,
    r0: i32,
) -> (i32, i32) {
    let in_grid = |c: i32, r: i32| c >= 0 && r >= 0 && c < cols && r < rows;
    if !taken.contains(&(c0, r0)) && in_grid(c0, r0) {
        return (c0, r0);
    }
    let max_radius = rows.max(cols);
    for radius in 1..=max_radius {
        let mut best: Option<((i32, i32, i32), (i32, i32))> = None;
        for dc in -radius..=radius {
            for dr in -radius..=radius {
                if dc.abs() != radius && dr.abs() != radius {
                    continue; // ring cells only
                }
                let (c, r) = (c0 + dc, r0 + dr);
                if !in_grid(c, r) || taken.contains(&(c, r)) {
                    continue;
                }
                let score = (dc.abs() + dr.abs(), c, r);
                if best.as_ref().map_or(true, |(s, _)| score < *s) {
                    best = Some((score, (c, r)));
                }
            }
        }
        if let Some((_, cell)) = best {
            return cell;
        }
    }
    (c0, r0) // grid full — accept the overlap rather than lose the icon
}

/// Resolve every name to a grid cell inside its region. Saved cells win
/// verbatim (render-time clamp only — the stored cell is never rewritten,
/// a bigger display restores the original spread); names without a cell
/// take the first free cell in column-major order AND KEEP it (the `newly`
/// list is persisted, so a new file's landing spot survives every refresh).
fn resolve_cells(
    names: &[String],
    saved: &std::collections::HashMap<String, (i32, i32)>,
    cols: i32,
    rows: i32,
    newly: &mut Vec<(String, (i32, i32))>,
) -> Vec<(i32, i32)> {
    let (cols, rows) = (cols.max(1), rows.max(1));
    let mut taken: HashSet<(i32, i32)> = HashSet::new();
    let mut result = vec![(0i32, 0i32); names.len()];
    let mut rehome: Vec<(usize, i32, i32)> = Vec::new();

    // Pass 1: saved cells.
    for (idx, name) in names.iter().enumerate() {
        if let Some(&(c, r)) = saved.get(name) {
            let cc = c.clamp(0, cols - 1);
            let rr = r.clamp(0, rows - 1);
            if taken.insert((cc, rr)) {
                result[idx] = (cc, rr);
            } else {
                rehome.push((idx, cc, rr));
            }
        }
    }

    // Pass 2: new names — first free cell, column-major.
    let total = (cols as i64) * (rows as i64);
    let mut cursor: i64 = 0;
    for (idx, name) in names.iter().enumerate() {
        if saved.contains_key(name) {
            continue;
        }
        let mut cell = None;
        while cursor < total {
            let c = (cursor / rows as i64) as i32;
            let r = (cursor % rows as i64) as i32;
            cursor += 1;
            if !taken.contains(&(c, r)) {
                cell = Some((c, r));
                break;
            }
        }
        match cell {
            Some((c, r)) => {
                taken.insert((c, r));
                result[idx] = (c, r);
                newly.push((name.clone(), (c, r)));
            }
            None => {
                // grid overflow: pile at the last cell, never lose the icon
                let (c, r) = (cols - 1, rows - 1);
                taken.insert((c, r));
                result[idx] = (c, r);
            }
        }
    }

    // Pass 3: rehome colliding saved cells to the nearest free spot.
    for (idx, c, r) in rehome {
        let cell = nearest_free_cell(&taken, rows, cols, c, r);
        taken.insert(cell);
        result[idx] = cell;
    }
    result
}

// ---------------------------------------------------------------------------
// Build both spaces -> HushtopView (+ emit DTO)
// ---------------------------------------------------------------------------

/// The space NEW desktop files land on: single display -> the ACTIVE space
/// (a file saved while desktop 2 is up belongs to desktop 2); dual -> the
/// monitor the cursor sits on (the user works where the cursor is).
fn landing_space(s: &Shared) -> usize {
    if s.hushtop_spaces.dual {
        unsafe {
            let mut pt = windows::Win32::Foundation::POINT { x: 0, y: 0 };
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
            for (i, m) in crate::util::monitors().iter().enumerate().take(2) {
                if pt.x >= m.x && pt.x < m.x + m.w && pt.y >= m.y && pt.y < m.y + m.h {
                    return i.min(1);
                }
            }
        }
        return s.cfg.active_space.min(1);
    }
    s.cfg.active_space.min(1)
}

unsafe fn build_spaces(s: &mut Shared) {
    let hh = hwnd(s.hwnd_hushtop);
    let monitors = monitor_regions_css(hh);
    let dual = monitors.len() >= 2;
    let active = s.cfg.active_space.min(1);

    let Some((user, public)) = desk_paths() else {
        s.hushtop_spaces = crate::shared::HushtopView {
            dual,
            active,
            monitors,
            items: vec![Vec::new(), Vec::new()],
        };
        return;
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut live: HashSet<String> = HashSet::new();
    let mut names: Vec<String> = Vec::new();
    let mut kinds: HashMap<String, bool> = HashMap::new();
    let mut icons: HashMap<String, Option<String>> = HashMap::new();
    // false when any folder could not be enumerated — cells are then kept
    // for everything (a transient read failure is not a deletion)
    let mut scan_ok = true;

    for dir in [&user, &public] {
        let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
        let mut sigs: Vec<(String, bool, u64)> = Vec::new();
        if !collect_dir(dir, &mut sigs) {
            log(&format!(
                "hushtop: desktop folder unreadable this round ({}) — layout refresh deferred",
                dir.display()
            ));
            scan_ok = false;
        }
        for (name, is_dir, _) in sigs {
            let p = dir.join(&name);
            entries.push((name, p, is_dir));
        }
        // Scan order is alphabetical ONLY as a stable enumeration — it has
        // zero effect on placement (cells are per-name, persisted).
        entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        for (name, path, is_dir) in entries {
            if !seen.insert(name.clone()) {
                continue; // public shadowed by user
            }
            let key = path.to_string_lossy().to_string();
            live.insert(key.clone());
            let sig = fs_sig(&path);
            let theme = "dark".to_string(); // dark-only shell; icon cache key
            let icon = match s.hushtop_icon_cache.get(&key) {
                Some((cached_sig, cached_theme, ic))
                    if *cached_sig == sig && cached_theme == &theme => ic.clone(),
                _ => {
                    let ic = shell_icon_dark(&path, &theme);
                    s.hushtop_icon_cache.insert(key, (sig, theme, ic.clone()));
                    ic
                }
            };
            kinds.insert(name.clone(), is_dir);
            icons.insert(name.clone(), icon);
            names.push(name);
        }
    }
    s.hushtop_icon_cache.retain(|k, _| live.contains(k));

    // ICONS ARE PER-SPACE. A name renders on a space only when that space's
    // map holds a cell for it; desktop 2 starts empty and only gains what is
    // dropped / created / moved onto it. A brand-new file (no cell anywhere)
    // lands on the landing space and nowhere else.
    let landing = landing_space(s);

    // Defensive: a name mapped in BOTH spaces collapses to the lowest index
    // (stage_drop_cells + cmd_move_space keep single ownership; this only
    // catches hand-edited configs). The collapse is persisted too — otherwise
    // every rescan would re-log it without ever fixing the file.
    let mut collapsed = false;
    for name in &names {
        let in_both =
            s.cfg.spaces[0].icon_cells.contains_key(name) && s.cfg.spaces[1].icon_cells.contains_key(name);
        if in_both {
            s.cfg.spaces[1].icon_cells.remove(name);
            collapsed = true;
            log(&format!(
                "hushtop: \"{name}\" was mapped on both desktops — desktop 1 wins"
            ));
        }
    }

    let regs: Vec<Region> = if dual {
        (0..2)
            .map(|i| {
                monitors.get(i).map(|m| Region { ox: m.x, oy: m.y, w: m.w, h: m.h }).unwrap_or(Region {
                    ox: 0, oy: 0, w: 1600, h: 900,
                })
            })
            .collect()
    } else {
        let r = full_region(&crate::shared::HushtopView {
            dual: false,
            active,
            monitors: monitors.clone(),
            items: Vec::new(),
        });
        vec![r, r]
    };

    let mut items: Vec<Vec<HushtopItem>> = Vec::with_capacity(2);
    let mut dirty = collapsed;
    for i in 0..2 {
        let reg = regs[i];
        // This space's roster: every live name it holds a cell for, plus —
        // on the landing space only — the brand-new files.
        let mut names_i: Vec<String> = Vec::new();
        for name in &names {
            if s.cfg.spaces[i].icon_cells.contains_key(name) {
                names_i.push(name.clone());
            }
        }
        if i == landing {
            for name in &names {
                if !s.cfg.spaces[0].icon_cells.contains_key(name)
                    && !s.cfg.spaces[1].icon_cells.contains_key(name)
                {
                    names_i.push(name.clone());
                }
            }
        }
        let mut newly: Vec<(String, (i32, i32))> = Vec::new();
        let cells = resolve_cells(&names_i, &s.cfg.spaces[i].icon_cells, reg.cols(), reg.rows(), &mut newly);
        if !newly.is_empty() {
            for (n, cell) in newly {
                s.cfg.spaces[i].icon_cells.insert(n, cell);
            }
            dirty = true;
        }
        let list: Vec<HushtopItem> = names_i
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let (c, r) = cells[idx];
                let (x, y) = reg.px_of(c, r);
                HushtopItem {
                    name: name.clone(),
                    kind: if kinds[name] { "folder" } else { "file" }.to_string(),
                    icon: icons[name].clone(),
                    x,
                    y,
                }
            })
            .collect();
        items.push(list);
    }
    // THE retain checks NAMES. It once checked `live` — which holds FULL
    // PATHS — so it never matched and EVERY rescan wiped every saved cell
    // of both spaces; the cell-less icons then re-landed on the ACTIVE
    // space (a drop on desktop 2 moved the whole desktop 1 over, and so on).
    // Only prune when the scan actually read the folders — a transient
    // enumerate failure must never eat the layout.
    if scan_ok {
        let live_names: HashSet<String> = names.iter().cloned().collect();
        for sp in s.cfg.spaces.iter_mut() {
            let before = sp.icon_cells.len();
            sp.icon_cells.retain(|k, _| live_names.contains(k));
            if sp.icon_cells.len() != before {
                dirty = true;
            }
        }
    }
    if dirty {
        s.cfg.save();
    }
    log(&format!(
        "hushtop: desktop 1 holds {} icons, desktop 2 holds {} (new files land on desktop {})",
        items[0].len(),
        items[1].len(),
        landing + 1
    ));
    s.hushtop_spaces = crate::shared::HushtopView { dual, active, monitors, items };
}

/// Cards of one space (the wire shape for the webview).
#[derive(serde::Serialize, Clone)]
pub struct SpaceCards {
    pub widgets: Vec<crate::config::HushWidget>,
    pub notepads: Vec<crate::config::Notepad>,
}

/// Everything the hushtop webview renders, in one payload: per-monitor
/// regions, both spaces' icon layouts, both spaces' cards, the active
/// space and the desktop effect.
#[derive(serde::Serialize, Clone)]
pub struct SpacesDto {
    pub dual: bool,
    pub active: usize,
    pub monitors: Vec<crate::shared::MonCss>,
    pub effect: String,
    pub items: Vec<Vec<HushtopItem>>,
    pub spaces: Vec<SpaceCards>,
}

pub fn spaces_dto(s: &Shared) -> SpacesDto {
    SpacesDto {
        dual: s.hushtop_spaces.dual,
        active: s.hushtop_spaces.active,
        monitors: s.hushtop_spaces.monitors.clone(),
        effect: s.cfg.effect.clone(),
        items: s.hushtop_spaces.items.clone(),
        spaces: (0..2)
            .map(|i| SpaceCards {
                widgets: s.cfg.spaces[i].widgets.clone(),
                notepads: s.cfg.spaces[i].notepads.clone(),
            })
            .collect(),
    }
}

#[derive(serde::Serialize, Clone)]
struct CardsDto {
    spaces: Vec<SpaceCards>,
}

fn cards_dto(s: &Shared) -> CardsDto {
    CardsDto {
        spaces: (0..2)
            .map(|i| SpaceCards {
                widgets: s.cfg.spaces[i].widgets.clone(),
                notepads: s.cfg.spaces[i].notepads.clone(),
            })
            .collect(),
    }
}

fn emit_cards(app: &AppHandle, s: &Shared) {
    let _ = app.emit_to("hushtop", "hushtop:cards", cards_dto(s));
}

/// Re-emit the full desktop view (dnd cleanup paths use it after moves).
pub fn emit_spaces(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    let _ = app.emit("hushtop:spaces", spaces_dto(&s));
}

pub unsafe fn rescan(app: &AppHandle, s: &mut Shared) {
    if s.hwnd_hushtop == 0 {
        return;
    }
    s.hushtop_scan_sig = scan_signature();
    s.hushtop_rescan = false;
    build_spaces(s);
    let _ = app.emit("hushtop:spaces", spaces_dto(s));
}

// ---------------------------------------------------------------------------
// Desktop band parenting — hushtop is a desktop REPLACEMENT, not a wallpaper.
// It must own the desktop surface: cover the original icon layer AND receive
// its clicks. That means parenting into the window that hosts SHELLDLL_DefView
// (the original icon layer's band) and sitting ABOVE that DefView — never into
// the spawned wallpaper WorkerW below it: a wallpaper-host child is permanently
// buried under the Progman band, whose icon layer then draws over the desktop
// and eats every click (hiding the icons changes nothing — the DefView window
// stays hit-testable even with no icons drawn).
// ---------------------------------------------------------------------------

/// The top-level window that directly hosts SHELLDLL_DefView — the original
/// icon layer's band. Stock Windows keeps it in Progman; rare shell states
/// re-home it into a sibling WorkerW. Either way this band sits at the bottom
/// of the top-level z-order, below every normal window and the taskbar, so a
/// child pinned above its DefView is the desktop for the whole shell.
unsafe fn find_icon_band_host() -> Option<HWND> {
    let mut found: isize = 0;
    let _ = EnumWindows(
        Some(defview_host_cb),
        LPARAM(&mut found as *mut isize as isize),
    );
    if found != 0 {
        return Some(hwnd(found));
    }
    // No DefView found (shell mid-restart): Progman is the band by definition.
    FindWindowW(w!("Progman"), PCWSTR::null()).ok()
}

/// EnumWindows body: the first shell window (Progman/WorkerW class only, so a
/// third-party lookalike can never win) that DIRECTLY hosts SHELLDLL_DefView.
unsafe extern "system" fn defview_host_cb(top: HWND, lparam: LPARAM) -> BOOL {
    let out = lparam.0 as *mut isize;
    if let Ok(dv) = FindWindowExW(top, HWND::default(), w!("SHELLDLL_DefView"), PCWSTR::null()) {
        if !dv.is_invalid() {
            let cls = crate::tasks::window_class(top);
            if cls == "Progman" || cls == "WorkerW" {
                *out = top.0 as isize;
                return BOOL(0);
            }
        }
    }
    BOOL(1)
}

fn wallpaper_root_ok(hh: HWND) -> bool {
    unsafe {
        let root = GetAncestor(hh, GA_ROOT);
        if root.0 == hh.0 || root.0.is_null() {
            return false;
        }
        let cls = crate::tasks::window_class(root);
        cls == "WorkerW" || cls == "Progman"
    }
}

/// Revert make_child: back to a plain top-level popup, so the next host
/// candidate attempt starts from the state the window was born in.
unsafe fn unchild(h: HWND) {
    let _ = SetParent(h, HWND::default());
    let style = GetWindowLongPtrW(h, GWL_STYLE);
    SetWindowLongPtrW(h, GWL_STYLE, (style & !WS_CHILD.0 as isize) | WS_POPUP.0 as isize);
}

unsafe fn make_child(h: HWND, parent: HWND) -> bool {
    let style = GetWindowLongPtrW(h, GWL_STYLE);
    let kill = WS_POPUP.0 as isize | WS_CAPTION.0 as isize | WS_THICKFRAME.0 as isize
        | WS_MINIMIZEBOX.0 as isize | WS_MAXIMIZEBOX.0 as isize;
    SetWindowLongPtrW(h, GWL_STYLE, (style & !kill) | WS_CHILD.0 as isize);
    match SetParent(h, parent) {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Client origin of `h` in screen coords (child coordinates are relative
/// to the parent's CLIENT area, which for WorkerW is usually the monitor,
/// but "usually" is not a coordinate system).
unsafe fn client_origin(h: HWND) -> (i32, i32) {
    let mut pt = POINT { x: 0, y: 0 };
    if ClientToScreen(h, &mut pt).as_bool() {
        (pt.x, pt.y)
    } else {
        (0, 0)
    }
}

pub static WE_HID_ICONS: AtomicBool = AtomicBool::new(false);

pub fn panic_restore_icons() {
    if WE_HID_ICONS.load(Ordering::Relaxed) {
        WE_HID_ICONS.store(false, Ordering::Relaxed);
        unsafe {
            if let Ok(pm) = FindWindowW(w!("Progman"), PCWSTR::null()) {
                let _ = PostMessageW(pm, WM_COMMAND, WPARAM(PROGMAN_TOGGLE_ICONS), LPARAM(0));
            }
        }
        log("hushtop: panic path — original desktop icons restored");
    }
}

unsafe fn hide_original_icons_inner(we_own_previous: bool) -> bool {
    let reg_hidden = read_hideicons();
    if !reg_hidden {
        if let Ok(pm) = FindWindowW(w!("Progman"), PCWSTR::null()) {
            let _ = PostMessageW(pm, WM_COMMAND, WPARAM(PROGMAN_TOGGLE_ICONS), LPARAM(0));
            WE_HID_ICONS.store(true, Ordering::Relaxed);
            log("hushtop: original desktop icons hidden");
            return true;
        }
        return false;
    }
    if we_own_previous {
        WE_HID_ICONS.store(true, Ordering::Relaxed);
        log("hushtop: claiming icons hidden by a previous run (restore on exit)");
        return true;
    }
    log("hushtop: original desktop icons were already hidden (user setting)");
    false
}

unsafe fn apply_core(hh: HWND) -> isize {
    com_init();
    // The custom desktop spans EVERY monitor: the desktop band (Progman or
    // whatever top-level window hosts the original icon layer) covers the
    // virtual screen, so the paper must too — icons and widgets can live on
    // any display. One webview, one map, all monitors.
    let (mx, my, mw, mh) = virtual_screen_rect();

    // Size + place while still hidden (opaque window — no dance needed).
    let _ = SetWindowPos(hh, HWND_BOTTOM, mx, my, mw, mh, SWP_NOACTIVATE);

    // Parent into the icon layer's band (normally Progman), ABOVE its
    // DefView — the only placement where the desktop replacement both covers
    // the original icons and receives the desktop's clicks. A retry covers a
    // shell mid-restart; with no band at all the beat re-dances later.
    let mut host = find_icon_band_host();
    if host.is_none() {
        std::thread::sleep(std::time::Duration::from_millis(350));
        host = find_icon_band_host();
    }

    let mut cands: Vec<HWND> = Vec::new();
    if let Some(h) = host {
        cands.push(h);
    }
    if let Ok(pm) = FindWindowW(w!("Progman"), PCWSTR::null()) {
        if !cands.iter().any(|c| c.0 == pm.0) {
            cands.push(pm);
        }
    }

    let mut parent: isize = 0;
    for cand in cands {
        if !make_child(hh, cand) {
            log("hushtop: SetParent failed for a candidate — trying the next");
            continue;
        }
        if !wallpaper_root_ok(hh) {
            log("hushtop: candidate did not take (root is not a desktop band host) — trying the next");
            unchild(hh);
            let _ = SetWindowPos(hh, HWND_BOTTOM, mx, my, mw, mh, SWP_NOACTIVATE);
            continue;
        }
        let (pox, poy) = client_origin(cand);
        let _ = SetWindowPos(
            hh,
            HWND_TOP,
            mx - pox,
            my - poy,
            mw,
            mh,
            SWP_NOACTIVATE,
        );
        parent = cand.0 as isize;
        log("hushtop: parented into the desktop band, above the original icon layer (verified by root class)");
        break;
    }
    if parent == 0 {
        log("hushtop: no host took us — top-level fallback (the beat re-dances)");
        let _ = SetWindowPos(hh, HWND_BOTTOM, mx, my, mw, mh, SWP_NOACTIVATE);
    }
    let _ = ShowWindow(hh, SW_SHOW);
    parent
}

/// Public wrapper for the display-hotplug path (native::display_change):
/// re-size + re-parent the desktop onto the CURRENT virtual screen.
pub unsafe fn apply_core_public(hh: HWND) -> isize {
    apply_core(hh)
}

pub unsafe fn init(app: &AppHandle, st: &crate::AppShared) {
    let (hh, we_own_previous) = {
        let s = crate::util::lock_shared(&st.0);
        if s.hwnd_hushtop == 0 {
            log("hushtop: no window handle yet");
            return;
        }
        (hwnd(s.hwnd_hushtop), s.cfg.hushtop_hid_icons)
    };
    let parent = apply_core(hh);
    let hid = hide_original_icons_inner(we_own_previous);
    {
        let mut s = crate::util::lock_shared(&st.0);
        s.hushtop_parent = parent;
        s.hushtop_applied = true;
        s.hushtop_scan_sig = 0;
        s.hushtop_rescan = true;
        s.hushtop_icons_hidden = hid;
        s.cfg.hushtop_hid_icons = hid;
        s.cfg.save();
    }
    let _ = app;
}

fn read_hideicons() -> bool {
    unsafe {
        let sub = to_wide("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced");
        let mut hk = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            0,
            KEY_QUERY_VALUE,
            &mut hk,
        ) != windows::Win32::Foundation::ERROR_SUCCESS
        {
            return false;
        }
        let name = to_wide("HideIcons");
        let mut ty = REG_VALUE_TYPE(0);
        let mut val: u32 = 0;
        let mut cb: u32 = 4;
        let r = RegQueryValueExW(
            hk,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(&mut val as *mut u32 as *mut u8),
            Some(&mut cb),
        );
        let _ = RegCloseKey(hk);
        r == windows::Win32::Foundation::ERROR_SUCCESS && val == 1
    }
}

/// Exit path: give the original desktop its icons back.
pub unsafe fn restore(s: &mut Shared) {
    if s.hushtop_icons_hidden {
        if let Ok(pm) = FindWindowW(w!("Progman"), PCWSTR::null()) {
            let _ = PostMessageW(pm, WM_COMMAND, WPARAM(PROGMAN_TOGGLE_ICONS), LPARAM(0));
            log("hushtop: original desktop icons restored");
        }
        s.hushtop_icons_hidden = false;
        WE_HID_ICONS.store(false, Ordering::Relaxed);
        s.cfg.hushtop_hid_icons = false;
        s.cfg.save();
    }
}

// ---------------------------------------------------------------------------
// The heartbeat beat (registered in native.rs build_cycle)
// ---------------------------------------------------------------------------

pub unsafe fn beat(app: &AppHandle, s: &mut Shared, beats: u64) {
    if s.hwnd_hushtop == 0 {
        return;
    }
    let hh = hwnd(s.hwnd_hushtop);

    if !IsWindow(hh).as_bool() {
        if !s.hushtop_rebuild_queued && s.hushtop_rebuild_fails < 5 {
            s.hushtop_rebuild_queued = true;
            log("hushtop: window lost (host restart?) — rebuilding");
            let a2 = app.clone();
            let _ = app.run_on_main_thread(move || {
                let st = a2.state::<crate::AppShared>();
                let built = (|| -> Result<isize, String> {
                    let win = tauri::WebviewWindowBuilder::new(
                        &a2,
                        "hushtop",
                        tauri::WebviewUrl::App("hushtop.html".into()),
                    )
                    .title("hushell-hushtop")
                    .decorations(false)
                    .transparent(false)
                    .shadow(false)
                    .always_on_top(false)
                    .skip_taskbar(true)
                    .resizable(false)
                    .focused(false)
                    .visible(false)
                    .build()
                    .map_err(|e| e.to_string())?;
                    win.hwnd().map_err(|e| e.to_string())?;
                    Ok(win.hwnd().map(|h| h.0 as isize).unwrap_or(0))
                })();
                let mut s2 = crate::util::lock_shared(&st.0);
                s2.hushtop_rebuild_queued = false;
                match built {
                    Ok(h) => {
                        // The old window's win32 guards died with it — the
                        // rebuild re-installs both subclasses, otherwise the
                        // fresh hushtop would be Alt+F4-killable (the Tauri
                        // close-request refusal still holds, but stay
                        // layered: win32 first, like every other window).
                        let nc = crate::frame::guard_no_close(h);
                        let fr = crate::frame::guard_frameless(h);
                        s2.hwnd_hushtop = h;
                        drag_hwnds::HUSHTOP.store(h, std::sync::atomic::Ordering::Relaxed);
                        crate::dnd::install(&a2);
                        log(&format!(
                            "hushtop: window rebuilt (no-close={nc}, frameless={fr}) — re-applying next beat"
                        ));
                    }
                    Err(e) => {
                        s2.hushtop_rebuild_fails += 1;
                        log(&format!("hushtop: rebuild failed ({e})"));
                    }
                }
            });
        }
        return;
    }

    if !s.hushtop_rebuild_queued && !s.hushtop_reapply_queued {
        if wallpaper_root_ok(hh) {
            s.hushtop_dance_cooldown = 0;
        } else if s.hushtop_dance_cooldown > 0 {
            s.hushtop_dance_cooldown -= 1;
        } else {
            log("hushtop: window is NOT inside the desktop band — re-dancing (main thread)");
            s.hushtop_dance_cooldown = 30; // 3 s at BEAT_MS=100 — no busy hammering
            s.hushtop_reapply_queued = true;
            let hh_id = hh.0 as isize;
            let a2 = app.clone();
            let _ = app.run_on_main_thread(move || unsafe {
                let st = a2.state::<crate::AppShared>();
                let parent = apply_core(hwnd(hh_id));
                let mut s2 = crate::util::lock_shared(&st.0);
                s2.hushtop_reapply_queued = false;
                s2.hushtop_parent = parent;
                s2.hushtop_applied = true;
                let hid = hide_original_icons_inner(s2.cfg.hushtop_hid_icons);
                s2.hushtop_icons_hidden = hid || s2.hushtop_icons_hidden;
                s2.cfg.hushtop_hid_icons = s2.hushtop_icons_hidden;
                s2.cfg.save();
            });
            return;
        }
    }

    if s.hushtop_applied && !s.hushtop_rebuild_queued && !s.hushtop_repair_queued {
        // Ancestor chain, bottom-up: [hh, parent, ... , root]. Capped at 8 —
        // a desktop band chain is 2 deep; the cap only bounds a pathological
        // re-parent race.
        let mut chain: Vec<HWND> = Vec::new();
        let mut cur = hh;
        for _ in 0..8 {
            chain.push(cur);
            match GetParent(cur) {
                Ok(p) if !p.0.is_null() && p.0 != cur.0 => cur = p,
                _ => break,
            }
        }
        let mut offenders: Vec<HWND> = Vec::new();
        for w in &chain {
            let visible_bit = GetWindowLongPtrW(*w, GWL_STYLE) & WS_VISIBLE.0 as isize != 0;
            if IsIconic(*w).as_bool() || !visible_bit {
                offenders.push(*w);
            }
        }
        let wrong = !offenders.is_empty();
        if wrong != s.hushtop_presence_wrong {
            log("hushtop: hidden (Win+D / Show Desktop?) — re-asserting presence (main thread)");
        }
        s.hushtop_presence_wrong = wrong;
        if wrong {
            s.hushtop_repair_queued = true;
            let hh_id = hh.0 as isize;
            let parent_id = s.hushtop_parent;
            let offenders_ids: Vec<isize> = offenders.iter().map(|w| w.0 as isize).collect();
            let a2 = app.clone();
            let _ = app.run_on_main_thread(move || unsafe {
                for id in offenders_ids.iter().rev() {
                    let w = hwnd(*id);
                    if IsWindow(w).as_bool() {
                        let _ = ShowWindow(w, SW_SHOWNOACTIVATE);
                    }
                }
                let h = hwnd(hh_id);
                if IsWindow(h).as_bool() {
                    let (mx, my, mw, mh) = virtual_screen_rect();
                    if parent_id != 0 && IsWindow(hwnd(parent_id)).as_bool() {
                        let (pox, poy) = client_origin(hwnd(parent_id));
                        let _ = SetWindowPos(
                            h,
                            HWND_TOP,
                            mx - pox,
                            my - poy,
                            mw,
                            mh,
                            SWP_NOACTIVATE,
                        );
                    } else {
                        let _ = SetWindowPos(h, HWND_BOTTOM, mx, my, mw, mh, SWP_NOACTIVATE);
                    }
                    let root = GetAncestor(h, GA_ROOT);
                    if !root.0.is_null() && root.0 != h.0 {
                        let _ = SetWindowPos(
                            root,
                            HWND_BOTTOM,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
                let st = a2.state::<crate::AppShared>();
                let mut s2 = crate::util::lock_shared(&st.0);
                s2.hushtop_repair_queued = false;
            });
        }

        // Icon ownership: an external toggle (a previous instance's exit
        // restore, a duplicate's panic path, a shell restart) can flip the
        // original icon layer back on over the applied desktop — it then
        // draws over the wallpaper and eats every click. One registry read
        // per 10 beats; the toggle fires only while the layer is visible.
        if beats % 10 == 0 && !read_hideicons() {
            if let Ok(pm) = FindWindowW(w!("Progman"), PCWSTR::null()) {
                let _ = PostMessageW(pm, WM_COMMAND, WPARAM(PROGMAN_TOGGLE_ICONS), LPARAM(0));
                WE_HID_ICONS.store(true, Ordering::Relaxed);
                log("hushtop: icon layer visible over the applied desktop — re-hidden");
                if !s.hushtop_icons_hidden {
                    s.hushtop_icons_hidden = true;
                    s.cfg.hushtop_hid_icons = true;
                    s.cfg.save();
                }
            }
        }
    }

    if s.hushtop_prompt_open && s.hushtop_prompt_fg != 0 {
        let fg = GetForegroundWindow().0 as isize;
        if fg != s.hushtop_prompt_fg {
            prompt_hide_with(s);
        }
    }

    // 4) Context menu auto-close on foreground change (clicks inside the
    //    wallpaper close it client-side; this catches clicks elsewhere).
    if s.hushtop_menu_fg != 0 {
        let fg = GetForegroundWindow().0 as isize;
        if fg != s.hushtop_menu_fg {
            s.hushtop_menu_fg = 0;
            let _ = app.emit("hushtop:menu_close", ());
        }
    }

    // 5) Desktop scan — signature only every 10 beats (1 s), real work
    //    only when the signature moved.
    if beats % 10 == 0 || s.hushtop_rescan {
        let sig = scan_signature();
        if sig != s.hushtop_scan_sig || s.hushtop_rescan {
            rescan(app, s);
        }
    }

    if beats % 10 == 0 {
        crate::dnd::tick(app, s.hwnd_hushtop);
    }
}

// ---------------------------------------------------------------------------
// Name dialog (rename / new folder / new file)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct PromptDto {
    mode: String,
    name: String,
}

pub unsafe fn prompt_show(app: &AppHandle, mode: &str, name: &str) {
    {
        let st = app.state::<crate::AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        s.hushtop_prompt_mode = mode.to_string();
        s.hushtop_prompt_name = name.to_string();
        s.hushtop_prompt_open = true;
    }
    let st = app.state::<crate::AppShared>();
    let hp = {
        let s = crate::util::lock_shared(&st.0);
        s.hwnd_prompt
    };
    if hp == 0 {
        log("hushtop: prompt window missing");
        return;
    }
    // Center-ish on the monitor the user is WORKING on (cursor) — the
    // desktop spans every display now, so "the hushtop monitor" is
    // meaningless. Falls back to the primary.
    let (mx, my, mw, mh) = monitor_rect_at_cursor();
    let dpr = (dpi_for(hwnd(hp)) as f64 / 96.0).max(1.0);
    let (w, h) = ((372.0 * dpr) as i32, (214.0 * dpr) as i32);
    let _ = SetWindowPos(
        hwnd(hp),
        HWND_TOPMOST,
        mx + (mw - w) / 2,
        my + (mh - h) / 3,
        w,
        h,
        SWP_NOACTIVATE,
    );
    crate::native::activate_hwnd(hp);
    let _ = ShowWindow(hwnd(hp), SW_SHOW);
    {
        let st = app.state::<crate::AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        s.hushtop_prompt_fg = GetForegroundWindow().0 as isize;
    }
    let _ = app.emit(
        "hushtop:prompt",
        PromptDto {
            mode: mode.into(),
            name: name.into(),
        },
    );
    log(&format!("hushtop: prompt shown ({mode})"));
}

/// Command entry (main thread): locks, then delegates to the core.
pub unsafe fn prompt_hide(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    prompt_hide_with(&mut s);
}

pub unsafe fn prompt_hide_with(s: &mut Shared) {
    let was = s.hushtop_prompt_open;
    s.hushtop_prompt_open = false;
    s.hushtop_prompt_fg = 0;
    let hp = s.hwnd_prompt;
    if hp != 0 {
        let _ = ShowWindow(hwnd(hp), SW_HIDE);
    }
    if was {
        log("hushtop: prompt closed");
    }
}

// ---------------------------------------------------------------------------
// File operations behind the IPC commands
// ---------------------------------------------------------------------------

const BAD_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

fn valid_name(name: &str) -> Result<(), String> {
    let t = name.trim();
    if t.is_empty() {
        return Err("Name is empty".into());
    }
    if t != name.trim_end() || t.ends_with('.') {
        return Err("Trailing spaces or dots are not allowed".into());
    }
    if t.len() > 200 {
        return Err("Name is too long".into());
    }
    if t == "." || t == ".." {
        return Err("That name is reserved".into());
    }
    if t.chars().any(|c| BAD_CHARS.contains(&c) || c.is_control()) {
        return Err("Name contains forbidden characters".into());
    }
    let stem = t.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6"
        | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6"
        | "LPT7" | "LPT8" | "LPT9"
    ) {
        return Err("That name is reserved by Windows".into());
    }
    Ok(())
}

pub(crate) fn item_path(name: &str) -> Result<PathBuf, String> {
    valid_name(name)?;
    let (user, public) = desk_paths().ok_or_else(|| "Desktop folder unavailable".to_string())?;
    let p = user.join(name);
    if p.exists() {
        return Ok(p);
    }
    let p = public.join(name);
    if p.exists() {
        return Ok(p);
    }
    Err(format!("\"{name}\" no longer exists"))
}

pub fn cmd_move(app: &AppHandle, name: String, x: i32, y: i32) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if s.hwnd_hushtop == 0 {
        return Err("hushtop is not running".into());
    }
    // The drop point picks the space: dual mode -> the monitor it landed on,
    // single mode -> the active space.
    let space = space_at(&s.hushtop_spaces, x, y);
    // The icon must be RENDERED somewhere — not necessarily on the drop
    // space: a dual-display drag across the boundary lands on the other
    // monitor and that is an ownership transfer, not an error.
    if !(0..2).any(|i| s.hushtop_spaces.items_of(i).iter().any(|it| it.name == name)) {
        return Err(format!("\"{name}\" is not on hushtop"));
    }
    let reg = region_for(&s.hushtop_spaces, space);
    let (c, r) = reg.cell_of(x, y);
    // Occupancy of every OTHER icon of THAT space.
    let mut taken: HashSet<(i32, i32)> = HashSet::new();
    for it in s.hushtop_spaces.items_of(space).iter().filter(|i| i.name != name) {
        taken.insert(reg.cell_of(it.x, it.y));
    }
    let (fc, fr) = nearest_free_cell(&taken, reg.rows(), reg.cols(), c, r);
    let (cx, cy) = reg.px_of(fc, fr);
    // A drop is an OWNERSHIP decision: the other space forgets the name
    // (the build_spaces collapse used to "snap back" cross-monitor moves —
    // the name was mapped on BOTH spaces and desktop 1 always won).
    for (i, sp) in s.cfg.spaces.iter_mut().enumerate() {
        if i != space {
            sp.icon_cells.remove(&name);
        }
    }
    s.cfg.spaces[space].icon_cells.insert(name.clone(), (fc, fr));
    s.cfg.save();
    // keep the wire model in sync: a cross-space move swaps the item over
    let mut carried: Option<HushtopItem> = None;
    for (i, list) in s.hushtop_spaces.items.iter_mut().enumerate() {
        if i == space {
            continue;
        }
        if let Some(pos) = list.iter().position(|it| it.name == name) {
            carried = Some(list.remove(pos));
            break;
        }
    }
    let hit = s.hushtop_spaces.items[space]
        .iter_mut()
        .find(|i| i.name == name);
    match (hit, carried) {
        (Some(it), _) => {
            it.x = cx;
            it.y = cy;
        }
        (None, Some(mut it)) => {
            it.x = cx;
            it.y = cy;
            s.hushtop_spaces.items[space].push(it);
        }
        (None, None) => {}
    }
    if (fc, fr) != (c, r) {
        log(&format!(
            "hushtop: \"{name}\" drop collided in space {} — moved to ({cx},{cy})",
            space + 1
        ));
    } else {
        log(&format!(
            "hushtop: \"{name}\" moved to cell ({fc},{fr}) of space {}",
            space + 1
        ));
    }
    // ALWAYS broadcast: the webview may never learn the drop point
    // otherwise (a silent move left the icon wherever the last emit put
    // it — the sorted first-free spot for adopted files, the drag ghost
    // spot for boomerangs — until some later rescan snapped it).
    let _ = app.emit("hushtop:spaces", spaces_dto(&s));
    Ok(())
}

/// Pre-stage drop cells for files adopted from another window: every name
/// gets a SAVED cell at the drop point (the k-th file steps one cell right)
/// of the space under the drop. A drop is an OWNERSHIP decision: every other
/// space's cell for these names is forgotten first (an existing icon
/// re-dropped from an Explorer window moves to the space it landed on).
/// Existing saved cells of the target space are the occupancy truth — a
/// staged cell that would displace one re-homes via nearest_free_cell,
/// exactly like cmd_move, so the rescan renders the adopted icons in their
/// final place on the first paint.
pub fn stage_drop_cells(s: &mut Shared, names: &[String], cx: i32, cy: i32) {
    let space = space_at(&s.hushtop_spaces, cx, cy).min(1);
    for (i, sp) in s.cfg.spaces.iter_mut().enumerate() {
        if i == space {
            continue;
        }
        for n in names {
            if sp.icon_cells.remove(n).is_some() {
                log(&format!(
                    "hushtop: drop staging moved \"{n}\" from desktop {} to desktop {}",
                    i + 1,
                    space + 1
                ));
            }
        }
    }
    let reg = region_for(&s.hushtop_spaces, space);
    let mut taken: HashSet<(i32, i32)> = HashSet::new();
    for (n, cell) in s.cfg.spaces[space].icon_cells.iter() {
        if names.iter().any(|k| k == n) {
            continue; // the moving batch re-stages itself below
        }
        taken.insert(*cell);
    }
    for (k, name) in names.iter().enumerate() {
        let (c0, r0) = reg.cell_of(cx + k as i32 * CELL_W, cy);
        let (c, r) = nearest_free_cell(&taken, reg.rows(), reg.cols(), c0, r0);
        taken.insert((c, r));
        s.cfg.spaces[space].icon_cells.insert(name.clone(), (c, r));
    }
}

/// Context-menu move: send icons to the OTHER desktop (the single-display
/// affordance — with two monitors a drag across the boundary already does
/// this via cmd_move). Each icon keeps its cell when the target cell is
/// free; otherwise it takes the nearest free cell of the target grid.
/// Names the saved map has forgotten still move — the RENDERED layout is
/// the fallback truth (an icon the user can see is an icon that can move);
/// names already on the target are a silent no-op, not an error (the old
/// "nothing to move" Err toasted "Could not move" at users who had done
/// nothing wrong).
pub fn cmd_move_space(app: &AppHandle, names: Vec<String>, target: usize) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if s.hwnd_hushtop == 0 {
        return Err("hushtop is not running".into());
    }
    let target = target.min(1);
    let reg = region_for(&s.hushtop_spaces, target);
    let mut moved: Vec<String> = Vec::new();
    let mut known = 0usize; // names we could locate at all
    for name in &names {
        let owner = cell_of_name(&s, name).or_else(|| rendered_cell_of(&s, name));
        let Some((from, cell)) = owner else {
            continue;
        };
        known += 1;
        if from == target {
            continue;
        }
        // Occupancy of the target's OTHER members (the moving batch re-homes
        // below, first-come wins).
        let mut taken: HashSet<(i32, i32)> = HashSet::new();
        for (n, c) in s.cfg.spaces[target].icon_cells.iter() {
            if names.contains(n) {
                continue;
            }
            taken.insert(*c);
        }
        let (fc, fr) = nearest_free_cell(&taken, reg.rows(), reg.cols(), cell.0, cell.1);
        s.cfg.spaces[from].icon_cells.remove(name);
        s.cfg.spaces[target].icon_cells.insert(name.clone(), (fc, fr));
        log(&format!(
            "hushtop: \"{name}\" moved desktop {} -> desktop {} at cell ({fc},{fr})",
            from + 1,
            target + 1
        ));
        moved.push(name.clone());
    }
    if known == 0 {
        return Err("none of the selected icons is on the desktop".into());
    }
    if !moved.is_empty() {
        s.cfg.save();
        unsafe { rescan(app, &mut s) };
    }
    Ok(())
}

pub fn cmd_menu(app: &AppHandle, open: bool) {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    s.hushtop_menu_fg = if open {
        unsafe { GetForegroundWindow().0 as isize }
    } else {
        0
    };
}

pub fn cmd_open(_app: &AppHandle, name: String) -> Result<(), String> {
    let path = item_path(&name)?;
    com_init();
    let verb = to_wide("open");
    let file = to_wide(&path.as_os_str().to_string_lossy());
    unsafe {
        let r = windows::Win32::UI::Shell::ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
        if (r.0 as usize) <= 32 {
            log(&format!("hushtop: open \"{name}\" failed"));
            return Err(format!("Could not open \"{name}\""));
        }
    }
    log(&format!("hushtop: opened \"{name}\""));
    Ok(())
}

pub unsafe fn cmd_delete(app: &AppHandle, name: String) -> Result<(), String> {
    cmd_delete_many(app, vec![name])
}

/// Recycle-bin delete of one or more desktop items — ONE SHFileOperationW
/// for the whole list (a bulk delete is one motion, not a wizard), then a
/// single rescan. Every space forgets the names.
pub unsafe fn cmd_delete_many(app: &AppHandle, names: Vec<String>) -> Result<(), String> {
    if names.is_empty() {
        return Ok(());
    }
    let mut from: Vec<u16> = Vec::new();
    let mut valid: Vec<String> = Vec::new();
    for n in &names {
        match item_path(n) {
            Ok(p) => {
                from.extend(to_wide(&p.as_os_str().to_string_lossy()));
                from.push(0); // pFrom entries are NUL separated
                valid.push(n.clone());
            }
            Err(_) => log(&format!("hushtop: delete skipped \"{n}\" (already gone)")),
        }
    }
    if from.is_empty() {
        return Err("nothing left to delete".into());
    }
    from.push(0); // double-NUL list terminator
    let mut op = windows::Win32::UI::Shell::SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: windows::Win32::UI::Shell::FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (windows::Win32::UI::Shell::FOF_ALLOWUNDO.0
            | windows::Win32::UI::Shell::FOF_NOCONFIRMATION.0
            | windows::Win32::UI::Shell::FOF_SILENT.0
            | windows::Win32::UI::Shell::FOF_NOERRORUI.0) as u16,
        ..Default::default()
    };
    let r = windows::Win32::UI::Shell::SHFileOperationW(&mut op);
    if r != 0 || op.fAnyOperationsAborted.as_bool() {
        log(&format!("hushtop: delete {} item(s) failed (r={r})", valid.len()));
        return Err("Could not delete".into());
    }
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    for sp in s.cfg.spaces.iter_mut() {
        for n in &valid {
            sp.icon_cells.remove(n);
        }
    }
    s.cfg.save();
    rescan(app, &mut s);
    log(&format!(
        "hushtop: deleted {} item(s) (recycle bin)",
        valid.len()
    ));
    Ok(())
}

pub unsafe fn cmd_rename(app: &AppHandle, old: String, new: String) -> Result<(), String> {
    valid_name(&new)?;
    if old == new {
        return Ok(());
    }
    let path = item_path(&old)?;
    let target = path.parent().unwrap_or(Path::new(".")).join(&new);
    if target.exists() {
        return Err(format!("\"{new}\" already exists"));
    }
    std::fs::rename(&path, &target).map_err(|e| e.to_string())?;
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    // The cell carries across the rename in whatever space owned it — the
    // icon stays on ITS desktop instead of re-homing as a "new" file.
    for sp in s.cfg.spaces.iter_mut() {
        if let Some(cell) = sp.icon_cells.remove(&old) {
            sp.icon_cells.insert(new.clone(), cell);
        }
    }
    s.cfg.save();
    rescan(app, &mut s);
    log(&format!("hushtop: renamed \"{old}\" -> \"{new}\""));
    Ok(())
}

pub unsafe fn cmd_create(app: &AppHandle, kind: String, name: String, ext: String) -> Result<(), String> {
    valid_name(&name)?;
    let (user, _) = desk_paths().ok_or_else(|| "Desktop folder unavailable".to_string())?;
    let final_name = match kind.as_str() {
        "folder" => name.clone(),
        "file" => {
            let e = ext.trim().trim_start_matches('.').to_string();
            if e.is_empty() {
                name.clone()
            } else {
                valid_name(&format!("{name}.{e}"))?;
                format!("{name}.{e}")
            }
        }
        _ => return Err("Unknown kind".into()),
    };
    let path = user.join(&final_name);
    if path.exists() {
        return Err(format!("\"{final_name}\" already exists"));
    }
    match kind.as_str() {
        "folder" => std::fs::create_dir(&path).map_err(|e| e.to_string())?,
        "file" => std::fs::write(&path, b"").map_err(|e| e.to_string())?,
        _ => unreachable!(),
    }
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    rescan(app, &mut s);
    log(&format!(
        "hushtop: created {kind} \"{final_name}\" (lands on desktop {})",
        landing_space(&s) + 1
    ));
    Ok(())
}

/// Create a widget and emit the card lists. Returns the widget id.
pub fn notepad_add(app: &AppHandle, col: i32, row: i32, space: usize) -> Result<u32, String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    let space = space.min(1);
    let id = next_card_id(&s);
    let (col, row) = (col.clamp(-1000, 1000), row.clamp(-1000, 1000));
    s.cfg.spaces[space].notepads.push(crate::config::Notepad {
        id,
        col,
        row,
        // Born box-to-box: 7 x 10 whole cells (252 x 180).
        w: 252,
        h: 180,
        text: String::new(),
    });
    s.cfg.save();
    log(&format!(
        "hushtop: HUSH created notepad widget #{id} at cell ({col},{row}) of space {}",
        space + 1
    ));
    emit_cards(app, &s);
    Ok(id)
}

/// One id space across BOTH spaces (cards move by id, never by space).
fn next_card_id(s: &Shared) -> u32 {
    s.cfg
        .spaces
        .iter()
        .flat_map(|sp| sp.notepads.iter().map(|n| n.id).chain(sp.widgets.iter().map(|w| w.id)))
        .max()
        .unwrap_or(0)
        + 1
}

/// Persist a notepad's layout-map cell + size + text (called by hushtop.js
/// on drop and on debounced edits). No re-emit: the JS already applied its
/// own state. Cells are stored verbatim (±1000 sanity bound only — out of
/// bounds is allowed by design; the bound just rejects garbage values),
/// no pixel math, nothing nudges a position back on screen.
pub fn notepad_save(
    app: &AppHandle,
    id: u32,
    col: i32,
    row: i32,
    w: i32,
    h: i32,
    text: String,
) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    let n = s
        .cfg
        .spaces
        .iter_mut()
        .flat_map(|sp| sp.notepads.iter_mut())
        .find(|n| n.id == id)
        .ok_or_else(|| format!("no notepad widget #{id}"))?;
    n.col = col.clamp(-1000, 1000);
    n.row = row.clamp(-1000, 1000);
    // Sizes are whole grid boxes (owner rule: every card fits box-to-box).
    // Snap after the sanity clamp; the cap re-clamps a snap overshoot.
    n.w = crate::config::snap_up(w.clamp(220, 720), crate::config::CARD_GRID_X).min(720);
    n.h = crate::config::snap_up(h.clamp(140, 540), crate::config::CARD_GRID_Y).min(540);
    if n.text != text {
        log(&format!(
            "hushtop: HUSH notepad #{id} text saved ({} chars)",
            text.len()
        ));
    }
    n.text = text;
    s.cfg.save();
    Ok(())
}

pub fn notepad_del(app: &AppHandle, id: u32) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    let before: usize = s.cfg.spaces.iter().map(|sp| sp.notepads.len()).sum();
    for sp in s.cfg.spaces.iter_mut() {
        sp.notepads.retain(|n| n.id != id);
    }
    let after: usize = s.cfg.spaces.iter().map(|sp| sp.notepads.len()).sum();
    if after == before {
        return Err(format!("no notepad widget #{id}"));
    }
    s.cfg.save();
    log(&format!("hushtop: HUSH notepad #{id} removed"));
    emit_cards(app, &s);
    Ok(())
}

/// Create a utility widget in `space`. Single-instance kinds live once per
/// space (each desktop is its own); the id space is shared across spaces.
pub fn widget_add(
    app: &AppHandle,
    kind: &str,
    col: i32,
    row: i32,
    space: usize,
) -> Result<u32, String> {
    // Defaults are WHOLE CELLS (36 x / 18 y cadence) — every widget is born
    // box-to-box on the ruled sheet.
    let (w, h) = match kind {
        "volume" => (252, 108),
        "clipboard" => (252, 252),
        "stats" => (216, 144),
        "lock" => (72, 72),
        "media" => (252, 252),
        "lyrics" => (252, 288),
        "spaces" => (216, 72),
        "brightness" => (252, 108),
        "effects" => (216, 72),
        other => return Err(format!("unknown widget kind '{other}'")),
    };
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    let space = space.min(1);
    if kind == "media" || kind == "brightness" || kind == "effects" || kind == "lyrics"
        || kind == "spaces"
    {
        if let Some(wd) = s.cfg.spaces[space]
            .widgets
            .iter()
            .find(|wd| wd.kind == kind)
        {
            let id = wd.id;
            log(&format!(
                "hushtop: {kind} widget add refused — one already exists in space {} (single instance)",
                space + 1
            ));
            return Ok(id);
        }
    }
    let id = next_card_id(&s);
    let (col, row) = (col.clamp(-1000, 1000), row.clamp(-1000, 1000));
    s.cfg.spaces[space].widgets.push(crate::config::HushWidget {
        id,
        kind: kind.to_string(),
        col,
        row,
        w,
        h,
    });
    s.cfg.save();
    log(&format!(
        "hushtop: HUSH created {kind} widget #{id} at cell ({col},{row}) of space {}",
        space + 1
    ));
    emit_cards(app, &s);
    Ok(id)
}

/// Persist a card's layout-map cell (and optional size). Cells are stored
/// verbatim (±1000 sanity bound only — the owner wants out-of-bounds
/// placements honored, the bound exists solely against garbage values).
/// The webview owns the grid, pixels are derived at render time, so nothing
/// here can silently rewrite an arrangement.
pub fn widget_save_pos(
    app: &AppHandle,
    id: u32,
    col: i32,
    row: i32,
    w: Option<i32>,
    h: Option<i32>,
) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if let Some(n) = s
        .cfg
        .spaces
        .iter_mut()
        .flat_map(|sp| sp.notepads.iter_mut())
        .find(|n| n.id == id)
    {
        n.col = col.clamp(-1000, 1000);
        n.row = row.clamp(-1000, 1000);
    } else if let Some(wd) = s
        .cfg
        .spaces
        .iter_mut()
        .flat_map(|sp| sp.widgets.iter_mut())
        .find(|w| w.id == id)
    {
        wd.col = col.clamp(-1000, 1000);
        wd.row = row.clamp(-1000, 1000);
        if let Some(cw) = w {
            if cw > 0 {
                // Stored sizes are whole grid boxes — snap whatever the
                // caller measured so the map never carries a fractional box.
                wd.w = crate::config::snap_card_w(cw);
            }
        }
        if let Some(ch) = h {
            if ch > 0 {
                wd.h = crate::config::snap_card_h(ch);
            }
        }
        // Fixed-canvas widgets keep their canonical size no matter what the
        // caller measured (the page enforces the same rule in renderWidgets)
        // — the spaces payload feeds the switch-wipe placeholder verbatim,
        // and an off-canvas card there hung past its grid slot.
        let (kw, kh) = crate::config::canonical_widget_dims(&wd.kind, wd.w, wd.h);
        wd.w = kw;
        wd.h = kh;
    } else {
        return Err(format!("no widget #{id}"));
    }
    s.cfg.save();
    Ok(())
}

pub fn widget_del_any(app: &AppHandle, id: u32) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    let before: usize = s
        .cfg
        .spaces
        .iter()
        .map(|sp| sp.notepads.len() + sp.widgets.len())
        .sum();
    for sp in s.cfg.spaces.iter_mut() {
        sp.notepads.retain(|n| n.id != id);
        sp.widgets.retain(|n| n.id != id);
    }
    let after: usize = s
        .cfg
        .spaces
        .iter()
        .map(|sp| sp.notepads.len() + sp.widgets.len())
        .sum();
    if after == before {
        return Err(format!("no widget #{id}"));
    }
    s.cfg.save();
    log(&format!("hushtop: HUSH widget #{id} removed"));
    emit_cards(app, &s);
    Ok(())
}

// ---------------------------------------------------------------------------
// Free-placing sorts (per space, explicit only)
// ---------------------------------------------------------------------------

/// Natural name compare: digit runs order numerically (File 2 < File 10).
fn nat_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut ai, mut bi) = (0usize, 0usize);
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    while ai < ab.len() && bi < bb.len() {
        let (ca, cb) = (ab[ai], bb[bi]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let a0 = ai;
            while ai < ab.len() && ab[ai].is_ascii_digit() {
                ai += 1;
            }
            let b0 = bi;
            while bi < bb.len() && bb[bi].is_ascii_digit() {
                bi += 1;
            }
            // strip leading zeros, then longer run wins, then byte compare
            let sa = a[a0..ai].trim_start_matches('0');
            let sb = b[b0..bi].trim_start_matches('0');
            if sa.len() != sb.len() {
                return sa.len().cmp(&sb.len());
            }
            let ord = sa.cmp(sb);
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        } else {
            if ca != cb {
                return ca.cmp(&cb);
            }
            ai += 1;
            bi += 1;
        }
    }
    (ab.len() - ai).cmp(&(bb.len() - bi))
}

/// Sort ONE space's icons (the space the menu was opened on; single-display
/// always sorts the active space). "name" | "date" | "type" — packed
/// column-major. Explicit only: nothing sorts on its own, ever.
pub fn sort_space(app: &AppHandle, space: usize, mode: String) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    let space = if s.hushtop_spaces.dual {
        space.min(1)
    } else {
        s.cfg.active_space.min(1)
    };
    let items = s.hushtop_spaces.items_of(space);
    if items.is_empty() {
        return Ok(());
    }
    let reg = region_for(&s.hushtop_spaces, space);

    struct Row {
        name: String,
        dir: bool,
        ext: String,
        mtime: u64,
    }
    let mut rows: Vec<Row> = Vec::with_capacity(items.len());
    for it in items {
        let path = item_path(&it.name).unwrap_or_else(|_| PathBuf::from(&it.name));
        let (dir, ext, mtime) = match std::fs::metadata(&path) {
            Ok(md) => (
                md.is_dir(),
                path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase(),
                md.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ),
            Err(_) => (it.kind == "folder", String::new(), 0),
        };
        rows.push(Row {
            name: it.name.clone(),
            dir,
            ext,
            mtime,
        });
    }

    match mode.as_str() {
        "date" => rows.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| nat_cmp(&a.name, &b.name))),
        "type" => rows.sort_by(|a, b| {
            b.dir
                .cmp(&a.dir)
                .then_with(|| a.ext.cmp(&b.ext))
                .then_with(|| nat_cmp(&a.name, &b.name))
        }),
        _ => rows.sort_by(|a, b| nat_cmp(&a.name, &b.name)),
    }

    let map = mode.as_str();
    log(&format!(
        "hushtop: sorting space {} by {map} ({} items)",
        space + 1,
        rows.len()
    ));

    // Pack column-major over the space's region grid.
    let sp = &mut s.cfg.spaces[space];
    sp.icon_cells.retain(|k, _| rows.iter().any(|r| &r.name == k));
    let (cols, rws) = (reg.cols(), reg.rows());
    let mut k = 0usize;
    'outer: for c in 0..cols {
        for r in 0..rws {
            if k >= rows.len() {
                break 'outer;
            }
            sp.icon_cells.insert(rows[k].name.clone(), (c, r));
            k += 1;
        }
    }
    s.cfg.save();
    unsafe {
        rescan(app, &mut s);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Second-desktop: space switch + effects
// ---------------------------------------------------------------------------

/// The spaces widget switched desktops. Refused on two displays — each
/// monitor already hosts its own space.
pub fn space_set(app: &AppHandle, idx: usize) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if s.hushtop_spaces.dual {
        log("hushtop: space switch refused — two displays are connected (each hosts its own space)");
        return Err("two displays are connected — each one hosts its own space".into());
    }
    let idx = idx.min(1);
    if s.hushtop_spaces.active != idx || s.cfg.active_space != idx {
        s.cfg.active_space = idx;
        // The rendered view mirrors hushtop_spaces.active — the config field
        // alone only matters at the next boot, so updating it without this
        // made the emitted payload carry the OLD active space: the webview
        // saw "no change" and the widget click looked dead.
        s.hushtop_spaces.active = idx;
        s.cfg.save();
        log(&format!("hushtop: active space -> {}", idx + 1));
    }
    let _ = app.emit("hushtop:spaces", spaces_dto(&s));
    Ok(())
}

/// The effects widget set the desktop particle layer.
pub fn effect_set(app: &AppHandle, kind: String) -> Result<(), String> {
    match kind.as_str() {
        "none" | "snow" | "leaves" | "star" => {}
        // Retired desktop picks: rain is gone (motion sickness), and the
        // wind farm is EXCLUSIVE to the lock screen now — the widget offers
        // an OFF button in their place; stale stored values land here too.
        "rain" => return Err("rain is retired — pick snow, leaves or off".into()),
        "wind" => return Err("wind lives on the lock screen — pick snow, leaves or off".into()),
        other => return Err(format!("unknown effect '{other}'")),
    }
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if s.cfg.effect != kind {
        s.cfg.effect = kind.clone();
        s.cfg.save();
        log(&format!("hushtop: effect -> {kind}"));
    }
    let _ = app.emit_to("hushtop", "hushtop:effect", s.cfg.effect.clone());
    Ok(())
}

// ---------------------------------------------------------------------------
// Icon bookkeeping helpers for the native drag paths
// ---------------------------------------------------------------------------

/// Which space owns an icon's cell (first hit), and the cell itself.
pub fn cell_of_name(s: &Shared, name: &str) -> Option<(usize, (i32, i32))> {
    for (i, sp) in s.cfg.spaces.iter().enumerate() {
        if let Some(c) = sp.icon_cells.get(name) {
            return Some((i, *c));
        }
    }
    None
}

/// The RENDERED layout as an ownership fallback: the space whose item list
/// shows the name, and the cell its top-left is painted on. Used when the
/// saved map has no entry for a name the user can plainly SEE — an explicit
/// move must not fail just because the map lagged behind the render.
pub fn rendered_cell_of(s: &Shared, name: &str) -> Option<(usize, (i32, i32))> {
    for (i, list) in s.hushtop_spaces.items.iter().enumerate() {
        if let Some(it) = list.iter().find(|it| it.name == name) {
            let reg = region_for(&s.hushtop_spaces, i);
            return Some((i, reg.cell_of(it.x, it.y)));
        }
    }
    None
}

/// Remove an icon's cell from EVERY space (dnd MOVE-out cleanup).
pub fn forget_name(s: &mut Shared, name: &str) {
    for sp in s.cfg.spaces.iter_mut() {
        sp.icon_cells.remove(name);
    }
}

/// A space cell -> viewport CSS px (dnd rehome path).
pub fn cell_px_viewport(s: &Shared, space: usize, c: i32, r: i32) -> (i32, i32) {
    region_for(&s.hushtop_spaces, space).px_of(c, r)
}

// ---------------------------------------------------------------------------
// Widget drag-out ghost (start menu third tab -> hushtop)
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, Clone)]
struct GhostDto {
    kind: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    on: bool,
}

fn ghost_size(kind: &str) -> (i32, i32) {
    match kind {
        "notes" => (252, 180),
        "volume" => (252, 108),
        "clipboard" => (252, 252),
        "stats" => (216, 144),
        "lock" => (72, 72),
        "media" => (252, 252),
        "lyrics" => (252, 288),
        "spaces" => (216, 72),
        "brightness" => (252, 108),
        "effects" => (216, 72),
        _ => (216, 144),
    }
}

// Lock-free hwnd mirrors for the widget-drag hot path. `widget_drag_move`
// is a SYNC command — it runs ON THE MAIN THREAD up to 25 times a second
// while the user drags, and taking the shared state Mutex here pitted it
// against the pump thread's beat batch (which holds that lock across every
// beat, window enumeration included). Two atomics keep the stream off the
// lock entirely; the mirrors are written at the only three sites that set
// the real fields (boot setup + the rebuilt-hushtop path).
pub mod drag_hwnds {
    use std::sync::atomic::{AtomicIsize, Ordering};
    pub static HUSHTOP: AtomicIsize = AtomicIsize::new(0);
    pub static STARTMENU: AtomicIsize = AtomicIsize::new(0);
    pub fn set(hushtop: isize, startmenu: isize) {
        HUSHTOP.store(hushtop, Ordering::Relaxed);
        STARTMENU.store(startmenu, Ordering::Relaxed);
    }
}

/// startmenu client px -> viewport CSS px on the hushtop paper (LOCK-FREE:
/// reads the drag_hwnds mirrors — this is the 25/s hot path).
fn sm_client_to_viewport_free(cx: f64, cy: f64) -> Option<(i32, i32)> {
    let hh = drag_hwnds::HUSHTOP.load(std::sync::atomic::Ordering::Relaxed);
    let sm = drag_hwnds::STARTMENU.load(std::sync::atomic::Ordering::Relaxed);
    if hh == 0 || sm == 0 {
        return None;
    }
    let hh = hwnd(hh);
    let sm = hwnd(sm);
    let mut pt = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    unsafe {
        if windows::Win32::Graphics::Gdi::ClientToScreen(sm, &mut pt).as_bool() {
            let dpr_sm = (dpi_for(sm) as f64 / 96.0).max(1.0);
            let sx = pt.x + (cx * dpr_sm) as i32;
            let sy = pt.y + (cy * dpr_sm) as i32;
            let mut p = windows::Win32::Foundation::POINT { x: sx, y: sy };
            if windows::Win32::Graphics::Gdi::ScreenToClient(hh, &mut p).as_bool() {
                let dpr = (dpi_for(hh) as f64 / 96.0).max(1.0);
                return Some((
                    (p.x as f64 / dpr).round() as i32,
                    (p.y as f64 / dpr).round() as i32,
                ));
            }
        }
    }
    None
}

/// startmenu client px -> viewport CSS px on the hushtop paper.
fn sm_client_to_viewport(s: &Shared, cx: f64, cy: f64) -> Option<(i32, i32)> {
    if s.hwnd_hushtop == 0 || s.hwnd_startmenu == 0 {
        return None;
    }
    let hh = hwnd(s.hwnd_hushtop);
    let sm = hwnd(s.hwnd_startmenu);
    let mut pt = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    unsafe {
        if windows::Win32::Graphics::Gdi::ClientToScreen(sm, &mut pt).as_bool() {
            let dpr_sm = (dpi_for(sm) as f64 / 96.0).max(1.0);
            let sx = pt.x + (cx * dpr_sm) as i32;
            let sy = pt.y + (cy * dpr_sm) as i32;
            let mut p = windows::Win32::Foundation::POINT { x: sx, y: sy };
            if windows::Win32::Graphics::Gdi::ScreenToClient(hh, &mut p).as_bool() {
                let dpr = (dpi_for(hh) as f64 / 96.0).max(1.0);
                return Some((
                    (p.x as f64 / dpr).round() as i32,
                    (p.y as f64 / dpr).round() as i32,
                ));
            }
        }
    }
    None
}

/// Pointer moved while dragging a widget out of the start menu. Deliberately
/// lock-free (see drag_hwnds): a SYNC command on the main thread must never
/// queue up behind the pump's beat batch 25 times a second.
pub fn widget_drag_move(app: &AppHandle, kind: String, cx: f64, cy: f64) {
    static DRAG_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some((x, y)) = sm_client_to_viewport_free(cx, cy) else {
        // Mirrors not set (early boot or a rebuilt hushtop) — say so once
        // per gesture instead of silently eating the stream.
        if !DRAG_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log(&format!(
                "widget drag: move ignored — hushtop/startmenu hwnd mirrors not set (kind={kind})"
            ));
        }
        return;
    };
    DRAG_LOGGED.store(false, std::sync::atomic::Ordering::Relaxed);
    // The ghost rides 10px right / 10px down of the cursor; the drop cell is
    // the cell under that anchor.
    let (x, y) = (x + 10, y + 10);
    let (gw, gh) = ghost_size(&kind);
    let _ = app.emit_to(
        "hushtop",
        "hushtop:widget_ghost",
        GhostDto { kind, x, y, w: gw, h: gh, on: true },
    );
}

/// Drag released: land the widget in the space the cursor is on.
pub fn widget_drag_drop(
    app: &AppHandle,
    kind: String,
    cx: f64,
    cy: f64,
) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let (space, col, row) = {
        let mut s = crate::util::lock_shared(&st.0);
        let Some((x0, y0)) = sm_client_to_viewport(&s, cx, cy) else {
            log("widget drag: drop failed — hushtop is not available");
            return Err("hushtop is not available".into());
        };
        let (x, y) = (x0 + 10, y0 + 10);
        // Off the paper (over the bar, outside the virtual screen): cancel.
        let (_, _, vw, vh) = virtual_screen_rect();
        let hh = hwnd(s.hwnd_hushtop);
        let dpr = (dpi_for(hh) as f64 / 96.0).max(1.0);
        let (vw_c, vh_c) = ((vw as f64 / dpr) as i32, (vh as f64 / dpr) as i32);
        if x < 0 || y < 0 || x >= vw_c || y >= vh_c {
            log(&format!(
                "widget drag: drop outside the desktop ({x},{y} of {vw_c}x{vh_c}) — cancelled"
            ));
            return Err("outside the desktop".into());
        }
        let space = space_at(&s.hushtop_spaces, x, y);
        let reg = region_for(&s.hushtop_spaces, space);
        // CARD cadence — the cell the webview's placeByCard will render.
        // (cell_of is the 92x72 icon grid; using it here decided every
        // widget's home on its own instead of where the user dropped it.)
        let (c, r) = reg.cell_of_card(x, y);
        log(&format!(
            "widget drag: drop (kind={kind}) at viewport ({x},{y}) -> space {} cell ({c},{r})",
            space + 1
        ));
        (space, c, r)
    };
    let landed = if kind == "notes" {
        notepad_add(app, col, row, space)
    } else {
        widget_add(app, &kind, col, row, space)
    };
    if let Err(e) = &landed {
        log(&format!("widget drag: drop FAILED — {e}"));
    }
    landed?;
    let _ = app.emit_to(
        "hushtop",
        "hushtop:widget_ghost",
        GhostDto {
            kind,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            on: false,
        },
    );
    crate::startmenu::hide(app);
    Ok(())
}

/// Drag aborted (Escape / pointercancel / released off the desktop).
pub fn widget_drag_cancel(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    log("widget drag: cancelled (ghost dismissed)");
    let _ = app.emit_to(
        "hushtop",
        "hushtop:widget_ghost",
        GhostDto {
            kind: String::new(),
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            on: false,
        },
    );
}
