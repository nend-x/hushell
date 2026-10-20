use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LWIN, VK_RWIN};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindow, ShowWindow, SW_RESTORE, SW_SHOWNORMAL,
};

use crate::util::log;

/// Poll cadence. A human Win tap spans >= ~50 ms; a 10 ms poll guarantees
/// several samples inside every tap, and the idle loop is two syscalls per
/// tick — nothing.
const POLL_MS: u64 = 10;
/// Ignore-window after a fired tap (bounce paranoia; imperceptible).
const TAP_DEBOUNCE_MS: u64 = 120;
/// 'D' — WATCHED, never swallowed: native Win+D gets its wallpaper repair.
const VK_D_CODE: u32 = 0x44;

// --- listener state (atomics only; the gesture state itself is thread-
//     local to the listener loop) ---

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static STAT_TAPS: AtomicU64 = AtomicU64::new(0);
static STAT_COMBOS: AtomicU64 = AtomicU64::new(0);
static STAT_WALLPAPER_FIXES: AtomicU64 = AtomicU64::new(0);
static LAST_HEALTH: AtomicU64 = AtomicU64::new(0);

/// Start the read-only listener + its health line. Called from Tauri setup,
/// where the AppHandle exists. The killer monitor must be started separately
/// (startmenu_killer::start()) — it is a policy of its own.
pub fn install(app: tauri::AppHandle) {
    if APP.set(app).is_err() {
        log("winhook: already installed");
        return;
    }
    spawn_wintap_listener();

    std::thread::Builder::new()
        .name("win-tap-health".into())
        .spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let t = STAT_TAPS.load(Ordering::Relaxed);
            let c = STAT_COMBOS.load(Ordering::Relaxed);
            let w = STAT_WALLPAPER_FIXES.load(Ordering::Relaxed);
            let cur = (t << 42) | (c << 21) | w;
            let prev = LAST_HEALTH.swap(cur, Ordering::Relaxed);
            if cur != prev {
                log(&format!(
                    "winhook: health (read-only) — taps={t} combos={c} wallpaper-fixes={w}"
                ));
            }
        })
        .ok();
}

struct TapState {
    /// Win is down (either VK).
    held: bool,
    clean: bool,
    /// 'D' went down while Win was held (native show-desktop chord ran).
    saw_d: bool,
    /// Instant of the last fired tap (debounce).
    last_tap: Option<Instant>,
}

fn spawn_wintap_listener() {
    std::thread::Builder::new()
        .name("win-tap-listener".into())
        .spawn(|| {
            log(&format!(
                "winhook: read-only Win listener up — no hook, no injection, {POLL_MS} ms GetAsyncKeyState tap edges; killer armed on Win-down"
            ));
            let mut st = TapState {
                held: false,
                clean: false,
                saw_d: false,
                last_tap: None,
            };
            loop {
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    listener_tick(&mut st);
                }));
                if r.is_err() {
                    log("winhook: listener tick panicked — continuing");
                    st.held = false;
                }
            }
        })
        .ok();
}

/// One poll tick: the whole gesture machine, readable in one screen.
fn listener_tick(st: &mut TapState) {
    let (lwin, rwin) = unsafe {
        (
            GetAsyncKeyState(VK_LWIN.0 as i32) < 0,
            GetAsyncKeyState(VK_RWIN.0 as i32) < 0,
        )
    };
    let down = lwin || rwin;

    if !st.held {
        if down {
            st.held = true;
            st.saw_d = false;
            // Seed: another key already down when Win lands (Shift held,
            // then Win) is a chord, not a tap.
            st.clean = !unsafe { other_key_down() };
            crate::startmenu_killer::arm();
        }
    } else if !down {
        st.held = false;
        crate::startmenu_killer::arm();
        if st.saw_d {
            wallpaper_repair_after_wind();
        }
        let debounced = st
            .last_tap
            .map(|t| t.elapsed() >= std::time::Duration::from_millis(TAP_DEBOUNCE_MS))
            .unwrap_or(true);
        if st.clean && debounced {
            st.last_tap = Some(Instant::now());
            STAT_TAPS.fetch_add(1, Ordering::Relaxed);
            dispatch_tap();
        } else if !st.clean {
            STAT_COMBOS.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        // HELD — watch, read-only, for combo keys and the D.
        if st.clean && unsafe { other_key_down() } {
            st.clean = false;
        }
        if !st.saw_d && unsafe { GetAsyncKeyState(VK_D_CODE as i32) < 0 } {
            st.saw_d = true;
        }
    }
}

/// True while any non-Win keyboard key is down (async key table, read-only
/// sweep). Mouse buttons are not swept — a Win+click is not a chord. Two Win
/// VKs excluded: they are the gesture itself.
unsafe fn other_key_down() -> bool {
    for vk in 0x08u32..=0xFE {
        if vk == 0x5B || vk == 0x5C {
            continue; // VK_LWIN / VK_RWIN
        }
        if GetAsyncKeyState(vk as i32) < 0 {
            return true;
        }
    }
    false
}

fn dispatch_tap() {
    std::thread::Builder::new()
        .name("win-tap-menu".into())
        .spawn(|| {
            if crate::lockscreen::is_locked() {
                log("winhook: clean tap IGNORED — shell locked");
                return;
            }
            if let Some(app) = APP.get() {
                log("winhook: clean tap (read-only) -> toggling start menu");
                crate::startmenu::toggle(app);
            }
        })
        .ok();
}

fn wallpaper_repair_after_wind() {
    std::thread::Builder::new()
        .name("wind-wallpaper-repair".into())
        .spawn(|| {
            let mut last_ms = 0u64;
            for ms in [450u64, 1200u64] {
                std::thread::sleep(std::time::Duration::from_millis(ms - last_ms));
                last_ms = ms;
                let Some(app) = APP.get() else {
                    return;
                };
                use tauri::Manager;
                let st = app.state::<crate::AppShared>();
                let hm = crate::util::lock_shared(&st.0).hwnd_hushtop;
                if hm == 0 {
                    return;
                }
                unsafe {
                    let h = hwnd(hm);
                    if !IsWindow(h).as_bool() {
                        return;
                    }
                    if IsIconic(h).as_bool() {
                        let _ = ShowWindow(h, SW_RESTORE);
                        STAT_WALLPAPER_FIXES.fetch_add(1, Ordering::Relaxed);
                        log("winhook: native Win+D — hushtop wallpaper re-pinned");
                        return;
                    }
                }
            }
            // Never iconic: native show desktop skipped the wallpaper
            // (tool window) — nothing to repair.
        })
        .ok();
}

fn hwnd(v: isize) -> HWND {
    HWND(v as _)
}

/// Launch helper shared with the start menu search: ShellExecute an
/// arbitrary target (an exe, a .lnk, a folder, an ms-settings: URI).
pub fn shell_open(target: &str, args: &str) -> Result<(), String> {
    let wide = windows::core::HSTRING::from(target);
    let wargs = windows::core::HSTRING::from(args);
    unsafe {
        let r = ShellExecuteW(None, w!("open"), &wide, &wargs, PCWSTR::null(), SW_SHOWNORMAL);
        if r.0 as isize <= 32 {
            return Err(format!(
                "ShellExecute failed for '{target}' (code {})",
                r.0 as isize
            ));
        }
    }
    Ok(())
}
