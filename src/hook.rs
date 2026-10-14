//! Low-level keyboard hook on its own thread. Used for the capture key so that
//! PrtScrn is swallowed before Windows (Snipping Tool, Lightshot) sees it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

static TARGET: AtomicIsize = AtomicIsize::new(0);
static CAPTURE_MSG: AtomicU32 = AtomicU32::new(0);
static CAPTURE_VK: AtomicU32 = AtomicU32::new(0);
static CAPTURE_MODS: AtomicU32 = AtomicU32::new(0);
static SWALLOW_UP: AtomicBool = AtomicBool::new(false);

/// Time of the last capture key press, read by the UI thread for metrics.
pub static PRESSED_AT: Mutex<Option<Instant>> = Mutex::new(None);

pub fn set_capture_key(hk: Option<Hotkey>) {
    CAPTURE_VK.store(hk.map(|h| h.vk).unwrap_or(0), Ordering::Relaxed);
    CAPTURE_MODS.store(hk.map(|h| h.mods).unwrap_or(0), Ordering::Relaxed);
}

pub fn start(target: HWND, msg: u32) {
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    CAPTURE_MSG.store(msg, Ordering::Relaxed);
    std::thread::spawn(|| unsafe {
        let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0) else {
            log::error!("keyboard hook failed");
            return;
        };
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        let _ = UnhookWindowsHookEx(hook);
    });
}

fn mods_now() -> u32 {
    let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0;
    let mut m = 0;
    if down(VK_CONTROL) {
        m |= MOD_CONTROL;
    }
    if down(VK_SHIFT) {
        m |= MOD_SHIFT;
    }
    if down(VK_MENU) {
        m |= MOD_ALT;
    }
    if down(VK_LWIN) || down(VK_RWIN) {
        m |= MOD_WIN;
    }
    m
}

unsafe extern "system" fn proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let vk = CAPTURE_VK.load(Ordering::Relaxed);
            if vk != 0 && k.vkCode == vk {
                let m = wp.0 as u32;
                if m == WM_KEYDOWN || m == WM_SYSKEYDOWN {
                    if mods_now() == CAPTURE_MODS.load(Ordering::Relaxed) {
                        if !SWALLOW_UP.swap(true, Ordering::Relaxed) {
                            *PRESSED_AT.lock().unwrap() = Some(Instant::now());
                            let hwnd = HWND(TARGET.load(Ordering::Relaxed) as _);
                            let _ = PostMessageW(Some(hwnd), CAPTURE_MSG.load(Ordering::Relaxed), WPARAM(0), LPARAM(0));
                        }
                        return LRESULT(1);
                    }
                } else if SWALLOW_UP.swap(false, Ordering::Relaxed) {
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}
