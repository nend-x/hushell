#![windows_subsystem = "windows"]
//! Tray Fetcher — standalone diagnostic tool (reference implementation,
//! provided by nend-x). Enumerates every icon in the Windows notification
//! area (visible tray + overflow flyout) via TB_GETBUTTON +
//! VirtualAllocEx + ReadProcessMemory, shows them in its own window, and
//! forwards mouse clicks to the real icons so they stay fully functional.
//!
//! Purpose inside hushell: an independent probe for the SAME toolbar-read
//! technique. If this tool shows icons on a machine where hushell's tray
//! does not, the fault is in hushell's integration; if this tool shows
//! nothing either, the machine has no enumerable toolbars (e.g. Win11
//! XAML overflow) and the spy-stream approach is the only way.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;

// ---------- Win32 type aliases ----------
type HWND = isize;
type HANDLE = isize;
type HDC = isize;
type HICON = isize;
type HMENU = isize;
type HBRUSH = isize;
type HINSTANCE = isize;
type HCURSOR = isize;
type HFONT = isize;
type BOOL = i32;
type WndProc = extern "system" fn(HWND, u32, usize, isize) -> isize;

#[repr(C)] #[derive(Clone, Copy)]
struct RECT { left: i32, top: i32, right: i32, bottom: i32 }
#[repr(C)] #[derive(Clone, Copy)]
struct POINT { x: i32, y: i32 }
#[repr(C)]
struct MSG { hwnd: HWND, message: u32, wparam: usize, lparam: isize, time: u32, pt: POINT }
#[repr(C)]
struct PAINTSTRUCT { hdc: HDC, f_erase: BOOL, rc_paint: RECT, f_restore: BOOL, f_inc_update: BOOL, rgb_reserved: [u8; 32] }
#[repr(C)]
struct WNDCLASSEXW {
    cb_size: u32, style: u32, lpfn_wnd_proc: WndProc,
    cb_cls_extra: i32, cb_wnd_extra: i32, h_instance: HINSTANCE,
    h_icon: isize, h_cursor: HCURSOR, hbr_background: HBRUSH,
    lpsz_menu_name: *const u16, lpsz_class_name: *const u16, h_icon_sm: isize,
}
#[repr(C)]
struct TBBUTTON {                       // 32 bytes on x64 (pointers force 8-align)
    i_bitmap: i32, id_command: i32, fs_state: u8, fs_style: u8,
    _pad: [u8; 2], dw_data: *mut c_void, i_string: isize,
}
#[repr(C)]
struct TRAYDATA {                       // 32 bytes on x64
    hwnd: HWND, u_id: u32, u_callback: u32, reserved: [u32; 2], h_icon: HICON,
}

// ---------- FFI ----------
#[link(name = "kernel32")] extern "system" {
    fn OpenProcess(a: u32, inh: BOOL, pid: u32) -> HANDLE;
    fn CloseHandle(h: HANDLE) -> BOOL;
    fn VirtualAllocEx(h: HANDLE, addr: *mut c_void, size: usize, at: u32, prot: u32) -> *mut c_void;
    fn VirtualFreeEx(h: HANDLE, addr: *mut c_void, size: usize, ft: u32) -> BOOL;
    fn ReadProcessMemory(h: HANDLE, base: *const c_void, buf: *mut c_void, n: usize, rd: *mut usize) -> BOOL;
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
}
#[link(name = "psapi")] extern "system" {
    fn GetModuleFileNameExW(h: HANDLE, module: HANDLE, name: *mut u16, size: u32) -> u32;
}
#[link(name = "gdi32")] extern "system" {
    fn CreateSolidBrush(cr: u32) -> HBRUSH;
    fn DeleteObject(obj: isize) -> BOOL;
}
#[link(name = "user32")] extern "system" {
    fn FindWindowW(class: *const u16, title: *const u16) -> HWND;
    fn FindWindowExW(parent: HWND, after: HWND, class: *const u16, title: *const u16) -> HWND;
    fn SendMessageW(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize;
    fn PostMessageW(hwnd: HWND, msg: u32, w: usize, l: isize) -> BOOL;
    fn GetWindowThreadProcessId(hwnd: HWND, pid: *mut u32) -> u32;
    fn RegisterClassExW(wc: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, title: *const u16, style: u32,
                       x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: HMENU,
                       inst: HINSTANCE, param: *mut c_void) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> BOOL;
    fn UpdateWindow(hwnd: HWND) -> BOOL;
    fn GetMessageW(msg: *mut MSG, hwnd: HWND, mn: u32, mx: u32) -> BOOL;
    fn TranslateMessage(msg: *const MSG) -> BOOL;
    fn DispatchMessageW(msg: *const MSG) -> isize;
    fn PostQuitMessage(code: i32);
    fn DestroyWindow(hwnd: HWND) -> BOOL;
    fn LoadCursorW(inst: HINSTANCE, name: *const u16) -> HCURSOR;
    fn BeginPaint(hwnd: HWND, ps: *mut PAINTSTRUCT) -> HDC;
    fn EndPaint(hwnd: HWND, ps: *const PAINTSTRUCT) -> BOOL;
    fn GetClientRect(hwnd: HWND, rc: *mut RECT) -> BOOL;
    fn FillRect(hdc: HDC, rc: *const RECT, brush: HBRUSH) -> i32;
    fn DrawIconEx(hdc: HDC, x: i32, y: i32, icon: HICON, cx: i32, cy: i32,
                  istep: u32, hbr: *const c_void, flags: u32) -> BOOL;
    fn DrawTextW(hdc: HDC, text: *const u16, n: i32, rc: *mut RECT, fmt: u32) -> i32;
    fn SetBkMode(hdc: HDC, mode: i32) -> i32;
    fn SetTextColor(hdc: HDC, color: u32) -> u32;
    fn SetTimer(hwnd: HWND, id: usize, elapse: u32, proc: *const c_void) -> usize;
    fn GetCursorPos(pt: *mut POINT) -> BOOL;
    fn TrackPopupMenu(m: HMENU, flags: u32, x: i32, y: i32, res: i32, hwnd: HWND, rc: *const RECT) -> BOOL;
    fn CreatePopupMenu() -> HMENU;
    fn AppendMenuW(m: HMENU, flags: u32, id: usize, text: *const u16) -> BOOL;
    fn DestroyMenu(m: HMENU) -> BOOL;
    fn SetForegroundWindow(hwnd: HWND) -> BOOL;
    fn InvalidateRect(hwnd: HWND, rc: *const RECT, erase: BOOL) -> BOOL;
    fn MessageBoxW(hwnd: HWND, text: *const u16, title: *const u16, typ: u32) -> i32;
    fn CopyIcon(icon: HICON) -> HICON;
    fn DestroyIcon(icon: HICON) -> BOOL;
    fn CreateFontW(h: i32, w: i32, esc: i32, ori: i32, wt: i32, ital: u32, und: u32,
                   strike: u32, charset: u32, outp: u32, clipp: u32, qual: u32,
                   pitch: u32, face: *const u16) -> HFONT;
    fn SelectObject(hdc: HDC, obj: isize) -> isize;
    fn GetStockObject(idx: i32) -> isize;
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> BOOL;
}

// ---------- Constants ----------
const PROCESS_VM_READ: u32 = 0x0010;
const PROCESS_VM_WRITE: u32 = 0x0020;
const PROCESS_VM_OPERATION: u32 = 0x0008;
const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_RELEASE: u32 = 0x8000;
const PAGE_READWRITE: u32 = 0x04;

const WM_USER: u32 = 0x0400;
const TB_GETBUTTON: u32 = WM_USER + 23;
const TB_BUTTONCOUNT: u32 = WM_USER + 24;
const TB_GETBUTTONTEXTW: u32 = WM_USER + 75;

const WM_CREATE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_SIZE: u32 = 0x0005;
const WM_PAINT: u32 = 0x000F;
const WM_TIMER: u32 = 0x0113;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSEWHEEL: u32 = 0x020A;

const CS_HREDRAW: u32 = 0x0002;
const CS_VREDRAW: u32 = 0x0001;
const WS_OVERLAPPEDWINDOW: u32 = 0x00CF0000;
const WS_VISIBLE: u32 = 0x10000000;
const CW_USEDEFAULT: i32 = -2147483648;
const SW_SHOW: i32 = 5;
const COLOR_WINDOW: isize = 5;
const DI_NORMAL: u32 = 3;
const DT_CENTER: u32 = 0x0001;
const DT_VCENTER: u32 = 0x0004;
const DT_SINGLELINE: u32 = 0x0020;
const DT_END_ELLIPSIS: u32 = 0x8000;
const TPM_RETURNCMD: u32 = 0x0100;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const MB_ICONERROR: u32 = 0x10;
const WHITE_BRUSH: i32 = 0;

const CELL_W: i32 = 110;
const CELL_H: i32 = 84;

// ---------- App state (single-threaded UI, static mut is fine here) ----------
struct TrayIcon {
    owner: HWND,
    id: u32,
    callback: u32,
    icon: HICON,          // our own CopyIcon copy
    label: Vec<u16>,
    tip: Vec<u16>,
    exe: Vec<u16>,
}
static mut ICONS: Vec<TrayIcon> = Vec::new();
static mut SCROLL: i32 = 0;
static mut HOVER: i32 = -1;
static mut FONT: HFONT = 0;
static mut HOVER_BRUSH: HBRUSH = 0;

fn w(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

// ---------- Tray enumeration ----------
unsafe fn collect_from(toolbar: HWND, out: &mut Vec<TrayIcon>) {
    if toolbar == 0 { return; }
    let count = SendMessageW(toolbar, TB_BUTTONCOUNT, 0, 0);
    if count <= 0 { return; }

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(toolbar, &mut pid);
    let proc = OpenProcess(PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION, 0, pid);
    if proc == 0 { return; }

    let remote = VirtualAllocEx(proc, null_mut(), 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() { CloseHandle(proc); return; }

    for i in 0..count {
        if SendMessageW(toolbar, TB_GETBUTTON, i as usize, remote as isize) == 0 { continue; }

        let mut btn: TBBUTTON = zeroed();
        if ReadProcessMemory(proc, remote as *const c_void,
                             &mut btn as *mut _ as *mut c_void,
                             size_of::<TBBUTTON>(), null_mut()) == 0 { continue; }
        if btn.dw_data.is_null() { continue; }

        let mut td: TRAYDATA = zeroed();
        if ReadProcessMemory(proc, btn.dw_data as *const c_void,
                             &mut td as *mut _ as *mut c_void,
                             size_of::<TRAYDATA>(), null_mut()) == 0 { continue; }
        if td.hwnd == 0 || td.h_icon == 0 { continue; }
        if out.iter().any(|ic| ic.owner == td.hwnd && ic.id == td.u_id) { continue; }

        // tooltip text (written by the shell into our remote buffer)
        let mut tip: Vec<u16> = Vec::new();
        let len = SendMessageW(toolbar, TB_GETBUTTONTEXTW, i as usize, remote as isize);
        if len > 0 && (len as usize) < 1024 {
            let n = len as usize;
            let mut buf = vec![0u16; n + 1];
            if ReadProcessMemory(proc, remote as *const c_void,
                                 buf.as_mut_ptr() as *mut c_void,
                                 (n + 1) * 2, null_mut()) != 0 {
                buf.truncate(n);
                tip = buf;
            }
        }

        // owning executable path
        let mut exe: Vec<u16> = Vec::new();
        let mut opid: u32 = 0;
        GetWindowThreadProcessId(td.hwnd, &mut opid);
        if opid != 0 {
            let oproc = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, opid);
            if oproc != 0 {
                let mut buf = vec![0u16; 520];
                let n = GetModuleFileNameExW(oproc, 0, buf.as_mut_ptr(), 520);
                if n > 0 { buf.truncate(n as usize); exe = buf; }
                CloseHandle(oproc);
            }
        }

        let copied = CopyIcon(td.h_icon);
        if copied == 0 { continue; }

        let label = make_label(&tip, &exe);
        out.push(TrayIcon { owner: td.hwnd, id: td.u_id, callback: td.u_callback,
                            icon: copied, label, tip, exe });
    }
    VirtualFreeEx(proc, remote, 0, MEM_RELEASE);
    CloseHandle(proc);
}

fn shorten(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if s.chars().count() > max { format!("{t}…") } else { t }
}

fn make_label(tip: &[u16], exe: &[u16]) -> Vec<u16> {
    let from_tip = if tip.is_empty() { String::new() } else {
        let s = String::from_utf16_lossy(tip);
        s.lines().next().unwrap_or("").trim().to_string()
    };
    if !from_tip.is_empty() { return w(&shorten(&from_tip, 22)); }
    if !exe.is_empty() {
        let s = String::from_utf16_lossy(exe);
        let base = s.rsplit('\\').next().unwrap_or(&s);
        return w(&shorten(base, 22));
    }
    w("(unknown)")
}

unsafe fn refresh_icons() {
    for ic in ICONS.iter() { DestroyIcon(ic.icon); }
    ICONS.clear();

    let c1 = w("Shell_TrayWnd");
    let tray = FindWindowW(c1.as_ptr(), std::ptr::null());
    let c2 = w("TrayNotifyWnd");
    let notify = FindWindowExW(tray, 0, c2.as_ptr(), std::ptr::null());
    let c3 = w("SysPager");
    let pager = FindWindowExW(notify, 0, c3.as_ptr(), std::ptr::null());
    let c4 = w("ToolbarWindow32");
    let toolbar = FindWindowExW(pager, 0, c4.as_ptr(), std::ptr::null());
    collect_from(toolbar, &mut ICONS);

    // overflow / hidden icons flyout
    let c5 = w("NotifyIconOverflowWindow");
    let overflow = FindWindowW(c5.as_ptr(), std::ptr::null());
    let c6 = w("ToolbarWindow32");
    let toolbar2 = FindWindowExW(overflow, 0, c6.as_ptr(), std::ptr::null());
    collect_from(toolbar2, &mut ICONS);

    HOVER = -1;
}

// ---------- Interaction ----------
unsafe fn forward(idx: i32, msgs: &[u32]) {
    if idx < 0 || idx as usize >= ICONS.len() { return; }
    let ic = &ICONS[idx as usize];
    if ic.owner == 0 || ic.callback == 0 { return; }
    for &m in msgs {
        PostMessageW(ic.owner, ic.callback, ic.id as usize, m as isize);
    }
}

unsafe fn hit_test(hwnd: HWND, l: isize) -> i32 {
    let x = (l & 0xFFFF) as i16 as i32;
    let y = (((l >> 16) & 0xFFFF) as i16 as i32) + SCROLL;
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let cols = ((rc.right - rc.left) / CELL_W).max(1);
    let idx = (y / CELL_H) * cols + (x / CELL_W);
    if idx >= 0 && idx < ICONS.len() as i32 { idx } else { -1 }
}

unsafe fn layout_cols(hwnd: HWND) -> i32 {
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    ((rc.right - rc.left) / CELL_W).max(1)
}

unsafe fn clamp_scroll(hwnd: HWND) {
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let cols = layout_cols(hwnd);
    let rows = (ICONS.len() as i32 + cols - 1) / cols;
    let max = (rows * CELL_H - (rc.bottom - rc.top)).max(0);
    SCROLL = SCROLL.clamp(0, max);
}

unsafe fn show_bg_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    let t1 = w("Refresh icons");       AppendMenuW(menu, 0, 100, t1.as_ptr());
    let sep = w("");                   AppendMenuW(menu, 0x800, 0, sep.as_ptr());
    let t2 = w("Exit");                AppendMenuW(menu, 0, 101, t2.as_ptr());
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
    DestroyMenu(menu);
    match cmd {
        100 => { refresh_icons(); InvalidateRect(hwnd, std::ptr::null(), 1); }
        101 => { DestroyWindow(hwnd); }
        _ => {}
    }
}

// ---------- Painting ----------
unsafe fn on_paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);

    FillRect(hdc, &rc, GetStockObject(WHITE_BRUSH) as HBRUSH);
    SelectObject(hdc, FONT);
    SetBkMode(hdc, 1); // TRANSPARENT
    SetTextColor(hdc, 0x00202020);

    let cols = layout_cols(hwnd);
    for (i, ic) in ICONS.iter().enumerate() {
        let i = i as i32;
        let x0 = (i % cols) * CELL_W;
        let y0 = (i / cols) * CELL_H - SCROLL;
        if y0 + CELL_H < 0 || y0 > rc.bottom - rc.top { continue; }

        if i == HOVER {
            let hrc = RECT { left: x0 + 2, top: y0 + 2, right: x0 + CELL_W - 2, bottom: y0 + CELL_H - 2 };
            FillRect(hdc, &hrc, HOVER_BRUSH);
        }
        DrawIconEx(hdc, x0 + (CELL_W - 32) / 2, y0 + 6, ic.icon, 32, 32, 0,
                   std::ptr::null(), DI_NORMAL);
        let mut trc = RECT { left: x0 + 3, top: y0 + 42, right: x0 + CELL_W - 3, bottom: y0 + CELL_H - 4 };
        DrawTextW(hdc, ic.label.as_ptr(), -1, &mut trc,
                  DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
    }
    if ICONS.is_empty() {
        let msg = w("No tray icons found (is explorer running?)");
        let mut trc = rc;
        DrawTextW(hdc, msg.as_ptr(), -1, &mut trc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
    EndPaint(hwnd, &ps);
}

// ---------- Window procedure ----------
extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: usize, l: isize) -> isize {
    unsafe {
        match msg {
            WM_CREATE => {
                let face = w("Segoe UI");
                FONT = CreateFontW(-12, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 0, 0, face.as_ptr());
                HOVER_BRUSH = CreateSolidBrush(0x00F0E6D2);
                SetTimer(hwnd, 1, 1500, std::ptr::null());
                refresh_icons();
                0
            }
            WM_TIMER => {
                let old = ICONS.len();
                refresh_icons();
                if ICONS.len() != old { clamp_scroll(hwnd); }
                InvalidateRect(hwnd, std::ptr::null(), 1);
                0
            }
            WM_PAINT => { on_paint(hwnd); 0 }
            WM_MOUSEMOVE => {
                let h = hit_test(hwnd, l);
                if h != HOVER { HOVER = h; InvalidateRect(hwnd, std::ptr::null(), 1); }
                0
            }
            WM_LBUTTONDOWN => { SetCapture(hwnd); 0 }
            WM_LBUTTONUP => {
                ReleaseCapture();
                forward(hit_test(hwnd, l), &[WM_LBUTTONDOWN, WM_LBUTTONUP]);
                0
            }
            WM_LBUTTONDBLCLK => { forward(hit_test(hwnd, l), &[WM_LBUTTONDBLCLK]); 0 }
            WM_RBUTTONUP => {
                let idx = hit_test(hwnd, l);
                if idx >= 0 {
                    // native app menu appears — full tray functionality
                    forward(idx, &[WM_RBUTTONDOWN, WM_RBUTTONUP]);
                } else {
                    show_bg_menu(hwnd);
                }
                0
            }
            WM_MBUTTONUP => { forward(hit_test(hwnd, l), &[WM_MBUTTONDOWN, WM_MBUTTONUP]); 0 }
            WM_MOUSEWHEEL => {
                let delta = ((wp >> 16) & 0xFFFF) as i16 as i32;
                SCROLL -= delta * CELL_H / 120;
                clamp_scroll(hwnd);
                InvalidateRect(hwnd, std::ptr::null(), 1);
                0
            }
            WM_SIZE => { clamp_scroll(hwnd); InvalidateRect(hwnd, std::ptr::null(), 1); 0 }
            WM_CLOSE => { DestroyWindow(hwnd); 0 }
            WM_DESTROY => {
                for ic in ICONS.iter() { DestroyIcon(ic.icon); }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, l),
        }
    }
}

fn msgbox(text: &str) {
    unsafe {
        let t = w(text);
        let cap = w("Tray Fetcher");
        MessageBoxW(0, t.as_ptr(), cap.as_ptr(), MB_ICONERROR);
    }
}

fn main() {
    unsafe {
        let inst = GetModuleHandleW(std::ptr::null());
        let class = w("TrayFetcherWndClass");
        let wc = WNDCLASSEXW {
            cb_size: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfn_wnd_proc: wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: inst,
            h_icon: 0,
            h_cursor: LoadCursorW(0, 32512 as *const u16), // IDC_ARROW
            hbr_background: (COLOR_WINDOW + 1) as HBRUSH,
            lpsz_menu_name: std::ptr::null(),
            lpsz_class_name: class.as_ptr(),
            h_icon_sm: 0,
        };
        if RegisterClassExW(&wc) == 0 { msgbox("RegisterClassEx failed"); return; }

        let title = w("Tray Fetcher — all notification-area icons");
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(),
                                   WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                                   CW_USEDEFAULT, CW_USEDEFAULT, 880, 520,
                                   0, 0, inst, std::ptr::null_mut());
        if hwnd == 0 { msgbox("CreateWindowEx failed"); return; }
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
