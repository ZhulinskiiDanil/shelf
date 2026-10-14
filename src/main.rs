#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod annot;
mod capture;
mod clipwatch;
mod clipboard;
mod config;
mod dragdrop;
mod geom;
mod history;
mod hook;
mod imageio;
mod logger;
mod names;
mod overlay;
mod shelf;

use std::cell::RefCell;
use std::path::Path;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const MAIN_CLASS: PCWSTR = w!("Shelf.Main");
const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_QUIT: u32 = WM_APP + 2;
const WM_APP_RELOAD: u32 = WM_APP + 3;
const WM_APP_CAPTURE: u32 = WM_APP + 4;

const MENU_SETTINGS: usize = 1;
const MENU_HISTORY: usize = 2;
const MENU_EXIT: usize = 3;

// Hotkey ids
pub const HK_SHELF: i32 = 2;

struct App {
    hwnd: HWND,
    config: config::Config,
    taskbar_created: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn main() {
    logger::init();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = windows::Win32::System::Ole::OleInitialize(None);
    }
    acquire_single_instance();

    let cfg = config::load();
    logger::set_level(&cfg.log_level);
    log::info!("Shelf {} starting", env!("CARGO_PKG_VERSION"));

    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: MAIN_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Top-level hidden window (not message-only) so it receives TaskbarCreated broadcasts.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            MAIN_CLASS,
            w!("Shelf"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();

        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        APP.with(|a| *a.borrow_mut() = Some(App { hwnd, config: cfg.clone(), taskbar_created }));

        add_tray_icon(hwnd);
        hook::start(hwnd, WM_APP_CAPTURE);
        overlay::init(hwnd, cfg.capture.magnifier);
        shelf::init();
        apply_config(&cfg, None);
        spawn_config_watcher(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        remove_tray_icon(hwnd);
    }
    log::info!("Shelf exited");
    log::logger().flush();
}

// ! --- Single instance --- !

/// If another Shelf (any milestone build) is running, ask it to quit and take over.
fn acquire_single_instance() {
    unsafe {
        let mutex = CreateMutexW(None, false, w!("Local\\Shelf.SingleInstance")).unwrap();
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log::info!("another instance is running, asking it to quit");
            if let Ok(old) = FindWindowW(MAIN_CLASS, None) {
                let _ = PostMessageW(Some(old), WM_APP_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        // Owned for the whole process lifetime; released by the OS on exit.
        let r = WaitForSingleObject(mutex, 5000);
        if r == WAIT_TIMEOUT {
            log::error!("previous instance did not exit in 5 s, exiting");
            std::process::exit(1);
        }
    }
}

// ! --- Tray --- !

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        ..Default::default()
    };
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        nid.hIcon = LoadIconW(Some(hinst.into()), PCWSTR(1 as _))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();
    }
    let tip = wide(concat!("Shelf ", env!("CARGO_PKG_VERSION")));
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid
}

fn add_tray_icon(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd));
    }
}

fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
    }
}

fn show_tray_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu().unwrap();
        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Настройки (config.toml)"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_HISTORY, w!("Открыть папку истории"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Выход"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

pub fn shell_open(path: &Path) {
    let p = wide_path(path);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
    }
}

// ! --- Config --- !

fn apply_config(cfg: &config::Config, old: Option<&config::Config>) {
    logger::set_level(&cfg.log_level);
    config::set_current(cfg);
    history::set_template(&cfg.save.filename);
    history::set_limits(cfg.history.max_items, cfg.history.max_mb);
    if old.map(|o| o.history.watch_clipboard) != Some(cfg.history.watch_clipboard) {
        let hwnd = APP.with(|a| a.borrow().as_ref().unwrap().hwnd);
        clipwatch::set_enabled(hwnd, cfg.history.watch_clipboard);
    }
    if old.map(|o| o.autostart) != Some(cfg.autostart) {
        set_autostart(cfg.autostart);
    }
    overlay::set_magnifier(cfg.capture.magnifier);
    if old.map(|o| &o.hotkeys) != Some(&cfg.hotkeys) {
        register_hotkeys(cfg);
    }
}

fn register_hotkeys(cfg: &config::Config) {
    let hwnd = APP.with(|a| a.borrow().as_ref().unwrap().hwnd);
    // The capture key goes through the low-level hook so it can override PrtScrn.
    match config::parse_hotkey(&cfg.hotkeys.capture) {
        Ok(hk) => {
            hook::set_capture_key(Some(hk));
            log::info!("hotkey capture = {}", cfg.hotkeys.capture);
        }
        Err(e) => {
            hook::set_capture_key(None);
            log::error!("hotkey capture: {e}");
        }
    }
    for (id, name, spec) in [(HK_SHELF, "shelf", &cfg.hotkeys.shelf)] {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
        match config::parse_hotkey(spec) {
            Ok(hk) => unsafe {
                let mods = HOT_KEY_MODIFIERS(hk.mods) | MOD_NOREPEAT;
                match RegisterHotKey(Some(hwnd), id, mods, hk.vk) {
                    Ok(()) => log::info!("hotkey {name} = {spec}"),
                    Err(e) => log::warn!("hotkey {name} = {spec} not registered: {e}"),
                }
            },
            Err(e) => log::error!("hotkey {name}: {e}"),
        }
    }
}

fn set_autostart(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"), Some(0), KEY_SET_VALUE, &mut key).is_err() {
            return;
        }
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let val = wide(&format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(val.as_ptr() as *const u8, val.len() * 2);
            let _ = RegSetValueExW(key, w!("Shelf"), Some(0), REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, w!("Shelf"));
        }
        let _ = RegCloseKey(key);
    }
    log::info!("autostart = {on}");
}

fn spawn_config_watcher(hwnd: HWND) {
    let hwnd_raw = hwnd.0 as isize;
    std::thread::spawn(move || unsafe {
        let dir = wide_path(&config::app_dir());
        let Ok(h) = FindFirstChangeNotificationW(PCWSTR(dir.as_ptr()), false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME) else {
            log::warn!("config watcher unavailable");
            return;
        };
        let path = config::config_path();
        let mut last = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        loop {
            if WaitForSingleObject(h, INFINITE) != WAIT_OBJECT_0 {
                return;
            }
            // Editors write in several steps; let them finish.
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = FindNextChangeNotification(h);
            let now = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            if now != last {
                last = now;
                let _ = PostMessageW(Some(HWND(hwnd_raw as _)), WM_APP_RELOAD, WPARAM(0), LPARAM(0));
            }
        }
    });
}

fn reload_config() {
    let new = config::load();
    let old = APP.with(|a| std::mem::replace(&mut a.borrow_mut().as_mut().unwrap().config, new.clone()));
    if old != new {
        log::info!("config reloaded");
        apply_config(&new, Some(&old));
    }
}

// ! --- Window proc --- !

fn on_hotkey(id: i32) {
    match id {
        HK_SHELF => shelf::toggle(),
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_TRAY => {
                let ev = (lp.0 & 0xFFFF) as u32;
                if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU {
                    show_tray_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                match wp.0 & 0xFFFF {
                    MENU_SETTINGS => shell_open(&config::config_path()),
                    MENU_HISTORY => {
                        let dir = config::app_dir().join("history");
                        let _ = std::fs::create_dir_all(&dir);
                        shell_open(&dir);
                    }
                    MENU_EXIT => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                on_hotkey(wp.0 as i32);
                LRESULT(0)
            }
            WM_APP_CAPTURE => {
                let t0 = hook::PRESSED_AT.lock().unwrap().take().unwrap_or_else(std::time::Instant::now);
                overlay::show(t0);
                LRESULT(0)
            }
            WM_RENDERFORMAT => {
                clipboard::render_format(wp.0 as u32);
                LRESULT(0)
            }
            WM_RENDERALLFORMATS => {
                clipboard::render_all(hwnd);
                LRESULT(0)
            }
            WM_CLIPBOARDUPDATE => {
                clipwatch::on_update(hwnd);
                LRESULT(0)
            }
            WM_DESTROYCLIPBOARD => {
                clipboard::release();
                LRESULT(0)
            }
            WM_APP_RELOAD => {
                reload_config();
                LRESULT(0)
            }
            WM_APP_QUIT => {
                log::info!("asked to quit by a newer instance");
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => {
                let tc = APP.with(|a| a.borrow().as_ref().map(|a| a.taskbar_created));
                if Some(msg) == tc {
                    add_tray_icon(hwnd);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
    }
}
