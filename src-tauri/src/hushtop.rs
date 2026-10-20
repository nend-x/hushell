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
    GetParent, GetWindowLongPtrW, IsIconic, IsWindow, PostMessageW, SendMessageTimeoutW,
    SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, GA_ROOT, GWL_STYLE, HWND_BOTTOM,
    HWND_TOP, HWND_TOPMOST, SMTO_NORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE,
    SW_SHOW, SW_SHOWNOACTIVATE, WM_COMMAND, WS_CAPTION, WS_CHILD, WS_MAXIMIZEBOX,
    WS_MINIMIZEBOX, WS_POPUP, WS_THICKFRAME, WS_VISIBLE, HICON,
};

use crate::icons;
use crate::shared::Shared;
use crate::util::{log, to_wide, monitor_rect_at_cursor, virtual_screen_rect, dpi_for};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const WM_SPAWN_WORKERW: u32 = 0x052C;
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

fn com_init() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if !DONE.swap(true, Ordering::Relaxed) {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if hr.is_err() {
                log("hushtop: CoInitializeEx failed (continuing)");
            }
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

fn collect_dir(dir: &Path, out: &mut Vec<(String, bool, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
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
// Items -> DTO (with icon cache + occupancy-resolved layout)
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

fn resolve_layout(
    names: &[String],
    saved: &std::collections::HashMap<String, (i32, i32)>,
    rows: usize,
    cols: usize,
) -> Vec<(i32, i32)> {
    let (rows, cols) = (rows.max(1) as i32, cols.max(1) as i32);
    let mut taken: HashSet<(i32, i32)> = HashSet::new();
    let mut result = vec![(0i32, 0i32); names.len()];
    let mut rehome: Vec<(usize, i32, i32)> = Vec::new(); // (idx, col, row)
    let mut auto_cursor: i32 = 0;

    let cell_of = |x: i32, y: i32| -> (i32, i32) {
        let c = ((x - GRID_MARGIN) as f32 / CELL_W as f32).round() as i32;
        let r = ((y - GRID_MARGIN) as f32 / CELL_H as f32).round() as i32;
        (c.clamp(0, cols - 1), r.clamp(0, rows - 1))
    };
    let px_of = |c: i32, r: i32| (GRID_MARGIN + c * CELL_W, GRID_MARGIN + r * CELL_H);

    // Pass 1+3 intake: saved positions.
    for (idx, name) in names.iter().enumerate() {
        match saved.get(name) {
            Some(&(x, y)) => {
                let (c, r) = cell_of(x, y);
                if taken.insert((c, r)) {
                    result[idx] = px_of(c, r);
                } else {
                    rehome.push((idx, c, r));
                }
            }
            None => {} // pass 2
        }
    }

    // Pass 2: first free cell in column-major order.
    let total = rows * cols;
    for (idx, name) in names.iter().enumerate() {
        if saved.contains_key(name) {
            continue;
        }
        let mut cell = None;
        while (auto_cursor as i64) < total as i64 {
            let c = auto_cursor / rows;
            let r = auto_cursor % rows;
            auto_cursor += 1;
            if !taken.contains(&(c, r)) {
                cell = Some((c, r));
                break;
            }
        }
        match cell {
            Some((c, r)) => {
                taken.insert((c, r));
                result[idx] = px_of(c, r);
            }
            None => rehome.push((idx, cols - 1, rows - 1)), // grid overflow: pile at the end
        }
    }

    // Pass 3: rehome the colliding saved positions to the nearest free cell.
    for (idx, c, r) in rehome {
        let cell = nearest_free_cell(&taken, rows, cols, c, r);
        taken.insert(cell);
        result[idx] = px_of(cell.0, cell.1);
    }
    result
}

unsafe fn build_items(s: &mut Shared) -> Vec<crate::shared::HushtopItem> {
    use crate::shared::HushtopItem;
    let Some((user, public)) = desk_paths() else {
        return Vec::new();
    };
    let hh = hwnd(s.hwnd_hushtop);
    let dpr = (dpi_for(hh) as f64 / 96.0).max(1.0);
    // The paper spans the whole virtual screen (every monitor) — the grid
    // math must count the same cells the webview draws.
    let (_, _, mw, mh) = virtual_screen_rect();
    let css_h = ((mh as f64) / dpr) as i32;
    let css_w = ((mw as f64) / dpr) as i32;
    let rows = (((css_h - GRID_MARGIN * 2) / CELL_H).max(1)) as usize;
    let cols = (((css_w - GRID_MARGIN * 2) / CELL_W).max(1)) as usize;

    let mut out: Vec<HushtopItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut live: HashSet<String> = HashSet::new();
    let mut names: Vec<String> = Vec::new();
    let mut kinds: HashMap<String, bool> = HashMap::new();
    let mut icons: HashMap<String, Option<String>> = HashMap::new();

    for dir in [&user, &public] {
        let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
        let mut sigs: Vec<(String, bool, u64)> = Vec::new();
        collect_dir(dir, &mut sigs);
        for (name, is_dir, _) in sigs {
            let p = dir.join(&name);
            entries.push((name, p, is_dir));
        }
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
    let positions =
        resolve_layout(&names, &s.cfg.hushtop_positions, rows, cols);
    for (idx, name) in names.iter().enumerate() {
        let (x, y) = positions[idx];
        out.push(HushtopItem {
            name: name.clone(),
            kind: if kinds[name] { "folder" } else { "file" }.to_string(),
            icon: icons[name].clone(),
            x,
            y,
        });
    }
    s.hushtop_icon_cache.retain(|k, _| live.contains(k));
    out
}

pub unsafe fn rescan(app: &AppHandle, s: &mut Shared) {
    if s.hwnd_hushtop == 0 {
        return;
    }
    s.hushtop_scan_sig = scan_signature();
    s.hushtop_rescan = false;
    let items = build_items(s);
    if items != s.hushtop_items {
        s.hushtop_items = items.clone();
        let _ = app.emit("hushtop:icons", &items);
    }
}

// ---------------------------------------------------------------------------
// WorkerW dance
// ---------------------------------------------------------------------------

unsafe fn find_wallpaper_host(spawn: bool) -> Option<HWND> {
    let progman = FindWindowW(w!("Progman"), PCWSTR::null()).ok()?;
    if spawn {
        for wp in [0xDusize, 0usize] {
            let mut r: usize = 0;
            let _ = SendMessageTimeoutW(
                progman,
                WM_SPAWN_WORKERW,
                WPARAM(wp),
                LPARAM(0),
                SMTO_NORMAL,
                1000,
                Some(&mut r),
            );
        }
    }
    let mut found: isize = 0;
    let _ = EnumWindows(
        Some(defview_host_cb),
        LPARAM(&mut found as *mut isize as isize),
    );
    if found != 0 {
        return Some(hwnd(found));
    }
    Some(progman)
}

/// EnumWindows body: on the top-level window hosting SHELLDLL_DefView, grab
/// the next top-level WorkerW (the wallpaper host) and stop the walk.
unsafe extern "system" fn defview_host_cb(top: HWND, lparam: LPARAM) -> BOOL {
    let out = lparam.0 as *mut isize;
    if let Ok(dv) = FindWindowExW(top, HWND::default(), w!("SHELLDLL_DefView"), PCWSTR::null()) {
        if !dv.is_invalid() {
            if let Ok(w) = FindWindowExW(HWND::default(), top, w!("WorkerW"), PCWSTR::null()) {
                if !w.is_invalid() {
                    *out = w.0 as isize;
                    return BOOL(0);
                }
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
    // The custom desktop spans EVERY monitor: WorkerW (the wallpaper host)
    // covers the virtual screen, so the paper must too — icons and widgets
    // can live on any display. One webview, one map, all monitors.
    let (mx, my, mw, mh) = virtual_screen_rect();

    // Size + place while still hidden (opaque window — no dance needed).
    let _ = SetWindowPos(hh, HWND_BOTTOM, mx, my, mw, mh, SWP_NOACTIVATE);

    let mut host = find_wallpaper_host(true);
    if host.is_none() {
        std::thread::sleep(std::time::Duration::from_millis(350));
        host = find_wallpaper_host(false);
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
            log("hushtop: candidate did not take (root is not a wallpaper host) — trying the next");
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
        log("hushtop: parented into the wallpaper host (verified by root class)");
        break;
    }
    if parent == 0 {
        log("hushtop: no host took us — top-level fallback (the beat re-dances)");
        let _ = SetWindowPos(hh, HWND_BOTTOM, mx, my, mw, mh, SWP_NOACTIVATE);
    }
    let _ = ShowWindow(hh, SW_SHOW);
    parent
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
                        s2.hwnd_hushtop = h;
                        crate::dnd::install(&a2);
                        log("hushtop: window rebuilt — re-applying next beat");
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
            log("hushtop: window is NOT inside a wallpaper host — re-dancing (main thread)");
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
        // a wallpaper host chain is 2 deep; the cap only bounds a pathological
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
            s.hushtop_scan_sig = sig;
            s.hushtop_rescan = false;
            let items = build_items(s);
            if items != s.hushtop_items {
                s.hushtop_items = items.clone();
                let _ = app.emit("hushtop:icons", &items);
            }
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
    if !s.hushtop_items.iter().any(|i| i.name == name) {
        return Err(format!("\"{name}\" is not on hushtop"));
    }
    let hh = hwnd(s.hwnd_hushtop);
    let dpr = (dpi_for(hh) as f64 / 96.0).max(1.0);
    let (_, _, mw, mh) = virtual_screen_rect();
    let css_w = ((mw as f64) / dpr) as i32;
    let css_h = ((mh as f64) / dpr) as i32;
    let cols = (((css_w - GRID_MARGIN * 2) / CELL_W).max(1)) as i32;
    let rows = (((css_h - GRID_MARGIN * 2) / CELL_H).max(1)) as i32;
    // Snap the drop point to the grid, clamped inside it.
    let c = (((x - GRID_MARGIN) as f32 / CELL_W as f32).round() as i32).clamp(0, cols - 1);
    let r = (((y - GRID_MARGIN) as f32 / CELL_H as f32).round() as i32).clamp(0, rows - 1);
    // Occupancy of every OTHER icon (their cells are already resolved).
    let mut taken: HashSet<(i32, i32)> = HashSet::new();
    for it in s.hushtop_items.iter().filter(|i| i.name != name) {
        let ic = (((it.x - GRID_MARGIN) as f32 / CELL_W as f32).round() as i32).clamp(0, cols - 1);
        let ir = (((it.y - GRID_MARGIN) as f32 / CELL_H as f32).round() as i32).clamp(0, rows - 1);
        taken.insert((ic, ir));
    }
    let (fc, fr) = nearest_free_cell(&taken, rows, cols, c, r);
    let (cx, cy) = (GRID_MARGIN + fc * CELL_W, GRID_MARGIN + fr * CELL_H);
    s.cfg.hushtop_positions.insert(name.clone(), (cx, cy));
    s.cfg.save();
    if let Some(it) = s.hushtop_items.iter_mut().find(|i| i.name == name) {
        it.x = cx;
        it.y = cy;
    }
    if (fc, fr) != (c, r) {
        log(&format!("hushtop: \"{name}\" drop collided — moved to ({cx},{cy})"));
        let items = s.hushtop_items.clone();
        let _ = app.emit("hushtop:icons", &items);
    } else {
        log(&format!("hushtop: \"{name}\" moved to ({cx},{cy})"));
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
    let path = item_path(&name)?;
    let mut from = to_wide(&path.as_os_str().to_string_lossy());
    from.push(0); // double-NUL terminated list
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
        log(&format!("hushtop: delete \"{name}\" failed (r={r})"));
        return Err(format!("Could not delete \"{name}\""));
    }
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    s.cfg.hushtop_positions.remove(&name);
    s.cfg.save();
    rescan(app, &mut s);
    log(&format!("hushtop: deleted \"{name}\" (recycle bin)"));
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
    s.cfg.hushtop_positions.remove(&old);
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
    log(&format!("hushtop: created {kind} \"{final_name}\" (both desktops)"));
    Ok(())
}

/// Create a widget and emit the full list to the hushtop webview. Returns
/// the widget id for the tool result message.
pub fn notepad_add(app: &AppHandle, col: i32, row: i32) -> Result<u32, String> {
    let st = app.state::<crate::AppShared>();
    let (id, list) = {
        let mut s = crate::util::lock_shared(&st.0);
        let id = s
            .cfg
            .notepads
            .iter()
            .map(|n| n.id)
            .chain(s.cfg.widgets.iter().map(|w| w.id))
            .max()
            .unwrap_or(0)
            + 1;
        let (col, row) = (col.clamp(-1000, 1000), row.clamp(-1000, 1000));
        s.cfg.notepads.push(crate::config::Notepad {
            id,
            col,
            row,
            w: 220,
            h: 170,
            text: String::new(),
            x: None,
            y: None,
        });
        s.cfg.save();
        log(&format!(
            "hushtop: HUSH created notepad widget #{id} at cell ({col},{row})"
        ));
        (id, s.cfg.notepads.clone())
    };
    let _ = app.emit_to("hushtop", "hushtop:notepads", list);
    Ok(id)
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
        .notepads
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| format!("no notepad widget #{id}"))?;
    n.col = col.clamp(-1000, 1000);
    n.row = row.clamp(-1000, 1000);
    n.w = w.clamp(220, 720);
    n.h = h.clamp(140, 540);
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
    let list = {
        let mut s = crate::util::lock_shared(&st.0);
        let before = s.cfg.notepads.len();
        s.cfg.notepads.retain(|n| n.id != id);
        if s.cfg.notepads.len() == before {
            return Err(format!("no notepad widget #{id}"));
        }
        s.cfg.save();
        log(&format!("hushtop: HUSH notepad #{id} removed"));
        s.cfg.notepads.clone()
    };
    let wl = {
        let st2 = app.state::<crate::AppShared>();
        let g = crate::util::lock_shared(&st2.0);
        g.cfg.widgets.clone()
    };
    let _ = app.emit_to("hushtop", "hushtop:widgets", wl);
    let _ = app.emit_to("hushtop", "hushtop:notepads", list);
    Ok(())
}

/// Create a utility widget. `kind`: "volume" | "clipboard". Emits BOTH
/// lists (the id space is shared, so both webview collections re-sync).
/// Position is a layout-map cell (col, row) — no pixel math anywhere.
pub fn widget_add(app: &AppHandle, kind: &str, col: i32, row: i32) -> Result<u32, String> {
    let (w, h) = match kind {
        "volume" => (236, 108),
        "clipboard" => (240, 236),
        "stats" => (208, 142),
        "lock" => (64, 64),
        "media" => (280, 156),
        other => return Err(format!("unknown widget kind '{other}'")),
    };
    let st = app.state::<crate::AppShared>();
    if kind == "media" {
        let existing = {
            let s = crate::util::lock_shared(&st.0);
            s.cfg.widgets.iter().find(|wd| wd.kind == "media").map(|wd| wd.id)
        };
        if let Some(id) = existing {
            log("hushtop: media widget add refused — one already exists (single instance)");
            return Ok(id);
        }
    }
    let (id, widgets) = {
        let mut s = crate::util::lock_shared(&st.0);
        let id = s
            .cfg
            .notepads
            .iter()
            .map(|n| n.id)
            .chain(s.cfg.widgets.iter().map(|w| w.id))
            .max()
            .unwrap_or(0)
            + 1;
        let (col, row) = (col.clamp(-1000, 1000), row.clamp(-1000, 1000));
        s.cfg.widgets.push(crate::config::HushWidget {
            id,
            kind: kind.to_string(),
            col,
            row,
            w,
            h,
            x: None,
            y: None,
        });
        s.cfg.save();
        log(&format!(
            "hushtop: HUSH created {kind} widget #{id} at cell ({col},{row})"
        ));
        (id, s.cfg.widgets.clone())
    };
    crate::media::set_wants_levels(app);
    let _ = app.emit_to("hushtop", "hushtop:widgets", widgets);
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
    if let Some(n) = s.cfg.notepads.iter_mut().find(|n| n.id == id) {
        n.col = col.clamp(-1000, 1000);
        n.row = row.clamp(-1000, 1000);
    } else if let Some(wd) = s.cfg.widgets.iter_mut().find(|w| w.id == id) {
        wd.col = col.clamp(-1000, 1000);
        wd.row = row.clamp(-1000, 1000);
        if let Some(cw) = w {
            if cw > 0 {
                wd.w = cw;
            }
        }
        if let Some(ch) = h {
            if ch > 0 {
                wd.h = ch;
            }
        }
    } else {
        return Err(format!("no widget #{id}"));
    }
    s.cfg.save();
    Ok(())
}

pub fn widget_del_any(app: &AppHandle, id: u32) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let (notes, widgets) = {
        let mut s = crate::util::lock_shared(&st.0);
        let before = s.cfg.notepads.len() + s.cfg.widgets.len();
        s.cfg.notepads.retain(|n| n.id != id);
        s.cfg.widgets.retain(|n| n.id != id);
        if s.cfg.notepads.len() + s.cfg.widgets.len() == before {
            return Err(format!("no widget #{id}"));
        }
        s.cfg.save();
        log(&format!("hushtop: HUSH widget #{id} removed"));
        (s.cfg.notepads.clone(), s.cfg.widgets.clone())
    };
    crate::media::set_wants_levels(app);
    let _ = app.emit_to("hushtop", "hushtop:notepads", notes);
    let _ = app.emit_to("hushtop", "hushtop:widgets", widgets);
    Ok(())
}
