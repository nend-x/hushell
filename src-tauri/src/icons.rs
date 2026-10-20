//! Icon extraction from windows + PNG base64 encoding + monochrome dark variant.
//!
//! Quality pipeline: prefer the 256px "jumbo" icon straight from the exe
//! (`SHDefExtractIconW`), fall back to the window's big icon, then the small
//! chain — then alpha-weighted box-downscale to a 64px canonical size. The
//! recolor pass runs AFTER the downscale so the dark variant is derived from
//! a clean, sharp source instead of a tiny blurry one.

use base64::Engine;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HDC,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{ExtractIconExW, SHDefExtractIconW};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassLongPtrW, GetIconInfo, SendMessageTimeoutW, GetWindowThreadProcessId, GCLP_HICON,
    GCLP_HICONSM, ICON_BIG, ICON_SMALL, ICON_SMALL2, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_GETICON,
    HICON, ICONINFO,
};

use crate::util::{del_gdi, log, to_wide};

pub fn get_window_icon(hwnd: HWND) -> Option<(HICON, bool)> {
    unsafe {
        // 1) WM_GETICON (small first, like the taskbar shows)
        for icon_type in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
            let mut result: usize = 0;
            let r = SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                windows::Win32::Foundation::WPARAM(icon_type as usize),
                windows::Win32::Foundation::LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                300,
                Some(&mut result),
            );
            if r.0 != 0 && result != 0 {
                let h = HICON(result as *mut core::ffi::c_void);
                if !h.is_invalid() {
                    return Some((h, false));
                }
            }
        }
        // 2) Class icons
        for idx in [GCLP_HICONSM, GCLP_HICON] {
            let v = GetClassLongPtrW(hwnd, idx);
            if v != 0 {
                let h = HICON(v as *mut core::ffi::c_void);
                if !h.is_invalid() {
                    return Some((h, false));
                }
            }
        }
        // 3) Extract from the exe image
        if let Some((h, owned)) = extract_exe_icon(hwnd) {
            return Some((h, owned));
        }
    }
    None
}

fn extract_exe_icon(hwnd: HWND) -> Option<(HICON, bool)> {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let proc_h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size: u32 = 1024;
        let r = QueryFullProcessImageNameW(
            proc_h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(proc_h);
        if r.is_err() || size == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        let path_w = to_wide(&path);
        let mut hbig = HICON::default();
        let mut hsmall = HICON::default();
        let n = ExtractIconExW(
            windows::core::PCWSTR(path_w.as_ptr()),
            0,
            Some(&mut hbig),
            Some(&mut hsmall),
            1,
        );
        if n > 0 && !hsmall.is_invalid() {
            Some((hsmall, true))
        } else if n > 0 && !hbig.is_invalid() {
            Some((hbig, true))
        } else {
            None
        }
    }
}

pub(crate) fn exe_path_of(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let proc_h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size: u32 = 1024;
        let r = QueryFullProcessImageNameW(
            proc_h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(proc_h);
        if r.is_err() || size == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Extract the exe's jumbo (256px) icon — the highest-fidelity source modern
/// apps ship. `SHDefExtractIconW` needs no COM and reports S_FALSE when the
/// exact size isn't available (still hands us the closest icon), so any
/// success code counts.
fn extract_exe_icon_jumbo(hwnd: HWND) -> Option<(HICON, bool)> {
    let path = exe_path_of(hwnd)?;
    let path_w = to_wide(&path);
    let mut hbig = HICON::default();
    let n = unsafe {
        SHDefExtractIconW(
            windows::core::PCWSTR(path_w.as_ptr()),
            0,
            0,
            Some(&mut hbig),
            None,
            256,
        )
    };
    if n.is_ok() && !hbig.is_invalid() {
        Some((hbig, true))
    } else {
        None
    }
}

pub fn get_window_icon_hq(hwnd: HWND) -> Option<(HICON, bool)> {
    if let Some(pair) = extract_exe_icon_jumbo(hwnd) {
        return Some(pair);
    }
    get_window_icon(hwnd)
}

pub const CANONICAL: i32 = 64;

/// Alpha-weighted area-average (box) downscale. Never upscales — sources
/// smaller than the target are returned unchanged. Premultiplies internally
/// so semi-transparent edges don't bleed garbage colors.
pub fn resize_box(rgba: Vec<u8>, w: i32, h: i32, target: i32) -> (Vec<u8>, i32, i32) {
    let (w, h) = (w.max(1), h.max(1));
    if w <= target && h <= target {
        return (rgba, w, h);
    }
    let scale = ((w as f32) / (target as f32))
        .max((h as f32) / (target as f32))
        .max(1.0);
    let dw = ((w as f32 / scale).round() as i32).clamp(1, target);
    let dh = ((h as f32 / scale).round() as i32).clamp(1, target);
    let (wu, hu, dwu, dhu) = (w as usize, h as usize, dw as usize, dh as usize);

    let mut out = vec![0u8; dwu * dhu * 4];
    for dy in 0..dhu {
        let sy0 = ((dy as f32) * scale).floor() as usize;
        let sy1 = (((dy + 1) as f32) * scale).ceil() as usize;
        let sy1 = sy1.clamp(sy0 + 1, hu);
        for dx in 0..dwu {
            let sx0 = ((dx as f32) * scale).floor() as usize;
            let sx1 = (((dx + 1) as f32) * scale).ceil() as usize;
            let sx1 = sx1.clamp(sx0 + 1, wu);
            let (mut ra, mut ga, mut ba, mut aa) = (0f32, 0f32, 0f32, 0f32);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let p = (sy * wu + sx) * 4;
                    let a = rgba[p + 3] as f32;
                    ra += rgba[p] as f32 * a;
                    ga += rgba[p + 1] as f32 * a;
                    ba += rgba[p + 2] as f32 * a;
                    aa += a;
                }
            }
            let q = (dy * dwu + dx) * 4;
            let n = ((sx1 - sx0) * (sy1 - sy0)) as f32;
            if aa > 0.0 {
                out[q] = (ra / aa).round().clamp(0.0, 255.0) as u8;
                out[q + 1] = (ga / aa).round().clamp(0.0, 255.0) as u8;
                out[q + 2] = (ba / aa).round().clamp(0.0, 255.0) as u8;
                out[q + 3] = (aa / n).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    (out, dw, dh)
}

const BBOX_ALPHA: u8 = 8;

fn content_bbox(rgba: &[u8], w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
    let (w, h) = (w.max(1), h.max(1));
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, -1, -1);
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            if rgba[((row + x) * 4 + 3) as usize] >= BBOX_ALPHA {
                if x < x0 { x0 = x; }
                if x > x1 { x1 = x; }
                if y < y0 { y0 = y; }
                if y > y1 { y1 = y; }
            }
        }
    }
    (x1 >= 0).then_some((x0, y0, x1, y1))
}

fn crop_rect(rgba: &[u8], w: i32, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> (Vec<u8>, i32, i32) {
    let cw = (x1 - x0 + 1).max(1);
    let ch = (y1 - y0 + 1).max(1);
    let mut out = vec![0u8; (cw * ch * 4) as usize];
    for row in 0..ch {
        let src = (((y0 + row) * w + x0) * 4) as usize;
        let dst = (row * cw * 4) as usize;
        out[dst..dst + (cw * 4) as usize]
            .copy_from_slice(&rgba[src..src + (cw * 4) as usize]);
    }
    (out, cw, ch)
}

/// Bilinear resample to exactly (tw, th). Premultiplied internally (the
/// resize_box contract): semi-transparent edges blend without bleeding
/// garbage colors, then the output is converted back to straight alpha.
pub fn scale_bilinear(rgba: Vec<u8>, w: i32, h: i32, tw: i32, th: i32) -> (Vec<u8>, i32, i32) {
    let (w, h) = (w.max(1), h.max(1));
    let (tw, th) = (tw.max(1), th.max(1));
    if (w, h) == (tw, th) {
        return (rgba, w, h);
    }
    let (wu, hu, twu, thu) = (w as usize, h as usize, tw as usize, th as usize);
    let (sxr, syr) = (w as f32 / tw as f32, h as f32 / th as f32);
    let mut out = vec![0u8; twu * thu * 4];
    for dy in 0..thu {
        let fy = (dy as f32 + 0.5) * syr - 0.5;
        let y0 = (fy.floor().max(0.0) as usize).min(hu - 1);
        let y1 = (y0 + 1).min(hu - 1);
        let ry = (fy - (fy).floor()).clamp(0.0, 1.0);
        for dx in 0..twu {
            let fx = (dx as f32 + 0.5) * sxr - 0.5;
            let x0 = (fx.floor().max(0.0) as usize).min(wu - 1);
            let x1 = (x0 + 1).min(wu - 1);
            let rx = (fx - (fx).floor()).clamp(0.0, 1.0);
            let w00 = (1.0 - rx) * (1.0 - ry);
            let w10 = rx * (1.0 - ry);
            let w01 = (1.0 - rx) * ry;
            let w11 = rx * ry;
            let (p00, p10, p01, p11) = (
                (y0 * wu + x0) * 4,
                (y0 * wu + x1) * 4,
                (y1 * wu + x0) * 4,
                (y1 * wu + x1) * 4,
            );
            let a = rgba[p00 + 3] as f32 * w00
                + rgba[p10 + 3] as f32 * w10
                + rgba[p01 + 3] as f32 * w01
                + rgba[p11 + 3] as f32 * w11;
            let q = (dy * twu + dx) * 4;
            if a < 0.5 {
                continue; // fully transparent target pixel stays zeros
            }
            // premultiplied channel blend, then unpremultiply
            let chan = |c: usize| -> f32 {
                (rgba[p00 + c] as f32 * rgba[p00 + 3] as f32 * w00
                    + rgba[p10 + c] as f32 * rgba[p10 + 3] as f32 * w10
                    + rgba[p01 + c] as f32 * rgba[p01 + 3] as f32 * w01
                    + rgba[p11 + c] as f32 * rgba[p11 + 3] as f32 * w11)
                    / a
            };
            out[q] = chan(0).round().clamp(0.0, 255.0) as u8;
            out[q + 1] = chan(1).round().clamp(0.0, 255.0) as u8;
            out[q + 2] = chan(2).round().clamp(0.0, 255.0) as u8;
            out[q + 3] = a.round().clamp(0.0, 255.0) as u8;
        }
    }
    (out, tw, th)
}

/// Longest side -> `target`, aspect kept (the pipeline's icons are square,
/// but a non-square source stays non-square). No-op when already the size.
fn scale_to(rgba: Vec<u8>, w: i32, h: i32, target: i32) -> (Vec<u8>, i32, i32) {
    let scale = target as f32 / w.max(h).max(1) as f32;
    let tw = ((w as f32 * scale).round() as i32).clamp(1, target);
    let th = ((h as f32 * scale).round() as i32).clamp(1, target);
    if (tw, th) == (w.max(1), h.max(1)) {
        return (rgba, w, h);
    }
    scale_bilinear(rgba, w, h, tw, th)
}

pub fn icon_canonical(rgba: Vec<u8>, w: i32, h: i32, target: i32) -> (Vec<u8>, i32, i32) {
    let side = w.max(h).max(1);
    if let Some((x0, y0, x1, y1)) = content_bbox(&rgba, w, h) {
        let cw = (x1 - x0 + 1).max(1);
        let ch = (y1 - y0 + 1).max(1);
        if side >= 96 && cw * 2 <= side && ch * 2 <= side {
            let (crop, cw, ch) = crop_rect(&rgba, w, (x0, y0, x1, y1));
            return scale_to(crop, cw, ch, target);
        }
    }
    let (out, w2, h2) = resize_box(rgba, w, h, target);
    if w2.max(h2) < target {
        scale_to(out, w2, h2, target)
    } else {
        (out, w2, h2)
    }
}

struct MemDc {
    hdc: HDC,
}

impl MemDc {
    fn new() -> Option<Self> {
        let hdc = unsafe { CreateCompatibleDC(HDC::default()) };
        if hdc.is_invalid() {
            None
        } else {
            Some(MemDc { hdc })
        }
    }
}

impl Drop for MemDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.hdc);
        }
    }
}

/// Decode an HICON into straight RGBA pixels (top-down), deriving alpha from
/// the 1bpp mask for legacy icons. Returns (rgba, width, height).
pub fn icon_rgba(hicon: HICON) -> Option<(Vec<u8>, i32, i32)> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(hicon, &mut ii).ok()?;

        let dcg = MemDc::new()?;
        let hdc = dcg.hdc;

        let mut bm = BITMAP::default();
        let got = GetObjectW(
            ii.hbmColor,
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut core::ffi::c_void),
        );
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
            del_gdi(ii.hbmColor);
            del_gdi(ii.hbmMask);
            return None;
        }
        let w = bm.bmWidth;
        let h = bm.bmHeight.abs();

        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut pixels: Vec<u8> = vec![0u8; (w * h * 4) as usize];
        let got = GetDIBits(
            hdc,
            ii.hbmColor,
            0,
            h as u32,
            Some(pixels.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        if got == 0 {
            del_gdi(ii.hbmColor);
            del_gdi(ii.hbmMask);
            return None;
        }

        // Detect whether the icon actually has an alpha channel.
        let mut has_alpha = false;
        let mut i = 3;
        while i < pixels.len() {
            if pixels[i] != 0 {
                has_alpha = true;
                break;
            }
            i += 4;
        }

        if !has_alpha {
            // Legacy icon: derive alpha from the 1bpp mask (0 = opaque).
            let mask_stride = (((w + 31) / 32) * 4) as usize;
            let mut mask: Vec<u8> = vec![0u8; mask_stride * h as usize];
            let mut bmi_m = BITMAPINFO::default();
            bmi_m.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 1,
                biCompression: 0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let got = GetDIBits(
                hdc,
                ii.hbmMask,
                0,
                h as u32,
                Some(mask.as_mut_ptr() as *mut core::ffi::c_void),
                &mut bmi_m,
                DIB_RGB_COLORS,
            );
            if got == 0 {
                del_gdi(ii.hbmColor);
                del_gdi(ii.hbmMask);
                return None;
            }
            let wu = w as usize;
            for px in 0..(w * h) as usize {
                let row = px / wu;
                let col = px % wu;
                let bit = (mask[row * mask_stride + col / 8] >> (7 - (col % 8))) & 1;
                pixels[px * 4 + 3] = if bit == 0 { 255 } else { 0 };
            }
        }

        del_gdi(ii.hbmColor);
        del_gdi(ii.hbmMask);
        Some((pixels, w, h))
    }
}

pub fn hicon_rgba_fallback(hicon: isize) -> Option<(Vec<u8>, i32, i32)> {
    if hicon == 0 {
        return None;
    }
    let h = HICON(hicon as _);
    if let Some(rgba) = icon_rgba(h) {
        return Some(rgba);
    }
    icon_rgba_monochrome(h)
}

/// Monochrome/null-color-bitmap icon extraction (see hicon_rgba_fallback).
fn icon_rgba_monochrome(hicon: HICON) -> Option<(Vec<u8>, i32, i32)> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(hicon, &mut ii).ok()?;
        // Only the monochrome case benefits: a valid color bitmap that already
        // failed icon_rgba will fail here for the same reason.
        if !ii.hbmColor.is_invalid() {
            del_gdi(ii.hbmColor);
            del_gdi(ii.hbmMask);
            return None;
        }
        let dcg = MemDc::new()?;
        let mut bm = BITMAP::default();
        let got = GetObjectW(
            ii.hbmMask,
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut core::ffi::c_void),
        );
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 || bm.bmHeight % 2 != 0 {
            del_gdi(ii.hbmMask);
            return None;
        }
        let w = bm.bmWidth;
        let h = bm.bmHeight / 2; // AND half + XOR half
        let stride = (w * 4) as usize;

        // Bottom-up 32bpp read of the full 2h-tall mask bitmap. Bottom-up
        // delivery starts at the bitmap's bottom scanline, and the XOR (color)
        // half sits BELOW the AND half — so scanlines 0..h are XOR color and
        // h..2h are the AND mask.
        let mut buf = vec![0u8; stride * (h * 2) as usize];
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: bm.bmHeight, // positive = bottom-up
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let got = GetDIBits(
            dcg.hdc,
            ii.hbmMask,
            0,
            bm.bmHeight as u32,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        del_gdi(ii.hbmMask);
        if got == 0 {
            return None;
        }

        // Visual row y (0 = top): XOR line at bottom-up index (h-1-y), AND
        // line at bottom-up index (2h-1-y). GetDIBits expanded the 1bpp mask
        // to 32bpp, so each AND byte is 0x00 (bit 0 = opaque) or 0xFF (bit 1
        // = transparent) — the same rule the crate applies.
        let mut rgba = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            let xor_line = &buf[(h as usize - 1 - y) * stride..];
            let and_line = &buf[(2 * h as usize - 1 - y) * stride..];
            let out = &mut rgba[y * stride..(y + 1) * stride];
            for x in 0..w as usize {
                let p = x * 4;
                out[p] = xor_line[p + 2]; // BGR -> RGB
                out[p + 1] = xor_line[p + 1];
                out[p + 2] = xor_line[p];
                out[p + 3] = if and_line[p] == 0xFF { 0 } else { 255 };
            }
        }
        Some((rgba, w, h))
    }
}

pub fn to_ink_tone(rgba: &mut [u8]) {
    to_tone(rgba, 16.0, 238.0);
}

pub fn to_chalk_tone(rgba: &mut [u8]) {
    to_tone(rgba, 100.0, 253.0);
}

/// The one tone engine: perceptual luminance -> gamma 1.35 shape -> ramp
/// [lo, hi]. to_ink_tone / to_chalk_tone are the two paper tunings.
fn to_tone(rgba: &mut [u8], lo: f32, hi: f32) {
    let mut i = 0;
    while i < rgba.len() {
        let r = rgba[i] as f32;
        let g = rgba[i + 1] as f32;
        let b = rgba[i + 2] as f32;

        let l = 0.299 * r + 0.587 * g + 0.114 * b;

        // Normalize to 0..1, then shape midtones (gamma 1.35) so colorful
        // bodies keep real ink weight instead of washing out to paper.
        let t = ((l - 6.0) / 238.0).clamp(0.0, 1.0);
        let shaped = t.powf(1.35);
        let v = (lo + shaped * (hi - lo) + 0.5) as u8;

        rgba[i] = v;
        rgba[i + 1] = v;
        rgba[i + 2] = v;
        i += 4;
    }
}

/// Encode RGBA pixels as a PNG and return it base64-encoded.
pub fn png_b64(rgba: &[u8], w: i32, h: i32) -> Option<String> {
    let mut out = Vec::with_capacity(rgba.len() / 2 + 64);
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&out))
}

pub fn build_icon_triple(hwnd: HWND) -> (Option<String>, Option<String>, Option<String>) {
    let mut normal = None;
    let mut ink = None;
    let mut chalk = None;
    if let Some((hicon, owned)) = get_window_icon_hq(hwnd) {
        if let Some((rgba, w, h)) = icon_rgba(hicon) {
            let (rgba, w, h) = icon_canonical(rgba, w, h, CANONICAL);
            normal = png_b64(&rgba, w, h);
            let mut ink_rgba = rgba.clone();
            to_ink_tone(&mut ink_rgba);
            ink = png_b64(&ink_rgba, w, h);
            let mut chalk_rgba = rgba;
            to_chalk_tone(&mut chalk_rgba);
            chalk = png_b64(&chalk_rgba, w, h);
        }
        if owned {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(hicon);
            }
        }
    }
    if normal.is_none() {
        log("icons: no icon found for a window");
    }
    (normal, ink, chalk)
}
