use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::{
    GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::shared::Shared;

#[derive(Clone, Serialize)]
pub struct Stats {
    pub cpu: f64,
    pub ram: u8,
    pub battery: Option<u8>,
    pub charging: bool,
}

const NO_BATTERY: u8 = 128; // BATTERY_FLAG_NO_BATTERY
const BATTERY_UNKNOWN: u8 = 255; // BatteryLifePercent "unknown"

fn cpu_prev() -> &'static Mutex<Option<(u64, u64)>> {
    static CELL: OnceLock<Mutex<Option<(u64, u64)>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

fn cache() -> &'static Mutex<Option<Stats>> {
    static CELL: OnceLock<Mutex<Option<Stats>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

fn ft(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

/// One live sample. Never fails — a failed read degrades to 0 / None, the
/// widget is a glance, not an instrument.
pub fn sample() -> Stats {
    // CPU: busy = total - idle; kernel time INCLUDES idle. The percentage
    // is the DELTA between the last two samples (a boot-fresh first sample
    // reports 0 instead of a fake spike).
    let mut cpu = 0.0;
    unsafe {
        let (mut idle, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() {
            let (i, k, u) = (ft(idle), ft(kernel), ft(user));
            let total = k.saturating_add(u);
            let busy = total.saturating_sub(i);
            let mut prev = cpu_prev().lock().ok();
            if let Some(p) = prev.as_deref_mut() {
                if let Some((pb, pt)) = *p {
                    let dt = total.saturating_sub(pt);
                    let db = busy.saturating_sub(pb);
                    if dt > 0 {
                        cpu = (db as f64 / dt as f64 * 100.0).clamp(0.0, 100.0);
                    }
                }
                *p = Some((busy, total));
            }
        }
    }

    let mut ram = 0u8;
    unsafe {
        let mut m = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut m).is_ok() {
            ram = m.dwMemoryLoad.min(100) as u8;
        }
    }

    // Battery: only when the machine actually HAS one.
    let (mut battery, mut charging) = (None, false);
    unsafe {
        let mut p = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut p).is_ok() {
            let has = p.BatteryFlag & NO_BATTERY == 0 && p.BatteryLifePercent != BATTERY_UNKNOWN;
            if has {
                battery = Some(p.BatteryLifePercent.min(100));
            }
            charging = p.ACLineStatus == 1;
        }
    }

    Stats {
        cpu,
        ram,
        battery,
        charging,
    }
}

/// The newest cached sample (or a fresh one when the heartbeat never ran —
/// e.g. a stats widget created before the first beat landed).
pub fn last() -> Stats {
    if let Ok(g) = cache().lock() {
        if let Some(s) = g.as_ref() {
            return s.clone();
        }
    }
    let s = sample();
    if let Ok(mut g) = cache().lock() {
        *g = Some(s.clone());
    }
    s
}

/// Heartbeat beat: sample + emit every 20 beats (2 s) when a stats widget
/// exists. Cheap when nobody is looking (one API trio, no allocation).
pub unsafe fn beat(app: &AppHandle, s: &mut Shared, beats: u64) {
    if beats % 20 != 0 {
        return;
    }
    if !s.cfg.widgets.iter().any(|w| w.kind == "stats") {
        return;
    }
    let st = sample();
    if let Ok(mut g) = cache().lock() {
        *g = Some(st.clone());
    }
    let _ = app.emit_to("hushtop", "hushtop:stats", &st);
}
