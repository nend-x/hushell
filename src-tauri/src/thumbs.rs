use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY,
    DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowRect, IsIconic, IsWindow, IsWindowVisible,
    SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_HIDE, SW_SHOW,
};

use crate::shared::{PreviewShow, PreviewSlot, Shared, ThumbCss};
use crate::tasks::window_title;
use crate::util::{screen_height, screen_width};

const HIDE_GRACE_MS: u64 = 320;

const CARD_W_CSS: f64 = 600.0;
const CARD_H_CSS: f64 = 300.0;
const CARD_SOLO_W_CSS: f64 = 360.0;
const CARD_SOLO_H_CSS: f64 = 240.0;
const CARD_PAIR_W_CSS: f64 = 660.0;
const CARD_PAIR_H_CSS: f64 = 230.0;
const PAD_CSS: f64 = 20.0;
const GAP_CSS: f64 = 12.0;
const SLOT_LABEL_H_CSS: f64 = 16.0;

/// Card size in CSS px for a group of `n` ALIVE windows.
fn card_size_css(n: usize) -> (f64, f64) {
    if n <= 1 {
        (CARD_SOLO_W_CSS, CARD_SOLO_H_CSS)
    } else if n == 2 {
        (CARD_PAIR_W_CSS, CARD_PAIR_H_CSS)
    } else {
        (CARD_W_CSS, CARD_H_CSS)
    }
}

/// Slot areas (x, y, w, h) in CSS px for `cells` slots, inside a card of
/// `card_w` x `card_h`. cells = the RENDERED cell count: n<=3 renders one
/// cell per window; n>=4 renders TWO cells (preview + overflow badge).
fn slot_rects(cells: usize, card_w: f64, card_h: f64) -> Vec<(f64, f64, f64, f64)> {
    let cols = if cells <= 1 { 1 } else { 2 };
    let rows = if cells <= 2 { 1 } else { 2 };
    let grid_w = card_w - PAD_CSS * 2.0;
    let grid_h = card_h - PAD_CSS * 2.0;
    let cw = (grid_w - GAP_CSS * (cols as f64 - 1.0)) / cols as f64;
    let ch = (grid_h - GAP_CSS * (rows as f64 - 1.0)) / rows as f64;
    let mut v = Vec::new();
    for i in 0..cells {
        let c = (i % cols) as f64;
        let r = (i / cols) as f64;
        v.push((
            PAD_CSS + c * (cw + GAP_CSS),
            PAD_CSS + r * (ch + GAP_CSS),
            cw,
            ch,
        ));
    }
    v
}

fn hwnd(v: isize) -> HWND {
    HWND(v as _)
}

fn bar_dpi(s: &Shared) -> f64 {
    if s.hwnd_bar != 0 {
        unsafe { GetDpiForWindow(hwnd(s.hwnd_bar)) as f64 / 96.0 }
    } else {
        1.0
    }
}

/// DPI of the CALLER bar (the bar webview that sent the command). Every
/// bar carries its own DPI, so the same CSS anchor scales correctly on any
/// display.
fn bar_dpi_h(bar_h: isize) -> f64 {
    if bar_h != 0 && unsafe { IsWindow(hwnd(bar_h)) }.as_bool() {
        unsafe { GetDpiForWindow(hwnd(bar_h)) as f64 / 96.0 }
    } else {
        1.0
    }
}

/// The caller bar's top edge (physical) — flyouts and cards rise from it.
fn bar_top_h(bar_h: isize) -> i32 {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd(bar_h), &mut rc);
    }
    rc.top
}

/// Monitor of the caller bar (primary fallback when the hwnd is dead).
fn monitor_of_bar(bar_h: isize) -> (i32, i32, i32, i32) {
    if bar_h != 0 && unsafe { IsWindow(hwnd(bar_h)) }.as_bool() {
        crate::util::monitor_rect(hwnd(bar_h))
    } else {
        (0, 0, screen_width(), screen_height())
    }
}

/// Anchor CSS (relative to the CALLER bar's left edge) → screen-space
/// physical x, translated with THAT bar's origin + DPI. On the primary
/// bar this is the old behavior; on a secondary bar the popup lands
/// above THAT strip instead of teleporting to the primary.
fn anchor_to_physical_h(bar_h: isize, anchor_css: f64) -> i32 {
    let mut rc = RECT::default();
    let bx = if bar_h != 0 && unsafe { GetWindowRect(hwnd(bar_h), &mut rc) }.is_ok() {
        rc.left
    } else {
        0
    };
    bx + (anchor_css * bar_dpi_h(bar_h)).round() as i32
}

/// Clamp a centered card inside the monitor `mon` — a secondary bar's
/// popup must stay on its own display, not drift across the gap.
fn clamp_x_in(anchor: i32, card_w: i32, mon: (i32, i32, i32, i32)) -> i32 {
    let (mx, _, mw, _) = mon;
    (anchor - card_w / 2).clamp(mx + 8, (mx + mw - card_w - 8).max(mx + 8))
}

/// Flip just the DWM thumbnail visibility (cheap; no handle churn).
fn set_thumb_visible(t: isize, vis: bool) {
    unsafe {
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_VISIBLE,
            fVisible: vis.into(),
            ..Default::default()
        };
        let _ = DwmUpdateThumbnailProperties(t, &props);
    }
}

/// Fully release every DWM thumbnail (group switch, death, reset).
fn kill_thumbs(s: &mut Shared) {
    for (_, t) in s.thumbs.drain(..) {
        if t != 0 {
            unsafe {
                let _ = DwmUnregisterThumbnail(t);
            }
        }
    }
}

unsafe fn place(h: HWND, x: i32, y: i32, w: i32, hgt: i32, already_visible: bool) {
    let mut rc = RECT::default();
    let same = GetWindowRect(h, &mut rc).is_ok()
        && rc.left == x
        && rc.top == y
        && (rc.right - rc.left) == w
        && (rc.bottom - rc.top) == hgt;
    if same && already_visible {
        return;
    }
    if already_visible {
        let _ = ShowWindow(h, SW_HIDE);
        let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, hgt, SWP_NOACTIVATE);
        let _ = ShowWindow(h, SW_SHOW);
    } else {
        let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, hgt, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

pub fn hide(app: &AppHandle, s: &mut Shared) {
    let was_visible = s.preview_visible;
    // Keep the thumbnails registered, just invisible, so re-hovering the
    // SAME group is a pure property update (no register/unregister churn).
    for (_, t) in &s.thumbs {
        set_thumb_visible(*t, false);
    }
    s.preview_visible = false;
    s.preview_pending_hide = None;
    if was_visible && s.hwnd_preview != 0 {
        unsafe {
            let _ = ShowWindow(hwnd(s.hwnd_preview), SW_HIDE);
        }
    }
    let _ = app.emit_to("preview", "preview:hide", ());
}

pub fn show(app: &AppHandle, bar_h: isize, ids: Vec<isize>, anchor_css: f64) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    unsafe {
        if s.hwnd_preview == 0 {
            return Ok(());
        }
        // The context menu or the wingroup sheet owns the moment — a hover
        // show armed just before they opened must never pop the card over
        // them (the classic "menu interrupted by something"). The next
        // genuine hover re-fires this command anyway.
        if s.menu_visible {
            return Ok(());
        }
        if s.hwnd_wingroup != 0
            && IsWindow(hwnd(s.hwnd_wingroup)).as_bool()
            && IsWindowVisible(hwnd(s.hwnd_wingroup)).as_bool()
        {
            return Ok(());
        }
        let targets: Vec<isize> = ids
            .into_iter()
            .filter(|id| *id != 0 && IsWindow(hwnd(*id)).as_bool())
            .collect();
        if targets.is_empty() {
            hide(app, &mut s);
            return Ok(());
        }
        // A fresh show always cancels a pending hide (the cursor came back).
        s.preview_pending_hide = None;

        let n = targets.len();
        let cells = if n >= 5 { 4 } else { n };
        let shown = if n >= 5 { 3 } else { n };
        let (card_w_css, card_h_css) = card_size_css(n);

        let prev = hwnd(s.hwnd_preview);
        let dpi = bar_dpi_h(bar_h);
        let anchor = anchor_to_physical_h(bar_h, anchor_css);
        let card_w = (card_w_css * dpi).round() as i32;
        let card_h = (card_h_css * dpi).round() as i32;
        let mon = monitor_of_bar(bar_h);
        let x = clamp_x_in(anchor, card_w, mon);
        let y = bar_top_h(bar_h) - card_h - 8;

        // Reuse the registered thumbnails for the SAME group — a hover storm
        // performs zero DWM register/unregister cycles. A group switch (or a
        // window count change) recycles all handles. The comparison is over
        // the REGISTERED set (the first `shown` windows), so a 6-window
        // group keeps its three thumbs when a 7th window appears.
        let registered: Vec<isize> = targets.iter().take(shown).copied().collect();
        let same_group = s.preview_group == targets
            && s.thumbs.len() == registered.len()
            && s.thumbs.iter().map(|(id, _)| *id).collect::<Vec<_>>() == registered;
        if !same_group {
            kill_thumbs(&mut s);
            for id in &registered {
                match DwmRegisterThumbnail(prev, hwnd(*id)) {
                    Ok(t) => s.thumbs.push((*id, t)),
                    Err(e) => {
                        crate::util::log(&format!(
                            "preview: DwmRegisterThumbnail failed: {}",
                            e
                        ));
                        s.thumbs.push((*id, 0));
                    }
                }
            }
        }

        place(prev, x, y, card_w, card_h, s.preview_visible);

        let rects = slot_rects(cells, card_w_css, card_h_css);
        let label_h = (SLOT_LABEL_H_CSS * dpi).round() as i32;
        let mut slots = Vec::with_capacity(shown);
        for (i, id) in registered.iter().enumerate() {
            let (sx, sy, sw, sh) = rects[i];
            let minimized = IsIconic(hwnd(*id)).as_bool();
            let title = window_title(hwnd(*id));
            let thumb_css = ThumbCss {
                x: sx,
                y: sy,
                w: sw,
                h: sh,
            };
            {
                let handle = s.thumbs.get(i).map(|(_, t)| *t).unwrap_or(0);
                if handle != 0 {
                    // Physical destination: slot thumb area, aspect-fit inside.
                    let d_left = (sx * dpi).round() as i32;
                    let d_top = (sy * dpi).round() as i32 + label_h;
                    let d_w = (sw * dpi).round() as i32;
                    let d_h = ((sh - SLOT_LABEL_H_CSS) * dpi).round() as i32;
                    let (src_w, src_h) = match DwmQueryThumbnailSourceSize(handle) {
                        Ok(sz) => (sz.cx.max(1), sz.cy.max(1)),
                        Err(_) => (160, 100),
                    };
                    let scale = ((d_w as f32) / (src_w as f32))
                        .min((d_h as f32) / (src_h as f32));
                    let tw = ((src_w as f32 * scale).floor().max(1.0)) as i32;
                    let th = ((src_h as f32 * scale).floor().max(1.0)) as i32;
                    let tx = d_left + (d_w - tw) / 2;
                    let ty = d_top + (d_h - th) / 2;
                    let props = DWM_THUMBNAIL_PROPERTIES {
                        dwFlags: DWM_TNP_RECTDESTINATION
                            | DWM_TNP_VISIBLE
                            | DWM_TNP_OPACITY
                            | DWM_TNP_SOURCECLIENTAREAONLY,
                        rcDestination: RECT {
                            left: tx,
                            top: ty,
                            right: tx + tw,
                            bottom: ty + th,
                        },
                        opacity: 255,
                        fVisible: true.into(),
                        fSourceClientAreaOnly: true.into(),
                        ..Default::default()
                    };
                    let _ = DwmUpdateThumbnailProperties(handle, &props);
                }
            }
            slots.push(PreviewSlot {
                id: *id,
                title,
                minimized,
                thumb: Some(thumb_css),
            });
        }

        s.preview_group = targets;
        s.preview_visible = true;
        let badge = if n >= 5 {
            let (bx, by, bw, bh) = rects[3];
            Some(ThumbCss {
                x: bx,
                y: by,
                w: bw,
                h: bh,
            })
        } else {
            None
        };
        let _ = app.emit_to(
            "preview",
            "preview:show",
            PreviewShow {
                slots,
                overflow: if n >= 5 { n - 3 } else { 0 },
                badge,
            },
        );
    }
    Ok(())
}

/// Public: the preview card was entered/left (JS). Entering cancels any
/// pending hide; leaving schedules one (executed by the cycle with grace).
pub fn hover(app: &AppHandle, enter: bool) {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if enter {
        s.preview_pending_hide = None;
    } else if s.preview_visible {
        s.preview_pending_hide = Some(std::time::Instant::now());
    }
}

pub fn activate(app: &AppHandle, id: isize) {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    let target = if id != 0 && s.preview_group.contains(&id) {
        Some(id)
    } else {
        s.preview_group.first().copied()
    };
    hide(app, &mut s);
    drop(s);
    if let Some(t) = target {
        crate::native::activate_hwnd(t);
    }
}

/// Cycle beat: process pending hides with a cursor check. Runs every beat;
/// cheap because a pending hide is the only state that does real work.
pub fn tick_hover(app: &AppHandle, s: &mut Shared) {
    if !s.preview_visible {
        s.preview_pending_hide = None;
        return;
    }
    let Some(requested) = s.preview_pending_hide else {
        return;
    };
    if requested.elapsed() < std::time::Duration::from_millis(HIDE_GRACE_MS) {
        return;
    }
    s.preview_pending_hide = None;
    // If the cursor is over the card after all (JS enter event missed), keep
    // the card; otherwise the escape is real — hide it.
    if !cursor_over_hwnd(s.hwnd_preview) {
        hide(app, s);
    }
}

/// Whether the cursor is currently inside the given window's rect.
pub(crate) fn cursor_over_hwnd(h: isize) -> bool {
    if h == 0 {
        return false;
    }
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return false;
        }
        let mut rc = RECT::default();
        GetWindowRect(hwnd(h), &mut rc).is_ok()
            && pt.x >= rc.left
            && pt.x < rc.right
            && pt.y >= rc.top
            && pt.y < rc.bottom
    }
}

/// If ANY preview target died, close the card (called from the tick).
pub fn tick_check(app: &AppHandle, s: &mut Shared) {
    let dead = s
        .preview_group
        .iter()
        .any(|t| !unsafe { IsWindow(hwnd(*t)).as_bool() });
    if !dead {
        return;
    }
    kill_thumbs(s);
    s.preview_group.clear();
    if s.preview_visible {
        s.preview_visible = false;
        if s.hwnd_preview != 0 {
            unsafe {
                let _ = ShowWindow(hwnd(s.hwnd_preview), SW_HIDE);
            }
        }
        let _ = app.emit_to("preview", "preview:hide", ());
    }
}

/// Open the wingroup list for the CURRENT preview group. Called from the
/// preview_overflow command (the badge click). Hides the preview card —
/// the sheet replaces it on screen.
pub fn wingroup_show(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if s.hwnd_wingroup == 0 {
        return Ok(());
    }
    // The CURRENT group, alive only. A group must exist (the badge lives on
    // the preview card; a race with the card's hide would have emptied it).
    let group: Vec<isize> = s
        .preview_group
        .iter()
        .copied()
        .filter(|id| *id != 0 && unsafe { IsWindow(hwnd(*id)).as_bool() })
        .collect();
    let overflow: Vec<isize> = group.iter().skip(3).copied().collect();
    if overflow.is_empty() {
        return Ok(());
    }
    let recolor = s.cfg.recolor_icons;
    let name = crate::icons::exe_path_of(hwnd(group[0]))
        .as_deref()
        .and_then(|p| {
            std::path::Path::new(p)
                .file_stem()
                .map(|f| f.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "Windows".into());
    let (normal, ink, _chalk) = crate::icons::build_icon_triple(hwnd(group[0]));
    let icon = if recolor { ink.or(normal) } else { normal };
    let items: Vec<crate::shared::WingroupItem> = overflow
        .iter()
        .map(|id| crate::shared::WingroupItem {
            id: *id,
            title: window_title(hwnd(*id)),
            minimized: unsafe { IsIconic(hwnd(*id)).as_bool() },
        })
        .collect();
    let payload = crate::shared::WingroupShow { name, icon, items };

    s.wingroup_last = Some(payload.clone());

    // The sheet replaces the card: hide the preview (thumbs stay registered,
    // invisible — re-hovering the group is a pure property update).
    hide(app, &mut s);
    let h = hwnd(s.hwnd_wingroup);
    // The card's rect (captured before the hide) anchors the sheet.
    let prev_rect = if s.hwnd_preview != 0 {
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(hwnd(s.hwnd_preview), &mut rc) }.is_ok() {
            Some(rc)
        } else {
            None
        }
    } else {
        None
    };
    // The sheet lives where the CARD lived — size it with the preview
    // window's DPI and clamp inside the card's monitor (on multi-monitor
    // the card may sit above a secondary bar).
    let (dpi, mon) = if s.hwnd_preview != 0 && unsafe { IsWindow(hwnd(s.hwnd_preview)).as_bool() } {
        let d = unsafe { GetDpiForWindow(hwnd(s.hwnd_preview)) };
        (
            if d != 0 { d as f64 / 96.0 } else { 1.0 },
            crate::util::monitor_rect(hwnd(s.hwnd_preview)),
        )
    } else {
        (bar_dpi(&s), (0, 0, screen_width(), screen_height()))
    };
    drop(s);

    // Payload FIRST, then the window: the webview paints the list while the
    // show dance runs, so the sheet never flashes empty.
    let _ = app.emit_to("wingroup", "wingroup:show", payload.clone());
    for delay in [200u64, 700u64] {
        let ra = app.clone();
        let rp = payload.clone();
        std::thread::Builder::new()
            .name("wingroup-reemit".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                let _ = ra.emit_to("wingroup", "wingroup:show", rp);
            })
            .ok();
    }

    unsafe {
        let w = (340.0 * dpi).round() as i32;
        let hgt = (420.0 * dpi).round() as i32;
        let (mx, my, mw, mh) = mon;
        let (mut x, mut y) = match prev_rect {
            Some(rc) => (
                rc.left + ((rc.right - rc.left) - w) / 2,
                rc.top + ((rc.bottom - rc.top) - hgt) / 2,
            ),
            None => (((mx + (mw - w) / 2).max(mx + 8)), my + 120),
        };
        // Stay inside the card's monitor and above its bar strip.
        let max_y = (my + mh) - hgt - 8;
        let max_x = (mx + mw - w - 8).max(mx + 8);
        x = x.clamp(mx + 8, max_x);
        y = y.min(max_y).max(my + 8);
        let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, hgt, SWP_NOACTIVATE);
        let _ = ShowWindow(h, SW_SHOW);
        // The sheet must own the focus (Escape + blur closing — the task
        // menu contract; activate_hwnd runs the ever_fg ladder).
        crate::native::activate_hwnd(h.0 as isize);
        // …and the keyboard must land in the browser child, or Escape never
        // reaches the sheet until the user clicks inside it.
        crate::native::webview_keys(app, "wingroup");
    }
    Ok(())
}

/// A row (or the sheet's own activation click) — activate that window and
/// close the sheet.
pub fn wingroup_activate(app: &AppHandle, id: isize) {
    unsafe {
        if id != 0 && IsWindow(hwnd(id)).as_bool() {
            crate::native::activate_hwnd(id);
        }
    }
    wingroup_hide(app);
}

/// Close the sheet. Idempotent — Escape, the X, blur and the row click all
/// land here.
pub fn wingroup_hide(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    if s.hwnd_wingroup != 0 {
        unsafe {
            let _ = ShowWindow(hwnd(s.hwnd_wingroup), SW_HIDE);
        }
    }
}

/// The sheet's rows checked against reality — lock-free core (runs under the
/// state lock from the heartbeat tick). A row whose window died (Close via
/// the row's context menu, End task, the app exiting on its own) disappears
/// from the open sheet; when the last row dies the sheet closes with it.
/// WM_CLOSE is async, so this is re-run on a slow beat rather than once.
pub unsafe fn wingroup_prune_locked(app: &AppHandle, s: &mut Shared) {
    if s.hwnd_wingroup == 0 || !IsWindowVisible(hwnd(s.hwnd_wingroup)).as_bool() {
        return;
    }
    let Some(payload) = s.wingroup_last.clone() else {
        return;
    };
    if payload.items.is_empty() {
        return;
    }
    let alive: Vec<crate::shared::WingroupItem> = payload
        .items
        .iter()
        .filter(|it| IsWindow(hwnd(it.id)).as_bool())
        .cloned()
        .collect();
    if alive.len() == payload.items.len() {
        return; // nothing died yet
    }
    if alive.is_empty() {
        s.wingroup_last = None;
        let _ = ShowWindow(hwnd(s.hwnd_wingroup), SW_HIDE);
        crate::util::log("wingroup: last row died — sheet closed");
        return;
    }
    let fresh = crate::shared::WingroupShow {
        name: payload.name,
        icon: payload.icon,
        items: alive,
    };
    s.wingroup_last = Some(fresh.clone());
    let _ = app.emit_to("wingroup", "wingroup:show", fresh);
    crate::util::log("wingroup: rows pruned (windows closed)");
}

/// Locking wrapper for the prune (task-menu paths that don't hold the lock).
pub fn wingroup_refresh(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    unsafe { wingroup_prune_locked(app, &mut s) };
}

const TRAYFLY_CELL_CSS: f64 = 54.0;
const TRAYFLY_GAP_CSS: f64 = 6.0;
const TRAYFLY_PAD_CSS: f64 = 12.0;

const TRAYFLY_MAX_ROWS: f64 = 5.0;

/// Column count adapts to the icon count (1..=3); the DOM grid is set to
/// the same number by trayfly.js — keep the two formulas identical.
fn trayfly_cols(n: usize) -> f64 {
    n.clamp(1, 3) as f64
}

/// Row count adapts to the icon count (1..=5). Same formula in trayfly.js.
fn trayfly_rows(n: usize) -> f64 {
    (((n as f64) / 3.0).ceil()).clamp(1.0, TRAYFLY_MAX_ROWS)
}

fn trayfly_size_css(n: usize) -> (f64, f64) {
    if n == 0 {
        return (160.0, 78.0);
    }
    let cols = trayfly_cols(n);
    let rows = trayfly_rows(n);
    let w = TRAYFLY_PAD_CSS * 2.0 + cols * TRAYFLY_CELL_CSS + (cols - 1.0) * TRAYFLY_GAP_CSS;
    let h = TRAYFLY_PAD_CSS * 2.0 + rows * TRAYFLY_CELL_CSS + (rows - 1.0) * TRAYFLY_GAP_CSS;
    (w, h)
}

/// The trayfly's OWN DPI (what WebView2 scales its DOM by). The bar's DPI is
/// only a fallback for the brief moment before the window handle exists.
fn trayfly_dpi(s: &Shared) -> f64 {
    if s.hwnd_trayfly != 0 {
        let dpi = unsafe { GetDpiForWindow(hwnd(s.hwnd_trayfly)) };
        if dpi != 0 {
            return dpi as f64 / 96.0;
        }
    }
    bar_dpi(s)
}

fn trayfly_geometry(s: &Shared, bar_h: isize, anchor_css: f64) -> (i32, i32, i32, i32) {
    let dpi = trayfly_dpi(s);
    let (w_css, h_css) = trayfly_size_css(s.tray_last.len());
    let card_w = (w_css * dpi).round() as i32;
    let card_h = (h_css * dpi).round() as i32;
    let mon = monitor_of_bar(bar_h);
    let anchor = anchor_to_physical_h(bar_h, anchor_css);
    let x = clamp_x_in(anchor + 4, card_w, mon);
    let y = bar_top_h(bar_h) - card_h - 10;
    (x, y, card_w, card_h)
}

pub fn trayfly_show(app: &AppHandle, bar_h: isize, anchor_css: f64) {
    let st = app.state::<crate::AppShared>();
    let (gen, rect, n, list_snapshot, was_empty) = {
        let mut s = crate::util::lock_shared(&st.0);
        if s.hwnd_trayfly == 0 {
            return;
        }
        s.trayfly_gen += 1;
        s.trayfly_visible = true;
        s.trayfly_click_used = false;
        s.trayfly_pong_wait = Some((s.trayfly_gen, std::time::Instant::now()));
        s.trayfly_probe_stage = 0;
        s.trayfly_busy = false;
        s.trayfly_busy_at = None;
        s.trayfly_ghost_since = None;
        s.trayfly_open_fg = unsafe { GetForegroundWindow().0 as isize };
        s.trayfly_geo_n = s.tray_last.len();
        let empty = s.tray_last.is_empty();
        let snap = s.tray_last.clone();
        (
            s.trayfly_gen,
            trayfly_geometry(&s, bar_h, anchor_css),
            s.trayfly_geo_n,
            snap,
            empty,
        )
    };
    if was_empty {
        crate::tray::request_reseed();
    }
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        {
            let st = a.state::<crate::AppShared>();
            if !crate::util::lock_shared(&st.0).trayfly_visible {
                return;
            }
        }
        let Some(ww) = a.get_webview_window("trayfly") else {
            return;
        };
        let Ok(h) = ww.hwnd() else {
            return;
        };
        let hf = HWND(h.0 as _);
        unsafe {
            let mut rc = RECT::default();
            let (cx, cy, cw, ch) = if GetWindowRect(hf, &mut rc).is_ok() {
                (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top)
            } else {
                (0, 0, 0, 0)
            };
            let same = (cx, cy, cw, ch) == rect;
            let visible = IsWindowVisible(hf).as_bool();
            if !same && visible {
                {
                    let st = a.state::<crate::AppShared>();
                    let mut stg = crate::util::lock_shared(&st.0);
                    stg.trayfly_busy = true;
                    stg.trayfly_busy_at = Some(std::time::Instant::now());
                }
                let _ = ShowWindow(hf, SW_HIDE);
            }
            if !same {
                let _ = SetWindowPos(hf, HWND_TOPMOST, rect.0, rect.1, rect.2, rect.3, SWP_NOACTIVATE);
            }
            if !same && visible {
                let _ = ShowWindow(hf, SW_SHOW);
            }
            if !visible {
                let _ = ShowWindow(hf, SW_SHOW);
                let _ = SetForegroundWindow(hf);
            }
            if !same && visible {
                let _ = SetForegroundWindow(hf);
                let st = a.state::<crate::AppShared>();
                let mut stg = crate::util::lock_shared(&st.0);
                stg.trayfly_busy = false;
                stg.trayfly_busy_at = None;
            }
            let mut rc2 = RECT::default();
            let got = if GetWindowRect(hf, &mut rc2).is_ok() {
                format!(
                    "({},{}) {}x{}",
                    rc2.left,
                    rc2.top,
                    rc2.right - rc2.left,
                    rc2.bottom - rc2.top
                )
            } else {
                "?".into()
            };
            crate::util::log(&format!(
                "trayfly: shown gen={} n={n} want=({},{}) {}x{} got={} vis={} fg={:#x}",
                gen,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                got,
                IsWindowVisible(hf).as_bool() as i32,
                GetForegroundWindow().0 as isize
            ));
        }
        let _ = a.emit_to("trayfly", "trayfly:show", ());
        let _ = a.emit_to("trayfly", "tray_icons", &list_snapshot);
    });
}

pub fn trayfly_sync_size(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let (gen, n) = {
        let s = st.0.lock().unwrap_or_else(|e| e.into_inner());
        if s.hwnd_trayfly == 0 || !s.trayfly_visible {
            return;
        }
        (s.trayfly_gen, s.trayfly_geo_n)
    };
    let a = app.clone();
    let _ = std::thread::Builder::new()
        .name("trayfly-resize".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let a2 = a.clone();
            let _ = a.run_on_main_thread(move || {
                let st = a2.state::<crate::AppShared>();
                let (ht, dpi, n_now) = {
                    let s = crate::util::lock_shared(&st.0);
                    // Re-validate: hidden, closed, a newer open, or a newer
                    // count (a bounce that already came back) -> nothing to
                    // do. The newest scheduled dance handles the live truth.
                    if s.hwnd_trayfly == 0
                        || !s.trayfly_visible
                        || s.trayfly_gen != gen
                        || s.trayfly_geo_n != n
                    {
                        return;
                    }
                    (s.hwnd_trayfly, trayfly_dpi(&s), s.tray_last.len())
                };
                let (w_css, h_css) = trayfly_size_css(n_now);
                let pw = (w_css * dpi).round() as i32;
                let ph = (h_css * dpi).round() as i32;
                {
                    let st = a2.state::<crate::AppShared>();
                    let mut s = crate::util::lock_shared(&st.0);
                    if !s.trayfly_visible || s.trayfly_gen != gen {
                        return;
                    }
                    s.trayfly_busy = true;
                    s.trayfly_busy_at = Some(std::time::Instant::now());
                }
                unsafe {
                    let hf = HWND(ht as _);
                    let mut rc = RECT::default();
                    if GetWindowRect(hf, &mut rc).is_ok() {
                        let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
                        if (cw - pw).abs() > 2 || (ch - ph).abs() > 2 {
                            let visible = IsWindowVisible(hf).as_bool();
                            // The flyout was placed above ITS bar: the old
                            // bottom edge + rise gap IS that bar's top — keep
                            // the anchor while the sheet resizes (works on
                            // every monitor; no primary-bar assumption).
                            let x = clamp_x_in(
                                rc.left + cw / 2,
                                pw,
                                crate::util::monitor_rect(hf),
                            );
                            let y = rc.top + ch + 10 - ph;
                            if visible {
                                let _ = ShowWindow(hf, SW_HIDE);
                            }
                            let _ =
                                SetWindowPos(hf, HWND_TOPMOST, x, y, pw, ph, SWP_NOACTIVATE);
                            if visible {
                                let _ = ShowWindow(hf, SW_SHOW);
                                let _ = SetForegroundWindow(hf);
                            }
                            crate::util::log(&format!(
                                "trayfly: count resize {}x{} -> {}x{} (n={n_now}, visible={}, debounced)",
                                cw, ch, pw, ph, visible as i32
                            ));
                        }
                    }
                }
                {
                    let st = a2.state::<crate::AppShared>();
                    let mut stg = crate::util::lock_shared(&st.0);
                    stg.trayfly_busy = false;
                    stg.trayfly_busy_at = None;
                }
                // No "trayfly:show" emission here — the grid already
                // re-rendered via tray_icons; replaying the entrance
                // animation mid-open would be wrong. The window resized
                // around the existing content.
            });
        });
}

/// Command entry: hide the trayfly (locks, then delegates).
pub fn trayfly_hide(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    trayfly_hide_with(app, &mut s);
}

fn trayfly_hide_with(app: &AppHandle, s: &mut Shared) {
    if !s.trayfly_visible {
        return;
    }
    s.trayfly_visible = false;
    s.trayfly_gen += 1;
    s.trayfly_ghost_since = None;
    s.trayfly_hide_pending = true; // the 150ms SW_HIDE thread owns visibility
    let gen = s.trayfly_gen;
    let ht = s.hwnd_trayfly;
    crate::util::log(&format!("trayfly: hide gen={gen}"));
    // CSS exit animation first, then hide the window.
    let _ = app.emit_to("trayfly", "trayfly:hide", ());
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let _ = a.run_on_main_thread({
            let a2 = a.clone();
            move || {
                let st = a2.state::<crate::AppShared>();
                let mut s = crate::util::lock_shared(&st.0);
                s.trayfly_hide_pending = false; // resolved either way
                let cur = s.trayfly_gen;
                if cur != gen {
                    crate::util::log(&format!(
                        "trayfly: stale hide gen={gen} dropped (cur={cur})"
                    ));
                    return;
                }
                // Raw SW_HIDE exactly like flyout_hide — NOT ww.hide(): tao's
                // flag machinery rewrites GWL_STYLE/EXSTYLE from its cache on
                // every call; the raw path never touches styles.
                if ht != 0 {
                    unsafe {
                        let _ = ShowWindow(HWND(ht as _), SW_HIDE);
                    }
                }
            }
        });
    });
    // The bar un-rotates its chevron on this (it does not know whether the
    // close came from the chevron itself, the poll, or Escape). Every bar
    // hears it — a secondary bar's chevron used to stick open.
    emit_to_bars(app, "trayfly:closed", &());
}

pub fn tick_trayfly(app: &AppHandle, s: &mut Shared) {
    if s.trayfly_busy {
        let stuck = match s.trayfly_busy_at {
            Some(t) => t.elapsed() > std::time::Duration::from_millis(600),
            // busy without a stamp can only be legacy/pre-show residue.
            None => true,
        };
        if stuck {
            crate::util::log(&format!(
                "trayfly: busy flag stuck {}ms — watchdog cleared (poll resumes)",
                s.trayfly_busy_at
                    .map(|t| t.elapsed().as_millis())
                    .unwrap_or(0)
            ));
            s.trayfly_busy = false;
            s.trayfly_busy_at = None;
        }
    }

    if !s.trayfly_visible {
        if s.hwnd_trayfly != 0
            && !s.trayfly_hide_pending
            && unsafe { IsWindowVisible(HWND(s.hwnd_trayfly as _)) }.as_bool()
        {
            let since = *s.trayfly_ghost_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() > std::time::Duration::from_millis(400) {
                let (ht, gen) = (s.hwnd_trayfly, s.trayfly_gen);
                s.trayfly_ghost_since = None;
                crate::util::log(&format!(
                    "trayfly: GHOST HEAL — window visible while state says hidden (gen {gen}); force-hiding"
                ));
                let a = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let st = a.state::<crate::AppShared>();
                    let s = crate::util::lock_shared(&st.0);
                    if s.trayfly_visible || s.trayfly_gen != gen {
                        crate::util::log("trayfly: ghost heal aborted (re-shown)");
                        return;
                    }
                    drop(s);
                    unsafe {
                        let _ = ShowWindow(HWND(ht as _), SW_HIDE);
                    }
                    crate::util::log("trayfly: ghost healed (SW_HIDE done)");
                });
            }
        } else {
            s.trayfly_ghost_since = None;
        }
        return;
    }
    if s.hwnd_trayfly == 0 {
        return;
    }
    if let Some((gen, armed)) = s.trayfly_pong_wait {
        if gen == s.trayfly_gen && armed.elapsed() > std::time::Duration::from_millis(1200) {
            if s.trayfly_probe_stage == 0 {
                s.trayfly_probe_stage = 1;
                s.trayfly_pong_wait = Some((gen, std::time::Instant::now()));
                crate::util::log(&format!(
                    "trayfly: no pong {}ms after show (gen {gen}) — re-emitting show (probe 1/2)",
                    armed.elapsed().as_millis()
                ));
                let _ = app.emit_to("trayfly", "trayfly:show", "probe");
                let snap = s.tray_last.clone();
                let _ = app.emit_to("trayfly", "tray_icons", &snap);
            } else {
                s.trayfly_pong_wait = None;
                s.trayfly_probe_stage = 0;
                let n = s.trayfly_reloads;
                s.trayfly_reloads += 1;
                crate::util::log(&format!(
                    "trayfly: webview DEAF — no pong on show AND probe (gen {gen}, reload #{}) — reloading",
                    n + 1
                ));
                let a = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(ww) = a.get_webview_window("trayfly") {
                        if let Err(e) = ww.reload() {
                            crate::util::log(&format!("trayfly: webview reload FAILED: {e}"));
                        } else {
                            crate::util::log("trayfly: webview reload issued");
                        }
                    }
                });
            }
        }
    }

    if s.trayfly_busy {
        return;
    }
    let fg = unsafe { GetForegroundWindow().0 as isize };
    let ours = [
        s.hwnd_bar,
        s.hwnd_preview,
        s.hwnd_trayfly,
        s.hwnd_host,
    ];
    let fg_moved = fg != s.trayfly_open_fg && !(fg != 0 && ours.contains(&fg));
    let outside_click = unsafe {
        (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000 != 0
            || GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000 != 0)
            && !cursor_over_hwnd(s.hwnd_trayfly)
            && !cursor_over_hwnd(s.hwnd_bar)
    };
    if fg_moved || outside_click {
        trayfly_hide_with(app, s);
    }
}

/// CSS width of the drop-up card.
const VOLFLY_W_CSS: f64 = 264.0;
const VOLFLY_FIXED_H_CSS: f64 = 135.0;

fn volfly_size_css() -> (f64, f64) {
    (VOLFLY_W_CSS, VOLFLY_FIXED_H_CSS)
}

fn volfly_dpi(s: &Shared) -> f64 {
    if s.hwnd_volfly != 0 {
        let dpi = unsafe { GetDpiForWindow(hwnd(s.hwnd_volfly)) };
        if dpi != 0 {
            return dpi as f64 / 96.0;
        }
    }
    bar_dpi(s)
}

fn volfly_geometry(s: &Shared, bar_h: isize, anchor_css: f64) -> (i32, i32, i32, i32) {
    let dpi = volfly_dpi(s);
    let (w_css, h_css) = volfly_size_css();
    let card_w = (w_css * dpi).round() as i32;
    let card_h = (h_css * dpi).round() as i32;
    let mon = monitor_of_bar(bar_h);
    let anchor = anchor_to_physical_h(bar_h, anchor_css);
    let x = clamp_x_in(anchor + 4, card_w, mon);
    // Same 10px rise-gap above the bar as the trayfly.
    let y = bar_top_h(bar_h) - card_h - 10;
    (x, y, card_w, card_h)
}

/// Emit an event to EVERY bar window — the primary "taskbar" plus every
/// secondary "taskbar-sN". Flyout open/close state is per-BAR UI (each bar
/// lights its own tray chevron / speaker button); a primary-only emit left
/// secondary bars' buttons stuck-open whenever the close came from the
/// poll, Escape, or another bar.
fn emit_to_bars<T: serde::Serialize + Clone>(app: &AppHandle, event: &str, payload: &T) {
    for label in app.webview_windows().keys() {
        if label == "taskbar" || label.starts_with("taskbar-s") {
            let _ = app.emit_to(label.as_str(), event, payload);
        }
    }
}

pub fn volfly_show(app: &AppHandle, bar_h: isize, anchor_css: f64) {
    // COM snapshot outside the lock (audio.rs wraps its own COM init).
    let state = crate::audio::state().unwrap_or(crate::audio::AudioState {
        vol: 0.0,
        muted: false,
    });
    let st = app.state::<crate::AppShared>();
    let rect = {
        let mut s = crate::util::lock_shared(&st.0);
        if s.hwnd_volfly == 0 {
            return;
        }
        s.volfly_visible = true;
        volfly_geometry(&s, bar_h, anchor_css)
    };
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        {
            let st = a.state::<crate::AppShared>();
            if !crate::util::lock_shared(&st.0).volfly_visible {
                return;
            }
        }
        let Some(ww) = a.get_webview_window("volfly") else {
            return;
        };
        let Ok(h) = ww.hwnd() else {
            return;
        };
        let hf = HWND(h.0 as _);
        unsafe {
            // The size CAN change between opens (mixer rows differ), and a
            // transparent WebView2 resize happens hidden: the trayfly dance.
            let mut rc = RECT::default();
            let (cx, cy, cw, ch) = if GetWindowRect(hf, &mut rc).is_ok() {
                (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top)
            } else {
                (0, 0, 0, 0)
            };
            let same = (cx, cy, cw, ch) == rect;
            let visible = IsWindowVisible(hf).as_bool();
            if !same && visible {
                let _ = ShowWindow(hf, SW_HIDE);
            }
            if !same {
                let _ = SetWindowPos(hf, HWND_TOPMOST, rect.0, rect.1, rect.2, rect.3, SWP_NOACTIVATE);
            }
            if !same && visible {
                let _ = ShowWindow(hf, SW_SHOW);
            }
            if !visible {
                let _ = ShowWindow(hf, SW_SHOW);
                let _ = SetForegroundWindow(hf);
            }
            if !same && visible {
                let _ = SetForegroundWindow(hf);
            }
        }
        // Payload ride-along on the show event: the webview renders from
        // this the moment it opens; the 600ms pull is only the healer.
        let _ = a.emit_to("volfly", "volfly:state", &state);
        let _ = a.emit_to("volfly", "volfly:show", ());
        emit_to_bars(&a, "volfly:opened", &());
        crate::util::log(&format!(
            "volfly: shown rect=({},{},{},{})",
            rect.0, rect.1, rect.2, rect.3
        ));
    });
}

pub fn volfly_hide_with(app: &AppHandle, s: &mut Shared) {
    if !s.volfly_visible {
        return;
    }
    s.volfly_visible = false;
    let hv = s.hwnd_volfly;
    let _ = app.run_on_main_thread(move || {
        if hv != 0 {
            unsafe {
                let _ = ShowWindow(HWND(hv as _), SW_HIDE);
            }
        }
    });
    // The bar un-lights its speaker button on this (the close may come from
    // the poll, Escape, or the toggle — it never knows which). Every bar
    // hears it: the button that opened it lives on ONE bar, but all bars
    // keep their own open-state honest.
    emit_to_bars(app, "volfly:closed", &());
}

pub fn tick_volfly(app: &AppHandle, s: &mut Shared) {
    if !s.volfly_visible || s.hwnd_volfly == 0 {
        return;
    }
    let ldown = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000 != 0 };
    let rdown = unsafe { GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000 != 0 };
    let over_fly = cursor_over_hwnd(s.hwnd_volfly);
    // THE DRAG LATCH: a button pressed DOWN over the open drop-up is a
    // slider drag in progress — it may travel outside the frame while held,
    // and that travel is not an outside click. Latch on any beat that sees
    // (button down ∧ cursor over the flyout); clear only when every button
    // is up. GetAsyncKeyState is global HARDWARE state — a release cannot
    // be missed by a focus change or a capture steal, so the latch can
    // never wedge the flyout open. A drag lasts many beats, so the latch
    // always sees at least one (down ∧ over) beat before the cursor
    // leaves; a sub-beat click starts and ends inside and never needs it.
    if (ldown || rdown) && over_fly {
        s.volfly_drag = true;
    } else if !ldown && !rdown {
        s.volfly_drag = false;
    }
    if s.volfly_drag {
        return;
    }
    // THE PRESS CONTRACT (the same one the task menu and the +N sheet live
    // by): the drop-up closes ONLY on lost interest — a press that lands
    // somewhere else — or on its own controls (the Mute button, Escape in
    // volfly.js, the bar's speaker toggle). Focus drift alone never closes
    // it: a background app waking up and taking the foreground is not the
    // user saying "go away", and the old foreground check murdered the
    // drop-up mid-adjust on exactly such a drift.
    let outside_click = (ldown || rdown)
        && !over_fly
        && !cursor_over_hwnd(s.hwnd_bar);
    if outside_click {
        volfly_hide_with(app, s);
    }
}
