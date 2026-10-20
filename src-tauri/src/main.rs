//! hushell — a quiet custom Windows shell (Tauri edition).
//! Beautifully animated white taskbar without replacing explorer.exe.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod clipboard;
mod config;
mod cycle;
mod dnd;
mod frame;
mod icons;
mod hushtop;
mod lang;
mod lockscreen;
mod media;
mod native;
mod shared;
mod startmenu;
mod startmenu_killer;
mod startup;
mod stats;
mod tasks;
mod thumbs;
mod tray;
mod util;
mod winhook;

use std::sync::Mutex;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

pub struct AppShared(pub Mutex<shared::Shared>);

fn main() {
    util::log(&format!(
        "boot: hushell v{} (pid {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
    std::panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        util::log(&format!("PANIC: {} @ {}", msg, loc));
        hushtop::panic_restore_icons();
    }));

    tauri::Builder::default()
        .manage(AppShared(Mutex::new(shared::Shared::new())))
        .invoke_handler(tauri::generate_handler![
            activate,
            preview_show,
            preview_hide,
            preview_hover,
            preview_activate,
            task_menu,
            task_menu_run,
            task_menu_hide,
            trayfly_show,
            trayfly_hide,
            trayfly_diag,
            trayfly_pong,
            tray_click,
            volfly_show,
            volfly_hide,
            sync_state,
            hushtop_move,
            hushtop_menu,
            hushtop_open,
            hushtop_delete,
            hushtop_rename,
            hushtop_create,
            hushtop_refresh,
            hushtop_prompt_show,
            hushtop_prompt_hide,
            hushtop_prompt_state,
            hushtop_notepad_save,
            hushtop_notepad_del,
            hushtop_notepad_list,
            hushtop_widget_del,
            hushtop_widget_save,
            audio_master_get,
            audio_master_set,
            audio_master_step,
            audio_mute_get,
            audio_mute_set,
            audio_state,
            clipboard_history,
            clipboard_copy,
            clipboard_clear,
            stats_now,
            hushtop_dragout,
            startmenu_hide,
            startmenu_close_finished,
            startmenu_ui_loaded,
            startmenu_ui_shown,
            startmenu_ui_error,
            startmenu_search,
            startmenu_open,
            startmenu_pinned_view,
            startmenu_pin,
            startmenu_unpin,
            startmenu_is_pinned,
            startmenu_add_widget,
            lockscreen_lock,
            lockscreen_unlock,
            lock_password_set,
            lock_password_get,
            lock_hotkey_get,
            lock_hotkey_set,
            startup_get,
            startup_set,
            preview_overflow,
            wingroup_activate,
            wingroup_hide,
            wingroup_data,
            media_now,
            media_ctl,
            system_sleep,
            system_restart,
            system_shutdown,
            shell_fresh_start,
            shell_shutdown
        ])
        // EVERY hushell window is unclosable: the bar used to die to
        // Alt+F4, and a shell that can be dismantled window-by-window is
        // not a shell. Any CloseRequested that slips past the win32
        // no-close subclass is refused here, once, for all windows —
        // present and future (secondary bars, rebuilt hushtop). The
        // shell's own exits ride request_exit → app.exit(0), which never
        // sends WM_CLOSE, so the deny cannot wedge a legit shutdown.
        .on_window_event(|w, e| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                api.prevent_close();
                util::log(&format!(
                    "frame: close requested for '{}' — denied (exits ride request_exit)",
                    w.label()
                ));
            }
        })
        .setup(|app| {
            // The foreground-lock timeout, session-only (never persisted):
            // Windows denies SetForegroundWindow for a while after the last
            // user input lands in another process, and the default 200 ms
            // lock is exactly the window in which the start menu tries to
            // take the keyboard. Zero the lock so the activation ladder
            // always plays its full rungs.
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    SystemParametersInfoW, SPIF_SENDCHANGE, SYSTEM_PARAMETERS_INFO_ACTION,
                };
                // SPI_SETFOREGROUNDLOCKTIMEOUT
                let _ = SystemParametersInfoW(
                    SYSTEM_PARAMETERS_INFO_ACTION(0x0019),
                    0,
                    None,
                    SPIF_SENDCHANGE,
                );
            }
            // shell:startup reconcile — cfg.startup (default on) is the
            // intent, the Startup folder gets to match. Off-thread: the
            // PowerShell shortcut author costs a few hundred ms.
            {
                let st = app.state::<AppShared>();
                let on = crate::util::lock_shared(&st.0).cfg.startup;
                std::thread::spawn(move || startup::reconcile(on));
            }
            // Preview card window (hidden; positioned by the native side).
            let wprev = WebviewWindowBuilder::new(
                app,
                "preview",
                WebviewUrl::App("preview.html".into()),
            )
            .title("hushell-preview")
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wprev.hwnd().map_err(|e| e.to_string())?;

            let wtfly = WebviewWindowBuilder::new(
                app,
                "trayfly",
                WebviewUrl::App("trayfly.html".into()),
            )
            .title("hushell-trayfly")
            .inner_size(198.0, 78.0)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wtfly.hwnd().map_err(|e| e.to_string())?;

            let wvf = WebviewWindowBuilder::new(
                app,
                "volfly",
                WebviewUrl::App("volfly.html".into()),
            )
            .title("hushell-volfly")
            .inner_size(264.0, 228.0)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wvf.hwnd().map_err(|e| e.to_string())?;

            // Hushtop: the live-wallpaper custom desktop. OPAQUE on purpose
            // (notepad paper) — transparent windows drag the whole resize
            // dance minefield along, and a wallpaper has no reason to be
            // transparent. Born hidden; the native thread sizes it, parents
            // it into Progman/WorkerW and shows it (hushtop::apply).
            //
            // The name dialog shares the flyout conventions: hidden at
            // boot, positioned + shown by the native side, always on top
            // (it must take focus — that is the whole point of a dialog).
            let wtop = WebviewWindowBuilder::new(
                app,
                "hushtop",
                WebviewUrl::App("hushtop.html".into()),
            )
            .title("hushell-hushtop")
            .inner_size(1600.0, 900.0)
            .decorations(false)
            .transparent(false)
            .shadow(false)
            .always_on_top(false)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wtop.hwnd().map_err(|e| e.to_string())?;

            let wpr = WebviewWindowBuilder::new(
                app,
                "prompt",
                WebviewUrl::App("prompt.html".into()),
            )
            .title("hushell-prompt")
            .inner_size(360.0, 176.0)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wpr.hwnd().map_err(|e| e.to_string())?;

            let wwg = WebviewWindowBuilder::new(
                app,
                "wingroup",
                WebviewUrl::App("wingroup.html".into()),
            )
            .title("hushell-wingroup")
            .inner_size(340.0, 420.0)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wwg.hwnd().map_err(|e| e.to_string())?;

            let wmenu = WebviewWindowBuilder::new(
                app,
                "menu",
                WebviewUrl::App("menu.html".into()),
            )
            .title("hushell-menu")
            .inner_size(260.0, 170.0)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wmenu.hwnd().map_err(|e| e.to_string())?;

            let wsm = WebviewWindowBuilder::new(
                app,
                "startmenu",
                WebviewUrl::App("startmenu.html".into()),
            )
            .title("hushell-startmenu")
            .inner_size(640.0, 560.0)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wsm.hwnd().map_err(|e| e.to_string())?;

            let wlock = WebviewWindowBuilder::new(
                app,
                "lockscreen",
                WebviewUrl::App("lockscreen.html".into()),
            )
            .title("hushell-lockscreen")
            .inner_size(800.0, 600.0)
            .decorations(false)
            .transparent(false)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;
            wlock.hwnd().map_err(|e| e.to_string())?;

            // Secondary-display bars: one bar window per non-primary
            // monitor, same page as the primary. The native thread makes
            // each an AppBar on ITS monitor and places it at that
            // monitor's bottom — the strip renders on every display,
            // while the start menu stays a main-display affair
            // (place_inner pins it to the primary's bottom-left).
            let sec_created: Vec<(String, isize, (i32, i32, i32, i32))> = {
                let mut v = Vec::new();
                for (i, m) in crate::util::monitors().into_iter().enumerate() {
                    if m.primary {
                        continue;
                    }
                    let label = format!("taskbar-s{}", i);
                    let mon = m.rect();
                    match WebviewWindowBuilder::new(
                        app,
                        label.clone(),
                        WebviewUrl::App("index.html".into()),
                    )
                    .title("hushell-bar")
                    .decorations(false)
                    .transparent(true)
                    .shadow(false)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .resizable(false)
                    .focused(false)
                    .visible(false)
                    // same backdrop family as the primary bar (config
                    // windowEffects: acrylic/mica/blur)
                    .effects(tauri::utils::config::WindowEffectsConfig {
                        effects: vec![
                            tauri::window::Effect::Acrylic,
                            tauri::window::Effect::Mica,
                            tauri::window::Effect::Blur,
                        ],
                        ..Default::default()
                    })
                    .build()
                    {
                        Ok(w) => {
                            if let Ok(h) = w.hwnd() {
                                v.push((label, h.0 as isize, mon));
                            }
                        }
                        Err(e) => util::log(&format!("setup: {label} failed: {e}")),
                    }
                }
                v
            };

            let dnd_app = app.handle().clone();
            dnd::install(&dnd_app);

            {
                let st = app.state::<AppShared>();
                let mut s = crate::util::lock_shared(&st.0);
                s.hwnd_preview = wprev.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_trayfly = wtfly.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_volfly = wvf.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_hushtop = wtop.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_prompt = wpr.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_wingroup = wwg.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_menu = wmenu.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_startmenu = wsm.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.hwnd_lock = wlock.hwnd().map(|h| h.0 as isize).unwrap_or(0);
                s.sec_bars = sec_created
                    .iter()
                    .map(|(_, h, m)| crate::shared::SecBar {
                        hwnd: *h,
                        mon: *m,
                        registered: false,
                        fs_streak: 0,
                        fs_hidden: false,
                    })
                    .collect();
                s.hwnd_bar = app
                    .get_webview_window("taskbar")
                    .and_then(|w| w.hwnd().ok())
                    .map(|h| h.0 as isize)
                    .unwrap_or(0);
                if s.hwnd_bar == 0 {
                    util::log("setup: taskbar window has NO hwnd — bar can never be placed");
                }
                util::log(&format!(
                    "setup: 10 windows + {} secondary bar(s) created, native thread starting",
                    s.sec_bars.len()
                ));
            }

            // Every shell window gets BOTH win32 subclasses: frameless
            // (no caption ever) and no-close (Alt+F4 / system-menu Close /
            // direct WM_CLOSE all die before tao sees them). This covers
            // the bar explicitly — it used to be killable with Alt+F4 —
            // plus the lockscreen, hushtop, every flyout and every
            // secondary bar. The Tauri-level handler above is the outer
            // net; app.exit(0) remains the only way out.
            {
                let st = app.state::<AppShared>();
                let s = crate::util::lock_shared(&st.0);
                let mut guarded: Vec<(String, isize)> = vec![
                    ("taskbar".into(), s.hwnd_bar),
                    ("preview".into(), s.hwnd_preview),
                    ("trayfly".into(), s.hwnd_trayfly),
                    ("volfly".into(), s.hwnd_volfly),
                    ("hushtop".into(), s.hwnd_hushtop),
                    ("prompt".into(), s.hwnd_prompt),
                    ("wingroup".into(), s.hwnd_wingroup),
                    ("menu".into(), s.hwnd_menu),
                    ("startmenu".into(), s.hwnd_startmenu),
                    ("lockscreen".into(), s.hwnd_lock),
                ];
                for (i, b) in s.sec_bars.iter().enumerate() {
                    guarded.push((format!("taskbar-s{}", i + 1), b.hwnd));
                }
                for (name, id) in guarded {
                    if id == 0 {
                        util::log(&format!("frame: {name} NO hwnd — guards skipped"));
                        continue;
                    }
                    let nc = unsafe { frame::guard_no_close(id) };
                    let fr = unsafe { frame::guard_frameless(id) };
                    util::log(&format!("frame: {name} nc-guard={fr} no-close={nc}"));
                }
            }

            let handle = app.handle().clone();
            std::thread::spawn(move || native::native_main(handle));

            winhook::install(app.handle().clone());
            startmenu_killer::start();

            media::start(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running hushell");
}

// ---------------------------------------------------------------------------
// IPC commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn activate(id: isize) {
    native::activate_hwnd(id);
}

#[tauri::command]
fn preview_show(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    ids: Vec<isize>,
    anchor_x: f64,
) -> Result<(), String> {
    // The caller bar decides the anchor space: hover on a secondary bar
    // must pop the card above THAT bar, on that monitor.
    let bar = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    thumbs::show(&app, bar, ids, anchor_x)
}

#[tauri::command]
fn preview_hide(app: tauri::AppHandle) {
    let st = app.state::<AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    thumbs::hide(&app, &mut s);
}

#[tauri::command]
fn preview_hover(app: tauri::AppHandle, enter: bool) {
    thumbs::hover(&app, enter);
}

#[tauri::command]
fn preview_activate(app: tauri::AppHandle, id: isize) {
    thumbs::activate(&app, id);
}

#[tauri::command]
fn preview_overflow(app: tauri::AppHandle) -> Result<(), String> {
    thumbs::wingroup_show(&app)
}

/// A row of the wingroup list was clicked — activate that window.
#[tauri::command]
fn wingroup_activate(app: tauri::AppHandle, id: isize) {
    thumbs::wingroup_activate(&app, id);
}

/// The wingroup sheet closes (Escape / titlebar X / blur).
#[tauri::command]
fn wingroup_hide(app: tauri::AppHandle) {
    thumbs::wingroup_hide(&app);
}

#[tauri::command]
fn wingroup_data(app: tauri::AppHandle) -> Option<shared::WingroupShow> {
    let st = app.state::<AppShared>();
    let data = crate::util::lock_shared(&st.0).wingroup_last.clone();
    data
}

/// Right-click on a taskbar icon: open the custom paper menu (centered on
/// the CALLER bar's monitor) with New window / Close / End task.
#[tauri::command]
fn task_menu(window: tauri::WebviewWindow, app: tauri::AppHandle, ids: Vec<isize>) {
    let bar = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    native::task_menu(&app, bar, ids);
}

#[tauri::command]
fn task_menu_run(app: tauri::AppHandle, action: String, ids: Vec<isize>) {
    native::task_menu_run(&app, action, ids);
}

/// Dismiss the custom menu (Escape / outside press / focus moved / any bar
/// press). No-op when the menu is not open.
#[tauri::command]
fn task_menu_hide(app: tauri::AppHandle) {
    native::task_menu_hide(&app);
}

#[tauri::command]
fn tray_click(app: tauri::AppHandle, key: String, button: String) {
    let drop_state = {
        let st = app.state::<AppShared>();
        let s = crate::util::lock_shared(&st.0);
        let win_visible = s.hwnd_trayfly != 0
            && unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(
                windows::Win32::Foundation::HWND(s.hwnd_trayfly as _),
            ) }
            .as_bool();
        let snap = (
            s.trayfly_visible,
            s.trayfly_click_used,
            win_visible,
            s.trayfly_busy,
            s.trayfly_gen,
            s.tray.len(),
        );
        if !s.trayfly_visible || s.trayfly_click_used {
            Some(snap)
        } else {
            None
        }
    };
    if let Some((vis, used, win_vis, busy, gen, n_icons)) = drop_state {
        util::log(&format!(
            "tray: click dropped ({button} {key}) — state vis={vis} used={used} win_vis={win_vis} busy={busy} gen={gen} icons={n_icons}"
        ));
        return;
    }
    if tray::forward_click_sync(&app, &key, &button) {
        // First (and only) forward for this open consumed. Reset on show.
        let st = app.state::<AppShared>();
        crate::util::lock_shared(&st.0).trayfly_click_used = true;
    }
}

#[tauri::command]
fn trayfly_show(window: tauri::WebviewWindow, app: tauri::AppHandle, anchor_x: f64) {
    let bar = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    thumbs::trayfly_show(&app, bar, anchor_x);
}

#[tauri::command]
fn trayfly_hide(app: tauri::AppHandle) {
    thumbs::trayfly_hide(&app);
}

#[tauri::command]
fn volfly_show(window: tauri::WebviewWindow, app: tauri::AppHandle, anchor_x: f64) {
    let bar = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    thumbs::volfly_show(&app, bar, anchor_x);
}

#[tauri::command]
fn volfly_hide(app: tauri::AppHandle) {
    let st = app.state::<AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    thumbs::volfly_hide_with(&app, &mut s);
}

/// One diagnostic line per trayfly open / JS failure, straight into
/// hushell.log (the native side logs its own state separately).
#[tauri::command]
fn trayfly_diag(msg: String) {
    util::log(&format!("trayfly[ui]: {msg}"));
}

#[tauri::command]
fn trayfly_pong(app: tauri::AppHandle) {
    let st = app.state::<AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    s.trayfly_probe_stage = 0; // any pong disarms the escalation
    if let Some((gen, armed)) = s.trayfly_pong_wait.take() {
        util::log(&format!(
            "trayfly: webview pong (gen {gen}, {}ms after show) — alive",
            armed.elapsed().as_millis()
        ));
    }
}

#[tauri::command]
fn hushtop_move(app: tauri::AppHandle, name: String, x: i32, y: i32) -> Result<(), String> {
    hushtop::cmd_move(&app, name, x, y)
}

#[tauri::command]
fn hushtop_menu(app: tauri::AppHandle, open: bool) {
    hushtop::cmd_menu(&app, open);
}

#[tauri::command]
fn hushtop_open(app: tauri::AppHandle, name: String) -> Result<(), String> {
    hushtop::cmd_open(&app, name)
}

#[tauri::command]
fn hushtop_delete(app: tauri::AppHandle, name: String) -> Result<(), String> {
    unsafe { hushtop::cmd_delete(&app, name) }
}

#[tauri::command]
fn hushtop_rename(app: tauri::AppHandle, old: String, new: String) -> Result<(), String> {
    unsafe { hushtop::cmd_rename(&app, old, new) }
}

#[tauri::command]
fn hushtop_create(
    app: tauri::AppHandle,
    kind: String,
    name: String,
    ext: String,
) -> Result<(), String> {
    unsafe { hushtop::cmd_create(&app, kind, name, ext) }
}

#[tauri::command]
fn hushtop_refresh(app: tauri::AppHandle) {
    let st = app.state::<AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    unsafe { hushtop::rescan(&app, &mut s) };
}

#[tauri::command]
fn hushtop_prompt_show(app: tauri::AppHandle, mode: String, name: String) {
    unsafe { hushtop::prompt_show(&app, &mode, &name) };
}

#[tauri::command]
fn hushtop_prompt_hide(app: tauri::AppHandle) {
    unsafe { hushtop::prompt_hide(&app) };
}

#[derive(serde::Serialize)]
struct HushtopPromptState {
    mode: String,
    name: String,
}

/// Safety net for a missed "hushtop:prompt" event (the dialog webview may
/// still be booting when the first show arrives).
#[tauri::command]
fn hushtop_prompt_state(app: tauri::AppHandle) -> HushtopPromptState {
    let st = app.state::<AppShared>();
    let s = crate::util::lock_shared(&st.0);
    HushtopPromptState {
        mode: s.hushtop_prompt_mode.clone(),
        name: s.hushtop_prompt_name.clone(),
    }
}

#[derive(serde::Serialize)]
struct SyncState {
    tasks: Vec<shared::TaskDto>,
    tray: Vec<tray::TrayDto>,
    hushtop: Vec<shared::HushtopItem>,
    notepads: Vec<config::Notepad>,
    widgets: Vec<config::HushWidget>,
    lang: Option<lang::LangDto>,
}

/// Frontend safety net: pull the current state (in case events were missed
/// while the webview was still booting).
#[tauri::command]
fn sync_state(app: tauri::AppHandle) -> SyncState {
    let st = app.state::<AppShared>();
    let s = crate::util::lock_shared(&st.0);
    SyncState {
        tasks: s.tasks.clone(),
        tray: s.tray_last.clone(),
        hushtop: s.hushtop_items.clone(),
        notepads: s.cfg.notepads.clone(),
        widgets: s.cfg.widgets.clone(),
        lang: lang::snapshot(),
    }
}

// ---------------------------------------------------------------------------
// HUSHTOP WIDGET PERSISTENCE (notepads + desktop widgets).
// ---------------------------------------------------------------------------

#[tauri::command]
fn hushtop_notepad_save(
    app: tauri::AppHandle,
    id: u32,
    col: i32,
    row: i32,
    w: i32,
    h: i32,
    text: String,
) -> Result<(), String> {
    hushtop::notepad_save(&app, id, col, row, w, h, text)
}

#[tauri::command]
fn hushtop_notepad_del(app: tauri::AppHandle, id: u32) -> Result<(), String> {
    hushtop::notepad_del(&app, id)
}

#[tauri::command]
fn hushtop_widget_del(app: tauri::AppHandle, id: u32) -> Result<(), String> {
    hushtop::widget_del_any(&app, id)
}

#[tauri::command]
fn hushtop_widget_save(
    app: tauri::AppHandle,
    id: u32,
    col: i32,
    row: i32,
    w: Option<i32>,
    h: Option<i32>,
) -> Result<(), String> {
    hushtop::widget_save_pos(&app, id, col, row, w, h)
}

#[tauri::command]
fn hushtop_notepad_list(app: tauri::AppHandle) -> Vec<config::Notepad> {
    let st = app.state::<AppShared>();
    let s = crate::util::lock_shared(&st.0);
    s.cfg.notepads.clone()
}

#[tauri::command]
fn audio_master_get() -> Result<f32, String> {
    audio::master_get()
}

#[tauri::command]
fn audio_master_set(vol: f32) -> Result<(), String> {
    audio::master_set(vol)
}

#[tauri::command]
fn audio_master_step(delta: f32) -> Result<f32, String> {
    audio::master_step(delta)
}

#[tauri::command]
fn audio_mute_get() -> Result<bool, String> {
    audio::mute_get()
}

#[tauri::command]
fn audio_mute_set(muted: bool) -> Result<(), String> {
    audio::mute_set(muted)
}

#[tauri::command]
fn audio_state() -> Result<audio::AudioState, String> {
    audio::state()
}

#[tauri::command]
fn clipboard_history(app: tauri::AppHandle) -> Vec<String> {
    clipboard::history(&app)
}

#[tauri::command]
fn clipboard_copy(app: tauri::AppHandle, text: String) -> Result<(), String> {
    clipboard::set_text(&app, &text)
}

#[tauri::command]
fn clipboard_clear(app: tauri::AppHandle) {
    clipboard::clear(&app)
}

#[tauri::command]
fn stats_now() -> stats::Stats {
    stats::last()
}

#[tauri::command]
fn hushtop_dragout(app: tauri::AppHandle, name: String, dx: i32, dy: i32) -> Result<(), String> {
    dnd::drag_out(&app, name, dx, dy)
}

#[tauri::command]
fn startmenu_hide(app: tauri::AppHandle) {
    startmenu::hide(&app);
}

#[tauri::command]
fn startmenu_close_finished(app: tauri::AppHandle) {
    startmenu::close_finished(&app);
}

#[tauri::command]
fn startmenu_ui_loaded() {
    crate::util::log("startmenu: ui loaded (page alive, scripts reachable)");
}

#[tauri::command]
fn startmenu_ui_shown() {
    crate::util::log("startmenu: ui shown (show event handled, sheet animating)");
}

#[tauri::command]
fn startmenu_ui_error(msg: String) {
    crate::util::log(&format!("startmenu: ui error — {msg}"));
}

/// One search turn. The blocking pool keeps the first (index-building) call
/// off the webview's IPC thread.
#[tauri::command]
async fn startmenu_search(q: String) -> Vec<startmenu::Hit> {
    tauri::async_runtime::spawn_blocking(move || startmenu::search(&q))
        .await
        .unwrap_or_default()
}

#[tauri::command]
fn startmenu_open(app: tauri::AppHandle, target: String) -> Result<(), String> {
    startmenu::open_target(&app, &target)
}

#[tauri::command]
async fn startmenu_pinned_view(app: tauri::AppHandle) -> Vec<startmenu::PinDto> {
    let a2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || startmenu::pinned_view(&a2))
        .await
        .unwrap_or_default()
}

#[tauri::command]
fn startmenu_pin(app: tauri::AppHandle, target: String) {
    startmenu::pin(&app, &target);
}

#[tauri::command]
fn startmenu_unpin(app: tauri::AppHandle, target: String) {
    startmenu::unpin(&app, &target);
}

#[tauri::command]
fn startmenu_is_pinned(app: tauri::AppHandle, name: String) -> bool {
    startmenu::is_pinned(&app, &name)
}

#[tauri::command]
fn startmenu_add_widget(app: tauri::AppHandle, kind: String) -> Result<u32, String> {
    startmenu::add_widget(&app, &kind)
}

#[tauri::command]
fn lockscreen_lock(app: tauri::AppHandle) {
    lockscreen::lock(&app);
}

#[tauri::command]
fn lock_hotkey_get(app: tauri::AppHandle) -> bool {
    let st = app.state::<AppShared>();
    let s = crate::util::lock_shared(&st.0);
    s.cfg.lock_hotkey
}

#[tauri::command]
fn lock_hotkey_set(app: tauri::AppHandle, on: bool) {
    {
        let st = app.state::<AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        s.cfg.lock_hotkey = on;
        s.cfg.save();
    }
    native::lock_hotkey_notify(&app);
    crate::util::log(&format!(
        "config: lock hotkey switch -> {on} (re-applied on the host thread)"
    ));
}

#[tauri::command]
fn startup_get(app: tauri::AppHandle) -> bool {
    let st = app.state::<AppShared>();
    let s = crate::util::lock_shared(&st.0);
    s.cfg.startup
}

#[tauri::command]
fn startup_set(app: tauri::AppHandle, on: bool) {
    {
        let st = app.state::<AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        s.cfg.startup = on;
        s.cfg.save();
    }
    // The PowerShell shortcut author takes a few hundred ms — off the
    // invoke thread, nothing here needs to be waited on.
    std::thread::spawn(move || startup::reconcile(on));
    crate::util::log(&format!("config: startup switch -> {on}"));
}

#[tauri::command]
fn lockscreen_unlock(app: tauri::AppHandle, password: String) -> Result<(), String> {
    lockscreen::unlock(&app, &password)
}

#[tauri::command]
fn lock_password_set(app: tauri::AppHandle, password: String) {
    lockscreen::password_set(&app, &password);
}

#[tauri::command]
fn lock_password_get(app: tauri::AppHandle) -> bool {
    lockscreen::password_is_set(&app)
}

/// Shell shutdown from the start menu controls view — the same clean exit
/// path the power menu uses (request_exit → appbar release,
/// native taskbar restore, desktop icons back, app.exit(0)).
#[tauri::command]
fn shell_shutdown(app: tauri::AppHandle) {
    native::request_exit(&app);
}

/// Start fresh — wipe config.json and relaunch from a clean sheet. The
/// relaunch itself is spawned by the native exit handler AFTER the teardown
/// (taskbar back, icons back), so the two shells never overlap. The fresh
/// boot then re-drops the startup shortcut (default on) and rebuilds
/// defaults.
#[tauri::command]
fn shell_fresh_start(app: tauri::AppHandle) {
    if let Some(dir) = config::data_dir() {
        let p = dir.join("config.json");
        match std::fs::remove_file(&p) {
            Ok(()) => util::log("fresh: config.json removed — clean sheet on relaunch"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                util::log("fresh: no config.json to remove")
            }
            Err(e) => util::log(&format!("fresh: config.json remove failed: {e}")),
        }
    }
    {
        let st = app.state::<AppShared>();
        crate::util::lock_shared(&st.0).fresh_relaunch = true;
    }
    native::request_exit(&app);
}

#[tauri::command]
fn media_now() -> media::MediaNow {
    media::media_now()
}

#[tauri::command]
fn media_ctl(action: String) -> Result<bool, String> {
    media::media_ctl(&action)
}

/// Enable SeShutdownPrivilege on the current process token.
fn power_enable_shutdown_privilege() -> Result<(), String> {
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_NOT_ALL_ASSIGNED, HANDLE, LUID,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(|e| format!("OpenProcessToken failed: {e}"))?;

        let mut luid = LUID::default();
        if let Err(e) = LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid) {
            let _ = CloseHandle(token);
            return Err(format!("LookupPrivilegeValueW failed: {e}"));
        }

        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // AdjustTokenPrivileges reports Ok even when the privilege was NOT
        // granted — the truth is in GetLastError == ERROR_NOT_ALL_ASSIGNED.
        let ok = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None).is_ok()
            && GetLastError() != ERROR_NOT_ALL_ASSIGNED;
        let _ = CloseHandle(token);
        if !ok {
            return Err("SeShutdownPrivilege was not granted".into());
        }
        Ok(())
    }
}

#[tauri::command]
fn system_sleep() -> Result<(), String> {
    use windows::Win32::System::Power::SetSuspendState;
    power_enable_shutdown_privilege()?;
    // (hibernate=false, force=false, wakeup-events-disabled=false) — a plain
    // sleep the power button or a schedule can wake from.
    let r = unsafe { SetSuspendState(false, false, false) };
    if !r.as_bool() {
        util::log("power: sleep failed (SetSuspendState returned FALSE)");
        return Err("SetSuspendState failed".into());
    }
    Ok(())
}

#[tauri::command]
fn system_restart() -> Result<(), String> {
    use windows::Win32::System::Shutdown::{ExitWindowsEx, EWX_FORCEIFHUNG, EWX_REBOOT};
    use windows::Win32::System::Shutdown::SHTDN_REASON_FLAG_PLANNED;
    power_enable_shutdown_privilege()?;
    // FORCEIFHUNG only cuts off HUNG apps — a normal save-and-exit for
    // everything alive, no data-loss FORCE anywhere.
    let r = unsafe { ExitWindowsEx(EWX_REBOOT | EWX_FORCEIFHUNG, SHTDN_REASON_FLAG_PLANNED) };
    if let Err(e) = &r {
        util::log(&format!("power: restart failed: {e}"));
    }
    r.map_err(|e| e.to_string())
}

#[tauri::command]
fn system_shutdown() -> Result<(), String> {
    use windows::Win32::System::Shutdown::{ExitWindowsEx, EWX_FORCEIFHUNG, EWX_SHUTDOWN};
    use windows::Win32::System::Shutdown::SHTDN_REASON_FLAG_PLANNED;
    power_enable_shutdown_privilege()?;
    let r = unsafe { ExitWindowsEx(EWX_SHUTDOWN | EWX_FORCEIFHUNG, SHTDN_REASON_FLAG_PLANNED) };
    if let Err(e) = &r {
        util::log(&format!("power: shutdown failed: {e}"));
    }
    r.map_err(|e| e.to_string())
}
