//! shell:startup self-registration.
//!
//! The switch drops a real .lnk for this exe into the user's Startup
//! folder (the folder `shell:startup` opens) — the one place Windows
//! launches from at logon without admin rights or registry edits.
//! Authoring a .lnk by hand means IShellLink COM plumbing; a hidden
//! PowerShell WScript.Shell call is the pragmatic author, and it only
//! ever runs on a state change (boot reconcile miss / switch flip),
//! never on a hot path.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Startup, KNOWN_FOLDER_FLAG};

use crate::util::log;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// <Startup>\hushell.lnk — the one shortcut this module owns.
pub fn lnk_path() -> Option<PathBuf> {
    unsafe {
        let pwstr = SHGetKnownFolderPath(
            &FOLDERID_Startup,
            KNOWN_FOLDER_FLAG(0),
            HANDLE(std::ptr::null_mut()),
        )
        .ok()?;
        if pwstr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *pwstr.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(pwstr.0, len);
        let s = String::from_utf16_lossy(slice);
        CoTaskMemFree(Some(pwstr.0 as *mut core::ffi::c_void));
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s).join("hushell.lnk"))
        }
    }
}

/// True when the shortcut is in place right now.
pub fn is_present() -> bool {
    lnk_path().map(|p| p.exists()).unwrap_or(false)
}

fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// Create the shortcut (hidden PowerShell, WScript.Shell). Returns true
/// when the lnk exists afterwards.
pub fn create() -> bool {
    let Some(lnk) = lnk_path() else {
        log("startup: cannot resolve the Startup folder");
        return false;
    };
    if lnk.exists() {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else {
        log("startup: current_exe failed");
        return false;
    };
    let Some(exe) = exe.to_str() else {
        log("startup: exe path is not valid unicode");
        return false;
    };
    let Some(workdir) = std::path::Path::new(exe).parent().and_then(|p| p.to_str()) else {
        log("startup: exe has no parent dir");
        return false;
    };
    let Some(lnk) = lnk.to_str() else {
        log("startup: lnk path is not valid unicode");
        return false;
    };
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');\
         $s.TargetPath='{}';$s.WorkingDirectory='{}';$s.Description='hushell shell';$s.Save()",
        ps_quote(lnk),
        ps_quote(exe),
        ps_quote(workdir),
    );
    let ok = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| {
            if !o.status.success() {
                log("startup: powershell author failed");
                false
            } else {
                is_present()
            }
        })
        .unwrap_or(false);
    log(&format!("startup: create -> {ok}"));
    ok
}

/// Remove the shortcut. Returns true when it is gone afterwards.
pub fn remove() -> bool {
    let Some(lnk) = lnk_path() else {
        return true;
    };
    if !lnk.exists() {
        return true;
    }
    let ok = std::fs::remove_file(&lnk).is_ok();
    log(&format!("startup: remove -> {ok}"));
    ok
}

/// Make the Startup folder match the switch. `on` = create if missing,
/// off = remove if present. Never panics; every outcome is logged.
pub fn reconcile(on: bool) {
    if on {
        if is_present() {
            log("startup: on — shortcut already in place");
        } else if create() {
            log("startup: on — dropped into shell:startup");
        }
    } else if is_present() {
        if remove() {
            log("startup: off — shortcut removed");
        }
    } else {
        log("startup: off — nothing to remove");
    }
}
