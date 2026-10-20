//! Shared state between the Tauri command threads and the native thread.
//!
//! HWNDs are stored as `isize` so that `Shared` stays `Send` inside the
//! managed mutex; they are converted back to `HWND` at every call site.

use std::collections::HashMap;

use serde::Serialize;

use crate::config::Config;

pub const BAR_H: i32 = 44;

/// How many logical px the bar WINDOW rides taller than its AppBar
/// reservation. The reservation (and the workarea) stay at BAR_H, but the
/// window's top edge is lifted so the painted bar reaches the top of the
/// reserved strip — kills the sliver of wallpaper that used to show above
/// the bar against light maximized windows. The bar is topmost, so the
/// lift simply paints over the bottom lift px of whatever sits behind it.
pub const BAR_LIFT: i32 = 4;

/// A secondary-display bar: one per non-primary monitor, same page as the
/// primary bar. `mon` is that monitor's physical rect captured at boot —
/// the window is born centered on the PRIMARY monitor, so
/// MonitorFromWindow lies until the first native placement lands.
#[derive(Clone, Copy, Debug)]
pub struct SecBar {
    pub hwnd: isize,
    pub mon: (i32, i32, i32, i32),
    /// Its own AppBar (ABM_NEW) is registered — explorer does the same for
    /// Shell_SecondaryTrayWnd, and maximized windows respect the dock only
    /// when the strip is a registered appbar on THAT monitor.
    pub registered: bool,
    /// Fullscreen streak + hidden flag for THIS monitor's foreground app.
    pub fs_streak: u32,
    pub fs_hidden: bool,
}

/// Plain rect (screen or client coords), Send-friendly.
#[derive(Clone, Copy, Default)]
pub struct Rc {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rc {
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
}

#[derive(Serialize, Clone, PartialEq)]
pub struct WinDto {
    pub id: isize,
    pub title: String,
    pub minimized: bool,
    pub active: bool,
}

#[derive(Serialize, Clone, PartialEq)]
pub struct TaskDto {
    pub pid: u32,
    pub id: isize,
    pub title: String,
    /// base64 PNG (already recolored if the setting is on)
    pub icon: Option<String>,
    pub minimized: bool,
    pub active: bool,
    pub wins: Vec<WinDto>,
}

/// Thumbnail rect in CSS pixels, relative to the preview window.
#[derive(Serialize, Clone)]
pub struct ThumbCss {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Serialize, Clone, PartialEq)]
pub struct HushtopItem {
    pub name: String,
    pub kind: String, // "folder" | "file"
    pub icon: Option<String>,
    pub x: i32,
    pub y: i32,
}

#[derive(Serialize, Clone)]
pub struct PreviewSlot {
    pub id: isize,
    pub title: String,
    pub minimized: bool,
    pub thumb: Option<ThumbCss>,
}

#[derive(Serialize, Clone)]
pub struct PreviewShow {
    pub slots: Vec<PreviewSlot>,
    pub overflow: usize,
    pub badge: Option<ThumbCss>,
}

#[derive(Serialize, Clone)]
pub struct WingroupItem {
    pub id: isize,
    pub title: String,
    pub minimized: bool,
}

/// The wingroup list payload: the process name + icon + every window.
#[derive(Serialize, Clone)]
pub struct WingroupShow {
    pub name: String,
    pub icon: Option<String>,
    pub items: Vec<WingroupItem>,
}

pub struct Shared {
    pub hwnd_bar: isize,
    pub hwnd_preview: isize,
    /// tray overflow flyout (chevron menu with all tray icons)
    pub hwnd_trayfly: isize,
    pub hwnd_volfly: isize,
    /// hidden host window (class `hushell_host`): heartbeat timer owner +
    /// exit receiver. Never shown on screen.
    pub hwnd_host: isize,
    pub hwnd_hushtop: isize,
    pub hwnd_prompt: isize,
    pub hwnd_wingroup: isize,
    pub hwnd_menu: isize,
    pub hwnd_startmenu: isize,
    pub hwnd_lock: isize,
    /// Bars for every non-primary monitor (empty on single-display rigs).
    pub sec_bars: Vec<SecBar>,
    pub startmenu_visible: bool,
    pub startmenu_opened_at: Option<std::time::Instant>,
    pub startmenu_prev_fg: isize,
    pub startmenu_ever_fg: bool,
    pub menu_visible: bool,
    pub menu_prev_fg: isize,
    pub menu_opened_at: Option<std::time::Instant>,
    pub hushtop_parent: isize,
    pub hushtop_applied: bool,
    pub hushtop_icons_hidden: bool,
    pub hushtop_scan_sig: u64,
    pub hushtop_items: Vec<HushtopItem>,
    pub hushtop_icon_cache: HashMap<String, (u64, String, Option<String>)>,
    pub hushtop_menu_fg: isize,
    pub hushtop_rebuild_queued: bool,
    pub hushtop_rebuild_fails: u32,
    pub hushtop_rescan: bool,
    pub hushtop_prompt_open: bool,
    pub hushtop_prompt_fg: isize,
    pub hushtop_prompt_mode: String,
    pub hushtop_prompt_name: String,
    pub fs_streak: u32,
    pub bar_fs_hidden: bool,
    pub hushtop_presence_wrong: bool,
    pub hushtop_repair_queued: bool,
    pub hushtop_reapply_queued: bool,
    pub hushtop_dance_cooldown: u32,
    pub ogbar_released: bool,

    pub bar: Rc,

    pub cfg: Config,

    /// last emitted task list
    pub tasks: Vec<TaskDto>,
    pub task_order: Vec<isize>,
    pub icons: HashMap<isize, (Option<String>, Option<String>, Option<String>)>,
    pub render_dirty: bool,

    pub thumbs: Vec<(isize, isize)>,
    /// the windows the visible card was built for (death check + click)
    pub preview_group: Vec<isize>,
    pub preview_visible: bool,
    pub wingroup_last: Option<WingroupShow>,
    pub preview_pending_hide: Option<std::time::Instant>,

    pub trayfly_visible: bool,
    pub volfly_visible: bool,
    pub volfly_drag: bool,
    pub trayfly_gen: u64,
    pub trayfly_geo_n: usize,
    pub trayfly_busy: bool,
    pub trayfly_busy_at: Option<std::time::Instant>,
    pub trayfly_click_used: bool,
    pub trayfly_hide_pending: bool,
    pub trayfly_pong_wait: Option<(u64, std::time::Instant)>,
    pub trayfly_probe_stage: u8,
    pub trayfly_ghost_since: Option<std::time::Instant>,
    pub trayfly_reloads: u64,
    /// foreground hwnd captured when the trayfly was last shown; the 100ms
    /// poll closes the flyout when the foreground moves to a DIFFERENT
    /// non-hushell window (the no-blur replacement for blur-to-close)
    pub trayfly_open_fg: isize,

    // --- tray (systray-util spy stream, insertion-ordered) ---
    pub tray: Vec<crate::tray::TrayIconState>,
    pub tray_last: Vec<crate::tray::TrayDto>,
    pub tray_pushed: Vec<crate::tray::TrayDto>,
    /// Instant of the last push. None until the first (boot) seed, which
    /// skips the cooldown; afterwards pushes are rate-limited to 1 per 5 s.
    pub tray_pushed_at: Option<std::time::Instant>,

    pub appbar_registered: bool,

    // --- original taskbar hiding state ---
    pub hidden_applied: bool,
    pub native_orig: Option<Rc>,

    /// last foreground hwnd seen by the task beat (fast-path compare)
    pub last_fg: isize,

    /// Start Fresh armed — the exit handler spawns a clean replacement
    /// after the teardown when this is set (set by shell_fresh_start).
    pub fresh_relaunch: bool,
}

impl Shared {
    pub fn new() -> Self {
        Shared {
            hwnd_bar: 0,
            hwnd_preview: 0,
            hwnd_trayfly: 0,
            hwnd_volfly: 0,
            hwnd_host: 0,
            hwnd_hushtop: 0,
            hwnd_prompt: 0,
            hwnd_wingroup: 0,
            hwnd_menu: 0,
            hwnd_startmenu: 0,
            hwnd_lock: 0,
            sec_bars: Vec::new(),
            startmenu_visible: false,
            startmenu_opened_at: None,
            startmenu_prev_fg: 0,
            startmenu_ever_fg: false,
            menu_visible: false,
            menu_prev_fg: 0,
            menu_opened_at: None,
            hushtop_parent: 0,
            hushtop_applied: false,
            hushtop_icons_hidden: false,
            hushtop_scan_sig: 0,
            hushtop_items: Vec::new(),
            hushtop_icon_cache: HashMap::new(),
            hushtop_menu_fg: 0,
            hushtop_rebuild_queued: false,
            hushtop_rebuild_fails: 0,
            hushtop_rescan: false,
            hushtop_prompt_open: false,
            hushtop_prompt_fg: 0,
            hushtop_prompt_mode: String::new(),
            hushtop_prompt_name: String::new(),
            fs_streak: 0,
            bar_fs_hidden: false,
            hushtop_presence_wrong: false,
            hushtop_repair_queued: false,
            hushtop_reapply_queued: false,
            hushtop_dance_cooldown: 0,
            ogbar_released: false,
            bar: Rc::default(),
            cfg: Config::load(),
            tasks: Vec::new(),
            task_order: Vec::new(),
            icons: HashMap::new(),
            render_dirty: false,
            thumbs: Vec::new(),
            preview_group: Vec::new(),
            preview_visible: false,
            wingroup_last: None,
            preview_pending_hide: None,
            trayfly_visible: false,
            volfly_visible: false,
            volfly_drag: false,
            trayfly_gen: 0,
            trayfly_geo_n: 0,
            trayfly_busy: false,
            trayfly_busy_at: None,
            trayfly_click_used: false,
            trayfly_hide_pending: false,
            trayfly_pong_wait: None,
            trayfly_probe_stage: 0,
            trayfly_ghost_since: None,
            trayfly_reloads: 0,
            trayfly_open_fg: 0,
            tray: Vec::new(),
            tray_last: Vec::new(),
            tray_pushed: Vec::new(),
            tray_pushed_at: None,
            appbar_registered: false,
            hidden_applied: false,
            native_orig: None,
            last_fg: 0,
            fresh_relaunch: false,
        }
    }
}
