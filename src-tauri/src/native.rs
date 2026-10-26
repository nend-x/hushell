//! Native thread: AppBar registration, original-taskbar hiding (sinjs/HideTaskbar
//! logic), the message host window (heartbeat timer owner), the task
//! list refresh and the Win32 message pump that drives the unified cycle.
//! Tray icons are enumerated by tray.rs directly from explorer's toolbars.

use tauri::{AppHandle, Emitter, Manager};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, SendInput, SetActiveWindow, SetFocus, UnregisterHotKey,
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MOD_ALT, VK_LBUTTON, VK_MENU,
    VK_RBUTTON,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows,
    FindWindowExW, GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowTextW,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SwitchToThisWindow, GWL_EXSTYLE, GWL_STYLE, HMENU, HWND_TOPMOST, LWA_ALPHA, MSG,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, WM_HOTKEY,
    WS_CAPTION, WS_DLGFRAME, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    WS_SYSMENU, WS_THICKFRAME,
};

use std::collections::HashMap;

pub(crate) const NC_FRAME_STYLES: isize = WS_THICKFRAME.0 as isize
    | WS_CAPTION.0 as isize
    | WS_DLGFRAME.0 as isize
    | WS_SYSMENU.0 as isize;

/// DWMWA_WINDOW_CORNER_PREFERENCE (Win 11): opt the bar out of the automatic
/// rounded corners, which notch away the bottom corners of a full-bleed bar.
const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWCP_DONOTROUND: u32 = 1;

unsafe fn strip_nc_frame(h: HWND) {
    let style = GetWindowLongPtrW(h, GWL_STYLE);
    let cleaned = style & !NC_FRAME_STYLES;
    if cleaned != style {
        SetWindowLongPtrW(h, GWL_STYLE, cleaned);
    }
    let pref: u32 = DWMWCP_DONOTROUND;
    let _ = DwmSetWindowAttribute(
        h,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &pref as *const u32 as *const std::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
    );
    let _ = SetWindowPos(
        h,
        HWND::default(),
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED
            | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
            | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
            | SWP_NOZORDER
            | SWP_NOACTIVATE,
    );
}

use crate::cycle::{self, Cycle};
use crate::shared::{Rc, Shared, TaskDto, WinDto};
use crate::{icons, lang, tasks, thumbs, util};
pub const WM_APP_EXIT: u32 = 0x8000 + 2; // WM_APP + 2
pub const WM_APP_LOCKHOTKEY: u32 = 0x8000 + 3; // WM_APP + 3
/// The RegisterHotKey id for Alt+L. Arbitrary local id; the OS posts
/// WM_HOTKEY with wparam = this id to the thread that registered.
const HOTKEY_LOCK_ID: i32 = 0x4C01; // 'L' | 0x01 (alt)
/// 'L' virtual-key code.
const VK_L: u32 = 0x4C;
const TIMER_TICK: usize = 1;

thread_local! {
    /// The unified heartbeat, built once on the native thread in native_main.
    static CYCLE: std::cell::RefCell<Cycle> = std::cell::RefCell::new(Cycle::new());
}

static APP_HANDLE: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

fn hwnd(v: isize) -> HWND {
    HWND(v as _)
}

// ---------------------------------------------------------------------------
// AppBar
// ---------------------------------------------------------------------------

unsafe fn desired_bar_rect_at(mon: (i32, i32, i32, i32), dpi_h: HWND) -> Rc {
    let (mx, my, mw, mh) = mon;
    let bh = util::bar_h_phys(dpi_h);
    Rc {
        l: mx,
        t: my + mh - bh,
        r: mx + mw,
        b: my + mh,
    }
}

unsafe fn desired_bar_rect(hbar: HWND) -> Rc {
    desired_bar_rect_at(util::monitor_rect(hbar), hbar)
}

/// The bar WINDOW's rect: the AppBar reservation plus BAR_LIFT — same
/// bottom edge, top edge lifted so the painted bar reaches the top of the
/// reserved strip. The AppBar math (and the workarea) never sees the lift.
unsafe fn bar_window_rect_at(mon: (i32, i32, i32, i32), dpi_h: HWND) -> Rc {
    let mut rc = desired_bar_rect_at(mon, dpi_h);
    let lift = (crate::shared::BAR_LIFT as u32 * util::dpi_for(dpi_h) / 96) as i32;
    rc.t -= lift;
    rc
}

unsafe fn bar_window_rect(hbar: HWND) -> Rc {
    bar_window_rect_at(util::monitor_rect(hbar), hbar)
}

unsafe fn appbar_register_at(hbar: HWND, mon: (i32, i32, i32, i32)) -> Option<Rc> {
    let want = desired_bar_rect_at(mon, hbar);
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hbar,
        uCallbackMessage: 0x8000 + 1, // WM_APP + 1
        uEdge: ABE_BOTTOM,
        ..Default::default()
    };
    let r = SHAppBarMessage(ABM_NEW, &mut abd);
    if r == 0 {
        util::log("appbar: ABM_NEW failed");
        return None;
    }
    abd.rc = RECT {
        left: want.l,
        top: want.t,
        right: want.r,
        bottom: want.b,
    };
    SHAppBarMessage(ABM_QUERYPOS, &mut abd);
    SHAppBarMessage(ABM_SETPOS, &mut abd);
    util::log("appbar: registered (bottom strip)");
    Some(want)
}

unsafe fn appbar_register(hbar: HWND) -> Option<Rc> {
    appbar_register_at(hbar, util::monitor_rect(hbar))
}

unsafe fn reassert_bar(s: &mut Shared) {
    if !s.appbar_registered {
        return;
    }
    // --- primary bar (unchanged logic, just no early return: the
    //     secondary bars below must re-assert even when the primary is
    //     fullscreen-hidden) ---
    if s.hwnd_bar != 0 && !s.bar_fs_hidden {
        reassert_one(hwnd(s.hwnd_bar), None);
    }
    // --- secondary bars: same strip on their own monitor ---
    for sb in s.sec_bars.iter() {
        if sb.hwnd == 0 || !sb.registered || sb.fs_hidden {
            continue;
        }
        if !IsWindow(hwnd(sb.hwnd)).as_bool() {
            continue;
        }
        reassert_one(hwnd(sb.hwnd), Some(sb.mon));
    }
}

/// Re-assert one bar's AppBar + window rect. `mon` is Some for a secondary
/// bar (the window's own monitor is wrong until placed — born on primary);
/// None uses MonitorFromWindow (the primary bar lives where it lives).
unsafe fn reassert_one(hbar: HWND, mon: Option<(i32, i32, i32, i32)>) {
    let want = match mon {
        Some(m) => desired_bar_rect_at(m, hbar),
        None => desired_bar_rect(hbar),
    };
    let wr = match mon {
        Some(m) => bar_window_rect_at(m, hbar),
        None => bar_window_rect(hbar),
    };
    let cur = {
        let mut rc = RECT::default();
        if GetWindowRect(hbar, &mut rc).is_ok() {
            Rc {
                l: rc.left,
                t: rc.top,
                r: rc.right,
                b: rc.bottom,
            }
        } else {
            return;
        }
    };
    if cur.l != wr.l || cur.t != wr.t || cur.r != wr.r || cur.b != wr.b {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: hbar,
            uEdge: ABE_BOTTOM,
            ..Default::default()
        };
        abd.rc = RECT {
            left: want.l,
            top: want.t,
            right: want.r,
            bottom: want.b,
        };
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        SHAppBarMessage(ABM_SETPOS, &mut abd);
        let _ = SetWindowPos(
            hbar,
            HWND_TOPMOST,
            wr.l,
            wr.t,
            wr.w(),
            wr.h(),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        if mon.is_some() {
            util::log("appbar: re-asserted secondary strip");
        } else {
            util::log("appbar: re-asserted bottom strip");
        }
    }
}

unsafe fn appbar_unregister(s: &mut Shared) {
    // Secondary bars first: each registered its own AppBar.
    for sb in s.sec_bars.iter() {
        if sb.hwnd != 0 && sb.registered && IsWindow(hwnd(sb.hwnd)).as_bool() {
            let mut abd = APPBARDATA {
                cbSize: std::mem::size_of::<APPBARDATA>() as u32,
                hWnd: hwnd(sb.hwnd),
                ..Default::default()
            };
            SHAppBarMessage(ABM_REMOVE, &mut abd);
            util::log("appbar: removed secondary strip");
        }
    }
    if s.appbar_registered {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: hwnd(s.hwnd_bar),
            ..Default::default()
        };
        SHAppBarMessage(ABM_REMOVE, &mut abd);
        s.appbar_registered = false;
        util::log("appbar: removed");
    }
}

// ---------------------------------------------------------------------------
// Bar slide choreography (the lock): the bars no longer pop out of and back
// into the lock — each one SLIDES off the bottom edge of its own monitor and
// slides back in when the shell returns. The HWND animates; the page inside
// just rides along. The AppBar reservation itself never moves (the work-area
// strip stays registered the whole time), so icons never reflow for a visual
// effect.
//
// PARK POLICY — never hide. Hiding an acrylic bar tears the DWM material
// down; showing it again re-attaches the backdrop and the first frames
// flash (the unlock "blink"). So after sliding off its monitor a bar is
// parked JUST BELOW THE VIRTUAL SCREEN — outside every monitor's area,
// invisible from anywhere — while it stays SHOWN the whole lock. No hide,
// no show on the way back, no material churn: the bar simply glides home.
// The docked rects are stashed here so the unlock slide knows where home
// is even though GetWindowRect now reads the parked position.
// ---------------------------------------------------------------------------

/// The docked rect of every bar that slid away, stashed at slide-down time
/// (the bars sit at their docked rects right before the slide starts).
static BAR_DOCKED: std::sync::OnceLock<std::sync::Mutex<HashMap<isize, RECT>>> =
    std::sync::OnceLock::new();

fn bar_docked_slot() -> &'static std::sync::Mutex<HashMap<isize, RECT>> {
    BAR_DOCKED.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// The y where a bar is invisible from EVERY monitor: just below the bottom
/// edge of the whole virtual screen (the union of all monitors).
unsafe fn vs_park_y() -> i32 {
    GetSystemMetrics(SM_YVIRTUALSCREEN)
        + GetSystemMetrics(SM_CYVIRTUALSCREEN)
        + 2
}

/// The lock's mid-lock re-park: when a bar has surfaced back onto a real
/// monitor (a stray re-assert, a display change re-flow), move it back below
/// the virtual screen — still SHOWN, per the never-hide park policy. A bar
/// already parked intersects no monitor and stays untouched. Returns true
/// when a park actually happened (for the caller's logging).
pub unsafe fn park_bar_if_on_screen(h: HWND) -> bool {
    if !IsWindow(h).as_bool() || !IsWindowVisible(h).as_bool() {
        return false;
    }
    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    let mut rc = RECT::default();
    if GetWindowRect(h, &mut rc).is_err() {
        return false;
    }
    // Intersects the virtual screen? (A parked bar starts below its bottom
    // edge; a bar fully above/left of it would be a display-gone case —
    // also left alone.)
    let on_screen = rc.left < vx + vw
        && rc.right > vx
        && rc.top < vy + vh
        && rc.bottom > vy;
    if !on_screen {
        return false;
    }
    let _ = SetWindowPos(
        h,
        HWND::default(),
        rc.left,
        vs_park_y(),
        rc.right - rc.left,
        rc.bottom - rc.top,
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    true
}

/// One slide at a time: a lock landing mid-unlock-slide must not fight the
/// unlock slide for the same HWNDs (both threads would write y every 16 ms).
/// try_lock semantics — a second slide waits its turn (<= ~0.5 s), it never
/// interleaves.
static BAR_SLIDE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// TRUE while a bar slide owns the bars. The heartbeat's bar-geometry beat
/// (reassert_bar + ensure_topmost_bar) reads this and stands down — without
/// the gate it re-asserted the bars onto their docked rects ~100 ms into
/// the 460 ms unlock slide, snapping the glide home mid-air and then
/// fighting the slide thread for the same windows every 100 ms. THE unlock
/// taskbar blink, alongside the acrylic teardown the no-hide park removes.
static BAR_SLIDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn bars_sliding() -> bool {
    BAR_SLIDING.load(std::sync::atomic::Ordering::SeqCst)
}

fn set_bars_sliding(v: bool) {
    BAR_SLIDING.store(v, std::sync::atomic::Ordering::SeqCst);
}

/// TRUE from lock() until bars_slide_up() actually begins. The gate
/// window that used to blink: LOCKED clears at the FIRST breath of the
/// unlock, but the bars' slide home only starts at +3150 ms of the
/// comeback — in between, the heartbeat's bar-geometry beat saw
/// "not locked, not sliding" and REASSERTED the parked bars onto their
/// docked rects with SWP_SHOWWINDOW, popping the taskbar onto the screen
/// instantly at ~100 ms ("the taskbar appears when the unlock effect
/// starts, without animation"), after which the slide had nowhere to
/// glide (it started at home). While held, the bars stay parked below
/// the virtual screen through the whole choreography.
static BAR_PARK_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn bars_park_held() -> bool {
    BAR_PARK_HELD.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn set_bars_park_held(v: bool) {
    BAR_PARK_HELD.store(v, std::sync::atomic::Ordering::SeqCst);
}

/// Set by lock() right before the overlay comes up: the ONE main_assert pass
/// that runs in the same breath must not park the bars out from under the
/// slide-down it is supposed to accompany (the beat's later passes park
/// instantly — a bar reappearing mid-lock never waits for an animation).
static BAR_SLIDE_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn arm_initial_bar_slide() {
    BAR_SLIDE_ARMED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// True when the caller should SKIP parking the bars this pass (the initial
/// lock pass while the slide-down owns them). Consumes the arm flag.
pub fn consume_initial_bar_slide() -> bool {
    BAR_SLIDE_ARMED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// One bar's animation plan: docked geometry (the truth) + the y where the
/// window sits fully below its monitor's bottom edge.
struct BarSlide {
    hwnd: isize,
    x: i32,
    y_docked: i32,
    w: i32,
    h: i32,
    y_off: i32,
}

unsafe fn current_top(b: &BarSlide) -> i32 {
    let mut rc = RECT::default();
    if GetWindowRect(hwnd(b.hwnd), &mut rc).is_ok() {
        rc.top
    } else {
        b.y_off
    }
}

/// Snapshot the bar windows: docked rect from GetWindowRect (it IS the docked
/// rect whenever the window is not mid-slide — see the module comment), the
/// off-screen y from the bar's own monitor (sec bars use the captured `mon`
/// — MonitorFromWindow lies for a window born on the primary).
fn collect_bars(app: &AppHandle) -> Vec<BarSlide> {
    let st = app.state::<crate::AppShared>();
    let ids: Vec<(isize, Option<(i32, i32, i32, i32)>)> = {
        let s = crate::util::lock_shared(&st.0);
        let mut v = vec![(s.hwnd_bar, None)];
        v.extend(s.sec_bars.iter().map(|b| (b.hwnd, Some(b.mon))));
        v
    };
    let mut out = Vec::new();
    for (id, mon) in ids {
        if id == 0 {
            continue;
        }
        let h = hwnd(id);
        unsafe {
            if !IsWindow(h).as_bool() {
                continue;
            }
            let mut rc = RECT::default();
            if GetWindowRect(h, &mut rc).is_err() {
                continue;
            }
            let (_mx, my, _mw, mh) = match mon {
                Some(m) => m,
                None => util::monitor_rect(h),
            };
            if mh <= 0 {
                continue;
            }
            out.push(BarSlide {
                hwnd: id,
                x: rc.left,
                y_docked: rc.top,
                w: (rc.right - rc.left).max(1),
                h: (rc.bottom - rc.top).max(1),
                y_off: my + mh + 2, // fully below the monitor's bottom edge
            });
        }
    }
    out
}

/// easeInOutCubic — the same S-curve the shell's sweep line uses.
fn ease_slide(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0f64).powi(3) / 2.0
    }
}

const SLIDE_TICK_MS: u64 = 16;

/// RAII for the BAR_SLIDING gate: the slide threads set it true on entry
/// and this guard clears it on every exit path (early return, panic). The
/// heartbeat reads it to keep its bar re-assert off the animator's windows.
struct SlideDone;
impl Drop for SlideDone {
    fn drop(&mut self) {
        set_bars_sliding(false);
    }
}

/// Interpolate every bar's top from starts[i] to ends[i] on one shared
/// S-curve. The curve is sampled from the CAPTURED endpoints only — never
/// from the live rect — so the easing shape cannot drift mid-flight.
/// The z-order is NEVER touched here (SWP_NOZORDER): a slide-down must stay
/// UNDER the lock overlay that is coming up, and a slide-up must not churn
/// the topmost band sixteen times a second (the bars are born TOPMOST and
/// keep that band membership without any help).
unsafe fn animate_bars_ys(bars: &[BarSlide], starts: &[i32], ends: &[i32], dur_ms: u64) {
    let steps = (dur_ms / SLIDE_TICK_MS).max(1);
    for i in 1..=steps {
        let p = ease_slide(i as f64 / steps as f64);
        for (n, b) in bars.iter().enumerate() {
            let y0 = starts.get(n).copied().unwrap_or(b.y_docked) as f64;
            let y1 = ends.get(n).copied().unwrap_or(b.y_docked) as f64;
            let _ = SetWindowPos(
                hwnd(b.hwnd),
                HWND::default(),
                b.x,
                (y0 + (y1 - y0) * p) as i32,
                b.w,
                b.h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOZORDER,
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(SLIDE_TICK_MS));
    }
}

/// Lock side: slide every VISIBLE bar below its monitor's bottom edge, then
/// park it just below the VIRTUAL SCREEN — still SHOWN, never hidden (the
/// acrylic material survives the whole lock untouched, so the unlock slide
/// cannot blink). Hidden bars are skipped (already parked — a lock over a
/// fullscreen-hidden bar is a no-op). Runs on its own thread; takes the
/// slide turnstile.
pub fn bars_slide_down(app: &AppHandle) {
    let a = app.clone();
    std::thread::Builder::new()
        .name("bar-slide".into())
        .spawn(move || {
            crate::util::register_thread("bar-slide");
            let _guard = BAR_SLIDE.lock().unwrap_or_else(|e| e.into_inner());
            set_bars_sliding(true);
            let _slide_done = SlideDone;
            let bars: Vec<BarSlide> = {
                let all = collect_bars(&a);
                all.into_iter()
                    .filter(|b| unsafe { IsWindowVisible(hwnd(b.hwnd)).as_bool() })
                    .collect()
            };
            if bars.is_empty() {
                return;
            }
            // Stash the docked rects BEFORE anything moves — this is the
            // ground truth the unlock slide glides home to (GetWindowRect
            // will read the parked position for the whole lock).
            {
                let mut slot = bar_docked_slot().lock().unwrap_or_else(|e| e.into_inner());
                slot.clear();
                unsafe {
                    for b in &bars {
                        let mut rc = RECT::default();
                        if GetWindowRect(hwnd(b.hwnd), &mut rc).is_ok() {
                            slot.insert(b.hwnd, rc);
                        }
                    }
                }
            }
            unsafe {
                let starts: Vec<i32> = bars.iter().map(|b| current_top(b)).collect();
                let ends: Vec<i32> = bars.iter().map(|b| b.y_off).collect();
                animate_bars_ys(&bars, &starts, &ends, 340);
                // Off every monitor for good: park below the VIRTUAL SCREEN,
                // still shown — no SW_HIDE, no acrylic teardown, no unlock
                // blink. x/w/h stay untouched; only y moves.
                let py = vs_park_y();
                for b in &bars {
                    let _ = SetWindowPos(
                        hwnd(b.hwnd),
                        HWND::default(),
                        b.x,
                        py,
                        b.w,
                        b.h,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
            }
            util::log("lockscreen: bars slid away for the lock (parked below the virtual screen, still shown)");
        })
        .ok();
}

/// Unlock side (+3150 ms, inside the comeback choreography): each bar rises
/// from its park below the virtual screen to its docked rect — the SAME
/// surface all the way (it was never hidden), so the acrylic backdrop and
/// the webview pixels survive intact and there is nothing left to flash.
/// A bar hidden by something else (fs-watcher) is shown at the park
/// position first, then joins the same glide.
pub fn bars_slide_up(app: &AppHandle) {
    // The park-hold ends HERE — before anything can early-return — so the
    // heartbeat's bar-geometry beat resumes the moment the slide owns the
    // bars (BAR_SLIDING covers the animation itself). Cleared even when no
    // bar exists: a latch left set would pin the beat forever.
    crate::native::set_bars_park_held(false);
    let a = app.clone();
    std::thread::Builder::new()
        .name("bar-slide".into())
        .spawn(move || {
            crate::util::register_thread("bar-slide");
            let _guard = BAR_SLIDE.lock().unwrap_or_else(|e| e.into_inner());
            set_bars_sliding(true);
            let _slide_done = SlideDone;
            let bars = collect_bars(&a);
            if bars.is_empty() {
                return;
            }
            unsafe {
                // A bar hidden by another system (fs-watcher hid it while a
                // fullscreen app ran) still gets the full choreography: park
                // position first, show there, then glide home with the rest.
                let py = vs_park_y();
                for b in bars.iter() {
                    if !IsWindowVisible(hwnd(b.hwnd)).as_bool() {
                        let _ = SetWindowPos(
                            hwnd(b.hwnd),
                            HWND::default(),
                            b.x,
                            py,
                            b.w,
                            b.h,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                        let _ = ShowWindow(hwnd(b.hwnd), SW_SHOW);
                    }
                }
                let starts: Vec<i32> = bars.iter().map(|b| current_top(b)).collect();
                let ends: Vec<i32> = bars
                    .iter()
                    .map(|b| {
                        // The stashed docked rect is the truth for bars that
                        // slid down (their live rect is the parked one). Bars
                        // that never slid (fs-hidden at lock time) kept
                        // their docked rect — the live top IS home.
                        let slot = bar_docked_slot().lock().unwrap_or_else(|e| e.into_inner());
                        match slot.get(&b.hwnd) {
                            Some(rc) => rc.top,
                            None => b.y_docked,
                        }
                    })
                    .collect();
                animate_bars_ys(&bars, &starts, &ends, 460);
                // Land exactly on the docked rect (the stash wins when it
                // exists — the live rect may still be one tick short).
                let mut slot = bar_docked_slot().lock().unwrap_or_else(|e| e.into_inner());
                for b in bars.iter() {
                    let (dx, dy) = match slot.get(&b.hwnd) {
                        Some(rc) => (rc.left, rc.top),
                        None => (b.x, b.y_docked),
                    };
                    let _ = SetWindowPos(
                        hwnd(b.hwnd),
                        HWND::default(),
                        dx,
                        dy,
                        b.w,
                        b.h,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                slot.clear();
            }
            util::log("lockscreen: bars slid back in");
        })
        .ok();
}

// ---------------------------------------------------------------------------
// Original taskbar hiding (adapted from sinjs/HideTaskbar, MIT)
// ---------------------------------------------------------------------------

fn find_primary_tray() -> Option<HWND> {
    crate::tray::find_real_shell_tray().map(hwnd)
}

fn find_secondary_trays() -> Vec<HWND> {
    let mut v = Vec::new();
    unsafe {
        let mut prev = HWND::default();
        loop {
            match FindWindowExW(HWND::default(), prev, w!("Shell_SecondaryTrayWnd"), PCWSTR::null())
            {
                Ok(h) if !h.is_invalid() => {
                    v.push(h);
                    prev = h;
                }
                _ => break,
            }
        }
    }
    v
}

unsafe fn set_window_alpha(h: HWND, alpha: u8) {
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
    if ex & WS_EX_LAYERED.0 as isize == 0 {
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
    }
    let _ = SetLayeredWindowAttributes(h, windows::Win32::Foundation::COLORREF(0), alpha, LWA_ALPHA);
    let _ = RedrawWindow(
        h,
        None,
        windows::Win32::Graphics::Gdi::HRGN::default(),
        RDW_ERASE | RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN,
    );
}

unsafe fn tick_hide_native(s: &mut Shared, beats: u64) {
    let hide = s.cfg.hide_native_taskbar;
    if !hide {
        if s.hidden_applied {
            restore_native(s);
        }
        return;
    }
    let _ = beats;

    if !s.hidden_applied {
        if let Some(tray) = find_primary_tray() {
            let mut rc = RECT::default();
            if GetWindowRect(tray, &mut rc).is_ok() {
                s.native_orig = Some(Rc {
                    l: rc.left,
                    t: rc.top,
                    r: rc.right,
                    b: rc.bottom,
                });
            }
            hide_one(tray);
        }
        for h in find_secondary_trays() {
            hide_one_tracked(h, &mut *s);
        }
        s.hidden_applied = true;
        util::log("hide: native taskbar hidden (one-shot)");
        return;
    }

    if let Some(tray) = find_primary_tray() {
        if is_taskbar_showing(tray) {
            util::log("hide: native taskbar re-appeared, re-hiding");
            hide_one(tray);
        }
    }
    for h in find_secondary_trays() {
        if is_taskbar_showing(h) {
            hide_one_tracked(h, s);
        }
    }
}

unsafe fn hide_one(h: HWND) {
    set_window_alpha(h, 0);
    let _ = SetWindowPos(
        h,
        HWND::default(),
        0,
        util::screen_height() + 400,
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_HIDEWINDOW,
    );
    let mut abd = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: h,
        ..Default::default()
    };
    let r = SHAppBarMessage(ABM_REMOVE, &mut abd);
    if r != 0 {
        S_OGBAR_RELEASED.store(true, std::sync::atomic::Ordering::Relaxed);
        util::log("appbar: released the hidden bar's reservation");
    }
}

/// Park a SECONDARY native tray, remembering where it lived so the restore
/// can put it back. The primary tray's rect is tracked separately
/// (Shared.native_orig); secondaries used to come back from the park
/// stranded off-screen below the screen (alpha restored, position never).
unsafe fn hide_one_tracked(h: HWND, s: &mut Shared) {
    let id = h.0 as isize;
    if !s.native_orig_sec.iter().any(|(k, _)| *k == id) {
        let mut rc = RECT::default();
        if GetWindowRect(h, &mut rc).is_ok() && rc.top < util::screen_height() {
            s.native_orig_sec.push((
                id,
                Rc {
                    l: rc.left,
                    t: rc.top,
                    r: rc.right,
                    b: rc.bottom,
                },
            ));
        }
    }
    hide_one(h);
}

/// ABM_REMOVE result handoff to Shared (hide_one has no &mut Shared).
static S_OGBAR_RELEASED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Slow-beat divider for the wingroup sheet's row prune (~500 ms cadence).
static WGROUP_PRUNE_BEATS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The taskbar is on-screen and visible (worth hiding again).
fn is_taskbar_showing(h: HWND) -> bool {
    unsafe { IsWindowVisible(h).as_bool() && !is_offscreen(h) }
}

fn is_offscreen(h: HWND) -> bool {
    let mut rc = RECT::default();
    unsafe { GetWindowRect(h, &mut rc).is_ok() && rc.top >= util::screen_height() }
}

/// Restore native taskbars (alpha 255, position back, shown). Secondary
/// trays return to their tracked pre-park rects — not to wherever the park
/// left them.
unsafe fn restore_native(s: &mut Shared) {
    if let Some(tray) = find_primary_tray() {
        set_window_alpha(tray, 255);
        let rc = s.native_orig.unwrap_or(Rc {
            l: 0,
            t: util::screen_height() - 48,
            r: util::screen_width(),
            b: util::screen_height(),
        });
        let _ = SetWindowPos(
            tray,
            HWND::default(),
            rc.l,
            rc.t,
            rc.w(),
            rc.h(),
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(tray, SW_SHOW);
    }
    for (id, rc) in &s.native_orig_sec {
        let h = hwnd(*id);
        if !IsWindow(h).as_bool() {
            continue;
        }
        set_window_alpha(h, 255);
        let _ = SetWindowPos(
            h,
            HWND::default(),
            rc.l,
            rc.t,
            rc.w(),
            rc.h(),
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(h, SW_SHOW);
    }
    s.native_orig_sec.retain(|(id, _)| IsWindow(hwnd(*id)).as_bool());
    s.hidden_applied = false;
}

// ---------------------------------------------------------------------------
// Tray: enumerated in tray.rs (direct toolbar reads — no injection). The host
// window below is only the heartbeat timer owner + exit receiver; it has no
// on-screen role and no message-stream duties anymore.
// ---------------------------------------------------------------------------

unsafe fn ensure_topmost_bar(s: &Shared) {
    let mut ids: Vec<isize> = Vec::new();
    if s.hwnd_bar != 0 {
        ids.push(s.hwnd_bar);
    }
    ids.extend(s.sec_bars.iter().map(|b| b.hwnd));
    for id in ids {
        if id != 0 && IsWindow(hwnd(id)).as_bool() {
            let _ = SetWindowPos(
                hwnd(id),
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                    | SWP_NOACTIVATE,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Task list refresh + activation
// ---------------------------------------------------------------------------

unsafe fn refresh_tasks(app: &AppHandle, s: &mut Shared, beats: u64) {
    let mut ours = vec![
        s.hwnd_bar,
        s.hwnd_preview,
        s.hwnd_trayfly,
        s.hwnd_host,
        s.hwnd_menu,
        s.hwnd_startmenu,
        s.hwnd_lock,
    ];
    ours.extend(s.sec_bars.iter().map(|b| b.hwnd));
    ours.retain(|id| *id != 0);
    let light = tasks::enumerate_light(&ours);
    let fg = GetForegroundWindow().0 as isize;

    let n_windows: usize = s.tasks.iter().map(|t| t.wins.len()).sum();
    let set_same = light.len() == n_windows
        && light.iter().all(|(h, m)| {
            s.tasks.iter().any(|t| {
                t.wins
                    .iter()
                    .any(|w| w.id == *h && w.minimized == *m)
            })
        });
    if set_same
        && fg == s.last_fg
        && !s.render_dirty
        && beats % cycle::TITLE_RECHECK_BEATS != 0
    {
        return;
    }
    s.last_fg = fg;

    // Stable order: existing ids keep their slots, new windows append in
    // discovery order, dead ids are dropped.
    for (h, _) in &light {
        if !s.task_order.contains(h) {
            s.task_order.push(*h);
        }
    }
    s.task_order.retain(|h| light.iter().any(|(l, _)| l == h));

    let mut groups: Vec<(u32, Vec<isize>)> = Vec::new();
    for key in &s.task_order {
        if !light.iter().any(|(h, _)| h == key) {
            continue;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd(*key), Some(&mut pid));
        if pid == 0 {
            continue;
        }
        match groups.iter_mut().find(|(p, _)| *p == pid) {
            Some((_, ws)) => ws.push(*key),
            None => groups.push((pid, vec![*key])),
        }
    }

    let mut list: Vec<TaskDto> = Vec::with_capacity(groups.len());
    for (pid, wins) in &groups {
        // Icon: per REPRESENTATIVE window (first of the group) — the cache
        // stays keyed per hwnd, so any group reshuffle reuses extraction.
        let rep = wins[0];
        if !s.icons.contains_key(&rep) {
            let triple = icons::build_icon_triple(hwnd(rep));
            s.icons.insert(rep, triple);
        }
        let (normal, ink, _chalk) = s.icons.get(&rep).cloned().unwrap_or((None, None, None));
        let mut dtos: Vec<WinDto> = Vec::with_capacity(wins.len());
        for key in wins {
            let Some((_, minimized)) = light.iter().find(|(h, _)| h == key) else {
                continue;
            };
            let title = tasks::window_title(hwnd(*key));
            if title.trim().is_empty() {
                continue;
            }
            dtos.push(WinDto {
                id: *key,
                title,
                minimized: *minimized,
                active: fg == *key,
            });
        }
        if dtos.is_empty() {
            continue;
        }
        // Representative title: the first group window that still has one
        // (the rep may legitimately carry an empty title after a swap).
        let rep_dto = dtos
            .iter()
            .find(|w| w.id == rep)
            .cloned()
            .unwrap_or_else(|| dtos[0].clone());
        list.push(TaskDto {
            pid: *pid,
            id: rep_dto.id,
            title: rep_dto.title,
            icon: if !s.cfg.recolor_icons {
                normal
            } else {
                ink
            },
            minimized: dtos.iter().all(|w| w.minimized),
            active: dtos.iter().any(|w| w.active),
            wins: dtos,
        });
    }

    if list != s.tasks || s.render_dirty {
        s.render_dirty = false;
        s.tasks = list.clone();
        let _ = app.emit("tasks", &list);
    }
    s.icons.retain(|k, _| s.task_order.contains(k));
}

pub fn activate_hwnd(id: isize) -> bool {
    unsafe {
        let h = hwnd(id);
        if !IsWindow(h).as_bool() {
            return false;
        }
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, windows::Win32::UI::WindowsAndMessaging::SW_RESTORE);
        }
        let _ = ShowWindow(h, SW_SHOW);
        if GetForegroundWindow() == h {
            return true;
        }

        // Rung 1: the classic attach trick.
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let this_thread = GetCurrentThreadId();
        let mut attached = false;
        if fg_thread != 0 && fg_thread != this_thread {
            attached = AttachThreadInput(this_thread, fg_thread, true).as_bool();
        }
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        let _ = SetFocus(h);
        let _ = SetActiveWindow(h);
        if attached {
            let _ = AttachThreadInput(this_thread, fg_thread, false);
        }
        if GetForegroundWindow() == h {
            return true;
        }

        // Rung 1.5: the synthetic-input foreground grant. With NO foreground
        // at all (fg == 0 — the state right after logon, exactly where the
        // attach trick has nothing to join) or with one of our own windows
        // up front, SetForegroundWindow is denied for lack of input rights.
        // A synthetic Alt tap makes THIS thread the last input owner — the
        // right the call demands (the PowerToys dance). Restricted to those
        // two cases so a foreign app never sees a stray Alt keypress.
        let fg2 = GetForegroundWindow();
        let grantable = fg2.is_invalid()
            || {
                let mut fgp = 0u32;
                GetWindowThreadProcessId(fg2, Some(&mut fgp));
                fgp == GetCurrentProcessId()
            };
        if grantable {
            let tap = |up: bool| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_MENU,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBDINPUT::default().dwFlags },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let sent = SendInput(&[tap(false), tap(true)], std::mem::size_of::<INPUT>() as i32);
            let _ = BringWindowToTop(h);
            let _ = SetForegroundWindow(h);
            let _ = SetFocus(h);
            let _ = SetActiveWindow(h);
            if GetForegroundWindow() == h {
                if sent == 0 {
                    util::log("activate: foreground won (input grant failed but ladder stuck)");
                }
                return true;
            }
        }

        // Rung 2: the alt-tab primitive — no foreground rights needed.
        SwitchToThisWindow(h, true);
        let _ = SetFocus(h);
        let _ = SetActiveWindow(h);
        GetForegroundWindow() == h
    }
}

/// Hand the keyboard into the webview's browser child. activate_hwnd puts
/// the Win32 focus on the host window, but WebView2 keeps its own input
/// chain: without a controller MoveFocus the DOM never sees a keystroke —
/// the popup looks active yet typing (and Escape) goes nowhere until the
/// user clicks inside it. Dispatched through with_webview, so the COM call
/// always runs on the main thread wherever this is called from.
pub fn webview_keys(app: &AppHandle, label: &str) {
    let Some(w) = app.get_webview_window(label) else {
        util::log(&format!("webview_keys: no webview window '{label}'"));
        return;
    };
    let name = label.to_string();
    let _ = w.with_webview(move |pw| {
        let controller = pw.controller();
        unsafe {
            let ok = controller
                .MoveFocus(
                    webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
                )
                .is_ok();
            if !ok {
                util::log(&format!("webview_keys: MoveFocus failed ({name})"));
            }
        }
    });
}

// ---------------------------------------------------------------------------
// The unified heartbeat (cycle.rs)
// ---------------------------------------------------------------------------

unsafe fn beat_hide_native(app: &AppHandle, s: &mut Shared, beats: u64) {
    tick_hide_native(s, beats);
    if S_OGBAR_RELEASED.load(std::sync::atomic::Ordering::Relaxed) && !s.ogbar_released {
        s.ogbar_released = true;
        util::log("appbar: ogbar reservation released (space reservation fixed)");
    }
    let _ = app;
    let _ = beats;
}

/// Bar geometry BEFORE the tray beat: the tray strip is inserted after the
/// bar in z-order, so whatever happens here, the strip ends up on top.
unsafe fn beat_bar_geometry(_app: &AppHandle, s: &mut Shared, _beats: u64) {
    // Locked: the overlay owns the screen and the bars are parked. Re-assert
    // here would SWP_SHOWWINDOW them right back ABOVE the lockscreen.
    // Park-held: the unlock choreography is playing — LOCKED is already
    // false but the bars must stay parked below the virtual screen until
    // the slide owns them (the beat used to pop them home at ~100 ms).
    // Sliding: the slide owns the bars — re-asserting would snap them onto
    // their docked rects mid-animation and duel the slide thread for the
    // same HWNDs (the unlock taskbar blink).
    if crate::lockscreen::is_locked() || bars_park_held() || bars_sliding() {
        return;
    }
    reassert_bar(s);
    if !s.bar_fs_hidden {
        ensure_topmost_bar(s);
    }
}

unsafe fn beat_tasks(app: &AppHandle, s: &mut Shared, beats: u64) {
    refresh_tasks(app, s, beats);
}

unsafe fn beat_thumbs(app: &AppHandle, s: &mut Shared, _beats: u64) {
    thumbs::tick_check(app, s);
    thumbs::tick_hover(app, s);
    thumbs::tick_trayfly(app, s);
    thumbs::tick_volfly(app, s);
    tick_menu(app, s);
}

unsafe fn beat_tray_refresh(_app: &AppHandle, _s: &mut Shared, beats: u64) {
    if beats % 300 == 0 {
        crate::tray::request_refresh();
    }
}

unsafe fn beat_tray_push(app: &AppHandle, s: &mut Shared, _beats: u64) {
    if s.tray_last.is_empty() && s.tray_pushed.is_empty() {
        return; // nothing to seed yet
    }
    if s.tray_last == s.tray_pushed {
        return; // webview is current
    }
    if let Some(t) = s.tray_pushed_at {
        if t.elapsed() < std::time::Duration::from_secs(5) {
            return; // cooldown between updates
        }
    }
    let first = s.tray_pushed_at.is_none();
    let list = s.tray_last.clone();
    s.tray_pushed = list.clone();
    s.tray_pushed_at = Some(std::time::Instant::now());
    let _ = app.emit_to("trayfly", "tray_icons", &list);
    util::log(&format!(
        "tray: pushed {} icon(s) to the flyout webview ({})",
        list.len(),
        if first {
            "boot seed"
        } else if list.is_empty() {
            "clear"
        } else {
            "update"
        }
    ));
}

unsafe fn beat_bar_sync(_app: &AppHandle, s: &mut Shared, _beats: u64) {
    if s.bar_fs_hidden {
        return;
    }
    if s.hwnd_bar != 0 {
        let mut rc = RECT::default();
        if GetWindowRect(hwnd(s.hwnd_bar), &mut rc).is_ok() {
            s.bar = Rc {
                l: rc.left,
                t: rc.top,
                r: rc.right,
                b: rc.bottom,
            };
        }
    }
}

unsafe fn beat_nc_frame(_app: &AppHandle, s: &mut Shared, beats: u64) {
    if beats % 10 != 0 {
        return;
    }
    let mut list: Vec<(String, isize)> = vec![
        ("bar".into(), s.hwnd_bar),
        ("hushtop".into(), s.hwnd_hushtop),
        ("preview".into(), s.hwnd_preview),
        ("trayfly".into(), s.hwnd_trayfly),
        ("prompt".into(), s.hwnd_prompt),
        ("wingroup".into(), s.hwnd_wingroup),
        ("menu".into(), s.hwnd_menu),
        ("startmenu".into(), s.hwnd_startmenu),
        ("lockscreen".into(), s.hwnd_lock),
    ];
    for (i, b) in s.sec_bars.iter().enumerate() {
        list.push((format!("bar-s{}", i + 1), b.hwnd));
    }
    for (name, id) in list {
        if id == 0 || !IsWindow(hwnd(id)).as_bool() {
            continue;
        }
        let h = hwnd(id);
        if GetWindowLongPtrW(h, GWL_STYLE) & NC_FRAME_STYLES != 0 {
            strip_nc_frame(h);
            util::log(&format!("ncframe: non-client styles re-appeared on {name} — stripped again"));
        }
    }
}

/// EnumWindows collector: every top-level window, unfiltered.
unsafe extern "system" fn census_cb(h: HWND, l: LPARAM) -> BOOL {
    let list = &mut *(l.0 as *mut Vec<isize>);
    list.push(h.0 as isize);
    BOOL(1)
}

unsafe fn census_line(h: HWND) -> String {
    let mut cls = [0u16; 64];
    let cn = GetClassNameW(h, &mut cls);
    let cls = String::from_utf16_lossy(&cls[..(cn.max(0) as usize).min(cls.len())]);
    let mut txt = [0u16; 64];
    let tn = GetWindowTextW(h, &mut txt);
    let title = String::from_utf16_lossy(&txt[..(tn.max(0) as usize).min(txt.len())]);
    let mut rc = RECT::default();
    let rect = if GetWindowRect(h, &mut rc).is_ok() {
        format!(
            "{}x{} @{},{}",
            rc.right - rc.left,
            rc.bottom - rc.top,
            rc.left,
            rc.top
        )
    } else {
        "?".into()
    };
    format!(
        "census: hwnd={:#x} cls=\"{}\" title=\"{}\" style={:#x} ex={:#x} vis={} rect={}",
        h.0 as isize,
        cls,
        title,
        GetWindowLongPtrW(h, GWL_STYLE),
        GetWindowLongPtrW(h, GWL_EXSTYLE),
        IsWindowVisible(h).as_bool(),
        rect
    )
}

unsafe fn window_census(s: &Shared) {
    let mut list: Vec<isize> = Vec::new();
    let _ = EnumWindows(Some(census_cb), LPARAM(&mut list as *mut Vec<isize> as isize));
    let our_pid = GetCurrentProcessId();
    let mut known: Vec<isize> = vec![
        s.hwnd_bar,
        s.hwnd_hushtop,
        s.hwnd_preview,
        s.hwnd_trayfly,
        s.hwnd_prompt,
        s.hwnd_wingroup,
        s.hwnd_host,
        s.hwnd_menu,
        s.hwnd_startmenu,
        s.hwnd_lock,
    ];
    known.extend(s.sec_bars.iter().map(|b| b.hwnd));
    let shell = ["Shell_TrayWnd", "Shell_SecondaryTrayWnd", "Progman", "WorkerW"];
    let mut n = 0usize;
    for id in &list {
        let h = HWND(*id as _);
        if !IsWindow(h).as_bool() {
            continue;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let cls = crate::tasks::window_class(h);
        let tag = if pid == our_pid {
            if known.contains(id) {
                "ours:guarded"
            } else {
                "ours:UNGUARDED"
            }
        } else if shell.iter().any(|c| *c == cls) {
            "shell"
        } else {
            continue;
        };
        util::log(&format!("{} [{}]", census_line(h), tag));
        n += 1;
    }
    util::log(&format!(
        "census: {n} relevant window(s) of {} enumerated",
        list.len()
    ));
}

unsafe fn beat_nc_sweep(_app: &AppHandle, s: &mut Shared, beats: u64) {
    if beats % 20 != 0 {
        return;
    }
    let mut list: Vec<isize> = Vec::new();
    let _ = EnumWindows(Some(census_cb), LPARAM(&mut list as *mut Vec<isize> as isize));
    let our_pid = GetCurrentProcessId();
    let mut known: Vec<isize> = vec![
        s.hwnd_bar,
        s.hwnd_hushtop,
        s.hwnd_preview,
        s.hwnd_trayfly,
        s.hwnd_prompt,
        s.hwnd_wingroup,
        s.hwnd_menu,
        s.hwnd_startmenu,
        s.hwnd_lock,
    ];
    known.extend(s.sec_bars.iter().map(|b| b.hwnd));
    for id in &list {
        if known.contains(id) {
            continue; // beat_nc_frame owns these
        }
        let h = HWND(*id as _);
        if !IsWindow(h).as_bool() {
            continue;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != our_pid {
            continue;
        }
        if GetWindowLongPtrW(h, GWL_STYLE) & NC_FRAME_STYLES == 0 {
            continue;
        }
        let name = crate::tasks::window_class(h);
        strip_nc_frame(h);
        util::log(&format!(
            "ncframe: stray window stripped (class \"{name}\") — was carrying caption bits"
        ));
    }
}

/// True when `fg` is a foreign, non-shell window covering ALL of `mon` —
/// a fullscreen app on that display. Shared by every bar's beat: each bar
/// only reacts to fullscreen on ITS OWN monitor.
unsafe fn fullscreen_on_monitor(fg: HWND, mon: (i32, i32, i32, i32)) -> bool {
    let mut pid = 0u32;
    GetWindowThreadProcessId(fg, Some(&mut pid));
    if pid == 0 || pid == GetCurrentProcessId() {
        return false;
    }
    let cls = tasks::window_class(fg);
    let shell = [
        "Progman",
        "WorkerW",
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
        "hushell_host",
    ];
    if shell.iter().any(|c| *c == cls) || tasks::is_cloaked(fg) {
        return false;
    }
    let mut rc = RECT::default();
    if GetWindowRect(fg, &mut rc).is_ok() {
        let (mx, my, mw, mh) = mon;
        rc.left <= mx && rc.top <= my && rc.right >= mx + mw && rc.bottom >= my + mh
    } else {
        false
    }
}

unsafe fn beat_fullscreen(_app: &AppHandle, s: &mut Shared, _beats: u64) {
    if crate::lockscreen::is_locked() || bars_sliding() {
        // Sliding: the fs-watcher hides/shows the bars via raw ShowWindow —
        // doing that mid-slide means SW_HIDE against an animator that is
        // writing rects every 16 ms. The watcher catches up next beat.
        return;
    }
    let fg = GetForegroundWindow();
    let fg_ok = !fg.is_invalid();

    // --- primary bar: unchanged semantics, refactored onto the helper ---
    if s.hwnd_bar != 0 {
        let mon = util::monitor_rect(hwnd(s.hwnd_bar));
        let ours = [
            s.hwnd_bar,
            s.hwnd_preview,
            s.hwnd_trayfly,
            s.hwnd_host,
            s.hwnd_hushtop,
            s.hwnd_prompt,
            s.hwnd_menu,
            s.hwnd_startmenu,
            s.hwnd_lock,
        ];
        let fs = fg_ok
            && !ours.contains(&(fg.0 as isize))
            && fullscreen_on_monitor(fg, mon);
        s.fs_streak = if fs {
            s.fs_streak.saturating_add(1)
        } else {
            0
        };
        let want_hidden = s.fs_streak >= 2;
        if want_hidden != s.bar_fs_hidden {
            s.bar_fs_hidden = want_hidden;
            let hb = hwnd(s.hwnd_bar);
            if want_hidden {
                let _ = ShowWindow(hb, SW_HIDE);
                util::log("bar: fullscreen app — primary bar hidden");
            } else {
                let _ = ShowWindow(hb, SW_SHOW);
                let _ = SetWindowPos(
                    hb,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                util::log("bar: fullscreen ended — primary bar back");
            }
        }
    }

    // --- secondary bars: fullscreen on THAT monitor hides THAT strip ---
    for i in 0..s.sec_bars.len() {
        let (hb_id, mon) = {
            let sb = &s.sec_bars[i];
            (sb.hwnd, sb.mon)
        };
        if hb_id == 0 {
            continue;
        }
        // Fullscreen on THAT monitor hides THAT strip (same rule as the
        // primary's, without the ours-list — the stage overlay exception
        // that used to live here died with the stage manager).
        let fs = fg_ok && fullscreen_on_monitor(fg, mon);
        let sb = &mut s.sec_bars[i];
        sb.fs_streak = if fs {
            sb.fs_streak.saturating_add(1)
        } else {
            0
        };
        let want_hidden = sb.fs_streak >= 2;
        if want_hidden != sb.fs_hidden {
            sb.fs_hidden = want_hidden;
            let hb = hwnd(hb_id);
            if want_hidden {
                let _ = ShowWindow(hb, SW_HIDE);
                util::log(&format!(
                    "bar: fullscreen app @ {},{} — secondary bar hidden",
                    mon.0, mon.1
                ));
            } else {
                let _ = ShowWindow(hb, SW_SHOW);
                let _ = SetWindowPos(
                    hb,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                util::log(&format!(
                    "bar: fullscreen ended @ {},{} — secondary bar back",
                    mon.0, mon.1
                ));
            }
        }
    }
}

unsafe fn beat_hushtop(app: &AppHandle, s: &mut Shared, beats: u64) {
    crate::hushtop::beat(app, s, beats);
}

unsafe fn beat_stats(app: &AppHandle, s: &mut Shared, beats: u64) {
    crate::stats::beat(app, s, beats);
}

unsafe fn beat_flyouts(app: &AppHandle, s: &mut Shared, _beats: u64) {
    crate::startmenu::tick(app, s);
}

unsafe fn beat_lock(app: &AppHandle, s: &mut Shared, beats: u64) {
    crate::lockscreen::beat(app, s, beats);
}

unsafe fn beat_start_mute(_app: &AppHandle, _s: &mut Shared, beats: u64) {
    if beats % 20 == 0 {
        crate::startmenu_killer::sweep();
    }
}

fn build_cycle() -> Cycle {
    let mut c = Cycle::new();
    c.register("hide_native", beat_hide_native);
    c.register("fullscreen", beat_fullscreen);
    c.register("bar_geometry", beat_bar_geometry);
    c.register("tasks", beat_tasks);
    c.register("thumbs", beat_thumbs);
    c.register("tray_refresh", beat_tray_refresh);
    c.register("tray_push", beat_tray_push);
    c.register("bar_sync", beat_bar_sync);
    c.register("hushtop", beat_hushtop);
    c.register("stats", beat_stats);
    c.register("flyouts", beat_flyouts);
    c.register("lock", beat_lock);
    c.register("startmute", beat_start_mute);
    c.register("nc_frame", beat_nc_frame);
    c.register("nc_sweep", beat_nc_sweep);
    c.register("lang", lang::beat);
    c
}

/// Messages whose host_proc arms take the state lock. A re-entrant delivery
/// of any of these DURING a beat must be deferred (see host_proc).
fn host_lock_msg(msg: u32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::WM_DISPLAYCHANGE;
    msg == WM_DISPLAYCHANGE || msg == crate::clipboard::WM_CLIPBOARDUPDATE
}

static DEFER_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // --- re-entrancy guard -------------------------------------------------
    // Sent messages are dispatched re-entrantly: while the pump thread sits
    // inside a beat (state mutex held by the beat's raw guard in the WM_TIMER
    // arm below) and blocks in a cross-thread window call the beat made
    // (SHAppBarMessage, SetWindowPos, ShowWindow on a main-owned bar), a
    // WM_DISPLAYCHANGE broadcast from a game's mode switch (RDR1, Lossless
    // Scaling — any resolution change) lands RIGHT THERE. Running its
    // handler inline means display_change -> lock_shared on the SAME thread:
    // a non-reentrant mutex against its own guard. The beat never returns,
    // the mutex is held forever, every thread piles up behind it, and the
    // bar is gone for good. Re-post to our own queue instead: the copy is
    // dispatched after the beat returns and the guard drops.
    if util::in_beat() && host_lock_msg(msg) {
        let _ = PostMessageW(hwnd, msg, wparam, lparam);
        if !DEFER_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            util::log(
                "host: lock message re-entered during a beat — deferred (re-entrancy guard)",
            );
        }
        return LRESULT(0);
    }
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == TIMER_TICK => {
            if let Some(app) = APP_HANDLE.get() {
                let st = app.state::<crate::AppShared>();
                let mut s = st.0.lock().unwrap_or_else(|e| e.into_inner());
                util::set_in_beat(true);
                CYCLE.with(|c| c.borrow_mut().run(app, &mut s));
                util::set_in_beat(false);
            }
            LRESULT(0)
        }
        WM_APP_EXIT => {
            if let Some(app) = APP_HANDLE.get() {
                // The cursor goes back to the user's own before the shell
                // dies — and before a Start Fresh relaunch inherits the
                // session (the fresh boot re-heals anyway; exact restore
                // here keeps the two shells' cursors from ever overlapping).
                crate::cursor::shutdown();
                let st = app.state::<crate::AppShared>();
                let mut s = st.0.lock().unwrap_or_else(|e| e.into_inner());
                appbar_unregister(&mut s);
                restore_native(&mut s);
                crate::hushtop::restore(&mut s);
                let fresh = s.fresh_relaunch;
                drop(s);
                if fresh {
                    // Start Fresh: the teardown is done (taskbar back, icons
                    // back). Every saved byte goes NOW — after all teardown
                    // save paths have run (they were muted by the FRESH_WIPE
                    // arm; this catches anything written before it).
                    crate::config::wipe_all();
                    // only NOW spawn the clean replacement, so the two
                    // shells never overlap.
                    fresh_relaunch();
                }
                crate::util::log("exit: cleanup done — exiting process");
                app.exit(0);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_LOCK_ID {
                if let Some(app) = APP_HANDLE.get() {
                    if crate::lockscreen::is_locked() {
                        util::log("hotkey: alt+l ignored — already locked");
                    } else {
                        util::log("hotkey: alt+l -> locking the shell");
                        crate::lockscreen::lock(app);
                    }
                }
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DISPLAYCHANGE => {
            // A monitor joined, left, or changed resolution. Without this
            // the secondary bars kept their stale monitor rects (bars and
            // reservations misplaced until restart) and hushtop kept the
            // old virtual-screen size. A mode switch can also reload the
            // cursor scheme over our swap — re-arm the set alongside.
            crate::cursor::rearm();
            if let Some(app) = APP_HANDLE.get() {
                unsafe { display_change(app) };
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE => {
            // Anything that reloaded the system cursor scheme over us (a
            // theme change, a driver reset, a SPI_SETCURSORS from any app)
            // lands here. The cursor module debounces and no-ops when idle
            // — a cheap, lock-free heal of the one thing round 2 lacked.
            crate::cursor::rearm();
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_THEMECHANGED => {
            // Theme engines reload cursor schemes on transitions.
            crate::cursor::rearm();
            LRESULT(0)
        }
        crate::clipboard::WM_CLIPBOARDUPDATE => {
            if let Some(app) = APP_HANDLE.get() {
                crate::clipboard::on_clipboard_update(app);
            }
            LRESULT(0)
        }
        msg if msg == WM_APP_LOCKHOTKEY => {
            lock_hotkey_apply();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Display topology changed (WM_DISPLAYCHANGE): re-sync every secondary bar
/// with the live monitor list and re-fit hushtop to the new virtual screen.
/// Runs on the native (host) thread; window create/destroy dispatches to the
/// main thread. Never touches Win32 placement under the state lock.
unsafe fn display_change(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    // LOCKED: the overlay must span the new virtual screen before anything
    // else re-flows — a fresh monitor must never show the bare desktop for
    // even one beat. The re-fit below and the bars follow after.
    if crate::lockscreen::is_locked() {
        crate::lockscreen::assert_full(app);
        util::log("display: LOCKED — lock overlay re-asserted over the new virtual screen");
    }
    let mons = util::monitors();

    // --- 1) refresh each bar's monitor rect; collect orphans -------------
    let mut orphans: Vec<isize> = Vec::new();
    let mut to_remove: Vec<isize> = Vec::new();
    {
        let mut s = util::lock_shared(&st.0);
        for sb in s.sec_bars.iter_mut() {
            if sb.hwnd == 0 || !IsWindow(hwnd(sb.hwnd)).as_bool() {
                to_remove.push(sb.hwnd);
                continue;
            }
            // The monitor whose rect CONTAINS the bar's center. The window
            // may be momentarily misplaced (hidden bar at parked coords), so
            // fall back to matching by rect union instead of position.
            let mut rc = RECT::default();
            if GetWindowRect(hwnd(sb.hwnd), &mut rc).is_err() {
                continue;
            }
            let cx = (rc.left + rc.right) / 2;
            let cy = (rc.top + rc.bottom) / 2;
            let hit = mons.iter().find(|m| {
                cx >= m.x && cx < m.x + m.w && cy >= m.y && cy < m.y + m.h
            });
            match hit {
                Some(m) => {
                    if sb.mon != m.rect() {
                        util::log(&format!(
                            "display: bar @ {:?} re-homed to monitor @ {},{} {}x{}",
                            sb.mon, m.x, m.y, m.w, m.h
                        ));
                        sb.mon = m.rect();
                        // The reservation must follow the new rect; the next
                        // geometry beat re-asserts (rect moved -> ABM runs).
                    }
                }
                None => {
                    // The bar's home monitor is gone (or its geometry moved
                    // entirely off) — retire this strip.
                    orphans.push(sb.hwnd);
                    to_remove.push(sb.hwnd);
                }
            }
        }
        s.sec_bars.retain(|b| !to_remove.contains(&b.hwnd));
        // Monitors that have no bar at all -> build one on the main thread.
        let covered = |mx: i32, my: i32| -> bool {
            s.sec_bars
                .iter()
                .any(|b| b.mon.0 == mx && b.mon.1 == my)
        };
        let missing: Vec<(i32, i32, i32, i32)> = mons
            .iter()
            .filter(|m| !m.primary && !covered(m.x, m.y))
            .map(|m| m.rect())
            .collect();
        if !missing.is_empty() {
            let a2 = app.clone();
            let _ = app.run_on_main_thread(move || unsafe {
                for mon in missing {
                    let st = a2.state::<crate::AppShared>();
                    // While LOCKED a new monitor gets its bar built but never
                    // shown — a live taskbar on a locked display is an
                    // unlocked desktop with extra steps. The unlock
                    // choreography re-shows every secondary bar (this one is
                    // pushed to sec_bars below), so nothing is lost.
                    let locked = crate::lockscreen::is_locked();
                    let label = {
                        let s = crate::util::lock_shared(&st.0);
                        let mut n = s.sec_bars.len() + 1;
                        while a2.get_webview_window(&format!("taskbar-s{}", n)).is_some() {
                            n += 1;
                        }
                        format!("taskbar-s{}", n)
                    };
                    let built = (|| -> Result<isize, String> {
                        let w = tauri::WebviewWindowBuilder::new(
                            &a2,
                            label.clone(),
                            tauri::WebviewUrl::App("index.html".into()),
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
                        .effects(tauri::utils::config::WindowEffectsConfig {
                            effects: vec![
                                tauri::window::Effect::Acrylic,
                                tauri::window::Effect::Mica,
                                tauri::window::Effect::Blur,
                            ],
                            ..Default::default()
                        })
                        .build()
                        .map_err(|e| e.to_string())?;
                        let h = w.hwnd().map_err(|e| e.to_string())?;
                        Ok(h.0 as isize)
                    })();
                    let h = match built {
                        Ok(h) => h,
                        Err(e) => {
                            util::log(&format!("display: {label} hotplug build failed: {e}"));
                            continue;
                        }
                    };
                    util::log(&format!("display: {label} built for monitor @ {},{}", mon.0, mon.1));
                    let mut s = crate::util::lock_shared(&st.0);
                    // Shell-window treatment: not a toolwindow (the AppBar
                    // system refuses those), no-activate, guards on.
                    let ex = GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE);
                    SetWindowLongPtrW(
                        hwnd(h),
                        GWL_EXSTYLE,
                        (ex & !WS_EX_TOOLWINDOW.0 as isize) | WS_EX_NOACTIVATE.0 as isize,
                    );
                    let nc = crate::frame::guard_no_close(h);
                    let fr = crate::frame::guard_frameless(h);
                    strip_nc_frame(hwnd(h));
                    let registered = appbar_register_at(hwnd(h), mon).is_some();
                    let wr = bar_window_rect_at(mon, hwnd(h));
                    let _ = SetWindowPos(
                        hwnd(h),
                        HWND_TOPMOST,
                        wr.l,
                        wr.t,
                        wr.w(),
                        wr.h(),
                        if locked {
                            SWP_NOACTIVATE
                        } else {
                            SWP_NOACTIVATE | SWP_SHOWWINDOW
                        },
                    );
                    if locked {
                        util::log(&format!(
                            "display: {label} built HIDDEN — screen is locked (unlock shows it)"
                        ));
                    } else {
                        let _ = ShowWindow(hwnd(h), SW_SHOW);
                    }
                    s.sec_bars.push(crate::shared::SecBar {
                        hwnd: h,
                        mon,
                        registered,
                        fs_streak: 0,
                        fs_hidden: false,
                    });
                    util::log(&format!(
                        "display: {label} placed @ {},{} (registered={registered}, no-close={nc}, frameless={fr})",
                        mon.0, mon.1
                    ));
                }
            });
        }
    }

    // --- 2) unregister orphaned bars + destroy their windows -------------
    for id in orphans {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: hwnd(id),
            ..Default::default()
        };
        SHAppBarMessage(ABM_REMOVE, &mut abd);
        util::log(&format!("display: orphaned bar {:#x} — appbar removed", id));
        let a2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            // Find the window by hwnd among ALL webview windows and destroy
            // it (destroy() bypasses the CloseRequested refusal on purpose —
            // the window is gone with its monitor).
            for (label, w) in a2.webview_windows() {
                if let Ok(h) = w.hwnd() {
                    if h.0 as isize == id {
                        util::log(&format!("display: destroying {label} (monitor gone)"));
                        let _ = w.destroy();
                        break;
                    }
                }
            }
        });
    }

    // --- 3) hushtop must span the NEW virtual screen ----------------------
    {
        let s = util::lock_shared(&st.0);
        let hh = s.hwnd_hushtop;
        if hh != 0 && IsWindow(hwnd(hh)).as_bool() {
            let a2 = app.clone();
            let _ = app.run_on_main_thread(move || unsafe {
                let st = a2.state::<crate::AppShared>();
                let parent = crate::hushtop::apply_core_public(hwnd(hh));
                let mut s = crate::util::lock_shared(&st.0);
                if parent != 0 {
                    s.hushtop_parent = parent;
                    s.hushtop_applied = true;
                }
                util::log("display: hushtop re-fitted to the new virtual screen");
            });
        }
    }
}

pub fn lock_hotkey_apply() {
    unsafe {
        let on = {
            let app = match APP_HANDLE.get() {
                Some(a) => a,
                None => return,
            };
            let st = app.state::<crate::AppShared>();
            let s = st.0.lock().unwrap_or_else(|e| e.into_inner());
            s.cfg.lock_hotkey
        };
        // Always unregister first — idempotent re-apply without duplicates.
        let _ = UnregisterHotKey(hwnd_current(), HOTKEY_LOCK_ID);
        if on {
            match RegisterHotKey(hwnd_current(), HOTKEY_LOCK_ID, MOD_ALT, VK_L) {
                Ok(()) => util::log("hotkey: alt+l registered (custom lock bound)"),
                Err(e) => util::log(&format!(
                    "hotkey: alt+l REGISTRATION FAILED (another app owns it?): {e}"
                )),
            }
        } else {
            util::log("hotkey: alt+l unregistered (switch off)");
        }
    }
}

/// The host window of THIS thread — RegisterHotKey associates with the
/// calling thread; passing the host hwnd makes the WM_HOTKEY deliver to
/// its wndproc deterministically.
fn hwnd_current() -> HWND {
    let app = match APP_HANDLE.get() {
        Some(a) => a,
        None => return HWND::default(),
    };
    let st = app.state::<crate::AppShared>();
    let h = st.0.lock().unwrap_or_else(|e| e.into_inner()).hwnd_host;
    HWND(h as _)
}

/// Thread-safe poke: re-apply the hotkey registration on the host pump.
/// Called by the Controls switch command after it writes cfg.
pub fn lock_hotkey_notify(app: &AppHandle) {
    unsafe {
        let host = {
            let st = app.state::<crate::AppShared>();
            let s = st.0.lock().unwrap_or_else(|e| e.into_inner());
            s.hwnd_host
        };
        if host != 0 {
            let _ = PostMessageW(HWND(host as _), WM_APP_LOCKHOTKEY, WPARAM(0), LPARAM(0));
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point of the native thread
// ---------------------------------------------------------------------------

pub fn native_main(app: AppHandle) {
    crate::util::register_thread("pump");
    let _ = APP_HANDLE.set(app.clone());
    unsafe {
        let st = app.state::<crate::AppShared>();

        let hbar = {
            let s = util::lock_shared(&st.0);
            if s.hwnd_bar == 0 {
                util::log("native: bar window missing");
                return;
            }
            hwnd(s.hwnd_bar)
        };
        {
            // No-activate, and NOT a tool window: the shell silently refuses
            // to reserve space (ABM_QUERYPOS/SETPOS) for WS_EX_TOOLWINDOW
            // appbars, so maximized windows would ignore the dock. Alt-Tab
            // stays clean via WS_EX_NOACTIVATE.
            // LOCK-FREE: SetWindowLongPtrW(GWL_EXSTYLE) sends WM_STYLECHANGING
            // to the bar's owner (the MAIN thread) — it must never run under
            // the state mutex.
            let ex = GetWindowLongPtrW(hbar, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hbar,
                GWL_EXSTYLE,
                (ex & !WS_EX_TOOLWINDOW.0 as isize) | WS_EX_NOACTIVATE.0 as isize,
            );
            strip_nc_frame(hbar);
        }
        {
            let ids: Vec<isize> = {
                let g = util::lock_shared(&st.0);
                vec![
                    g.hwnd_hushtop,
                    g.hwnd_preview,
                    g.hwnd_trayfly,
                    g.hwnd_volfly,
                    g.hwnd_prompt,
                    g.hwnd_wingroup,
                    g.hwnd_startmenu,
                    g.hwnd_lock,
                ]
            };
            for id in ids {
                if id != 0 && IsWindow(hwnd(id)).as_bool() {
                    strip_nc_frame(hwnd(id));
                }
            }
        }
        {
            let ids: Vec<(&str, isize)> = {
                let g = util::lock_shared(&st.0);
                vec![
                    ("bar", g.hwnd_bar),
                    ("hushtop", g.hwnd_hushtop),
                    ("preview", g.hwnd_preview),
                    ("trayfly", g.hwnd_trayfly),
                    ("prompt", g.hwnd_prompt),
                    ("wingroup", g.hwnd_wingroup),
                    ("startmenu", g.hwnd_startmenu),
                    ("lockscreen", g.hwnd_lock),
                ]
            };
            for (name, id) in ids {
                if id != 0 && IsWindow(hwnd(id)).as_bool() {
                    let style = GetWindowLongPtrW(hwnd(id), GWL_STYLE);
                    let ex = GetWindowLongPtrW(hwnd(id), GWL_EXSTYLE);
                    util::log(&format!(
                        "ncframe: boot styles {name}: style={style:#x} ex={ex:#x}"
                    ));
                }
            }
        }
        {
            let mut s = util::lock_shared(&st.0);
            if s.cfg.hide_native_taskbar {
                tick_hide_native(&mut s, 0);
            }
        }
        let rc = appbar_register(hbar).unwrap_or_else(|| desired_bar_rect(hbar));
        let wr = bar_window_rect(hbar);
        let _ = SetWindowPos(
            hbar,
            HWND_TOPMOST,
            wr.l,
            wr.t,
            wr.w(),
            wr.h(),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hbar, SW_SHOW);
        {
            let mut s = util::lock_shared(&st.0);
            s.bar = rc;
            s.appbar_registered = true;
        }
        let _ = app.emit("bar:reveal", ());
        util::log("native: bar positioned + revealed");

        // Secondary bars: strip the toolwindow style (the AppBar system
        // silently refuses WS_EX_TOOLWINDOW docks), each registers its OWN
        // AppBar on its monitor — explorer does the same with
        // Shell_SecondaryTrayWnd — then places at that monitor's bottom.
        {
            let sec: Vec<crate::shared::SecBar> = util::lock_shared(&st.0).sec_bars.clone();
            for sb in &sec {
                if sb.hwnd == 0 || !IsWindow(hwnd(sb.hwnd)).as_bool() {
                    continue;
                }
                let h = hwnd(sb.hwnd);
                let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                SetWindowLongPtrW(
                    h,
                    GWL_EXSTYLE,
                    (ex & !WS_EX_TOOLWINDOW.0 as isize) | WS_EX_NOACTIVATE.0 as isize,
                );
                strip_nc_frame(h);
                let registered = appbar_register_at(h, sb.mon).is_some();
                let wr = bar_window_rect_at(sb.mon, h);
                let _ = SetWindowPos(
                    h,
                    HWND_TOPMOST,
                    wr.l,
                    wr.t,
                    wr.w(),
                    wr.h(),
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                let _ = ShowWindow(h, SW_SHOW);
                let mut s2 = util::lock_shared(&st.0);
                if let Some(b) = s2.sec_bars.iter_mut().find(|b| b.hwnd == sb.hwnd) {
                    b.registered = registered;
                }
                util::log(&format!(
                    "native: secondary bar placed @ {},{} (registered={registered})",
                    sb.mon.0, sb.mon.1
                ));
            }
        }

        {
            let s = util::lock_shared(&st.0);
            window_census(&s);
        }

        crate::hushtop::init(&app, &st);

        // Boot choreography, native half: with the live desktop applied,
        // every foreign top-level window steps aside so the shell starts
        // ALONE on its sheet — then the webview lifts the ink veil, sweeps
        // the ruled-line curtain left -> right and fades the icons/widgets
        // in. Nothing is recorded: boot cleanup is not a lock, these stay
        // minimized (a later lock records ITS OWN sweep for the unlock).
        {
            let swept = crate::tasks::minimize_all_except_shell();
            let mut s = util::lock_shared(&st.0);
            s.lock_minimized.clear();
            util::log(&format!(
                "native: boot sweep minimized {} window(s) — the desktop starts alone",
                swept.len()
            ));
        }

        if st.0.lock().unwrap_or_else(|e| e.into_inner()).hwnd_preview != 0 {
            let hp = hwnd(st.0.lock().unwrap_or_else(|e| e.into_inner()).hwnd_preview);
            let ex = GetWindowLongPtrW(hp, GWL_EXSTYLE);
            SetWindowLongPtrW(hp, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
        }

        // --- message host window (WM_COPYDATA receiver for the tray hook,
        //     heartbeat timer owner; never shown on screen) ---
        let hmodule = match GetModuleHandleW(PCWSTR::null()) {
            Ok(h) => h,
            Err(_) => return,
        };
        let wc = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
            lpfnWndProc: Some(host_proc),
            hInstance: hmodule.into(),
            lpszClassName: w!("hushell_host"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let host = match CreateWindowExW(
            WS_EX_NOACTIVATE,
            w!("hushell_host"),
            w!(""),
            WS_POPUP, // no WS_VISIBLE: this window has no on-screen role
            -32000,
            -32000,
            1,
            1,
            HWND::default(),
            HMENU::default(),
            windows::Win32::Foundation::HINSTANCE(hmodule.0),
            None,
        ) {
            Ok(h) => h,
            Err(_) => {
                util::log("native: failed to create host window");
                return;
            }
        };
        {
            let mut s = st.0.lock().unwrap_or_else(|e| e.into_inner());
            s.hwnd_host = host.0 as isize;
        }
        crate::clipboard::start_listener(host);

        SetTimer(host, TIMER_TICK, cycle::BEAT_MS, None);
        CYCLE.with(|c| *c.borrow_mut() = build_cycle());
        util::log("native: heartbeat starting (100ms cycle)");

        crate::startmenu::prewarm();

        lock_hotkey_apply();

        // Tray engine: event-driven stream thread (systray-util spy), NOT a
        // beat — tray data arrives via WM_COPYDATA callbacks, the beat is
        // only a fallback sweep inside that thread.
        crate::tray::spawn_engine(app.clone());

        // --- message pump ---
        let mut msg = MSG::default();
        loop {
            let r = windows::Win32::UI::WindowsAndMessaging::GetMessageW(&mut msg, HWND::default(), 0, 0);
            if r.0 <= 0 {
                break;
            }
            let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        util::log("native: pump ended");
    }
}

pub fn request_exit(app: &AppHandle) {
    unsafe {
        let st = app.state::<crate::AppShared>();
        let host = crate::util::lock_shared(&st.0).hwnd_host;
        if host != 0 {
            let _ = PostMessageW(hwnd(host), WM_APP_EXIT, WPARAM(0), LPARAM(0));
        } else {
            appbar_unregister(&mut crate::util::lock_shared(&st.0));
            app.exit(0);
        }
    }
}

/// Spawn a detached copy of this exe — the Start Fresh relaunch. Runs only
/// from the exit handler, after the shell teardown has restored the stock
/// desktop, so the outgoing and incoming shells never overlap.
fn fresh_relaunch() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::env::current_exe() {
        Ok(exe) => match std::process::Command::new(&exe)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(child) => {
                crate::util::log(&format!("fresh: clean instance relaunched (pid {})", child.id()));
            }
            Err(e) => crate::util::log(&format!("fresh: relaunch spawn failed: {e}")),
        },
        Err(e) => crate::util::log(&format!("fresh: current_exe failed: {e}")),
    }
}

/// One row of the menu payload. `t`/`a` are None for a separator row.
#[derive(serde::Serialize, Clone)]
struct MenuEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    t: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    a: Option<&'static str>,
}

/// Payload for `menu:show` — the menu window rebuilds its sheet from it and
/// echoes `ids` back to task_menu_run (the group is re-derived there, so
/// windows that died while the menu was open can never be acted on).
#[derive(serde::Serialize, Clone)]
struct MenuShow {
    items: Vec<MenuEntry>,
    ids: Vec<isize>,
}

fn menu_items() -> Vec<MenuEntry> {
    vec![
        MenuEntry {
            t: Some("New window"),
            a: Some("new_window"),
        },
        MenuEntry { t: None, a: None },
        MenuEntry {
            t: Some("Close"),
            a: Some("close"),
        },
        MenuEntry {
            t: Some("End task"),
            a: Some("end_task"),
        },
    ]
}

/// Public command entry: show the context menu for a task GROUP (every hwnd
/// of one process). Snapshot under a short lock, then NEVER touch the mutex
/// under Win32 (standing rule). The window dance runs on the main thread.
pub fn task_menu(app: &AppHandle, bar: isize, ids: Vec<isize>) {
    let st = app.state::<crate::AppShared>();
    let hm = crate::util::lock_shared(&st.0).hwnd_menu;
    let (pid, alive, exe) = unsafe { group_info(&ids) };
    if alive.is_empty() {
        util::log("task_menu: no alive window in the group — dropped");
        return;
    }
    if hm == 0 {
        util::log("task_menu: menu hwnd is 0 — dropped");
        return;
    }
    // The menu belongs to the bar it was clicked on: centered on THAT
    // bar's monitor, not the primary.
    let mon = if bar != 0 && unsafe { IsWindow(hwnd(bar)).as_bool() } {
        util::monitor_rect(hwnd(bar))
    } else {
        (0, 0, util::screen_width(), util::screen_height())
    };
    let prev_fg = unsafe { GetForegroundWindow().0 as isize };
    {
        let mut s = crate::util::lock_shared(&st.0);
        s.menu_visible = true;
        s.menu_prev_fg = prev_fg;
        s.menu_opened_at = Some(std::time::Instant::now());
    }
    util::log(&format!(
        "task_menu: pid={pid} exe={:?} windows={} menu opening (bar monitor @ {},{})",
        exe,
        alive.len(),
        mon.0,
        mon.1
    ));
    let _ = app.emit_to("menu", "menu:show", MenuShow { items: menu_items(), ids });
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || unsafe {
        task_menu_place(&app2, hm, mon);
    });
}

/// Center the menu window on the CALLER bar's monitor and give it the
/// keyboard focus. An unfocused popup can neither receive Escape nor fire
/// blur — the attach trick grants the foreground (same primitive
/// activate_hwnd uses; the bar is NOACTIVATE so hushell itself never
/// stole it).
unsafe fn task_menu_place(app: &AppHandle, hm: isize, mon: (i32, i32, i32, i32)) {
    {
        let st = app.state::<crate::AppShared>();
        let s = crate::util::lock_shared(&st.0);
        if !s.menu_visible {
            return; // closed again before the main thread got here
        }
    }
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    let h = hwnd(hm);
    let dpi = GetDpiForWindow(h) as f64 / 96.0;
    let w = (260.0 * dpi).round() as i32;
    let hgt = (170.0 * dpi).round() as i32;
    let (mx, my, mw, mh) = mon;
    let _ = SetWindowPos(
        h,
        HWND_TOPMOST,
        mx + (mw - w) / 2,
        my + (mh - hgt) / 2,
        w,
        hgt,
        SWP_SHOWWINDOW,
    );
    activate_hwnd(hm);
    // keyboard into the browser child — without it Escape never reaches the
    // sheet until the user clicks inside the menu
    webview_keys(app, "menu");
}

/// A verb was picked (the JS sheet already hid itself and its class). The
/// group is re-derived — windows may have died while the menu was open —
/// the executor fires, then the foreground goes back to the app that was
/// active at open time (unless the action itself already moved it).
pub fn task_menu_run(app: &AppHandle, action: String, ids: Vec<isize>) {
    let st = app.state::<crate::AppShared>();
    let (hm, prev_fg) = {
        let mut s = crate::util::lock_shared(&st.0);
        if !s.menu_visible {
            util::log(&format!("task_menu: run '{action}' dropped — menu closed"));
            return;
        }
        s.menu_visible = false;
        s.menu_opened_at = None;
        (s.hwnd_menu, s.menu_prev_fg)
    };
    unsafe {
        if hm != 0 {
            let _ = ShowWindow(hwnd(hm), SW_HIDE);
        }
    }
    let (pid, alive, exe) = unsafe { group_info(&ids) };
    if alive.is_empty() {
        util::log("task_menu: run — no alive window left in the group, dropped");
    } else {
        unsafe { task_menu_execute(&action, pid, &alive, &exe) };
    }
    unsafe { restore_fg_after_menu(hm, prev_fg) };
}

pub fn task_menu_hide(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    let (hm, prev_fg) = {
        let mut s = crate::util::lock_shared(&st.0);
        if !s.menu_visible {
            return;
        }
        s.menu_visible = false;
        s.menu_opened_at = None;
        (s.hwnd_menu, s.menu_prev_fg)
    };
    unsafe {
        let fg = GetForegroundWindow().0 as isize;
        if hm != 0 {
            let _ = ShowWindow(hwnd(hm), SW_HIDE);
        }
        if fg == hm && prev_fg != 0 && prev_fg != hm && IsWindow(hwnd(prev_fg)).as_bool() {
            activate_hwnd(prev_fg);
        }
    }
    util::log("task_menu: dismissed");
}

/// Focus hand-back after an action ran. Skipped when the foreground already
/// moved on its own (ShellExecute launches, the closing app's own chain).
unsafe fn restore_fg_after_menu(hm: isize, prev_fg: isize) {
    if prev_fg == 0 || prev_fg == hm {
        return;
    }
    let fg = GetForegroundWindow().0 as isize;
    if fg != 0 && fg != hm && fg != prev_fg {
        return; // focus already moved — respect it
    }
    if IsWindow(hwnd(prev_fg)).as_bool() {
        activate_hwnd(prev_fg);
    }
}

unsafe fn task_menu_execute(action: &str, pid: u32, alive: &[isize], exe: &str) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNORMAL, WM_CLOSE};
    match action {
        "new_window" => {
            if exe.is_empty() {
                util::log("task_menu: New window — no exe path for this process, dropped");
            } else {
                let wide = windows::core::HSTRING::from(exe);
                let _ = ShellExecuteW(
                    None,
                    w!("open"),
                    &wide,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
                util::log(&format!("task_menu: New window -> {exe}"));
            }
        }
        "close" => {
            let mut n = 0usize;
            for id in alive {
                if PostMessageW(hwnd(*id), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() {
                    n += 1;
                }
            }
            util::log(&format!("task_menu: Close -> WM_CLOSE to {n} window(s)"));
        }
        "end_task" => {
            if pid == GetCurrentProcessId() {
                util::log("task_menu: End task refused (own process)");
            } else {
                match OpenProcess(PROCESS_TERMINATE, false, pid) {
                    Ok(h) => {
                        let ok = TerminateProcess(h, 1).is_ok();
                        let _ = CloseHandle(h);
                        util::log(&format!("task_menu: End task pid={pid} ok={ok}"));
                    }
                    Err(e) => util::log(&format!("task_menu: End task OpenProcess failed: {e}")),
                }
            }
        }
        other => util::log(&format!("task_menu: unknown action '{other}'")),
    }
}

/// Popup contract tick (the task context menu + the wingroup sheet): a
/// popup disappears ONLY when the user loses interest — a press lands
/// somewhere else — or presses a button on it (a menu item, a row, the X,
/// Escape). Focus drift alone never closes a popup: a background app
/// waking up or the activation ladder losing a race is not the user's
/// intent, and the old foreground heartbeat murdered the menu half a
/// second after every open on exactly such a drift. The menu webview's
/// margin handler covers presses inside its own window; this tick catches
/// presses everywhere else via the hardware key state (same pattern as
/// the trayfly/volfly polls). 100 ms beat: a click shorter than one beat
/// can slip through — the same tolerance the trayfly lives with.
/// True when the cursor sits over ANY hushell bar — the primary or a
/// secondary strip. Popup outside-press detection exempts all of them:
/// a press on the bar that owns the popup must not dismiss it (the old
/// check only knew the primary bar, so a menu opened from a secondary
/// died the moment the user pressed that bar again).
fn cursor_over_any_bar(s: &Shared) -> bool {
    (s.hwnd_bar != 0 && thumbs::cursor_over_hwnd(s.hwnd_bar))
        || s.sec_bars
            .iter()
            .any(|b| b.hwnd != 0 && thumbs::cursor_over_hwnd(b.hwnd))
}

pub unsafe fn tick_menu(app: &AppHandle, s: &mut Shared) {
    // --- the task context menu ---
    if s.menu_visible {
        let hm = s.hwnd_menu;
        if hm == 0 || !IsWindow(hwnd(hm)).as_bool() || !IsWindowVisible(hwnd(hm)).as_bool() {
            // Window died or was hidden out-of-band — resync the flag.
            s.menu_visible = false;
            s.menu_opened_at = None;
        } else {
            let ldown = GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000 != 0;
            let rdown = GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000 != 0;
            let outside = (ldown || rdown)
                && !thumbs::cursor_over_hwnd(hm)
                && !cursor_over_any_bar(s);
            if outside {
                s.menu_visible = false;
                s.menu_opened_at = None;
                let _ = ShowWindow(hwnd(hm), SW_HIDE);
                util::log("task_menu: dismissed (press outside the menu)");
            }
        }
    }
    // --- the wingroup sheet (the +N badge on the preview card) ---
    let hw = s.hwnd_wingroup;
    if hw != 0 && IsWindow(hwnd(hw)).as_bool() && IsWindowVisible(hwnd(hw)).as_bool() {
        let ldown = GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000 != 0;
        let rdown = GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000 != 0;
        let outside = (ldown || rdown)
            && !thumbs::cursor_over_hwnd(hw)
            && !cursor_over_any_bar(s);
        if outside {
            let _ = ShowWindow(hwnd(hw), SW_HIDE);
            util::log("wingroup: dismissed (press outside the sheet)");
        } else if WGROUP_PRUNE_BEATS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 5 == 0 {
            // The sheet must track reality: rows whose window died (Close via
            // the row's context menu, End task, the app exiting on its own)
            // disappear — WM_CLOSE is async, so this re-checks on a slow
            // beat (~500 ms) for as long as the sheet stays open.
            thumbs::wingroup_prune_locked(app, s);
        }
    }
}

/// Owner-process info of a group: pid, alive hwnds (order preserved), exe
/// path ("" when the process does not expose one — UWP hosts).
unsafe fn group_info(ids: &[isize]) -> (u32, Vec<isize>, String) {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let mut pid = 0u32;
    let mut alive = Vec::new();
    for id in ids {
        if IsWindow(hwnd(*id)).as_bool() {
            alive.push(*id);
            if pid == 0 {
                GetWindowThreadProcessId(hwnd(*id), Some(&mut pid));
            }
        }
    }
    let mut exe = String::new();
    if pid != 0 {
        if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(
                h,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok()
            {
                exe = String::from_utf16_lossy(&buf[..len as usize]);
            }
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
    (pid, alive, exe)
}
