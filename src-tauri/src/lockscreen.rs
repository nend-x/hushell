use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetForegroundWindow, GetWindowRect, IsWindow, IsWindowVisible,
    SetWindowPos, HWND_TOPMOST, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW,
};

use crate::util::log;

/// The one lock truth. Set by lock(), cleared by a verified unlock. Every
/// gate (winhook tap, startmenu::show) and the heartbeat beat read this.
static LOCKED: AtomicBool = AtomicBool::new(false);

/// One main-thread assertion in flight (no queue pile-up: the beat skips
/// while a queued assert has not run yet — the next beat re-checks anyway).
static ASSERT_QUEUED: AtomicBool = AtomicBool::new(false);

const FULL_ASSERT_EVERY: u64 = 10;

/// The desktop page fired boot_reveal_done (its reveal sweep FINISHED — the
/// line is resting at the right edge, the rules fully revealed). The boot
/// lock fires on this signal, not on a fixed sleep: every stage of the boot
/// animation completes before the shell goes behind the password. One shot.
static BOOT_REVEAL_DONE: AtomicBool = AtomicBool::new(false);

/// The lockscreen page has greeted (lockscreen_hello with page =
/// "lockscreen") — its listeners are registered and its DOM is painted.
/// The overlay is only ever revealed to a page that can play the entrance;
/// a hold covers the boot-land window where the page may still be loading.
static LOCK_HELLO_SEEN: AtomicBool = AtomicBool::new(false);

/// The lock is live but the overlay show is HELD (the lockscreen page has
/// not greeted yet — showing it now would put an empty transparent window
/// over the choreography). The hello (or the 1500 ms grace) releases it.
/// While this is up, main_assert positions the overlay WITHOUT the show
/// flag — a beat assert can never defeat the hold by accident.
static LOCK_SHOW_PENDING: AtomicBool = AtomicBool::new(false);

pub const BOOT_FALLBACK_MS: u64 = 6000;
const SHOW_GRACE_MS: u64 = 1500;

pub fn is_locked() -> bool {
    LOCKED.load(Ordering::SeqCst)
}

/// The desktop's boot reveal is DONE (the sweep finished crossing). Lock now
/// — the choreography hands over at the line's resting pose, so the boot
/// never reads as interrupted. Idempotent: the first call wins, the webview
/// cannot double-fire, and a call after the fallback already locked is a
/// quiet no-op.
pub fn boot_reveal_done(app: &AppHandle) {
    if is_locked() {
        return;
    }
    if BOOT_REVEAL_DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    log("lockscreen: boot reveal done — the shell locks as the sweep completes");
    lock(app);
}

/// The boot thread's backstop: the reveal signal never came (a dead or
/// starved webview). Lock anyway — the shell must not stay open past the
/// fallback window, whatever the page did.
pub fn boot_lock_fallback(app: &AppHandle) {
    if is_locked() {
        return;
    }
    log("lockscreen: boot lock fallback — no reveal signal, locking anyway");
    lock(app);
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

/// The start menu options row. Empty password = clear the lock password.
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
    // The desktop takes the stage: every foreign top-level window steps
    // aside (the shell's own windows and the wallpaper hosts are excluded
    // by the Alt-Tab filter). The list is what a verified unlock restores —
    // the boot sweep intentionally records nothing (boot cleanup is not a
    // lock), and an already-iconic window is left alone and not recorded.
    {
        let swept = crate::tasks::minimize_all_except_shell();
        let st = app.state::<crate::AppShared>();
        let mut s = crate::util::lock_shared(&st.0);
        log(&format!(
            "lockscreen: minimized {} window(s) for the lock",
            swept.len()
        ));
        s.lock_minimized = swept;
    }
    // The taskbar does not vanish for the lock — it SLIDES off the bottom
    // edge of its own monitor while the desktop's icon fade plays (the
    // overlay is transparent at this stage, so the slide is the first thing
    // the eye catches). The slide owns the bars: the one main_assert pass
    // below must not park them out from under the animation (later passes
    // park instantly — a bar reappearing mid-lock never waits for art).
    // The PARK-HOLD also starts here: it keeps the heartbeat's bar-geometry
    // beat off the parked bars until the unlock slide takes over (the beat
    // used to pop them home the moment LOCKED cleared — the blink).
    crate::native::set_bars_park_held(true);
    crate::native::arm_initial_bar_slide();
    crate::native::bars_slide_down(app);
    let a2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        // THE HOLD ARMS BEFORE THE ASSERT: main_assert is hold-aware (it
        // positions the overlay WITHOUT SWP_SHOWWINDOW while a show is
        // pending), so the pending flag must be up before it runs — the
        // boot-land path must never reveal the window as a side effect of
        // its own geometry pass.
        let greeted = LOCK_HELLO_SEEN.load(Ordering::SeqCst);
        if !greeted {
            LOCK_SHOW_PENDING.store(true, Ordering::SeqCst);
        }
        main_assert(&a2);
        if greeted {
            // The lockscreen page already greeted (listeners registered, DOM
            // painted) — the normal path, every post-boot lock lands here.
            overlay_show_emit(&a2);
        } else {
            // Boot-land race: the lock fired before the lockscreen page ever
            // greeted. Showing the overlay NOW could put an empty transparent
            // window over the choreography — hold it. The hello releases the
            // show the instant the page is ready; a 1500 ms grace backstop
            // shows it anyway if the page never comes up (degraded but never
            // stuck — the desktop is already locked and swept).
            log("lockscreen: locked (overlay held until the lockscreen page greets)");
            let a3 = a2.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(SHOW_GRACE_MS));
                if LOCK_SHOW_PENDING.swap(false, Ordering::SeqCst) {
                    log("lockscreen: boot land grace elapsed — showing the overlay without a hello");
                    let a4 = a3.clone();
                    let _ = a3.run_on_main_thread(move || overlay_show_emit(&a4));
                }
            });
        }
    });
}

/// The overlay's one true reveal: show, focus, broadcast the show event.
/// Main thread only (every caller arrives via run_on_main_thread). The
/// window is already positioned TOPMOST by main_assert — this is the moment
/// it becomes visible AND the moment the pages hear about the lock, in that
/// order, so the entrance never plays behind glass.
fn overlay_show_emit(app: &AppHandle) {
    LOCK_SHOW_PENDING.store(false, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("lockscreen") {
        let _ = w.show();
        let _ = w.set_focus();
    }
    let _ = app.emit("lockscreen:show", ());
    log("lockscreen: locked (overlay on screen, show event emitted)");
}

/// The pages' listener handshake. Each page invokes lockscreen_hello with
/// its own name once its lock listeners ARE registered (the lockscreen page
/// also proves its DOM is up — the greeting is sent from script bottom).
/// Three duties, in order:
/// 1. RELEASE a held boot-land show — the overlay stayed hidden because this
///    page was still loading; it is ready now, reveal it.
/// 2. No hold and no lock: a plain no-op (a normal alt+l lock minutes later
///    shows the overlay directly from lock()).
/// 3. Locked and no hold: this page booted (or reloaded) INTO a live lock
///    and missed the broadcast show — re-emit it TARGETED at the two
///    choreography pages only, so a broadcast replay can never double-run
///    anyone else. The pages dedupe a show that lands right behind the real
///    one (800 ms window).
pub fn hello(app: &AppHandle, page: &str) {
    if page == "lockscreen" {
        LOCK_HELLO_SEEN.store(true, Ordering::SeqCst);
    }
    if LOCK_SHOW_PENDING.swap(false, Ordering::SeqCst) {
        log("lockscreen: hello — the boot land show released");
        let a2 = app.clone();
        let _ = app.run_on_main_thread(move || overlay_show_emit(&a2));
        return;
    }
    if !is_locked() {
        return;
    }
    let _ = app.emit_to("lockscreen", "lockscreen:show", ());
    let _ = app.emit_to("hushtop", "lockscreen:show", ());
    log("lockscreen: hello — the boot show re-emitted to the pages");
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
    // The return choreography first: the lock UI fades out (this page
    // listens for the event), hushtop sweeps the curtain back left -> right
    // and fades the icons/widgets in — the window itself stays up (its
    // content is transparent by then) so the reveal is never cut short.
    let _ = app.emit("lockscreen:hide", ());
    let a2 = app.clone();
    let _ = std::thread::spawn(move || {
        // The desktop choreography waits for the lock's wind farm to melt
        // out first (0.6 s fade, engine clear at 700 ms — hushtop.js owns
        // that clock), THEN sweeps (1500 ms) and fades the icons/widgets in
        // (900 ms). The transparent overlay goes at 1700 ms — its ink stack
        // (0.4 s) and turbines are long gone by then, so hiding it mid-sweep
        // cuts nothing. The bars are deliberately NOT here: a bar rising
        // while the sweep crosses underneath it reads as a stutter (the eye
        // catches the bar fighting the line), so the slide waits for the
        // whole comeback to finish — see the +3150 step below.
        std::thread::sleep(std::time::Duration::from_millis(1700));
        let a3 = a2.clone(); // a2 stays the dispatcher handle — a3 goes inside
        let _ = a2.run_on_main_thread(move || {
            if let Some(w) = a3.get_webview_window("lockscreen") {
                let _ = w.hide();
            }
            // The overlay is GONE — tell the desktop. hushtop re-asserts its
            // own effect on this event and kicks the shared engine: a loop
            // that stalled while the (transparent) overlay covered the
            // webview restarts the moment the desktop is truly visible
            // again, so "unlock leaves the desktop bare" cannot happen.
            let _ = a3.emit("lockscreen:gone", ());
            log("lockscreen: overlay hidden");
        });
        // The windows the LOCK minimized come back while the icon fade is
        // still playing. The desktop sweep starts at +700 (after the wind
        // fade) and runs 1500 ms, so it ends at +2200 and the icon fade
        // plays 2200 -> 3100; restoring here (+2850) lands ~72 % into that
        // fade — the exact relative position the choreography always used —
        // and only the ones the user has not already re-raised themselves
        // (still iconic).
        std::thread::sleep(std::time::Duration::from_millis(1150));
        let a4 = a2.clone();
        let _ = a2.run_on_main_thread(move || {
            let st = a4.state::<crate::AppShared>();
            let parked: Vec<isize> = {
                let mut s = crate::util::lock_shared(&st.0);
                std::mem::take(&mut s.lock_minimized)
            };
            let back = crate::tasks::restore_windows(&parked);
            log(&format!(
                "lockscreen: {back}/{} parked window(s) back on screen",
                parked.len()
            ));
        });
        // The bars come back LAST — after the sweep (ends +2200) AND the
        // icon fade (ends +3100) have both finished, so the taskbar rises
        // onto a settled desktop instead of fighting the comeback effects
        // (a bar appearing mid-sweep read as a glitch, and the old explicit
        // ShowWindow right here raced the slide thread and could pop the
        // bar at its docked rect before the animation ever started — the
        // slide shows its own bars off-screen first, no help needed).
        std::thread::sleep(std::time::Duration::from_millis(300));
        let a5 = a2.clone();
        let _ = a2.run_on_main_thread(move || {
            crate::native::bars_slide_up(&a5);
            log("lockscreen: comeback done — bars slide home");
        });
    });
    log("lockscreen: unlocked (choreography playing)");
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
        // THE HOLD: while the boot-land show is pending (the lockscreen page
        // has not greeted yet) this assert positions the overlay but never
        // reveals it — SWP_SHOWWINDOW would pop an empty transparent window
        // over the choreography, and the periodic beat asserts would defeat
        // the hold within 100 ms. The show happens exactly once, in
        // overlay_show_emit, when the hello (or the grace) releases it.
        let show_flags = if LOCK_SHOW_PENDING.load(Ordering::SeqCst) {
            SWP_NOACTIVATE
        } else {
            SWP_SHOWWINDOW | SWP_NOACTIVATE
        };
        let _ = SetWindowPos(
            h,
            HWND_TOPMOST,
            x,
            y,
            w,
            ht,
            show_flags,
        );
        if needs_focus && !LOCK_SHOW_PENDING.load(Ordering::SeqCst) {
            if let Some(win) = app.get_webview_window("lockscreen") {
                let _ = win.set_focus();
            }
        }
        let hb = crate::util::lock_shared(&st.0).hwnd_bar;
        if hb != 0 {
            // PARK POLICY — the bars are never hidden for the lock, they are
            // PARKED just below the virtual screen (still shown — hiding an
            // acrylic bar makes the unlock slide blink when the material
            // re-attaches). So this pass only re-parks a bar that is back
            // ON SCREEN (surfaced mid-lock by some re-assert or display
            // change): it moves below the virtual screen again. A bar
            // already parked below the virtual screen intersects nothing
            // and is left alone.
            if crate::native::consume_initial_bar_slide() {
                log("lockscreen: bar park skipped — slide owns the bars");
            } else {
                let hb = HWND(hb as _);
                if IsWindow(hb).as_bool() {
                    crate::native::park_bar_if_on_screen(hb);
                }
            }
        }
        // Secondary bars too: same never-hide park policy.
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
            if IsWindow(h).as_bool() && crate::native::park_bar_if_on_screen(h) {
                parked += 1;
            }
        }
        if parked > 0 {
            log(&format!(
                "lockscreen: {} bar(s) re-parked below the virtual screen",
                parked
            ));
        }
    }
}

/// One full geometry assert, queued to the main thread — for display
/// changes and anything else that must re-span the overlay NOW, not on the
/// next beat. While locked a fresh monitor must never show the desktop.
pub fn assert_full(app: &AppHandle) {
    let a2 = app.clone();
    let _ = app.run_on_main_thread(move || main_assert(&a2));
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
    // A display joined or left: the overlay must span the NEW virtual
    // screen, and it must do it NOW — between the periodic asserts a fresh
    // monitor would show the bare desktop (icons, widgets — a peek).
    let (vx, vy, vw, vh) = (
        GetSystemMetrics(SM_XVIRTUALSCREEN),
        GetSystemMetrics(SM_YVIRTUALSCREEN),
        GetSystemMetrics(SM_CXVIRTUALSCREEN),
        GetSystemMetrics(SM_CYVIRTUALSCREEN),
    );
    let mut rc = windows::Win32::Foundation::RECT::default();
    let fits = GetWindowRect(h, &mut rc).is_ok()
        && rc.left <= vx
        && rc.top <= vy
        && rc.right >= vx + vw
        && rc.bottom >= vy + vh;
    let periodic = beats % FULL_ASSERT_EVERY == 0;
    // Re-park EVERY beat — this IS the global 100 ms heartbeat (cycle.rs,
    // registered as "lock"), no private timer of our own: anything that
    // surfaced since the lock (spawned app windows, self-restoring windows,
    // a display hotplug re-flow) steps aside within one beat, not a second
    // later. The sweep only minimizes VISIBLE foreign windows — the ones
    // already parked are skipped, so the steady-state cost is one
    // enumerate_light pass (the beat already carries one) — and the result
    // is appended (deduped) to the parked list so a verified unlock brings
    // every one of them back.
    {
        let swept = crate::tasks::minimize_all_except_shell();
        if !swept.is_empty() {
            let mut added = 0usize;
            for hw in swept {
                if !s.lock_minimized.contains(&hw) {
                    s.lock_minimized.push(hw);
                    added += 1;
                }
            }
            if added > 0 {
                log(&format!(
                    "lockscreen: beat re-parked {added} surfaced window(s)"
                ));
            }
        }
    }
    if !(periodic || !visible || foreign_above || !fits) {
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
    } else if !fits {
        "virtual screen changed"
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
