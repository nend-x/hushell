//! cursor.rs — the shell's own cursor.
//!
//! The dot-grid family, round 2.1 (the fallback round):
//!   BASIC     — a white-outlined CIRCLE (translucent slate fill, soft
//!               baked shadow). Static on purpose: the round-1 glint read
//!               as "some weird blue thing moving behind the cursor".
//!   CLICKABLE — the white-outlined SQUARE rides the system hand role; the
//!               shell's webviews map every CSS cursor:pointer to OCR_HAND,
//!               so the hover morph costs zero IPC.
//!   TEXT      — the family-built I-beam.
//!   LOADING   — a SPINNING TRIANGLE on OCR_APPSTARTING, delivered as a
//!               self-animating .ani (frames inside a proper LIST('fram')
//!               chunk, rates in jiffies). If the parser ever rejects the
//!               .ani again, the static rest-frame triangle takes the role.
//!
//! The two failure modes this round closes — together they answer "did it
//! fall back?": YES, silently, all four roles at once.
//!
//!   1. ONE BAD ROLE KILLED THE SET. Round 2 bailed before ANY install when
//!      loading.ani failed to load — and it DID fail: the writer had put
//!      the frames in a bare `fram` chunk, which the RIFF parser skips
//!      (frames must live in LIST('fram')), so LoadImageW returned NULL and
//!      the early-return took circle/square/ibeam down with it while the
//!      log message claimed only the loading cursor stayed stock. Now every
//!      role installs or fails alone, and the loading role degrades to the
//!      static triangle instead of vanishing.
//!
//!   2. NO RECOVERY FROM A SCHEME RELOAD. The swap is not protected: any
//!      SPI_SETCURSORS from outside (a game's mode switch, a theme change,
//!      a driver reset) wipes it mid-session. Round 1 masked that with its
//!      motion loop; round 2 has none. The host window calls rearm() on the
//!      scheme-bearing broadcasts — the files on disk are the masters, and
//!      every re-arm consumes fresh handles.
//!
//! The OPTION GATE (config: cursor_custom, default on): boot(off) heals a
//! stale swap a killed session may have left behind and then keeps hands
//! off — stock scheme for the whole session. Toggling the option live goes
//! through apply(on): install() re-runs the boot path (snapshot first when
//! none is alive), restore() hands the originals back. rearm() no-ops
//! whenever the set is not ACTIVE, so an external scheme reload survives
//! untouched while the option is off.
//!
//! The laws of this module:
//!
//! 1. SetSystemCursor CONSUMES the handle it is given. Every install hands
//!    over a handle freshly loaded from disk; the ONLY handles kept alive
//!    all session are the snapshotted originals — private CopyIcon copies,
//!    because the shared handles LoadCursorW returns must never reach
//!    SetSystemCursor.
//! 2. THE SESSION MUST HEAL ITSELF. Boot FIRST reloads the scheme from the
//!    registry (SPI_SETCURSORS — undoes any stale swap), THEN snapshots the
//!    user's true cursors, THEN installs ours; rearm() repeats the install
//!    whenever the scheme is reloaded over us. Exit restores the originals
//!    exactly.
//!
//! The state-mutex law (AGENTS.md) is respected by construction: this
//! module never touches AppShared — its statics are self-owned, and rearm
//! runs on the host window's thread without taking any lock.

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    CopyIcon, HCURSOR, IDC_APPSTARTING, IDC_ARROW, IDC_HAND, IDC_IBEAM, IMAGE_CURSOR,
    LR_LOADFROMFILE, LoadCursorW, LoadImageW, OCR_APPSTARTING, OCR_HAND, OCR_IBEAM, OCR_NORMAL,
    SetSystemCursor, SPI_SETCURSORS, SYSTEM_CURSOR_ID, SystemParametersInfoW,
};

use crate::util::log;

static CIRCLE: &[u8] = include_bytes!("../cursors/circle.cur");
static SQ: &[u8] = include_bytes!("../cursors/sq.cur");
static IBEAM: &[u8] = include_bytes!("../cursors/ibeam.cur");
static LOADING: &[u8] = include_bytes!("../cursors/loading.ani");
static LOADING_STATIC: &[u8] = include_bytes!("../cursors/loading_static.cur");

static ACTIVE: AtomicBool = AtomicBool::new(false);
static LAST_REARM: AtomicU64 = AtomicU64::new(0);
/// Serializes install/restore: the options switch can be flipped faster
/// than a worker thread finishes, and an install racing a restore would
/// interleave the consume-happy SetSystemCursor calls. One at a time.
static APPLY: Mutex<()> = Mutex::new(());

/// the user's true cursors, snapshotted before the first install. At
/// restore they are handed to SetSystemCursor — which consumes them — on
/// purpose: they die exactly where their job ends.
struct Originals {
    normal: Option<HCURSOR>,
    ibeam: Option<HCURSOR>,
    hand: Option<HCURSOR>,
    loading: Option<HCURSOR>,
}
static ORIG: Mutex<Option<Originals>> = Mutex::new(None);
// HCURSOR is a raw-pointer wrapper (not Send by default); cursor handles are
// not thread-affine — moving them between our own threads is sound.
unsafe impl Send for Originals {}

unsafe fn snapshot_originals() -> Originals {
    let to_cur = |h: windows::Win32::UI::WindowsAndMessaging::HICON| HCURSOR(h.0);
    let snap = |id: PCWSTR| LoadCursorW(None, id).ok().and_then(|h| CopyIcon(h).ok().map(to_cur));
    Originals {
        normal: snap(IDC_ARROW),
        ibeam: snap(IDC_IBEAM),
        hand: snap(IDC_HAND),
        loading: snap(IDC_APPSTARTING),
    }
}

/// Where the extracted cursors live (per-user, like the hook companions).
fn dir() -> PathBuf {
    std::env::temp_dir().join("hushell-cursors")
}

fn extract() -> Option<PathBuf> {
    let d = dir();
    std::fs::create_dir_all(&d).ok()?;
    std::fs::write(d.join("circle.cur"), CIRCLE).ok()?;
    std::fs::write(d.join("sq.cur"), SQ).ok()?;
    std::fs::write(d.join("ibeam.cur"), IBEAM).ok()?;
    std::fs::write(d.join("loading.ani"), LOADING).ok()?;
    std::fs::write(d.join("loading_static.cur"), LOADING_STATIC).ok()?;
    Some(d)
}

/// LoadImageW with IMAGE_CURSOR loads .cur AND .ani (it is what
/// LoadCursorFromFile does underneath).
fn load_cursor(path: &PathBuf) -> Option<HCURSOR> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let h = LoadImageW(
            None,
            PCWSTR(wide.as_ptr()),
            IMAGE_CURSOR,
            0,
            0,
            LR_LOADFROMFILE,
        )
        .ok()?;
        Some(HCURSOR(h.0))
    }
}

/// one arm of the law: hand a FRESH file-loaded handle to the role.
/// SetSystemCursor consumes it — nothing here is ever reused.
fn swap_in(h: HCURSOR, role: SYSTEM_CURSOR_ID) {
    unsafe {
        let _ = SetSystemCursor(h, role);
    }
}

/// Boot, gated by the cursor_custom option (default on). Runs on its own
/// thread either way — the shell's first frame never waits on it. Off: heal
/// a stale swap a killed session may have left behind, then hands off —
/// the stock scheme stays for the whole session (rearm() no-ops too).
fn boot_impl(enabled: bool) {
    if !enabled {
        unsafe {
            if SystemParametersInfoW(SPI_SETCURSORS, 0, None, Default::default()).is_ok() {
                log("cursor: system scheme reloaded (stale swap healed)");
            }
        }
        log("cursor: custom off — stock scheme stays");
        return;
    }
    install();
}

/// Boot: heal → extract → load → snapshot originals → install, per role.
pub fn boot(enabled: bool) {
    std::thread::Builder::new()
        .name("cursor".into())
        .spawn(move || {
            crate::util::register_thread("cursor");
            boot_impl(enabled);
        })
        .ok();
}

/// The options switch flipped live: on installs the set (or re-installs it
/// over an already-active one), off restores the user's originals. Both
/// legs run off the invoke thread — disk I/O, LoadImageW, four system
/// cursor swaps.
pub fn apply(on: bool) {
    std::thread::Builder::new()
        .name("cursor-apply".into())
        .spawn(move || {
            crate::util::register_thread("cursor-apply");
            if on {
                install();
            } else {
                restore();
            }
        })
        .ok();
}

/// Extract → load every role from disk → snapshot the true originals (only
/// when none is alive — a re-install over an ACTIVE set keeps the boot-time
/// originals, ours are already on the roles) → per-role install: one bad
/// load must never take the set down.
fn install() {
    let _g = APPLY.lock().unwrap_or_else(|e| e.into_inner());
    let d = match extract() {
        Some(d) => d,
        None => {
            log("cursor: extraction FAILED — stock cursors stay");
            return;
        }
    };
    let circle = load_cursor(&d.join("circle.cur"));
    let sq = load_cursor(&d.join("sq.cur"));
    let ibeam = load_cursor(&d.join("ibeam.cur"));
    // The .ani may legitimately lose to a picky parser — the static
    // rest frame is the DEGRADED role, never a reason to drop the
    // set (round 2's abort here left the user on the stock arrow).
    let loading_ani = load_cursor(&d.join("loading.ani"));
    let loading_static = load_cursor(&d.join("loading_static.cur"));
    if circle.is_none() {
        log("cursor: circle load FAILED — the pointer role stays stock");
    }
    if sq.is_none() {
        log("cursor: square load FAILED — the clickable role stays stock");
    }
    if ibeam.is_none() {
        log("cursor: ibeam load FAILED — the text role stays stock");
    }
    if loading_ani.is_none() && loading_static.is_none() {
        log("cursor: loading role FAILED — the stock loading cursor stays");
    }
    unsafe {
        // snapshot the true originals (private copies — the shared
        // handles LoadCursorW returns must not be handed to
        // SetSystemCursor, which destroys what it is given); skipped
        // when a live snapshot already exists
        let fresh_snap = if ACTIVE.load(Ordering::SeqCst) {
            None
        } else {
            Some(snapshot_originals())
        };
        let mut installed = 0usize;
        if let Some(h) = circle {
            swap_in(h, OCR_NORMAL);
            installed += 1;
        }
        if let Some(h) = sq {
            swap_in(h, OCR_HAND);
            installed += 1;
        }
        if let Some(h) = ibeam {
            swap_in(h, OCR_IBEAM);
            installed += 1;
        }
        if circle.is_some() && sq.is_some() {
            log("cursor: custom set — circle basic, square on clickables");
        }
        match (loading_ani, loading_static) {
            (Some(h), _) => {
                swap_in(h, OCR_APPSTARTING);
                installed += 1;
                log("cursor: loading state — spinning triangle armed");
            }
            (None, Some(h)) => {
                swap_in(h, OCR_APPSTARTING);
                installed += 1;
                log("cursor: loading.ani rejected — static triangle on the loading role");
            }
            (None, None) => {}
        }
        if installed > 0 {
            if let Some(o) = fresh_snap {
                *ORIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(o);
            }
            ACTIVE.store(true, Ordering::SeqCst);
        } else if fresh_snap.is_some() {
            log("cursor: nothing installed — stock cursors stay");
        }
    }
}

/// Hand the user's true cursors back (SetSystemCursor consumes the
/// snapshots — that IS the restore). Safe to call from any state.
fn restore() {
    let _g = APPLY.lock().unwrap_or_else(|e| e.into_inner());
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut g = ORIG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = g.take() {
        unsafe {
            if let Some(h) = o.normal {
                let _ = SetSystemCursor(h, OCR_NORMAL);
            }
            if let Some(h) = o.ibeam {
                let _ = SetSystemCursor(h, OCR_IBEAM);
            }
            if let Some(h) = o.hand {
                let _ = SetSystemCursor(h, OCR_HAND);
            }
            if let Some(h) = o.loading {
                let _ = SetSystemCursor(h, OCR_APPSTARTING);
            }
        }
        log("cursor: originals restored");
    }
}

/// The scheme was reloaded OVER our swap — a game's mode switch, a theme
/// change, a driver reset: anything calling SPI_SETCURSORS (and its
/// WM_SETTINGCHANGE broadcast, which the host window forwards here; the
/// display-change arm calls us too). Re-extract, reload from disk,
/// re-install: the files are the masters, every arm consumes fresh handles.
/// No re-snapshot — the boot-time originals remain the truth. Debounced to
/// one arm per 2 s: broadcasts also ride on mundane setting changes.
pub fn rearm() {
    if !ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now.saturating_sub(LAST_REARM.swap(now, Ordering::Relaxed)) < 2000 {
        return;
    }
    let d = match extract() {
        Some(d) => d,
        None => return,
    };
    let circle = load_cursor(&d.join("circle.cur"));
    let sq = load_cursor(&d.join("sq.cur"));
    let ibeam = load_cursor(&d.join("ibeam.cur"));
    let loading = load_cursor(&d.join("loading.ani"))
        .or_else(|| load_cursor(&d.join("loading_static.cur")));
    if !ACTIVE.load(Ordering::SeqCst) {
        return; // shutdown raced us — do not resurrect the set
    }
    let mut installed = 0usize;
    if let Some(h) = circle {
        swap_in(h, OCR_NORMAL);
        installed += 1;
    }
    if let Some(h) = sq {
        swap_in(h, OCR_HAND);
        installed += 1;
    }
    if let Some(h) = ibeam {
        swap_in(h, OCR_IBEAM);
        installed += 1;
    }
    if let Some(h) = loading {
        swap_in(h, OCR_APPSTARTING);
        installed += 1;
    }
    if installed > 0 {
        log("cursor: scheme reloaded over us — roles re-armed");
    }
}

/// Exit: restore the user's true cursors exactly (SetSystemCursor consumes
/// the snapshots — that IS the restore). Safe to call twice.
pub fn shutdown() {
    restore();
}
