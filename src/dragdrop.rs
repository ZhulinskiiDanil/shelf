//! Drag-out of files (CF_HDROP and the shell formats via SHCreateDataObject) plus an
//! optional CF_DIB of the image, for targets that only accept bitmaps.

use crate::clipboard::{self, Image};
use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::*;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::core::*;
use windows_core::implement;

/// `click_to_drop`: the drag starts with no mouse button held (from a hotkey) and
/// the file follows the cursor until the next click. Otherwise it is a normal drag.
#[implement(IDropSource)]
struct Source {
    click_to_drop: bool,
    seen_down: Cell<bool>,
}

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, esc: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if esc.as_bool() || keys.0 & MK_RBUTTON.0 != 0 && !self.click_to_drop {
            return DRAGDROP_S_CANCEL;
        }
        let down = keys.0 & MK_LBUTTON.0 != 0;
        if self.click_to_drop {
            if keys.0 & MK_RBUTTON.0 != 0 {
                return DRAGDROP_S_CANCEL;
            }
            if down {
                self.seen_down.set(true);
                S_OK
            } else if self.seen_down.get() {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        } else if down {
            S_OK
        } else {
            DRAGDROP_S_DROP
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Blocks in the OLE drag loop until dropped or cancelled. Returns true if dropped.
pub fn drag_files(paths: &[PathBuf], image: Option<&Image>, click_to_drop: bool) -> bool {
    unsafe {
        let pidls: Vec<*mut ITEMIDLIST> = paths
            .iter()
            .map(|p| {
                let w: Vec<u16> = p.as_os_str().encode_wide_z();
                ILCreateFromPathW(PCWSTR(w.as_ptr()))
            })
            .filter(|p| !p.is_null())
            .collect();
        if pidls.is_empty() {
            return false;
        }
        let consts: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
        // The shell's own data object (as Explorer drags it): unlike SHCreateDataObject it
        // also renders CF_HDROP, which Chromium/Electron targets (Discord, browsers) require.
        let data: Result<IDataObject> = SHCreateShellItemArrayFromIDLists(&consts).and_then(|a: IShellItemArray| a.BindToHandler(None, &BHID_DataObject));
        for p in &pidls {
            ILFree(Some(*p));
        }
        let Ok(data) = data else {
            log::error!("SHCreateDataObject failed");
            return false;
        };
        if let Some(img) = image {
            if let Some(h) = clipboard::dib_global(img) {
                let fmt = FORMATETC { cfFormat: CF_DIB.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
                let med = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: ManuallyDrop::new(None) };
                let _ = data.SetData(&fmt, &med, true);
            }
        }
        let src: IDropSource = Source { click_to_drop, seen_down: Cell::new(false) }.into();
        let mut effect = DROPEFFECT_NONE;
        let hr = DoDragDrop(&data, &src, DROPEFFECT_COPY | DROPEFFECT_LINK, &mut effect);
        let dropped = hr == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE;
        log::info!("drag-out of {} file(s): dropped={dropped}", paths.len());
        dropped
    }
}

trait WideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}

impl WideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

/// Drag-out of plain text (CF_UNICODETEXT) with a normal (button held) drag.
pub fn drag_text(text: &str) -> bool {
    unsafe {
        let Ok(data) = SHCreateDataObject::<_, IDataObject>(None, None, None) else { return false };
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        let Some(h) = clipboard::global_bytes(bytes) else { return false };
        let fmt = FORMATETC { cfFormat: 13 /* CF_UNICODETEXT */, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        let med = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: ManuallyDrop::new(None) };
        if data.SetData(&fmt, &med, true).is_err() {
            return false;
        }
        let src: IDropSource = Source { click_to_drop: false, seen_down: Cell::new(false) }.into();
        let mut effect = DROPEFFECT_NONE;
        let hr = DoDragDrop(&data, &src, DROPEFFECT_COPY, &mut effect);
        hr == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE
    }
}
