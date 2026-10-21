use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetForegroundWindow, IsWindow, IsWindowVisible, SetWindowPos, ShowWindow,
    HWND_TOPMOST, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_HIDE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOW,
};

use crate::util::log;

/// The one lock truth. Set by lock(), cleared by a verified unlock. Every
/// gate (winhook tap, startmenu::show) and the heartbeat beat read this.
static LOCKED: AtomicBool = AtomicBool::new(false);

/// One main-thread assertion in flight (no queue pile-up: the beat skips
/// while a queued assert has not run yet — the next beat re-checks anyway).
static ASSERT_QUEUED: AtomicBool = AtomicBool::new(false);

const FULL_ASSERT_EVERY: u64 = 10;

pub fn is_locked() -> bool {
    LOCKED.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Password (casual by design — see the module header)
// ---------------------------------------------------------------------------

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(data: &[u8], seed: u64) -> u64 {
    let mut h = seed;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

pub fn hash_password(salt: &str, pw: &str) -> String {
    let mut h = fnv1a64(format!("{salt}\u{1}{pw}").as_bytes(), FNV_OFFSET);
    for i in 0..512u32 {
        h = fnv1a64(format!("{salt}:{h:016x}:{i}").as_bytes(), FNV_OFFSET);
    }
    format!("{h:016x}")
}

/// A fresh hex salt per set. No OS CSPRNG dependency: time + pid + an
/// address-space sample mixed together — plenty for a casual lock.
fn random_salt() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let marker = &n as *const u128 as u128;
    let mix = n ^ (pid << 64) ^ marker.rotate_left(17);
    format!("{mix:032x}")
}

fn verify(app: &AppHandle, password: &str) -> bool {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    if s.cfg.lock_hash.is_empty() {
        return true; // no password configured — any input unlocks
    }
    hash_password(&s.cfg.lock_salt, password) == s.cfg.lock_hash
}

/// The start menu controls row. Empty password = clear the lock password.
pub fn password_set(app: &AppHandle, password: &str) {
    let st = app.state::<crate::AppShared>();
    let mut s = crate::util::lock_shared(&st.0);
    if password.is_empty() {
        s.cfg.lock_salt.clear();
        s.cfg.lock_hash.clear();
        log("lockscreen: password cleared");
    } else {
        let salt = random_salt();
        s.cfg.lock_hash = hash_password(&salt, password);
        s.cfg.lock_salt = salt;
        log("lockscreen: password set (salted digest stored)");
    }
    s.cfg.save();
}

pub fn password_is_set(app: &AppHandle) -> bool {
    let st = app.state::<crate::AppShared>();
    let s = crate::util::lock_shared(&st.0);
    !s.cfg.lock_hash.is_empty()
}

// ---------------------------------------------------------------------------
// Lock / unlock
// ---------------------------------------------------------------------------

pub fn lock(app: &AppHandle) {
    if LOCKED.swap(true, Ordering::SeqCst) {
        log("lockscreen: lock ignored — already locked");
        return;
    }
    crate::startmenu::hide(app);
    crate::native::task_menu_hide(app);
    crate::thumbs::trayfly_hide(app);
    {
        // volfly_hide_with wants the state lock — same shape as the command
        // wrapper in main.rs.
        let st = app.state::<crate::AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        crate::thumbs::volfly_hide_with(app, &mut s);
    }
    let a2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        // Full geometry assert (places over the virtual screen, TOPMOST,
        // SWP_SHOWWINDOW) — then the explicit webview show + focus on top
        // of it (the startmenu convention: the controller must agree the
        // window is visible, and the password field must own the keyboard).
        main_assert(&a2);
        if let Some(w) = a2.get_webview_window("lockscreen") {
            let _ = w.show();
            let _ = w.set_focus();
        }
        let _ = a2.emit("lockscreen:show", ());
        log("lockscreen: locked (overlay on screen, show event emitted)");
    });
}

/// The overlay's password check. Wrong password = Err (the webview shakes
/// and stays); right password (or no password configured) = the overlay
/// hides and the shell is back.
pub fn unlock(app: &AppHandle, password: &str) -> Result<(), String> {
    if !LOCKED.load(Ordering::SeqCst) {
        return Ok(()); // not locked — nothing to unlock
    }
    if !verify(app, password) {
        log("lockscreen: unlock denied (wrong password)");
        return Err("wrong password".into());
    }
    LOCKED.store(false, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("lockscreen") {
        let _ = w.hide();
    }
    let _ = app.emit("lockscreen:hide", ());
    let a2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        // Restore EVERY bar the lock parked — the primary plus every
        // secondary strip. Raw ShowWindow + TOPMOST re-raise (same
        // primitive the fullscreen-hide path uses; tao flag rewrites are
        // avoided on purpose).
        let st = a2.state::<crate::AppShared>();
        let ids: Vec<isize> = {
            let s = crate::util::lock_shared(&st.0);
            let mut v = vec![s.hwnd_bar];
            v.extend(s.sec_bars.iter().map(|b| b.hwnd));
            v
        };
        for id in ids {
            if id != 0 {
                unsafe {
                    let h = HWND(id as _);
                    if IsWindow(h).as_bool() {
                        let _ = ShowWindow(h, SW_SHOW);
                        let _ = SetWindowPos(
                            h,
                            HWND_TOPMOST,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
            }
        }
        if let Some(w) = a2.get_webview_window("taskbar") {
            let _ = w.show();
        }
        log("lockscreen: all bars restored");
    });
    log("lockscreen: unlocked");
    Ok(())
}

// ---------------------------------------------------------------------------
// Geometry + the heartbeat belt
// ---------------------------------------------------------------------------

fn main_assert(app: &AppHandle) {
    let st = app.state::<crate::AppShared>();
    // Short lock: read the hwnd only (the guard is a temporary that drops
    // with the statement — no window work happens under it).
    let hl = crate::util::lock_shared(&st.0).hwnd_lock;
    if hl == 0 {
        log("lockscreen: assert SKIPPED — no hwnd");
        return;
    }
    let h = HWND(hl as _);
    unsafe {
        if !IsWindow(h).as_bool() {
            log("lockscreen: assert SKIPPED — hwnd dead");
            return;
        }
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let ht = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let fg = GetForegroundWindow().0 as isize;
        let needs_focus = fg != hl;
        use windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW;
        let _ = SetWindowPos(
            h,
            HWND_TOPMOST,
            x,
            y,
            w,
            ht,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );
        if needs_focus {
            if let Some(win) = app.get_webview_window("lockscreen") {
                let _ = win.set_focus();
            }
        }
        let hb = crate::util::lock_shared(&st.0).hwnd_bar;
        if hb != 0 {
            let hb = HWND(hb as _);
            if IsWindow(hb).as_bool() && IsWindowVisible(hb).as_bool() {
                let _ = ShowWindow(hb, SW_HIDE);
                log("lockscreen: hushbar parked for the lock");
            }
        }
        // Secondary bars too: they are TOPMOST on their own monitors, and
        // the beat was re-raising them above the overlay — a live taskbar
        // on a locked desktop is an unlocked desktop with extra steps.
        let sec: Vec<isize> = {
            let s = crate::util::lock_shared(&st.0);
            s.sec_bars.iter().map(|b| b.hwnd).collect()
        };
        let mut parked = 0usize;
        for id in sec {
            if id == 0 {
                continue;
            }
            let h = HWND(id as _);
            if IsWindow(h).as_bool() && IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
                parked += 1;
            }
        }
        if parked > 0 {
            log(&format!(
                "lockscreen: {} secondary bar(s) parked for the lock",
                parked
            ));
        }
    }
}

pub unsafe fn beat(app: &AppHandle, s: &mut crate::shared::Shared, beats: u64) {
    if !LOCKED.load(Ordering::SeqCst) {
        return;
    }
    let hl = s.hwnd_lock;
    if hl == 0 {
        return;
    }
    let h = HWND(hl as _);
    if !IsWindow(h).as_bool() {
        return;
    }
    use windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE;
    let visible = IsWindowVisible(h).as_bool();
    let fg = GetForegroundWindow().0 as isize;
    let fg_topmost = fg != 0
        && (windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            HWND(fg as _),
            GWL_EXSTYLE,
        )
            & windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST.0 as isize)
            != 0;
    let foreign_above = fg != hl && fg != 0 && fg_topmost;
    let periodic = beats % FULL_ASSERT_EVERY == 0;
    if !(periodic || !visible || foreign_above) {
        return;
    }
    // One assertion in flight at a time; the next beat re-checks anyway.
    if ASSERT_QUEUED.swap(true, Ordering::SeqCst) {
        return;
    }
    let why = if !visible {
        "overlay hidden"
    } else if foreign_above {
        "foreign topmost foreground"
    } else {
        "periodic"
    };
    let a2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        main_assert(&a2);
        ASSERT_QUEUED.store(false, Ordering::SeqCst);
        log(&format!("lockscreen: beat assert ({why})"));
    });
}
