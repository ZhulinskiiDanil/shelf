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
        hook_thread_loop();
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
            let m = wp.0 as u32;
            let down = m == WM_KEYDOWN || m == WM_SYSKEYDOWN;
            // Scrolling capture: Enter/Esc finish it and must not reach the scrolled window.
            if crate::scroll::ACTIVE.load(Ordering::Relaxed) && (k.vkCode == VK_RETURN.0 as u32 || k.vkCode == VK_ESCAPE.0 as u32) {
                if down {
                    crate::scroll::STOP.store(true, Ordering::Relaxed);
                }
                return LRESULT(1);
            }
            if dict_key(k.vkCode, down) {
                return LRESULT(1);
            }
            if k.flags.0 & LLKHF_INJECTED.0 == 0 && ink_key(k.vkCode, down) {
                return LRESULT(1);
            }
            if down && FEATURES.load(Ordering::Relaxed) & F_KEYS != 0 {
                post_ink(INK_KEY, (k.vkCode | mods_now() << 16) as isize);
            }
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

// ! --- Screen ink input --- !

/// Events posted to the main window as `INK_MSG` with the kind in WPARAM.
pub const INK_HOLD_DOWN: usize = 1;
pub const INK_HOLD_UP: usize = 2;
pub const INK_MOVE: usize = 3; // LPARAM = packed (x, y)
pub const INK_COLOR: usize = 4; // LPARAM = palette index
pub const INK_STICKY_DOWN: usize = 5;
pub const INK_STICKY_UP: usize = 6;
pub const INK_STICKY_OFF: usize = 7;
pub const INK_LASER_DOWN: usize = 8;
pub const INK_LASER_UP: usize = 9;
pub const INK_MARKER: usize = 10; // LPARAM = packed (x, y)
pub const INK_CLICK: usize = 11; // LPARAM = packed (x, y)
pub const INK_WHEEL: usize = 12; // LPARAM = 1 up, 0 down
pub const INK_ZOOM_OFF: usize = 13;
pub const INK_KEY: usize = 14; // LPARAM = vk | mods << 16

/// Ink features that need mouse or key events (see `set_features`).
pub const F_SPOT: u32 = 1;
pub const F_ZOOM: u32 = 2;
pub const F_RINGS: u32 = 4;
pub const F_KEYS: u32 = 8;
static FEATURES: AtomicU32 = AtomicU32::new(0);
/// The current hold is the laser (hold key pressed together with Ctrl).
static LASER: AtomicBool = AtomicBool::new(false);
static SWALLOW_RBUTTON_UP: AtomicBool = AtomicBool::new(false);

pub fn set_features(bits: u32) {
    FEATURES.store(bits, Ordering::Relaxed);
    update_mouse_hook();
}

static INK_MSG: AtomicU32 = AtomicU32::new(0);
/// Keyboard hold key (vk), or 0 when the hold control is a mouse X button.
static HOLD_VK: AtomicU32 = AtomicU32::new(0);
/// 1 = XBUTTON1, 2 = XBUTTON2, 0 = keyboard.
static HOLD_XBUTTON: AtomicU32 = AtomicU32::new(0);
static HOLDING: AtomicBool = AtomicBool::new(false);
/// When the hold started and where the cursor was: a short press without moving is a tap.
static HOLD_START: Mutex<Option<(Instant, POINT)>> = Mutex::new(None);
pub static STICKY: AtomicBool = AtomicBool::new(false);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
const WM_HOOK_MOUSE: u32 = WM_USER + 1;

pub fn set_ink_msg(msg: u32) {
    INK_MSG.store(msg, Ordering::Relaxed);
}

/// "CapsLock", any key name, or "XButton1" / "XButton2".
pub fn set_hold(spec: &str) -> Result<(), String> {
    match spec.trim().to_ascii_lowercase().as_str() {
        "xbutton1" | "mouse4" => {
            HOLD_VK.store(0, Ordering::Relaxed);
            HOLD_XBUTTON.store(1, Ordering::Relaxed);
        }
        "xbutton2" | "mouse5" => {
            HOLD_VK.store(0, Ordering::Relaxed);
            HOLD_XBUTTON.store(2, Ordering::Relaxed);
        }
        _ => {
            let hk = crate::config::parse_hotkey(spec)?;
            if hk.mods != 0 {
                return Err(format!("hold key '{spec}' must be a single key"));
            }
            HOLD_VK.store(hk.vk, Ordering::Relaxed);
            HOLD_XBUTTON.store(0, Ordering::Relaxed);
        }
    }
    update_mouse_hook();
    Ok(())
}

fn post_ink(kind: usize, lp: isize) {
    let hwnd = HWND(TARGET.load(Ordering::Relaxed) as _);
    unsafe {
        let _ = PostMessageW(Some(hwnd), INK_MSG.load(Ordering::Relaxed), WPARAM(kind), LPARAM(lp));
    }
}

/// The mouse hook is only installed while it is needed (it touches every mouse event system-wide).
pub fn update_mouse_hook() {
    let need = HOLD_XBUTTON.load(Ordering::Relaxed) != 0
        || HOLDING.load(Ordering::Relaxed)
        || STICKY.load(Ordering::Relaxed)
        || FEATURES.load(Ordering::Relaxed) & (F_SPOT | F_ZOOM | F_RINGS) != 0;
    let t = HOOK_THREAD.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(t, WM_HOOK_MOUSE, WPARAM(need as usize), LPARAM(0));
        }
    }
}

pub fn set_sticky(on: bool) {
    STICKY.store(on, Ordering::Relaxed);
    update_mouse_hook();
}

fn pack(x: i32, y: i32) -> isize {
    ((x as i16 as u16 as u32) | ((y as i16 as u16 as u32) << 16)) as i32 as isize
}

pub fn unpack(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

/// Keyboard part of ink handling; returns true to swallow the key.
fn ink_key(vk: u32, down: bool) -> bool {
    let hold = HOLD_VK.load(Ordering::Relaxed);
    if hold != 0 && vk == hold {
        let was = HOLDING.swap(down, Ordering::Relaxed);
        if down && !was {
            let laser = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
            LASER.store(laser, Ordering::Relaxed);
            let mut pt = POINT::default();
            let _ = unsafe { GetCursorPos(&mut pt) };
            *HOLD_START.lock().unwrap() = Some((Instant::now(), pt));
            update_mouse_hook();
            post_ink(if laser { INK_LASER_DOWN } else { INK_HOLD_DOWN }, 0);
        } else if !down && was {
            update_mouse_hook();
            post_ink(if LASER.load(Ordering::Relaxed) { INK_LASER_UP } else { INK_HOLD_UP }, 0);
            if hold == VK_CAPITAL.0 as u32 && was_tap() {
                toggle_caps_lock();
            }
        }
        return true;
    }
    let active = HOLDING.load(Ordering::Relaxed) || STICKY.load(Ordering::Relaxed);
    if active && (0x31..=0x38).contains(&vk) {
        if down {
            post_ink(INK_COLOR, (vk - 0x31) as isize);
        }
        return true;
    }
    // Esc leaves sticky mode, spotlight and zoom (INK_STICKY_OFF means "ink Esc").
    let modal = STICKY.load(Ordering::Relaxed) || FEATURES.load(Ordering::Relaxed) & (F_SPOT | F_ZOOM) != 0;
    if modal && vk == VK_ESCAPE.0 as u32 {
        if down {
            post_ink(INK_STICKY_OFF, 0);
        }
        return true;
    }
    false
}

/// The hold key was tapped (short press, cursor did not move): nothing was drawn.
fn was_tap() -> bool {
    let Some((t, p0)) = HOLD_START.lock().unwrap().take() else { return false };
    let mut p = POINT::default();
    let _ = unsafe { GetCursorPos(&mut p) };
    t.elapsed().as_millis() < 300 && (p.x - p0.x).abs() <= 4 && (p.y - p0.y).abs() <= 4
}

/// A tap on Caps Lock keeps its usual meaning: replay it as an injected press, which the hook lets through.
fn toggle_caps_lock() {
    let ev = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_CAPITAL, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    };
    unsafe {
        SendInput(&[ev(false), ev(true)], size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
            let msg = wp.0 as u32;
            let xb = HOLD_XBUTTON.load(Ordering::Relaxed);
            let sticky = STICKY.load(Ordering::Relaxed);
            let holding = HOLDING.load(Ordering::Relaxed);
            let f = FEATURES.load(Ordering::Relaxed);
            let pt = pack(m.pt.x, m.pt.y);
            match msg {
                WM_MOUSEMOVE => {
                    if holding || sticky || f & (F_SPOT | F_ZOOM) != 0 {
                        post_ink(INK_MOVE, pt);
                    }
                }
                WM_XBUTTONDOWN | WM_XBUTTONUP if xb != 0 && (m.mouseData >> 16) == xb => {
                    let down = msg == WM_XBUTTONDOWN;
                    let was = HOLDING.swap(down, Ordering::Relaxed);
                    if down != was {
                        post_ink(if down { INK_HOLD_DOWN } else { INK_HOLD_UP }, 0);
                    }
                    return LRESULT(1);
                }
                WM_LBUTTONDOWN | WM_LBUTTONUP => {
                    let down = msg == WM_LBUTTONDOWN;
                    if down && f & F_RINGS != 0 {
                        post_ink(INK_CLICK, pt);
                    }
                    let ctrl = GetAsyncKeyState(VK_CONTROL.0 as i32) < 0;
                    if holding && !LASER.load(Ordering::Relaxed) || sticky && ctrl {
                        // Numbered marker; the click does not reach the window below.
                        if down {
                            post_ink(INK_MARKER, pt);
                        }
                        return LRESULT(1);
                    }
                    if sticky {
                        post_ink(if down { INK_STICKY_DOWN } else { INK_STICKY_UP }, pt);
                        return LRESULT(1);
                    }
                    if f & F_ZOOM != 0 {
                        return LRESULT(1); // zoomed image is a picture; do not click blindly below it
                    }
                }
                WM_RBUTTONDOWN if f & F_ZOOM != 0 => {
                    SWALLOW_RBUTTON_UP.store(true, Ordering::Relaxed);
                    post_ink(INK_ZOOM_OFF, 0);
                    return LRESULT(1);
                }
                WM_RBUTTONUP if SWALLOW_RBUTTON_UP.swap(false, Ordering::Relaxed) => return LRESULT(1),
                WM_RBUTTONDOWN if f & F_RINGS != 0 => post_ink(INK_CLICK, pt),
                WM_MOUSEWHEEL if f & (F_SPOT | F_ZOOM) != 0 => {
                    let delta = (m.mouseData >> 16) as u16 as i16;
                    post_ink(INK_WHEEL, (delta > 0) as isize);
                    return LRESULT(1);
                }
                _ => {}
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

fn hook_thread_loop() {
    unsafe {
        HOOK_THREAD.store(windows::Win32::System::Threading::GetCurrentThreadId(), Ordering::Relaxed);
        let mut mouse: Option<HHOOK> = None;
        update_mouse_hook();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_HOOK_MOUSE {
                let need = msg.wParam.0 != 0;
                if need && mouse.is_none() {
                    mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0).ok();
                } else if !need {
                    if let Some(h) = mouse.take() {
                        let _ = UnhookWindowsHookEx(h);
                    }
                }
            }
        }
    }
}

// ! --- Dictation hold key --- !

pub const INK_DICT_DOWN: usize = 20;
pub const INK_DICT_UP: usize = 21;
static DICT_VK: AtomicU32 = AtomicU32::new(0);
static DICT_MODS: AtomicU32 = AtomicU32::new(0);
static DICT_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_dictation_key(hk: Option<Hotkey>) {
    DICT_VK.store(hk.map(|h| h.vk).unwrap_or(0), Ordering::Relaxed);
    DICT_MODS.store(hk.map(|h| h.mods).unwrap_or(0), Ordering::Relaxed);
}

/// Swallowing a key of an Alt/Win combo would leave a lone Alt/Win press, which opens
/// the window menu / Start. A "mask" key in between prevents that (same trick as AutoHotkey).
fn send_mask_key() {
    let ev = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0xE8), dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    };
    unsafe {
        SendInput(&[ev(false), ev(true)], size_of::<INPUT>() as i32);
    }
}

/// Returns true to swallow.
fn dict_key(vk: u32, down: bool) -> bool {
    let dvk = DICT_VK.load(Ordering::Relaxed);
    if dvk == 0 || vk != dvk {
        return false;
    }
    if down {
        if DICT_ACTIVE.load(Ordering::Relaxed) {
            return true; // auto-repeat
        }
        let mods = DICT_MODS.load(Ordering::Relaxed);
        if mods_now() != mods {
            return false;
        }
        DICT_ACTIVE.store(true, Ordering::Relaxed);
        if mods & (MOD_ALT | MOD_WIN) != 0 {
            send_mask_key();
        }
        post_ink(INK_DICT_DOWN, 0);
        true
    } else if DICT_ACTIVE.swap(false, Ordering::Relaxed) {
        post_ink(INK_DICT_UP, 0);
        true
    } else {
        false
    }
}
