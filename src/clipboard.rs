//! Clipboard output. CF_DIB is placed immediately (it is a plain memcpy); the "PNG"
//! format is delay-rendered so encoding never sits on the Ctrl+C path.

use std::sync::Mutex;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_DIB;
use windows::core::w;

#[derive(Clone)]
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

pub fn global_bytes(b: &[u8]) -> Option<HGLOBAL> {
    global_from(&[b])
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

/// CF_HDROP payload: DROPFILES header followed by double-NUL-terminated UTF-16 paths.
pub fn dropfiles_bytes(paths: &[std::path::PathBuf]) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let mut v = Vec::new();
    v.extend_from_slice(&20u32.to_le_bytes()); // pFiles: offset of the list
    v.extend_from_slice(&[0u8; 8]); // pt
    v.extend_from_slice(&0u32.to_le_bytes()); // fNC
    v.extend_from_slice(&1u32.to_le_bytes()); // fWide
    for p in paths {
        for c in p.as_os_str().encode_wide().chain(Some(0)) {
            v.extend_from_slice(&c.to_le_bytes());
        }
    }
    v.extend_from_slice(&[0, 0]);
    v
}

/// Puts files on the clipboard (paste into Explorer, Discord, browsers).
pub fn set_files(owner: HWND, paths: &[std::path::PathBuf]) -> bool {
    let Some(h) = global_from(&[&dropfiles_bytes(paths)]) else { return false };
    if !open(owner) {
        return false;
    }
    unsafe {
        let _ = EmptyClipboard();
        let ok = SetClipboardData(windows::Win32::System::Ole::CF_HDROP.0 as u32, Some(HANDLE(h.0))).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropfiles_layout() {
        let b = dropfiles_bytes(&[std::path::PathBuf::from("C:/a.mp4"), std::path::PathBuf::from("D:/b")]);
        assert_eq!(&b[0..4], &20u32.to_le_bytes());
        assert_eq!(&b[16..20], &1u32.to_le_bytes());
        let wide: Vec<u16> = b[20..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let s = String::from_utf16(&wide).unwrap();
        assert_eq!(s, "C:/a.mp4\0D:/b\0\0");
    }
}

/// Very large images (e.g. long scrolling captures) go to the clipboard as PNG only:
/// a CF_DIB would be hundreds of megabytes.
pub fn set_png_only(owner: HWND, img: &Image) -> bool {
    let png = crate::imageio::encode_png(img, png::Compression::Fast);
    let Some(h) = global_from(&[&png]) else { return false };
    if !open(owner) {
        return false;
    }
    unsafe {
        let _ = EmptyClipboard();
        let ok = SetClipboardData(png_format(), Some(HANDLE(h.0))).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// Above this, images are copied as PNG only.
pub fn is_huge(img: &Image) -> bool {
    img.h > 8000 || img.bgra.len() > 96 * 1024 * 1024
}
