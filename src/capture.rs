//! Freezing the screen. BitBlt of the whole virtual screen into a reusable DIB section.
//! See NOTES.md for why this is used instead of DXGI Desktop Duplication.

use crate::geom::Rect;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

pub struct Frame {
    pub bounds: Rect,
    /// Top-down BGRA, `bounds.w() * 4` bytes per row. Points into the DIB section.
    pub pixels: *mut u8,
    dc: HDC,
    bmp: HBITMAP,
}

impl Frame {
    pub fn stride(&self) -> usize {
        self.bounds.w() as usize * 4
    }
    pub fn data(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pixels, self.stride() * self.bounds.h() as usize) }
    }
    /// Copies the given virtual-screen rect into a tightly packed BGRA buffer.
    pub fn crop(&self, r: &Rect) -> Vec<u8> {
        let r = r.intersect(&self.bounds);
        let (w, h) = (r.w() as usize, r.h() as usize);
        let mut out = Vec::with_capacity(w * h * 4);
        let data = self.data();
        for y in 0..h {
            let row = (r.t - self.bounds.t) as usize + y;
            let start = row * self.stride() + (r.l - self.bounds.l) as usize * 4;
            out.extend_from_slice(&data[start..start + w * 4]);
        }
        out
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

pub fn virtual_screen() -> Rect {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        Rect::new(x, y, x + GetSystemMetrics(SM_CXVIRTUALSCREEN), y + GetSystemMetrics(SM_CYVIRTUALSCREEN))
    }
}

/// Grabs the screen into `slot`, reusing its buffer when the virtual screen size is unchanged.
pub fn grab(slot: &mut Option<Frame>) -> bool {
    let vs = virtual_screen();
    unsafe {
        if slot.as_ref().map(|f| f.bounds) != Some(vs) {
            *slot = None;
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: vs.w(),
                    biHeight: -vs.h(),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0);
            ReleaseDC(None, screen);
            let Ok(bmp) = bmp else {
                let _ = DeleteDC(dc);
                return false;
            };
            SelectObject(dc, bmp.into());
            *slot = Some(Frame { bounds: vs, pixels: bits as *mut u8, dc, bmp });
        }
        let f = slot.as_ref().unwrap();
        let screen = GetDC(None);
        let ok = BitBlt(f.dc, 0, 0, vs.w(), vs.h(), Some(screen), vs.l, vs.t, SRCCOPY).is_ok();
        ReleaseDC(None, screen);
        let _ = GdiFlush();
        ok
    }
}

/// Monitor rects with their effective DPI.
pub fn monitors() -> Vec<(Rect, u32)> {
    unsafe extern "system" fn cb(hm: HMONITOR, _: HDC, r: *mut RECT, lp: LPARAM) -> BOOL {
        unsafe {
            let v = &mut *(lp.0 as *mut Vec<(Rect, u32)>);
            let r = &*r;
            let (mut dx, mut dy) = (96, 96);
            let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            v.push((Rect::new(r.left, r.top, r.right, r.bottom), dx));
        }
        TRUE
    }
    let mut v: Vec<(Rect, u32)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Visible top-level windows in z-order (topmost first), excluding `skip`.
pub fn window_rects(skip: HWND) -> Vec<Rect> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let (v, skip) = &mut *(lp.0 as *mut (Vec<Rect>, HWND));
            if hwnd == *skip || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return TRUE;
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
            if cloaked != 0 {
                return TRUE;
            }
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TRANSPARENT.0 != 0 && ex & WS_EX_LAYERED.0 != 0 {
                return TRUE; // click-through overlays
            }
            let mut r = RECT::default();
            if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as _, size_of::<RECT>() as u32).is_err() {
                let _ = GetWindowRect(hwnd, &mut r);
            }
            let rect = Rect::new(r.left, r.top, r.right, r.bottom);
            if rect.w() > 4 && rect.h() > 4 {
                v.push(rect);
            }
        }
        TRUE
    }
    let mut data = (Vec::new(), skip);
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut data as *mut _ as isize));
    }
    data.0
}
