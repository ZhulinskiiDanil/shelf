//! Clipboard output. CF_DIB is placed immediately (it is a plain memcpy); the "PNG"
//! format is delay-rendered so encoding never sits on the Ctrl+C path.

use std::sync::Mutex;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_DIB;
use windows::core::w;

pub struct Image {
    pub w: u32,
    pub h: u32,
    /// Top-down BGRA.
    pub bgra: Vec<u8>,
}

static PENDING_PNG: Mutex<Option<std::sync::Arc<Image>>> = Mutex::new(None);

fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

fn global_from(bytes: &[&[u8]]) -> Option<HGLOBAL> {
    let len: usize = bytes.iter().map(|b| b.len()).sum();
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, len).ok()?;
        let p = GlobalLock(h) as *mut u8;
        let mut off = 0;
        for b in bytes {
            std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(off), b.len());
            off += b.len();
        }
        let _ = GlobalUnlock(h);
        Some(h)
    }
}

fn open(owner: HWND) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

pub fn dib_global(img: &Image) -> Option<HGLOBAL> {
    let hdr = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: img.w as i32,
        biHeight: img.h as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: img.w * img.h * 4,
        ..Default::default()
    };
    let hdr_bytes = unsafe { std::slice::from_raw_parts(&hdr as *const _ as *const u8, size_of::<BITMAPINFOHEADER>()) };
    // Bottom-up rows: some readers (e.g. .NET, older Office) mishandle negative heights.
    let stride = img.w as usize * 4;
    let mut parts: Vec<&[u8]> = vec![hdr_bytes];
    parts.extend(img.bgra.chunks_exact(stride).rev());
    global_from(&parts)
}

pub fn set_image(owner: HWND, img: std::sync::Arc<Image>) -> bool {
    let Some(dib) = dib_global(&img) else { return false };
    if !open(owner) {
        log::warn!("clipboard busy");
        unsafe {
            let _ = GlobalFree(Some(dib));
        }
        return false;
    }
    unsafe {
        let _ = EmptyClipboard();
        let ok = SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(dib.0))).is_ok();
        *PENDING_PNG.lock().unwrap() = Some(img);
        let _ = SetClipboardData(png_format(), None);
        let _ = CloseClipboard();
        ok
    }
}

pub fn set_text(owner: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    let Some(h) = global_from(&[bytes]) else { return false };
    if !open(owner) {
        return false;
    }
    unsafe {
        let _ = EmptyClipboard();
        let ok = SetClipboardData(13 /* CF_UNICODETEXT */, Some(HANDLE(h.0))).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// WM_RENDERFORMAT: the clipboard is already open by the requester.
pub fn render_format(fmt: u32) {
    if fmt != png_format() {
        return;
    }
    let Some(img) = PENDING_PNG.lock().unwrap().clone() else { return };
    let png = crate::imageio::encode_png(&img, png::Compression::Fast);
    if let Some(h) = global_from(&[&png]) {
        unsafe {
            let _ = SetClipboardData(fmt, Some(HANDLE(h.0)));
        }
    }
}

/// WM_RENDERALLFORMATS: called before the owner window is destroyed.
pub fn render_all(owner: HWND) {
    if PENDING_PNG.lock().unwrap().is_none() || !open(owner) {
        return;
    }
    unsafe {
        if GetClipboardOwner().ok() == Some(owner) {
            render_format(png_format());
        }
        let _ = CloseClipboard();
    }
}

/// Called on WM_DESTROYCLIPBOARD so the image is not kept alive needlessly.
pub fn release() {
    *PENDING_PNG.lock().unwrap() = None;
}
