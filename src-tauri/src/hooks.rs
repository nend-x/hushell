//! hooks.rs — the embedded AutoHotkey hook companions.
//!
//! The repo ships two compiled AHK v2 scripts (the owner uploads them as
//! binaries): `lockscreen.exe` — the LOCK-time hooks — and `shell.exe` —
//! the session hooks (the owner's own keyboard choreography, alt+tab
//! included). Both are embedded into the shell binary and extracted to a
//! per-user temp dir; the shell owns their whole lifecycle:
//!
//! * shell.exe      spawned when the shell is open (boot), killed when the
//!                  lockscreen hook takes over, brought back when it lets
//!                  go, killed on shell shutdown.
//! * lockscreen.exe spawned when the LOCKING choreography has fully
//!                  finished (the lock page reports it: `hook_lock_on`,
//!                  fired after the wind farm's fade-in), killed when the
//!                  UNLOCKING choreography has fully finished (hushtop
//!                  reports it: `hook_unlock_done`) — the exact moment the
//!                  session hooks come back.
//!
//! The two never run together: lock_on kills the session pair before the
//! lock pair starts, lock_off does the mirror. PID files next to the
//! extracted exes let a fresh boot reap the pair a crashed session left
//! behind — every reap verifies the process IMAGE PATH first, so a reused
//! PID that belongs to some innocent program is never shot.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::util::log;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where the companions live. The dir is created per-user under %TEMP%
/// (the owner's words: "extract onto temp dir").
fn dir() -> PathBuf {
    std::env::temp_dir().join("hushell-hooks")
}

fn lockscreen_path() -> PathBuf {
    dir().join("lockscreen.exe")
}

fn shell_path() -> PathBuf {
    dir().join("shell.exe")
}

struct HookProc {
    child: std::process::Child,
}

static SHELL_PROC: Mutex<Option<HookProc>> = Mutex::new(None);
static LOCK_PROC: Mutex<Option<HookProc>> = Mutex::new(None);

/// Write the child's pid next to its exe — the breadcrumb a future boot
/// follows to reap this process if the shell dies without a goodbye.
fn stamp_pid(path: &PathBuf, pid: u32) {
    let pf = path.with_extension("pid");
    let _ = std::fs::write(&pf, pid.to_string());
}

fn clear_pid(path: &PathBuf) {
    let _ = std::fs::remove_file(path.with_extension("pid"));
}

/// True when `pid` is alive AND its image is exactly `expect` (case-insensitive
/// full-path match — the guard that makes killing by pid safe).
fn pid_is_our_image(pid: u32, expect: &PathBuf) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(HANDLE(h.0));
        if !ok {
            return false;
        }
        let img = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
        img == expect.to_string_lossy().to_lowercase()
    }
}

/// Kill whatever a previous session left running under one of our extracted
/// exes. Called from boot() BEFORE anything spawns.
fn reap_orphan(path: &PathBuf) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_TERMINATE, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let pf = path.with_extension("pid");
    let Ok(text) = std::fs::read_to_string(&pf) else {
        return;
    };
    let pid: u32 = match text.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            let _ = std::fs::remove_file(&pf);
            return;
        }
    };
    unsafe {
        if let Ok(h) = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            false,
            pid,
        ) {
            if pid_is_our_image(pid, path) {
                let _ = TerminateProcess(h, 1);
                log(&format!(
                    "hooks: reaped orphan pid {pid} ({})",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            let _ = CloseHandle(h);
        }
    }
    let _ = std::fs::remove_file(&pf);
}

/// Extract one companion. The cached copy must be BYTE-IDENTICAL to the one
/// embedded in this build — the old guard compared file SIZES only, and two
/// compiled AHK builds landed on the exact same byte count (989184), so a
/// stale cached shell.exe survived every re-upload and the owner's alt+tab
/// stayed blocked. The guard is a SHA-512 comparison now.
fn extract(name: &str, bytes: &[u8]) -> Option<PathBuf> {
    use sha2::{Digest, Sha512};
    let dir = dir();
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join(name);
    let mut want = Sha512::new();
    want.update(bytes);
    let want = want.finalize();
    let stale = match std::fs::read(&p) {
        Ok(cur) => {
            let mut h = Sha512::new();
            h.update(&cur);
            h.finalize() != want
        }
        Err(_) => true,
    };
    if stale {
        // A RUNNING exe image can be neither written nor removed on Windows,
        // but it CAN be renamed: write the fresh bytes to a sibling, park the
        // stale copy aside as .old (a busy one included) and move the fresh
        // file into place. Parked leftovers are swept on the next boot.
        let fresh = dir.join(format!("{name}.new"));
        std::fs::write(&fresh, bytes).ok()?;
        if std::fs::rename(&fresh, &p).is_err() {
            let parked = dir.join(format!("{name}.old"));
            let _ = std::fs::remove_file(&parked);
            let _ = std::fs::rename(&p, &parked);
            if std::fs::rename(&fresh, &p).is_err() {
                let _ = std::fs::remove_file(&fresh);
                log(&format!("hooks: {name} replace FAILED — cached copy stayed"));
                return None;
            }
        }
        log(&format!(
            "hooks: {name} replaced — cached copy differed from the embedded bytes"
        ));
    }
    Some(p)
}

/// Sweep the park-aside leftovers of a busy replace (.old / .new siblings
/// next to the companions). Runs at boot before anything spawns.
fn sweep_junk() {
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.ends_with(".old") || name.ends_with(".new") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Start Fresh — the extracted companions go too (exes, parked leftovers and
/// pid crumbs). Only called from the exit path after shutdown() killed both
/// processes; the next boot re-extracts from the bytes this build embeds.
pub fn wipe_cached() {
    let _ = std::fs::remove_dir_all(dir());
}

/// Spawn one companion if it is not already running under our management.
fn spawn(slot: &Mutex<Option<HookProc>>, path: &PathBuf, tag: &str) {
    {
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cur) = g.as_mut() {
            // still alive? nothing to do; a dead child gets replaced below
            if matches!(cur.child.try_wait(), Err(_) | Ok(None)) {
                *g = None; // dead — fall through and respawn
            } else {
                return;
            }
        }
    }
    let spawned = std::process::Command::new(path)
        .current_dir(dir())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    match spawned {
        Ok(child) => {
            stamp_pid(path, child.id());
            log(&format!("hooks: {tag} up (pid {})", child.id()));
            let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
            *g = Some(HookProc { child });
        }
        Err(e) => {
            log(&format!("hooks: {tag} spawn FAILED: {e}"));
        }
    }
}

/// Kill one companion (and its pid breadcrumb).
fn kill(slot: &Mutex<Option<HookProc>>, path: &PathBuf, tag: &str) {
    let child = slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|h| h.child);
    if let Some(mut c) = child {
        let pid = c.id();
        match c.kill() {
            Ok(()) => {
                let _ = c.wait();
                log(&format!("hooks: {tag} killed (pid {pid})"));
            }
            Err(e) => {
                log(&format!("hooks: {tag} kill failed (pid {pid}): {e}"));
            }
        }
    }
    clear_pid(path);
}

// ---------------------------------------------------------------------------
// Lifecycle (the only entry points the rest of the shell may call)
// ---------------------------------------------------------------------------

/// Shell boot: reap crashed-session leftovers, extract both companions,
/// spawn the session pair. Runs on its own thread — the 2 MB extraction
/// must never delay the shell's first frame.
pub fn boot() {
    std::thread::Builder::new()
        .name("hooks-boot".into())
        .spawn(|| {
            crate::util::register_thread("hooks-boot");
            let lp = lockscreen_path();
            let sp = shell_path();
            reap_orphan(&lp);
            reap_orphan(&sp);
            sweep_junk();
            if extract("lockscreen.exe", LOCKSCREEN_EXE).is_none() {
                log("hooks: lockscreen.exe extraction FAILED — lock hooks unavailable");
            }
            if extract("shell.exe", SHELL_EXE).is_none() {
                log("hooks: shell.exe extraction FAILED — session hooks unavailable");
            }
            spawn(&SHELL_PROC, &sp, "shell.exe");
        })
        .ok();
}

/// The lock screen's own hooks take over (the lock page calls hook_lock_on
/// once the locking choreography — icons fade, sweep, UI rise, wind fade-in
/// — has fully finished). The session pair dies first: two hook sets would
/// double-swallow every key.
pub fn lock_on() {
    let sp = shell_path();
    kill(&SHELL_PROC, &sp, "shell.exe");
    let lp = lockscreen_path();
    spawn(&LOCK_PROC, &lp, "lockscreen.exe");
}

/// The unlock choreography is done (hushtop calls hook_unlock_done after
/// the sweep + icon fade): the lock pair dies, the session pair returns.
pub fn lock_off() {
    let lp = lockscreen_path();
    kill(&LOCK_PROC, &lp, "lockscreen.exe");
    let sp = shell_path();
    spawn(&SHELL_PROC, &sp, "shell.exe");
}

/// Shell shutdown — neither companion outlives the shell that owns it.
pub fn shutdown() {
    let lp = lockscreen_path();
    kill(&LOCK_PROC, &lp, "lockscreen.exe");
    let sp = shell_path();
    kill(&SHELL_PROC, &sp, "shell.exe");
}

// The binaries themselves (repo root, uploaded by the owner).
static LOCKSCREEN_EXE: &[u8] = include_bytes!("../../lockscreen.exe");
static SHELL_EXE: &[u8] = include_bytes!("../../shell.exe");
