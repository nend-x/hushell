use serde::Serialize;
use windows::core::{Interface, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eRender, eMultimedia, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionEnumerator,
    IAudioSessionManager2, ISimpleAudioVolume, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::util::log;

fn com_mta() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn endpoint() -> windows::core::Result<IAudioEndpointVolume> {
    unsafe {
        let enumr: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = enumr.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
    }
}

pub fn master_get() -> Result<f32, String> {
    com_mta();
    let v = endpoint()
        .and_then(|e| unsafe { e.GetMasterVolumeLevelScalar() })
        .map_err(|e| e.to_string())?;
    Ok(v.clamp(0.0, 1.0))
}

pub fn master_set(v: f32) -> Result<(), String> {
    com_mta();
    let v = v.clamp(0.0, 1.0);
    endpoint()
        .and_then(|e| unsafe { e.SetMasterVolumeLevelScalar(v, std::ptr::null()) })
        .map_err(|e| e.to_string())
}

pub fn mute_get() -> Result<bool, String> {
    com_mta();
    let m = endpoint()
        .and_then(|e| unsafe { e.GetMute() })
        .map_err(|e| e.to_string())?;
    Ok(m.as_bool())
}

pub fn mute_set(m: bool) -> Result<(), String> {
    com_mta();
    endpoint()
        .and_then(|e| unsafe { e.SetMute(m, std::ptr::null()) })
        .map_err(|e| e.to_string())?;
    log(&format!("audio: mute -> {m}"));
    Ok(())
}

pub fn master_step(delta: f32) -> Result<f32, String> {
    com_mta();
    let e = endpoint().map_err(|e| e.to_string())?;
    unsafe {
        let cur = e
            .GetMasterVolumeLevelScalar()
            .map_err(|e| e.to_string())?
            .clamp(0.0, 1.0);
        let next = (cur + delta).clamp(0.0, 1.0);
        if (next - cur).abs() >= f32::EPSILON {
            e.SetMasterVolumeLevelScalar(next, std::ptr::null())
                .map_err(|e| e.to_string())?;
        }
        Ok(next)
    }
}

#[derive(Serialize, Clone)]
pub struct AudioState {
    pub vol: f32,
    pub muted: bool,
}

pub fn state() -> Result<AudioState, String> {
    let vol = master_get()?;
    let muted = mute_get().unwrap_or(false);
    Ok(AudioState { vol, muted })
}

// ---------------------------------------------------------------------------
// Per-app mixer (the volume widget's "Mixer" expander). One row per ACTIVE
// render session on the default output device: name = owning process image,
// id = the session identifier string (stable across re-enumeration within a
// boot), vol = ISimpleAudioVolume 0..1.
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct AudioSession {
    pub id: String,
    pub name: String,
    pub vol: f32,
}

/// Free a COM-allocated PWSTR (GetSessionIdentifier / GetDisplayName output).
unsafe fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.0.add(len) != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len));
    CoTaskMemFree(Some(p.0 as *mut core::ffi::c_void));
    s
}

/// Friendly name for a session: the owning process's image base name, with
/// the system-sounds session (no process) special-cased.
unsafe fn session_name(pid: u32, display: &str) -> String {
    if pid != 0 {
        if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                h,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(h);
            if ok {
                let full = String::from_utf16_lossy(&buf[..len as usize]);
                let base = full.rsplit(['\\', '/']).next().unwrap_or(&full);
                let base = base.strip_suffix(".exe").unwrap_or(base);
                if !base.is_empty() {
                    return base.to_string();
                }
            }
        }
    }
    if pid == 0 && !display.is_empty() {
        return "System sounds".to_string();
    }
    if !display.is_empty() {
        return display.to_string();
    }
    if pid == 0 {
        return "System sounds".to_string();
    }
    "Unknown".to_string()
}

/// Enumerate every ACTIVE render session on the default output device.
fn session_manager() -> windows::core::Result<IAudioSessionManager2> {
    unsafe {
        let enumr: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = enumr.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)
    }
}

pub fn sessions() -> Result<Vec<AudioSession>, String> {
    com_mta();
    let mgr = session_manager().map_err(|e| e.to_string())?;
    let enumr = unsafe { mgr.GetSessionEnumerator() }.map_err(|e| e.to_string())?;
    let count = unsafe { enumr.GetCount() }.map_err(|e| e.to_string())?;
    let mut out: Vec<AudioSession> = Vec::new();
    for i in 0..count {
        let Ok(ctrl) = (unsafe { enumr.GetSession(i) }) else {
            continue;
        };
        // Only sessions that are actually playing something right now.
        let st = unsafe { ctrl.GetState() };
        if st.is_err() || st.unwrap() != AudioSessionStateActive {
            continue;
        }
        let Ok(c2) = ctrl.cast::<IAudioSessionControl2>() else {
            continue;
        };
        let Ok(svol) = ctrl.cast::<ISimpleAudioVolume>() else {
            continue;
        };
        let pid = unsafe { c2.GetProcessId() }.unwrap_or(0);
        let display = match unsafe { ctrl.GetDisplayName() } {
            Ok(p) => unsafe { take_pwstr(p) },
            Err(_) => String::new(),
        };
        // Stable identity: the session identifier (COM-allocated string).
        let mut id = match unsafe { c2.GetSessionIdentifier() } {
            Ok(p) => unsafe { take_pwstr(p) },
            Err(_) => String::new(),
        };
        if id.is_empty() {
            id = format!("pid{pid}#{i}"); // fallback: stable within this enumeration
        }
        let vol = unsafe { svol.GetMasterVolume() }.unwrap_or(0.0);
        out.push(AudioSession {
            id,
            name: unsafe { session_name(pid, &display) },
            vol: vol.clamp(0.0, 1.0),
        });
    }
    Ok(out)
}

pub fn session_set(id: &str, vol: f32) -> Result<(), String> {
    com_mta();
    let mgr = session_manager().map_err(|e| e.to_string())?;
    let enumr = unsafe { mgr.GetSessionEnumerator() }.map_err(|e| e.to_string())?;
    let count = unsafe { enumr.GetCount() }.map_err(|e| e.to_string())?;
    let v = vol.clamp(0.0, 1.0);
    for i in 0..count {
        let Ok(ctrl) = (unsafe { enumr.GetSession(i) }) else {
            continue;
        };
        let Ok(c2) = ctrl.cast::<IAudioSessionControl2>() else {
            continue;
        };
        let mut this = match unsafe { c2.GetSessionIdentifier() } {
            Ok(p) => unsafe { take_pwstr(p) },
            Err(_) => String::new(),
        };
        if this.is_empty() {
            let pid = unsafe { c2.GetProcessId() }.unwrap_or(0);
            this = format!("pid{pid}#{i}");
        }
        if this != id {
            continue;
        }
        let Ok(svol) = ctrl.cast::<ISimpleAudioVolume>() else {
            return Err("session has no volume control".into());
        };
        unsafe { svol.SetMasterVolume(v, std::ptr::null()) }.map_err(|e| e.to_string())?;
        return Ok(());
    }
    Err(format!("no session '{id}' (it may have closed)"))
}
