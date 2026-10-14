//! Settings window: native controls, created on demand (opens in a few ms), no animations.
//! Values are written into config.toml (comments preserved); hot reload applies them.

use crate::settings_model::{self as model, FIELDS, Kind, PAGES};
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const CLASS: PCWSTR = w!("Shelf.Settings");
const ID_PAGES: i32 = 900;
const ID_SAVE: i32 = 901;
const ID_OPEN: i32 = 902;
const ID_CLOSE: i32 = 903;
const ID_FIELD: i32 = 1000;

struct Settings {
    hwnd: HWND,
    pages: HWND,
    status: HWND,
    /// Per field: (label, editor, value shown when the window opened).
    controls: Vec<(HWND, HWND, String)>,
    font: HFONT,
}

thread_local! {
    static SETTINGS: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Hotkeys that Windows refused to register (taken by another program), set by main.
static FAILED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn set_failed_hotkeys(v: Vec<String>) {
    *FAILED.lock().unwrap() = v;
}

/// Lets Tab/Enter work between the controls (call from the main message loop).
pub fn is_dialog_message(msg: &MSG) -> bool {
    SETTINGS.with(|s| match s.try_borrow().ok().and_then(|s| s.as_ref().map(|s| s.hwnd)) {
        Some(h) => unsafe { IsDialogMessageW(h, msg).as_bool() },
        None => false,
    })
}

/// Creates the (hidden) window at start-up, so opening it later is just a show.
pub fn init() {
    unsafe { create() };
}

pub fn open() {
    let t0 = std::time::Instant::now();
    let Some(hwnd) = SETTINGS.with(|s| s.borrow().as_ref().map(|s| s.hwnd)) else { return };
    refresh();
    unsafe {
        // Centre on the monitor under the cursor.
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = crate::capture::monitors().into_iter().map(|m| m.0).find(|r| r.contains(pt.x, pt.y));
        let mut wr = RECT::default();
        let _ = GetWindowRect(hwnd, &mut wr);
        if let Some(m) = mon {
            let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
            let _ = SetWindowPos(hwnd, None, m.l + (m.w() - w) / 2, m.t + (m.h() - h) / 2, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        crate::overlay::force_foreground(hwnd);
    }
    log::info!("metric: settings open {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
}

/// Loads the current config into the controls and shows known problems.
fn refresh() {
    let cfg = crate::config::current();
    SETTINGS.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            for (fl, c) in FIELDS.iter().zip(s.controls.iter_mut()) {
                let val = model::read(&cfg, fl);
                unsafe {
                    if fl.kind == Kind::Bool {
                        SendMessageW(c.1, BM_SETCHECK, Some(WPARAM((val == "1") as usize)), None);
                    } else {
                        let t = wide(&val);
                        let _ = SetWindowTextW(c.1, PCWSTR(t.as_ptr()));
                    }
                }
                c.2 = val;
            }
        }
    });
    let mut problems = model::global_conflicts(&cfg);
    problems.extend(FAILED.lock().unwrap().iter().map(|h| format!("{h} занят другой программой")));
    set_status(&problems.join("
"));
}

fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

unsafe fn create() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mons = crate::capture::monitors();
        let s = crate::geom::scale_at(&mons, pt.x, pt.y);
        let mon = mons.iter().map(|m| m.0).find(|r| r.contains(pt.x, pt.y)).unwrap_or(crate::geom::Rect::new(0, 0, 1280, 800));
        let px = |v: f32| (v * s).round() as i32;
        let (w, h) = (px(780.0), px(560.0));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            w!("Shelf — настройки"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            mon.l + (mon.w() - w) / 2,
            mon.t + (mon.h() - h) / 2,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let font = CreateFontW(-px(15.0), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let child = |class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, x: i32, y: i32, cw: i32, ch: i32, id: i32| -> HWND {
            let t = wide(text);
            let c = CreateWindowExW(ex, class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | style, x, y, cw, ch, Some(hwnd), Some(HMENU(id as _)), Some(hinst.into()), None).unwrap_or_default();
            SendMessageW(c, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            c
        };
        let m = px(12.0);
        let list_w = px(150.0);
        let pages = child(w!("LISTBOX"), "", WINDOW_STYLE((LBS_NOTIFY | WS_TABSTOP.0 as i32 | WS_BORDER.0 as i32) as u32), WINDOW_EX_STYLE::default(), m, m, list_w, rc.bottom - px(70.0), ID_PAGES);
        for p in PAGES {
            let t = wide(p);
            SendMessageW(pages, LB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        }
        let cfg = crate::config::current();
        let x0 = m * 2 + list_w;
        let label_w = px(330.0);
        let edit_w = rc.right - x0 - label_w - m;
        let row = px(32.0);
        let mut rows_on_page = std::collections::HashMap::<&str, i32>::new();
        let mut controls = Vec::new();
        for (i, fl) in FIELDS.iter().enumerate() {
            let r = rows_on_page.entry(fl.page).or_insert(0);
            let y = m + *r * row;
            *r += 1;
            let val = model::read(&cfg, fl);
            let id = ID_FIELD + i as i32;
            let (label, editor) = if fl.kind == Kind::Bool {
                let cb = child(w!("BUTTON"), fl.label, WINDOW_STYLE((BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32) as u32), WINDOW_EX_STYLE::default(), x0, y + px(4.0), label_w + edit_w, px(24.0), id);
                SendMessageW(cb, BM_SETCHECK, Some(WPARAM(if val == "1" { 1 } else { 0 })), None);
                (HWND::default(), cb)
            } else {
                let l = child(w!("STATIC"), fl.label, WINDOW_STYLE(0), WINDOW_EX_STYLE::default(), x0, y + px(6.0), label_w - m, px(22.0), 0);
                let e = child(w!("EDIT"), &val, WINDOW_STYLE((ES_AUTOHSCROLL | WS_TABSTOP.0 as i32) as u32), WS_EX_CLIENTEDGE, x0 + label_w, y + px(2.0), edit_w, px(26.0), id);
                (l, e)
            };
            controls.push((label, editor, val));
        }
        let by = rc.bottom - px(46.0);
        let bw = px(150.0);
        child(w!("BUTTON"), "Сохранить", WINDOW_STYLE((BS_DEFPUSHBUTTON | WS_TABSTOP.0 as i32) as u32), WINDOW_EX_STYLE::default(), rc.right - m - bw * 3 - m * 2, by, bw, px(32.0), ID_SAVE);
        child(w!("BUTTON"), "Открыть config.toml", WINDOW_STYLE(WS_TABSTOP.0), WINDOW_EX_STYLE::default(), rc.right - m - bw * 2 - m, by, bw, px(32.0), ID_OPEN);
        child(w!("BUTTON"), "Закрыть", WINDOW_STYLE(WS_TABSTOP.0), WINDOW_EX_STYLE::default(), rc.right - m - bw, by, bw, px(32.0), ID_CLOSE);
        let status = child(w!("STATIC"), "", WINDOW_STYLE(0), WINDOW_EX_STYLE::default(), x0, rc.bottom - px(108.0), rc.right - x0 - m, px(56.0), 0);
        SETTINGS.with(|c| *c.borrow_mut() = Some(Settings { hwnd, pages, status, controls, font }));
        SendMessageW(pages, LB_SETCURSEL, Some(WPARAM(0)), None);
        show_page(0);
    }
}

fn set_status(text: &str) {
    SETTINGS.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            let t = wide(text);
            unsafe {
                let _ = SetWindowTextW(s.status, PCWSTR(t.as_ptr()));
            }
        }
    });
}

fn show_page(page: usize) {
    let name = PAGES[page.min(PAGES.len() - 1)];
    SETTINGS.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            for (fl, (label, editor, _)) in FIELDS.iter().zip(&s.controls) {
                let cmd = if fl.page == name { SW_SHOW } else { SW_HIDE };
                unsafe {
                    if !label.is_invalid() {
                        let _ = ShowWindow(*label, cmd);
                    }
                    let _ = ShowWindow(*editor, cmd);
                }
            }
        }
    });
}

fn editor_text(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(h, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

fn save() {
    let Some((updates, errors)) = SETTINGS.with(|s| {
        let s = s.borrow();
        let s = s.as_ref()?;
        let mut updates = Vec::new();
        let mut errors = Vec::new();
        for (fl, (_, editor, original)) in FIELDS.iter().zip(&s.controls) {
            let text = if fl.kind == Kind::Bool {
                let on = unsafe { SendMessageW(*editor, BM_GETCHECK, None, None).0 } == 1;
                if on { "1".to_string() } else { "0".to_string() }
            } else {
                editor_text(*editor)
            };
            if text.trim() == original.trim() {
                continue;
            }
            match model::parse(fl, &text) {
                Ok(v) => updates.push((fl, v)),
                Err(e) => errors.push(e),
            }
        }
        Some((updates, errors))
    }) else {
        return;
    };
    if !errors.is_empty() {
        set_status(&errors.join("\n"));
        return;
    }
    let path = crate::config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| crate::config::DEFAULT_TOML.to_string());
    let mut doc: toml_edit::DocumentMut = match text.parse() {
        Ok(d) => d,
        Err(e) => {
            set_status(&format!("config.toml повреждён, исправьте вручную: {e}"));
            return;
        }
    };
    model::apply(&mut doc, &updates);
    let new_text = doc.to_string();
    let cfg = match crate::config::parse(&new_text) {
        Ok(c) => c,
        Err(e) => {
            set_status(&e);
            return;
        }
    };
    let conflicts = model::global_conflicts(&cfg);
    if !conflicts.is_empty() {
        set_status(&format!("Не сохранено — конфликт клавиш:\n{}", conflicts.join("\n")));
        return;
    }
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, &new_text).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        set_status("Не удалось записать config.toml");
        return;
    }
    // Fresh baseline for the next save.
    SETTINGS.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            for (fl, c) in FIELDS.iter().zip(s.controls.iter_mut()) {
                c.2 = model::read(&cfg, fl);
            }
        }
    });
    set_status(if updates.is_empty() { "Нет изменений" } else { "Сохранено, применяется сразу" });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xFFFF) as i32;
                let code = ((wp.0 >> 16) & 0xFFFF) as u32;
                match id {
                    ID_PAGES if code == LBN_SELCHANGE => {
                        let sel = SETTINGS.with(|s| s.borrow().as_ref().map(|s| SendMessageW(s.pages, LB_GETCURSEL, None, None).0)).unwrap_or(0);
                        show_page(sel.max(0) as usize);
                    }
                    ID_SAVE => save(),
                    ID_OPEN => crate::shell_open(&crate::config::config_path()),
                    ID_CLOSE | 2 /* IDCANCEL: Esc */ => hide(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                let dc = HDC(wp.0 as _);
                SetBkMode(dc, TRANSPARENT);
                LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
            }
            WM_DESTROY => {
                SETTINGS.with(|s| {
                    if let Some(s) = s.borrow_mut().take() {
                        let _ = DeleteObject(s.font.into());
                    }
                });
                LRESULT(0)
            }
            WM_CLOSE => {
                hide(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
