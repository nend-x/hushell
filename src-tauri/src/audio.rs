use serde::Serialize;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eRender, eMultimedia, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
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
