#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod annot;
mod audio;
mod board;
mod capture;
mod clipwatch;
mod chip;
mod clipboard;
mod clipkind;
mod config;
mod dictation;
mod dragdrop;
mod explain;
mod geom;
mod history;
mod hook;
mod imageio;
mod ink;
mod logger;
mod names;
mod ocr;
mod overlay;
mod record;
mod redact;
mod settings;
mod settings_model;
mod scroll;
mod shelf;
mod stitch;
mod texthist;
mod textpick;
mod transcode;
mod gifout;

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
const WM_APP_INK: u32 = WM_APP + 5;
const WM_APP_CALL: u32 = WM_APP + 6;

static MAIN_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

const MENU_SETTINGS: usize = 1;
const MENU_HISTORY: usize = 2;
const MENU_EXIT: usize = 3;
const MENU_DICT_HELP: usize = 4;
const MENU_CONFIG: usize = 5;

// Hotkey ids
pub const HK_SHELF: i32 = 2;
pub const HK_INK_STICKY: i32 = 3;
pub const HK_INK_CLEAR: i32 = 4;
pub const HK_SPOTLIGHT: i32 = 5;
pub const HK_ZOOM: i32 = 6;
pub const HK_RINGS: i32 = 7;
pub const HK_KEYS: i32 = 8;
pub const HK_BOARD: i32 = 9;
pub const HK_REC_STOP: i32 = 10;
pub const HK_CHIP: i32 = 11;
pub const HK_TEXT_PICK: i32 = 12;
pub const HK_TEXT_PAUSE: i32 = 13;
pub const HK_REDACT_HIDE: i32 = 14;

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
        MAIN_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        APP.with(|a| *a.borrow_mut() = Some(App { hwnd, config: cfg.clone(), taskbar_created }));

        add_tray_icon(hwnd);
        hook::start(hwnd, WM_APP_CAPTURE);
        overlay::init(hwnd, cfg.capture.magnifier);
        shelf::init();
        board::init();
        textpick::init();
        chip::init();
        settings::init();
        ink::init();
        hook::set_ink_msg(WM_APP_INK);
        apply_config(&cfg, None);
        spawn_config_watcher(hwnd);
        trim_working_set();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if settings::is_dialog_message(&msg) {
                continue;
            }
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
        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Настройки…"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CONFIG, w!("Открыть config.toml"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_HISTORY, w!("Открыть папку истории"));
        if !dictation::model_present() {
            let _ = AppendMenuW(menu, MF_STRING, MENU_DICT_HELP, w!("Диктовка: где взять модель…"));
        }
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
    let listen = |c: &config::Config| (c.history.watch_clipboard, c.clipboard_chip.enabled, c.text_history.enabled);
    if old.map(listen) != Some(listen(cfg)) {
        let hwnd = APP.with(|a| a.borrow().as_ref().unwrap().hwnd);
        clipwatch::set_enabled(hwnd, cfg.history.watch_clipboard, cfg.history.watch_clipboard || cfg.clipboard_chip.enabled || cfg.text_history.enabled);
    }
    if old.map(|o| o.autostart) != Some(cfg.autostart) {
        set_autostart(cfg.autostart);
    }
    overlay::set_magnifier(cfg.capture.magnifier);
    board::configure(cfg.board.opacity, config::parse_color(&cfg.board.background).unwrap_or(0x1E1E1E));
    if old.map(|o| (&o.hotkeys, &o.ink, &o.board.hotkey, &o.dictation.hold, &o.record.stop, &o.clipboard_chip, &o.text_history, &o.redaction.hide_hotkey)) != Some((&cfg.hotkeys, &cfg.ink, &cfg.board.hotkey, &cfg.dictation.hold, &cfg.record.stop, &cfg.clipboard_chip, &cfg.text_history, &cfg.redaction.hide_hotkey)) {
        register_hotkeys(cfg);
    }
    sync_ink_features();
}

fn register_hotkeys(cfg: &config::Config) {
    let hwnd = APP.with(|a| a.borrow().as_ref().unwrap().hwnd);
    let mut failed = Vec::new();
    // A config change ends a temporary on/off made with the chip hotkey.
    chip::ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    for c in settings_model::global_conflicts(cfg) {
        log::warn!("hotkey conflict: {c}");
    }
    match config::parse_hotkey(&cfg.dictation.hold) {
        Ok(hk) => hook::set_dictation_key(Some(hk)),
        Err(e) => {
            hook::set_dictation_key(None);
            log::error!("dictation hold: {e}");
        }
    }
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
    if let Err(e) = hook::set_hold(&cfg.ink.hold) {
        log::error!("ink hold: {e}");
    }
    ink::with(|i| {
        i.fade_delay = std::time::Duration::from_millis(cfg.ink.fade_ms);
        i.width = cfg.ink.width.clamp(1.0, 40.0);
        i.color = config::parse_color(&cfg.ink.color).unwrap_or(annot::PALETTE[0]);
        // Runs when the [ink] section changes: the config is the source of truth again.
        i.rings_on = cfg.ink.click_rings;
        i.keys_on = cfg.ink.show_keys;
    });
    for (id, name, spec) in [
        (HK_SHELF, "shelf", &cfg.hotkeys.shelf),
        (HK_INK_STICKY, "ink sticky", &cfg.ink.sticky),
        (HK_INK_CLEAR, "ink clear", &cfg.ink.clear),
        (HK_SPOTLIGHT, "spotlight", &cfg.ink.spotlight),
        (HK_ZOOM, "zoom", &cfg.ink.zoom),
        (HK_RINGS, "click rings", &cfg.ink.click_rings_toggle),
        (HK_KEYS, "show keys", &cfg.ink.keys_toggle),
        (HK_BOARD, "board", &cfg.board.hotkey),
        (HK_REC_STOP, "record stop", &cfg.record.stop),
        (HK_CHIP, "clipboard chip", &cfg.clipboard_chip.toggle),
        (HK_TEXT_PICK, "text picker", &cfg.text_history.picker),
        (HK_TEXT_PAUSE, "text history pause", &cfg.text_history.pause),
        (HK_REDACT_HIDE, "hide personal data", &cfg.redaction.hide_hotkey),
    ] {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
        match config::parse_hotkey(spec) {
            Ok(hk) => unsafe {
                let mods = HOT_KEY_MODIFIERS(hk.mods) | MOD_NOREPEAT;
                match RegisterHotKey(Some(hwnd), id, mods, hk.vk) {
                    Ok(()) => log::info!("hotkey {name} = {spec}"),
                    Err(e) => {
                        log::warn!("hotkey {name} = {spec} not registered: {e}");
                        failed.push(spec.to_string());
                    }
                }
            },
            Err(e) => log::error!("hotkey {name}: {e}"),
        }
    }
    settings::set_failed_hotkeys(failed);
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
        HK_BOARD => board::toggle(),
        HK_REC_STOP => stop_recording(),
        HK_TEXT_PICK => textpick::toggle(),
        HK_REDACT_HIDE => overlay::hide_pending(),
        HK_TEXT_PAUSE => {
            use std::sync::atomic::Ordering;
            let paused = !texthist::PAUSED.fetch_xor(true, Ordering::Relaxed);
            log::info!("text history paused = {paused}");
            let text = if paused { "История текста: пауза" } else { "История текста: запись" };
            chip::show(chip::Content::Notice(text.into()), cursor());
        }
        HK_CHIP => {
            use std::sync::atomic::Ordering;
            let on = !chip::ENABLED.fetch_xor(true, Ordering::Relaxed);
            log::info!("clipboard chip = {on}");
            let p = cursor();
            let text = if on { "Чип буфера обмена: вкл" } else { "Чип буфера обмена: выкл" };
            chip::show(chip::Content::Notice(text.into()), p);
        }
        HK_INK_STICKY => set_ink_sticky(!hook::STICKY.load(std::sync::atomic::Ordering::Relaxed)),
        HK_INK_CLEAR => {
            ink::with(|i| i.clear());
        }
        HK_SPOTLIGHT => {
            let (x, y) = cursor();
            ink::with(|i| i.toggle_spotlight(x, y));
            sync_ink_features();
        }
        HK_ZOOM => {
            let (x, y) = cursor();
            ink::with(|i| i.toggle_zoom(x, y));
            sync_ink_features();
        }
        HK_RINGS => {
            ink::with(|i| i.rings_on = !i.rings_on);
            sync_ink_features();
        }
        HK_KEYS => {
            ink::with(|i| i.keys_on = !i.keys_on);
            sync_ink_features();
        }
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
                    MENU_SETTINGS => settings::open(),
                    MENU_CONFIG => shell_open(&config::config_path()),
                    MENU_HISTORY => {
                        let dir = config::app_dir().join("history");
                        let _ = std::fs::create_dir_all(&dir);
                        shell_open(&dir);
                    }
                    MENU_DICT_HELP => show_model_help(hwnd),
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
            WM_APP_CALL => {
                let f = Box::from_raw(lp.0 as *mut Box<dyn FnOnce() + Send>);
                f();
                LRESULT(0)
            }
            WM_APP_INK => {
                on_ink(wp.0, lp);
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
                chip::on_update();
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

// ! --- Screen ink --- !

fn keep_held() -> bool {
    let vk = config::current().ink.keep_vk();
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

fn cursor() -> (i32, i32) {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

fn set_ink_sticky(on: bool) {
    hook::set_sticky(on);
    ink::with(|i| {
        if !on {
            i.end_with(keep_held());
        }
        i.sticky = on;
        i.render();
    });
    log::info!("ink sticky = {on}");
}

fn on_ink(kind: usize, lp: LPARAM) {
    match kind {
        hook::INK_HOLD_DOWN => {
            let (x, y) = cursor();
            ink::with(|i| i.begin(x, y, keep_held()));
        }
        hook::INK_STICKY_DOWN => {
            let (x, y) = hook::unpack(lp);
            ink::with(|i| i.begin(x, y, keep_held()));
        }
        hook::INK_MOVE => {
            let (x, y) = hook::unpack(lp);
            ink::with(|i| i.on_move(x, y));
        }
        hook::INK_HOLD_UP | hook::INK_STICKY_UP => {
            ink::with(|i| i.end_with(keep_held()));
        }
        hook::INK_COLOR => {
            let idx = (lp.0 as usize).min(annot::PALETTE.len() - 1);
            ink::with(|i| i.set_color(annot::PALETTE[idx]));
        }
        hook::INK_STICKY_OFF => {
            // Esc: leave every modal ink mode.
            if hook::STICKY.load(std::sync::atomic::Ordering::Relaxed) {
                set_ink_sticky(false);
            }
            ink::with(|i| i.exit_modes());
            sync_ink_features();
        }
        hook::INK_ZOOM_OFF => {
            ink::with(|i| i.exit_modes());
            sync_ink_features();
        }
        hook::INK_LASER_DOWN => {
            let (x, y) = cursor();
            ink::with(|i| i.laser(true, x, y));
        }
        hook::INK_LASER_UP => {
            ink::with(|i| i.laser(false, 0, 0));
        }
        hook::INK_MARKER => {
            let (x, y) = hook::unpack(lp);
            ink::with(|i| i.marker(x, y));
        }
        hook::INK_CLICK => {
            let (x, y) = hook::unpack(lp);
            ink::with(|i| i.ring(x, y));
        }
        hook::INK_WHEEL => {
            ink::with(|i| i.wheel(lp.0 != 0));
        }
        hook::INK_DICT_DOWN => {
            if dictation::start() {
                let p = cursor();
                ink::with(|i| {
                    i.indicator = Some((p, true));
                    i.request_render();
                });
            }
        }
        hook::INK_DICT_UP => {
            ink::with(|i| {
                if let Some((p, _)) = i.indicator {
                    i.indicator = Some((p, false));
                    i.request_render();
                }
            });
            dictation::stop(|res| {
                ink::with(|i| {
                    i.indicator = None;
                    i.request_render();
                });
                match res {
                    Ok(text) if !text.is_empty() => {
                        clipboard::set_text(main_hwnd(), &text);
                        if config::current().dictation.paste {
                            send_paste();
                        }
                    }
                    Ok(_) => log::info!("dictation: nothing recognised"),
                    Err(e) => {
                        log::error!("dictation failed: {e}");
                        notify("Shelf", "Диктовка не удалась — подробности в логе");
                    }
                }
            });
        }
        hook::INK_KEY => {
            let (vk, mods) = ((lp.0 & 0xFFFF) as u32, (lp.0 >> 16) as u32 & 0xF);
            if let Some(c) = explain::format_combo(vk, mods) {
                ink::with(|i| i.key(c));
            }
        }
        _ => {}
    }
}

/// Tells the hook which ink features need mouse/key events right now.
fn sync_ink_features() {
    let bits = ink::with(|i| i.feature_bits()).unwrap_or(0);
    hook::set_features(bits);
}

// ! --- Helpers for background work --- !

pub fn main_hwnd() -> HWND {
    HWND(MAIN_HWND.load(std::sync::atomic::Ordering::Relaxed) as _)
}

/// Runs `f` on the UI thread (clipboard ownership, windows).
pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    let b: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(f));
    let p = Box::into_raw(b);
    unsafe {
        if PostMessageW(Some(main_hwnd()), WM_APP_CALL, WPARAM(0), LPARAM(p as isize)).is_err() {
            drop(Box::from_raw(p));
        }
    }
}

/// A small, silent tray balloon. Used only for things the user asked for that failed
/// or produced nothing (never after a normal capture).
pub fn notify(title: &str, text: &str) {
    let hwnd = main_hwnd();
    let mut nid = tray_data(hwnd);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
    for (dst, src) in [(&mut nid.szInfoTitle[..], title), (&mut nid.szInfo[..], text)] {
        let w: Vec<u16> = src.encode_utf16().take(dst.len() - 1).collect();
        dst[..w.len()].copy_from_slice(&w);
        dst[w.len()] = 0;
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// OCR on a worker thread, result to the clipboard as text.
pub fn ocr_to_clipboard(img: std::sync::Arc<clipboard::Image>) {
    std::thread::spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let t0 = std::time::Instant::now();
        let res = ocr::recognize(&img);
        log::info!("ocr {}x{} in {:.0} ms", img.w, img.h, t0.elapsed().as_secs_f64() * 1000.0);
        run_on_main(move || match res {
            Ok(text) if !text.trim().is_empty() => {
                clipboard::set_text(main_hwnd(), &text);
            }
            Ok(_) => notify("Shelf", "Текст не найден"),
            Err(ocr::OcrError::NoLanguages) => {
                log::warn!("OCR: no ru/en recognizer installed");
                notify(
                    "Shelf: OCR недоступен",
                    "Нет языкового пакета распознавания (ru/en). Параметры → Время и язык → Язык → добавьте язык с компонентом «Оптическое распознавание».",
                );
            }
            Err(ocr::OcrError::Failed(e)) => {
                log::error!("OCR failed: {e}");
                notify("Shelf", "Не удалось распознать текст");
            }
        });
    });
}

pub fn send_paste() {
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    };
    unsafe {
        SendInput(&[key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)], size_of::<INPUT>() as i32);
    }
}

fn show_model_help(hwnd: HWND) {
    let dir = dictation::default_model_path();
    let text = dictation::MODEL_HELP.replace("{path}", &dir.display().to_string()) + "\n\nОткрыть папку для модели?";
    let w: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let r = MessageBoxW(Some(hwnd), PCWSTR(w.as_ptr()), w!("Shelf — модель диктовки"), MB_OKCANCEL | MB_ICONINFORMATION);
        if r == IDOK {
            if let Some(parent) = dir.parent() {
                let _ = std::fs::create_dir_all(parent);
                shell_open(parent);
            }
        }
    }
}

/// After start-up warm-up the pre-allocated capture buffers are idle; let Windows page
/// them out of the working set. They come back as soft faults on the next capture.
pub fn trim_working_set() {
    unsafe {
        let _ = windows::Win32::System::ProcessStatus::EmptyWorkingSet(windows::Win32::System::Threading::GetCurrentProcess());
    }
}

// ! --- Area recording --- !

pub const GIF_FPS: u32 = 15;
pub const GIF_MAX_W: u32 = 720;

fn stop_recording() {
    if !record::is_recording() {
        return;
    }
    ink::with(|i| {
        i.rec_rect = None;
        i.request_render();
    });
    // Finalizing the MP4 takes a moment; keep the UI thread free.
    std::thread::spawn(|| match record::stop() {
        Some(Ok((tmp, first))) => {
            let cfg = config::current().record;
            let max_bytes = (cfg.max_mb.max(0.0) * 1_000_000.0) as u64;
            let (file, ext, thumb) = if cfg.format.eq_ignore_ascii_case("gif") {
                match gifout::from_video(&tmp, GIF_FPS, GIF_MAX_W, max_bytes) {
                    Ok((bytes, thumb)) => {
                        let g = tmp.with_extension("gif");
                        let _ = std::fs::write(&g, bytes);
                        let _ = std::fs::remove_file(&tmp);
                        (g, "gif", thumb)
                    }
                    Err(e) => {
                        log::error!("gif failed: {e}");
                        (tmp, "mp4", first)
                    }
                }
            } else {
                if let Err(e) = transcode::fit_to_limit(&tmp, max_bytes) {
                    log::warn!("record: could not fit the size limit: {e}");
                }
                (tmp, "mp4", first)
            };
            if let Some(path) = history::add_file(&file, ext, &thumb, "record") {
                let mb = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
                log::info!("record: saved {} ({mb:.1} MB)", path.display());
                run_on_main(move || {
                    clipboard::set_files(main_hwnd(), &[path]);
                });
            }
        }
        Some(Err(e)) => {
            log::error!("record failed: {e}");
            run_on_main(|| notify("Shelf", "Запись не удалась — подробности в логе"));
        }
        None => {}
    });
}
