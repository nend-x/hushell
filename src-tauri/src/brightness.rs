//! Monitor brightness — WMI (internal panels) + DXVA2 high-level monitor
//! configuration (external DDC/CI displays).
//!
//! `get` returns the average of every controllable display (0..=100),
//! `set` drives ALL of them so the single slider in the widget does one
//! obvious thing on every rig. A laptop's internal panel only answers WMI;
//! an external monitor over DP/HDMI only answers DDC/CI — the union covers
//! both worlds the shell runs on.

use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitor, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
};
use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HMONITOR, HDC};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, EOAC_NONE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_NONE, RPC_C_AUTHN_WINNT};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WBEM_GENERIC_FLAG_TYPE,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WbemLocator,
};

use crate::util::log;

// ---------------------------------------------------------------------------
// DXVA2 / physical monitors (external displays)
// ---------------------------------------------------------------------------

struct PhysMon {
    h: windows::Win32::Foundation::HANDLE,
}

impl Drop for PhysMon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyPhysicalMonitor(self.h);
        }
    }
}

unsafe extern "system" fn mon_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<HMONITOR>);
    out.push(hmon);
    BOOL(1)
}

fn physical_monitors() -> Vec<PhysMon> {
    unsafe {
        let mut hmons: Vec<HMONITOR> = Vec::new();
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(mon_cb),
            LPARAM(&mut hmons as *mut Vec<HMONITOR> as isize),
        );
        let mut out = Vec::new();
        let mut total = 0usize;
        for h in hmons {
            let mut n = 0u32;
            if GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n).is_ok() && n > 0 {
                let mut arr: Vec<PHYSICAL_MONITOR> =
                    vec![PHYSICAL_MONITOR::default(); n as usize];
                if GetPhysicalMonitorsFromHMONITOR(h, &mut arr).is_ok() {
                    total += n as usize;
                    for pm in arr {
                        out.push(PhysMon {
                            h: pm.hPhysicalMonitor,
                        });
                    }
                } else {
                    log(&format!(
                        "brightness: GetPhysicalMonitorsFromHMONITOR failed (GLE {}) for one display",
                        windows::Win32::Foundation::GetLastError().0
                    ));
                }
            }
        }
        log(&format!(
            "brightness: enum -> {total} physical monitor(s) on {} display(s)",
            out.len()
        ));
        out
    }
}

/// (min, current, max) of one physical monitor, if it speaks brightness.
/// GetMonitorBrightness returns BOOL as a raw i32 here (non-zero = success) —
/// the old code tested `!= 0` as FAILURE, so every capable display looked
/// unsupported and the DDC/CI path never fired.
fn dxva2_read(pm: &PhysMon) -> Option<(u32, u32, u32)> {
    unsafe {
        let (mut lo, mut cur, mut hi) = (0u32, 0u32, 0u32);
        if GetMonitorBrightness(pm.h, &mut lo, &mut cur, &mut hi) == 0 {
            log(&format!(
                "brightness: dxva2 read refused by a monitor (GLE {}) — no DDC/CI brightness",
                windows::Win32::Foundation::GetLastError().0
            ));
            return None;
        }
        if hi <= lo {
            return None;
        }
        Some((lo, cur, hi))
    }
}

fn dxva2_write(pm: &PhysMon, v: u32) -> bool {
    unsafe {
        let ok = SetMonitorBrightness(pm.h, v) != 0;
        if !ok {
            log(&format!(
                "brightness: dxva2 write {v} refused (GLE {})",
                windows::Win32::Foundation::GetLastError().0
            ));
        }
        ok
    }
}

// ---------------------------------------------------------------------------
// WMI root\wmi (internal panels + WMI-capable externals)
// ---------------------------------------------------------------------------

/// One WMI pass bound to this thread's apartment. COM init is per-call
/// (cheap S_FALSE when already initialized, same policy as hushtop's).
struct Wmi {
    svc: IWbemServices,
}

fn wbem_flags() -> WBEM_GENERIC_FLAG_TYPE {
    WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0)
}

impl Wmi {
    fn connect() -> Option<Wmi> {
        unsafe {
            // RPC_E_CHANGED_MODE just means an apartment already exists.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            // Process-wide security may already be set (WebView2 / OLE did
            // it) — RPC_E_TOO_LATE then, expected and harmless.
            let _ = CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            );
            let loc: IWbemLocator =
                CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
            let svc = loc
                .ConnectServer(
                    &windows::core::BSTR::from("ROOT\\WMI"),
                    &windows::core::BSTR::new(),
                    &windows::core::BSTR::new(),
                    &windows::core::BSTR::new(),
                    0,
                    &windows::core::BSTR::new(),
                    None,
                )
                .ok()?;
            let _ = CoSetProxyBlanket(
                &svc,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHN_NONE,
                windows::core::PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            );
            Some(Wmi { svc })
        }
    }

    fn query(&self, wql: &str) -> Option<IEnumWbemClassObject> {
        unsafe {
            self.svc
                .ExecQuery(
                    &windows::core::BSTR::from("WQL"),
                    &windows::core::BSTR::from(wql),
                    wbem_flags(),
                    None,
                )
                .ok()
        }
    }

    /// (current, max) per WmiMonitorBrightness instance — internal panels
    /// report 0..max with max often 100.
    fn read(&self) -> Vec<(u32, u32)> {
        let Some(en) = self.query(
            "SELECT CurrentBrightness, MaxBrightness FROM WmiMonitorBrightness",
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            let mut objs: [Option<IWbemClassObject>; 1] = [None];
            let mut got = 0u32;
            unsafe {
                if en.Next(500, &mut objs, &mut got).is_err() || got == 0 {
                    break;
                }
            }
            let Some(obj) = objs[0].take() else { continue };
            let cur = obj_prop_u32(&obj, "CurrentBrightness");
            let max = obj_prop_u32(&obj, "MaxBrightness").unwrap_or(100).max(1);
            if let Some(cur) = cur {
                out.push((cur.min(max), max));
            }
        }
        out
    }

    /// WmiSetBrightness on every WmiMonitorBrightnessMethods instance.
    fn write(&self, percent: u32) -> usize {
        let Some(en) = self.query("SELECT * FROM WmiMonitorBrightnessMethods") else {
            return 0;
        };
        unsafe {
            // Class object -> GetMethod -> fresh in-parameter instance.
            let mut cls: Option<IWbemClassObject> = None;
            if self
                .svc
                .GetObject(
                    &windows::core::BSTR::from("WmiMonitorBrightnessMethods"),
                    WBEM_GENERIC_FLAG_TYPE(0),
                    None,
                    Some(&mut cls),
                    None,
                )
                .is_err()
            {
                return 0;
            }
            let Some(cls) = cls else { return 0 };
            let mut in_def: Option<IWbemClassObject> = None;
            let mut out_def: Option<IWbemClassObject> = None;
            if cls
                .GetMethod(
                    &windows::core::BSTR::from("WmiSetBrightness"),
                    0,
                    &mut in_def,
                    &mut out_def,
                )
                .is_err()
            {
                return 0;
            }
            let Some(in_def) = in_def else { return 0 };
            let Ok(in_inst) = in_def.SpawnInstance(0) else { return 0 };
            let zero = windows::core::VARIANT::from(0u32);
            let bright = windows::core::VARIANT::from(percent);
            if in_inst
                .Put(&windows::core::BSTR::from("Timeout"), 0, &zero, 0)
                .is_err()
            {
                return 0;
            }
            if in_inst
                .Put(&windows::core::BSTR::from("Brightness"), 0, &bright, 0)
                .is_err()
            {
                return 0;
            }
            let mut n = 0usize;
            loop {
                let mut objs: [Option<IWbemClassObject>; 1] = [None];
                let mut got = 0u32;
                if en.Next(500, &mut objs, &mut got).is_err() || got == 0 {
                    break;
                }
                let Some(obj) = objs[0].take() else { continue };
                let Some(path) = obj_prop_bstr(&obj, "__PATH") else { continue };
                if self
                    .svc
                    .ExecMethod(
                        &path,
                        &windows::core::BSTR::from("WmiSetBrightness"),
                        WBEM_GENERIC_FLAG_TYPE(0),
                        None,
                        &in_inst,
                        None,
                        None,
                    )
                    .is_ok()
                {
                    n += 1;
                }
            }
            n
        }
    }
}

fn obj_prop_u32(obj: &IWbemClassObject, name: &str) -> Option<u32> {
    unsafe {
        let mut v = windows::core::VARIANT::default();
        obj.Get(
            &windows::core::BSTR::from(name),
            0,
            &mut v,
            None,
            None,
        )
        .ok()?;
        u32::try_from(&v).ok()
    }
}

fn obj_prop_bstr(obj: &IWbemClassObject, name: &str) -> Option<windows::core::BSTR> {
    unsafe {
        let mut v = windows::core::VARIANT::default();
        obj.Get(
            &windows::core::BSTR::from(name),
            0,
            &mut v,
            None,
            None,
        )
        .ok()?;
        windows::core::BSTR::try_from(&v).ok()
    }
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

fn pct_of(lo: u32, cur: u32, hi: u32) -> u32 {
    if hi <= lo {
        return 0;
    }
    ((cur - lo) * 100) / (hi - lo)
}

/// Average brightness of every controllable display, 0..=100. None = no
/// display on this rig answers either API (a desktop panel without
/// brightness control) — the widget shows a disabled state.
pub fn get() -> Option<u32> {
    let mut vals: Vec<u32> = Vec::new();

    for pm in physical_monitors() {
        if let Some((lo, cur, hi)) = dxva2_read(&pm) {
            log(&format!(
                "brightness: dxva2 reports min {lo} cur {cur} max {hi} ({}%)",
                pct_of(lo, cur, hi)
            ));
            vals.push(pct_of(lo, cur, hi));
        }
    }

    if let Some(w) = Wmi::connect() {
        let rows = w.read();
        if rows.is_empty() {
            log("brightness: wmi query answered but no WmiMonitorBrightness instance (desktop panel?)");
        }
        for (cur, max) in rows {
            log(&format!("brightness: wmi reports cur {cur} max {max}"));
            vals.push((cur * 100 / max.max(1)).min(100));
        }
    } else {
        log("brightness: wmi connect failed (ROOT\\WMI unreachable)");
    }

    if vals.is_empty() {
        log("brightness: get -> none (no display answered either API)");
        None
    } else {
        let v = (vals.iter().sum::<u32>() / vals.len() as u32).min(100);
        log(&format!(
            "brightness: get -> {v}% (avg of {} source(s))",
            vals.len()
        ));
        Some(v)
    }
}

/// Drive EVERY controllable display to `percent`. Returns how many
/// displays actually took the value.
pub fn set(percent: u32) -> usize {
    let pct = percent.min(100);
    let mut n = 0usize;

    for pm in physical_monitors() {
        if let Some((lo, _, hi)) = dxva2_read(&pm) {
            if hi > lo {
                let v = lo + (hi - lo) * pct / 100;
                if dxva2_write(&pm, v) {
                    n += 1;
                }
            }
        }
    }
    let dx = n;

    if let Some(w) = Wmi::connect() {
        n += w.write(pct);
    }

    log(&format!(
        "brightness: set {pct}% -> {n} display(s) took it (dxva2 {dx}, wmi {})",
        n - dx
    ));
    n
}

// ---------------------------------------------------------------------------
// Display power — the off/on switch the brightness registers never had
// ---------------------------------------------------------------------------

/// Power the display(s) via the classic SC_MONITORPOWER broadcast — the
/// same channel Windows itself uses for idle display sleep. lParam 2 = off,
/// -1 = on. All monitors at once (the broadcast carries no display target)
/// and any input wakes the screen — both by design: this serves the
/// lockscreen burn-protection "disable monitor" mode, where waking on the
/// first keypress or mouse move is exactly the wanted behavior. A value
/// the brightness slider can never reach: luminance 0 is minimum
/// brightness, never a panel off.
pub fn monitor_power(on: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_SYSCOMMAND,
    };
    // HWND_BROADCAST (0xffff), WM_SYSCOMMAND (0x0112), SC_MONITORPOWER
    // (0xF170). SendMessageTimeoutW so a hung window can never hang the
    // caller; SMTO_ABORTIFHUNG + 2 s cap.
    let (lparam, what) = if on { (-1i64, "on") } else { (2i64, "off") };
    // non-zero LRESULT = at least one recipient processed the message
    let res = unsafe {
        SendMessageTimeoutW(
            windows::Win32::Foundation::HWND(0xffff as _),
            WM_SYSCOMMAND,
            windows::Win32::Foundation::WPARAM(0xF170),
            windows::Win32::Foundation::LPARAM(lparam as isize),
            SMTO_ABORTIFHUNG,
            2000,
            None,
        )
    };
    log(&format!(
        "display: monitor power {what} (SC_MONITORPOWER broadcast, delivered {})",
        res.0 != 0
    ));
}
