use std::cell::Cell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::core::{implement, Interface, PCWSTR};
use windows::Win32::Foundation::{BOOL, DRAGDROP_E_INVALIDHWND, HANDLE, HWND, LPARAM, POINT, POINTL};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    CoTaskMemFree, IDataObject, FORMATETC, DVASPECT_CONTENT, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    DoDragDrop, ReleaseStgMedium, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, RevokeDragDrop, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE,
    OleInitialize, OleUninitialize, CF_HDROP,
};
use windows::Win32::System::SystemServices::{
    MK_CONTROL, MK_LBUTTON, MODIFIERKEYS_FLAGS,
};
use windows::Win32::UI::Shell::{
    DragQueryFileW, ILCreateFromPathW, ILFree, SHCreateDataObject, SHFileOperationW,
    SHParseDisplayName, FO_COPY, FO_DELETE, FO_MOVE, HDROP, SHFILEOPSTRUCTW, FOF_ALLOWUNDO,
    FOF_NOCONFIRMATION, FOF_SILENT, FOF_NOERRORUI,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetPropW, SetPropW,
};

use crate::hushtop;
use crate::util::{dpi_for, lock_shared, log};

// DRAGDROP_S_* are S-codes with meaning, not plain success:
const DRAGDROP_S_CANCEL: windows::core::HRESULT = windows::core::HRESULT(0x0004_0101);
const DRAGDROP_S_USEDEFAULTCURSORS: windows::core::HRESULT = windows::core::HRESULT(0x0004_0182);

/// The icon currently being dragged OUT by us (its name). Some = the drop
/// target must treat the drop as a boomerang (reposition, never copy).
static DRAG_NAME: Mutex<Option<String>> = Mutex::new(None);
static DROPPED_OWN: AtomicBool = AtomicBool::new(false);
/// One drag at a time (the JS side is single-pointer anyway; this is the
/// native backstop).
static DRAG_BUSY: AtomicBool = AtomicBool::new(false);
/// Grab offset of the icon being dragged out (cursor minus cell top-left,
/// CSS px). DragOver streams live positions through it so the icon does
/// not jump when the native side takes the pointer over.
static DRAG_OFF: Mutex<(i32, i32)> = Mutex::new((0, 0));
/// Throttle state for the hushtop:drag_move stream (last emit, last x, y).
static LAST_MOVE: Mutex<Option<(Instant, i32, i32)>> = Mutex::new(None);
/// The ONE drop-target COM object, registered on the hushtop top-level
/// window and on every WebView2 child below it. Created once by install;
/// the heartbeat tick re-uses it for late/recreated children.
/// Stored as a RAW leaked pointer: COM interface wrappers are not Send in
/// windows 0.58, but a usize crosses thread bounds freely and is re-wrapped
/// (ManuallyDrop — the refcount is deliberately leaked, the object lives
/// for the whole process) wherever a &IDropTarget is needed.
static OUR_TARGET: Mutex<usize> = Mutex::new(0);

const OWN_PROP: PCWSTR = windows::core::w!("HushDndOwn");

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

/// Register the drop target on the hushtop window and steal every WebView2
/// child below it. MUST run on the main thread (the windows' owner thread).
/// Called from setup() and again after an explorer-restart rebuild (a new
/// HWND tree needs its own registration).
pub fn install(app: &AppHandle) {
    let Some(win) = app.get_webview_window("hushtop") else {
        log("dnd: no hushtop window — drop target not installed");
        return;
    };
    let Ok(t) = win.hwnd() else {
        log("dnd: hushtop has no hwnd — drop target not installed");
        return;
    };
    // tauri's hwnd() carries ITS windows-crate HWND — convert by raw value
    // into OURS (the same conversion hushtop.rs uses to store the value).
    let h = HWND(t.0 as _);
    unsafe {
        // OLE STA on the window's thread. WebView2 usually did it; S_FALSE
        // is Ok and RPC_E_CHANGED_MODE is acceptable too (an apartment with
        // a different concurrency — drops still marshal).
        if let Err(e) = OleInitialize(None) {
            if e.code() != windows::core::HRESULT(0x8001_0106u32 as i32) {
                log(&format!("dnd: OleInitialize failed: {e}"));
            }
        }
        // One target object for every registration (top-level + children).
        // Created once, refcount leaked (process-lifetime object), stored
        // as a raw usize — see OUR_TARGET.
        let raw = {
            let mut slot = OUR_TARGET.lock().unwrap_or_else(|p| p.into_inner());
            if *slot == 0 {
                let t: IDropTarget = HushtopDropTarget {
                    app: app.clone(),
                    src_mask: Cell::new(0),
                }
                .into();
                *slot = t.as_raw() as usize;
                std::mem::forget(t); // leak — never Released
            }
            *slot
        };
        let target =
            std::mem::ManuallyDrop::new(IDropTarget::from_raw(raw as *mut c_void));
        // A rebuilt window reuses the module — revoke first, ignore errors.
        let _ = RevokeDragDrop(h);
        match RegisterDragDrop(h, &*target) {
            Ok(()) => log("dnd: drop target registered on hushtop"),
            Err(e) => log(&format!("dnd: RegisterDragDrop FAILED: {e}")),
        }
        steal_descendants(h, &target);
    }
}

/// Heartbeat hook (the hushtop beat calls this every 10 beats): the
/// WebView2 child windows appear ASYNCHRONOUSLY after boot (the browser
/// process connects late) and are REBUILT on renderer crashes — at
/// install() time the tree may be empty, and a new tree carries no
/// registration of ours. The tick re-walks the current tree on the main
/// thread and re-registers wherever an unmarked window shows up.
/// `top` is the hushtop HWND raw value handed over by the beat (the beat
/// already holds the state lock — this function must NOT re-lock it).
pub fn tick(app: &AppHandle, top: isize) {
    let raw = *OUR_TARGET.lock().unwrap_or_else(|p| p.into_inner());
    if top == 0 || raw == 0 {
        return; // install() never ran — nothing to re-apply
    }
    let _ = app.run_on_main_thread(move || unsafe {
        // Re-wrap the leaked COM pointer (ManuallyDrop: the refcount was
        // leaked once at install and must not be Released here).
        let target = std::mem::ManuallyDrop::new(IDropTarget::from_raw(raw as *mut c_void));
        steal_descendants(HWND(top as _), &target);
    });
}

struct StealCtx<'a> {
    target: &'a IDropTarget,
    n: usize,
}

unsafe extern "system" fn steal_cb(h: HWND, l: LPARAM) -> BOOL {
    let ctx = &mut *(l.0 as *mut StealCtx);
    if !GetPropW(h, OWN_PROP).0.is_null() {
        return BOOL(1); // already ours
    }
    if RevokeDragDrop(h) == Err(DRAGDROP_E_INVALIDHWND.into()) {
        return BOOL(1); // dead window — skip it
    }
    match RegisterDragDrop(h, ctx.target) {
        Ok(()) => {
            if SetPropW(h, OWN_PROP, HANDLE(1usize as *mut c_void)).is_err() {
                // Registration still worked; only the bookkeeping failed.
                log("dnd: own-prop set failed on a child");
            }
            ctx.n += 1;
            log(&format!(
                "dnd: drop target registered on webview child {:#x}",
                h.0 as isize
            ));
        }
        Err(e) => log(&format!(
            "dnd: child register failed on {:#x}: {e}",
            h.0 as isize
        )),
    }
    BOOL(1)
}

unsafe fn steal_descendants(top: HWND, target: &IDropTarget) {
    let mut ctx = StealCtx { target, n: 0 };
    let _ = EnumChildWindows(
        top,
        Some(steal_cb),
        LPARAM(&mut ctx as *mut StealCtx as isize),
    );
    if ctx.n > 0 {
        log(&format!("dnd: stole {} webview drop target(s)", ctx.n));
    }
}

// ---------------------------------------------------------------------------
// drop target (drag TO the desktop)
// ---------------------------------------------------------------------------

#[implement(IDropTarget)]
struct HushtopDropTarget {
    app: AppHandle,
    /// Effects the SOURCE offers (read once at DragEnter; reused to decide
    /// the final effect at DragOver/Drop). Cell: the COM wrapper only ever
    /// gives us &self (no DerefMut by design).
    src_mask: Cell<u32>,
}

/// Live drag-follow payload: the icon (by name) moves to CSS (x, y) while
/// the OLE drag hovers over the desktop.
#[derive(Serialize, Clone)]
struct DragMoveDto {
    name: String,
    x: i32,
    y: i32,
}

impl HushtopDropTarget {
    fn decide(&self, grfkeystate: MODIFIERKEYS_FLAGS) -> DROPEFFECT {
        let boomerang = DRAG_NAME.lock().map(|g| g.is_some()).unwrap_or(false);
        if boomerang {
            return DROPEFFECT_MOVE;
        }
        let src = self.src_mask.get();
        let want = if grfkeystate & MK_CONTROL != MODIFIERKEYS_FLAGS(0) {
            DROPEFFECT_COPY.0
        } else {
            DROPEFFECT_MOVE.0
        };
        if src & want != 0 {
            DROPEFFECT(want)
        } else {
            DROPEFFECT(src & (DROPEFFECT_MOVE.0 | DROPEFFECT_COPY.0))
        }
    }

    fn hushtop_hwnd(&self) -> Option<HWND> {
        let st = self.app.state::<crate::AppShared>();
        let s = lock_shared(&st.0);
        (s.hwnd_hushtop != 0).then_some(HWND(s.hwnd_hushtop as _))
    }

    fn stream_drag_move(&self, pt: &POINTL) {
        let Ok(g) = DRAG_NAME.lock() else { return };
        let Some(name) = g.as_ref() else { return };
        let name = name.clone();
        drop(g);
        let Some(hh) = self.hushtop_hwnd() else { return };
        let (cx, cy) = screen_css(hh, pt.x, pt.y);
        let (dx, dy) = DRAG_OFF.lock().map(|o| *o).unwrap_or((0, 0));
        let (x, y) = (cx - dx, cy - dy);
        let now = Instant::now();
        let mut last = LAST_MOVE.lock().unwrap_or_else(|p| p.into_inner());
        let emit = match *last {
            Some((t, lx, ly)) => {
                now.duration_since(t).as_millis() > 24
                    || (x - lx).abs() > 2
                    || (y - ly).abs() > 2
            }
            None => true,
        };
        if emit {
            *last = Some((now, x, y));
            drop(last);
            let _ = self
                .app
                .emit_to("hushtop", "hushtop:drag_move", DragMoveDto { name, x, y });
        }
    }
}

impl IDropTarget_Impl for HushtopDropTarget_Impl {
    fn DragEnter(
        &self,
        _pdataobj: Option<&IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            let src = *pdweffect;
            self.src_mask.set(src.0);
            *pdweffect = intersect(self.decide(grfkeystate), src);
        }
        Ok(())
    }

    fn DragOver(
        &self,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            let src = *pdweffect;
            *pdweffect = intersect(self.decide(grfkeystate), src);
        }
        self.stream_drag_move(pt);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Option<&IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let files = pdataobj.and_then(hdrop_files);
        let effect = match (files, self.hushtop_hwnd()) {
            (Some(files), Some(hh)) => {
                let (cx, cy) = screen_css(hh, pt.x, pt.y);
                let boomerang = DRAG_NAME.lock().map(|g| g.is_some()).unwrap_or(false);
                if boomerang {
                    let name = DRAG_NAME.lock().ok().and_then(|mut g| g.take());
                    DROPPED_OWN.store(true, Ordering::SeqCst);
                    match name {
                        Some(n) => {
                            let _ = hushtop::cmd_move(&self.app, n, cx, cy);
                            DROPEFFECT_MOVE
                        }
                        None => DROPEFFECT(0),
                    }
                } else {
                    let want_move = self.decide(grfkeystate) == DROPEFFECT_MOVE;
                    match adopt_files(&self.app, &files, cx, cy, want_move) {
                        n if n > 0 => {
                            if want_move {
                                DROPEFFECT_MOVE
                            } else {
                                DROPEFFECT_COPY
                            }
                        }
                        _ => DROPEFFECT(0),
                    }
                }
            }
            _ => DROPEFFECT(0),
        };
        unsafe {
            *pdweffect = effect;
        }
        Ok(())
    }
}

fn intersect(ours: DROPEFFECT, theirs: DROPEFFECT) -> DROPEFFECT {
    DROPEFFECT(ours.0 & theirs.0)
}

/// Screen POINTL -> hushtop CSS px (the icon-grid coordinate space).
fn screen_css(hh: HWND, sx: i32, sy: i32) -> (i32, i32) {
    let mut p = POINT { x: sx, y: sy };
    unsafe {
        let _ = ScreenToClient(hh, &mut p);
        let dpr = (dpi_for(hh) as f64 / 96.0).max(1.0);
        (
            (p.x as f64 / dpr).round() as i32,
            (p.y as f64 / dpr).round() as i32,
        )
    }
}

/// Pull the CF_HDROP file list out of a data object. None = no files there.
fn hdrop_files(data: &IDataObject) -> Option<Vec<String>> {
    unsafe {
        let fmt = FORMATETC {
            cfFormat: CF_HDROP.0,
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        let mut medium = data.GetData(&fmt).ok()?;
        let hd = HDROP(medium.u.hGlobal.0);
        let n = DragQueryFileW(hd, 0xFFFF_FFFF, None);
        let mut out: Vec<String> = Vec::with_capacity(n as usize);
        let mut buf = [0u16; 1024];
        for i in 0..n {
            let len = DragQueryFileW(hd, i, Some(&mut buf)) as usize;
            if len > 0 {
                out.push(String::from_utf16_lossy(&buf[..len]));
            }
        }
        ReleaseStgMedium(&mut medium);
        (!out.is_empty()).then_some(out)
    }
}

fn adopt_files(app: &AppHandle, files: &[String], cx: i32, cy: i32, want_move: bool) -> usize {
    let Some((user, _)) = hushtop::desk_paths() else {
        log("dnd: no desktop folder — drop ignored");
        return 0;
    };
    let mut landed: Vec<String> = Vec::new();
    for raw in files {
        let src = Path::new(raw);
        let Ok(_meta) = std::fs::metadata(src) else {
            log(&format!("dnd: dropped item vanished: {raw}"));
            continue;
        };
        // Already ours? (dragging a desktop item through an Explorer window
        // of the same folder) — never copy over itself, just re-place it.
        if src.parent() == Some(user.as_path()) {
            if let Some(name) = src.file_name().and_then(|n| n.to_str()) {
                landed.push(name.to_string());
            }
            continue;
        }
        let Some(base) = src.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_dir = src.is_dir();
        let mut dest = user.join(base);
        let (stem, ext) = split_ext(base);
        let mut n = 2;
        while dest.exists() {
            dest = user.join(if ext.is_empty() {
                format!("{stem} ({n})")
            } else {
                format!("{stem} ({n}).{ext}")
            });
            n += 1;
        }
        let op = if want_move { FO_MOVE } else { FO_COPY };
        match sh_file_op(op, src, &dest, is_dir) {
            Ok(()) => {
                if let Some(name) = dest.file_name().and_then(|x| x.to_str()) {
                    landed.push(name.to_string());
                }
            }
            Err(e) => log(&format!("dnd: {} failed for {base}: {e}", op_name(op))),
        }
    }
    if landed.is_empty() {
        return 0;
    }
    // Rescan brings the new names onto the grid, then each icon walks to
    // the drop point (cmd_move re-homes on collision, one cell per file).
    {
        let st = app.state::<crate::AppShared>();
        let mut s = lock_shared(&st.0);
        unsafe { hushtop::rescan(app, &mut s) };
    }
    for (k, name) in landed.iter().enumerate() {
        let _ = hushtop::cmd_move(app, name.clone(), cx + k as i32 * hushtop::CELL_W, cy);
        log(&format!("dnd: adopted \"{name}\" at the drop point"));
    }
    landed.len()
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

fn op_name(op: u32) -> &'static str {
    if op == FO_MOVE {
        "move"
    } else {
        "copy"
    }
}

/// SHFileOperationW — handles folders, same/cross-volume moves and the
/// real rename/copy semantics. Flags suppress every dialog: a drop is one
/// motion, not a wizard.
fn sh_file_op(op: u32, from: &Path, to: &Path, _is_dir: bool) -> Result<(), String> {
    let mut from_w = crate::util::to_wide(&from.as_os_str().to_string_lossy());
    from_w.push(0); // pFrom is a double-NUL-terminated LIST
    let mut to_w = crate::util::to_wide(&to.as_os_str().to_string_lossy());
    to_w.push(0);
    let mut op_struct = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: op,
        pFrom: PCWSTR(from_w.as_ptr()),
        pTo: PCWSTR(to_w.as_ptr()),
        fFlags: (FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op_struct) };
    if r != 0 || op_struct.fAnyOperationsAborted.as_bool() {
        return Err(format!("SHFileOperationW -> {r}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// drag source (drag OFF the desktop)
// ---------------------------------------------------------------------------

#[implement(IDropSource)]
struct HushDropSource;

impl IDropSource_Impl for HushDropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if grfkeystate & MK_LBUTTON == MODIFIERKEYS_FLAGS(0) {
            // Left button released — the target decides where it landed.
            return windows::core::HRESULT(0);
        }
        windows::core::HRESULT(0)
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

pub fn drag_out(app: &AppHandle, name: String, dx: i32, dy: i32) -> Result<(), String> {
    if DRAG_BUSY.swap(true, Ordering::SeqCst) {
        return Err("a drag is already in flight".into());
    }
    // Resolve BEFORE any state is left set: a bad name must not wedge the
    // busy flag (the ? below would otherwise return with DRAG_BUSY held).
    let path = match hushtop::item_path(&name) {
        Ok(p) => p,
        Err(e) => {
            DRAG_BUSY.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    if let Err(e) = DRAG_NAME.lock().map(|mut g| *g = Some(name.clone())) {
        DRAG_BUSY.store(false, Ordering::SeqCst);
        return Err(e.to_string());
    }
    if let Ok(mut o) = DRAG_OFF.lock() {
        *o = (dx, dy);
    }
    if let Ok(mut l) = LAST_MOVE.lock() {
        *l = None;
    }
    DROPPED_OWN.store(false, Ordering::SeqCst);

    let app2 = app.clone();
    let path2 = path.clone();
    let name2 = name.clone();
    std::thread::Builder::new()
        .name("hushell-dragout".into())
        .spawn(move || unsafe {
            if let Err(e) = OleInitialize(None) {
                if e.code() != windows::core::HRESULT(0x8001_0106u32 as i32) {
                    log(&format!("dnd: drag thread OleInitialize failed: {e}"));
                    end_drag_markers();
                    cleanup_drag(&app2, &name2, &path2, Err("ole init failed".into()));
                    return;
                }
            }
            let result = run_drag(&path2);
            cleanup_drag(&app2, &name2, &path2, result);
            let _ = OleUninitialize();
        })
        .map_err(|e| {
            end_drag_markers();
            format!("drag thread spawn failed: {e}")
        })?;
    log(&format!("dnd: drag-out started for \"{name}\""));
    Ok(())
}

unsafe fn run_drag(path: &Path) -> Result<DROPEFFECT, String> {
    let child = ILCreateFromPathW(PCWSTR(
        crate::util::to_wide(&path.as_os_str().to_string_lossy()).as_ptr(),
    ));
    if child.is_null() {
        return Err("ILCreateFromPathW failed".into());
    }
    let parent = path
        .parent()
        .map(|p| p.as_os_str().to_string_lossy().to_string())
        .unwrap_or_default();
    let mut parent_pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    if SHParseDisplayName(
        PCWSTR(crate::util::to_wide(&parent).as_ptr()),
        None,
        &mut parent_pidl,
        0,
        None,
    )
    .is_err()
    {
        ILFree(Some(child as *const ITEMIDLIST));
        return Err("SHParseDisplayName failed".into());
    }
    let child_c: *const ITEMIDLIST = child;
    let data: IDataObject = SHCreateDataObject(Some(parent_pidl), Some(&[child_c]), None)
        .map_err(|e| {
            ILFree(Some(child as *const ITEMIDLIST));
            format!("SHCreateDataObject failed: {e}")
        })?;
    let source: IDropSource = HushDropSource.into();
    let mut effect = DROPEFFECT(0);
    let hr = DoDragDrop(
        &data,
        &source,
        DROPEFFECT_COPY | DROPEFFECT_MOVE,
        &mut effect,
    );
    ILFree(Some(child as *const ITEMIDLIST));
    CoTaskMemFree(Some(parent_pidl as *mut core::ffi::c_void));
    if hr.is_err() {
        // DRAGDROP_S_* codes are SUCCESS codes; anything genuinely failing
        // lands here.
        return Err(format!("DoDragDrop failed: {hr:?}"));
    }
    Ok(effect)
}

fn end_drag_markers() {
    if let Ok(mut g) = DRAG_NAME.lock() {
        *g = None;
    }
    DROPPED_OWN.store(false, Ordering::SeqCst);
    DRAG_BUSY.store(false, Ordering::SeqCst);
}

fn rehome_icon(app: &AppHandle, name: &str) {
    let st = app.state::<crate::AppShared>();
    let home = {
        let s = lock_shared(&st.0);
        s.cfg.hushtop_positions.get(name).copied()
    };
    let Some((ox, oy)) = home else { return };
    let _ = hushtop::cmd_move(app, name.to_string(), ox, oy);
    let items = {
        let s = lock_shared(&st.0);
        s.hushtop_items.clone()
    };
    let _ = app.emit("hushtop:icons", &items);
}

/// Recycle-bin delete (folders included). A raw remove_file/remove_dir_all
/// used to obliterate the source for good — a MOVE drag-out of the desktop
/// lost the file forever if the move target later vanished. The shell's own
/// move-out sends the source to the bin; so do we.
unsafe fn recycle_delete(path: &Path) -> Result<(), String> {
    let mut from = crate::util::to_wide(&path.as_os_str().to_string_lossy());
    from.push(0); // double-NUL terminated list
    let mut op = SHFILEOPSTRUCTW {
        hwnd: windows::Win32::Foundation::HWND::default(),
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        ..Default::default()
    };
    let r = SHFileOperationW(&mut op);
    if r != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(format!("SHFileOperationW(FO_DELETE) -> {r}"));
    }
    Ok(())
}

/// Post-drop bookkeeping, then the JS-side gesture teardown. A true MOVE
/// out of the desktop deletes the SOURCE file (the target already copied
/// it — the IDropSource contract); a boomerang never deletes (cmd_move did
/// the re-homing); COPY / cancel / no-accept re-home the icon.
unsafe fn cleanup_drag(
    app: &AppHandle,
    name: &str,
    path: &Path,
    result: Result<DROPEFFECT, String>,
) {
    let own = DROPPED_OWN.load(Ordering::SeqCst);
    match result {
        Ok(effect) => {
            if effect.0 & DROPEFFECT_MOVE.0 != 0 && !own {
                match recycle_delete(path) {
                    Ok(()) => {
                        let st = app.state::<crate::AppShared>();
                        let mut s = lock_shared(&st.0);
                        s.cfg.hushtop_positions.remove(name);
                        s.cfg.save();
                        hushtop::rescan(app, &mut s);
                        log(&format!("dnd: \"{name}\" moved off the desktop (to the recycle bin)"));
                    }
                    Err(e) => {
                        log(&format!("dnd: MOVE cleanup failed for \"{name}\": {e}"));
                        let st = app.state::<crate::AppShared>();
                        let mut s = lock_shared(&st.0);
                        hushtop::rescan(app, &mut s);
                    }
                }
            } else if own {
                log(&format!("dnd: \"{name}\" drop landed back on the desktop"));
            } else {
                // COPY elsewhere (the file stays ours), ESC cancel, or a
                // drop nobody accepted: the icon goes back to its cell.
                log(&format!(
                    "dnd: drag finished without a move (effect={:#x}) — re-homing \"{name}\"",
                    effect.0
                ));
                rehome_icon(app, name);
            }
        }
        Err(e) => {
            log(&format!("dnd: drag failed: {e} — re-homing \"{name}\""));
            rehome_icon(app, name);
        }
    }
    end_drag_markers();
    let _ = app.emit_to("hushtop", "hushtop:drag_ended", name.to_string());
}
