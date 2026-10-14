//! Puts images copied anywhere in the system into the history.

use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP};
use windows::core::w;

static LAST_HASH: Mutex<u64> = Mutex::new(0);

static WATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `watch`: save copied images to history. `listen`: receive WM_CLIPBOARDUPDATE at all
/// (also needed by the clipboard chip).
pub fn set_enabled(hwnd: HWND, watch: bool, listen: bool) {
    WATCH.store(watch, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
        if listen {
            let _ = AddClipboardFormatListener(hwnd);
        }
    }
    log::info!("clipboard watch = {watch}, listener = {listen}");
}

fn read_format(fmt: u32) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(fmt).ok()?;
        let hg = HGLOBAL(h.0);
        let p = GlobalLock(hg) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, GlobalSize(hg)).to_vec();
        let _ = GlobalUnlock(hg);
        Some(v)
    }
}

/// WM_CLIPBOARDUPDATE. Copies the raw bytes on the UI thread (fast), decodes and saves on the worker.
pub fn on_update(own: HWND) {
    if !WATCH.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    unsafe {
        if GetClipboardOwner().ok() == Some(own) || IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() {
            return;
        }
        let png_fmt = RegisterClipboardFormatW(w!("PNG"));
        let has_png = IsClipboardFormatAvailable(png_fmt).is_ok();
        if !has_png && IsClipboardFormatAvailable(CF_DIB.0 as u32).is_err() {
            return;
        }
        if OpenClipboard(Some(own)).is_err() {
            return;
        }
        let data = if has_png { read_format(png_fmt).map(|b| (true, b)) } else { None }
            .or_else(|| read_format(CF_DIB.0 as u32).map(|b| (false, b)));
        let _ = CloseClipboard();
        let Some((is_png, bytes)) = data else { return };
        crate::history::run_async(move || {
            let img = if is_png { crate::imageio::decode_png(&bytes) } else { crate::imageio::dib_to_image(&bytes) };
            let Some(img) = img else {
                log::debug!("clipboard image in unsupported format");
                return;
            };
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (img.w, img.h, &img.bgra).hash(&mut hasher);
            let h = hasher.finish();
            let mut last = LAST_HASH.lock().unwrap();
            if *last == h {
                return;
            }
            *last = h;
            drop(last);
            crate::history::add(&img, "clipboard");
        });
    }
}
