//! Settings window: native controls, created on demand (opens in a few ms), no animations.
//! Values are written into config.toml (comments preserved); hot reload applies them.

use crate::settings_model::{self as model, FIELDS, Kind, PAGES};
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
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
    /// Page heading above the fields.
    title: HWND,
    title_font: HFONT,
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
                        set_check(c.1, val == "1");
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
            hbrBackground: HBRUSH(brushes()[0] as _),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mons = crate::capture::monitors();
        let s = crate::geom::scale_at(&mons, pt.x, pt.y);
        let mon = mons.iter().map(|m| m.0).find(|r| r.contains(pt.x, pt.y)).unwrap_or(crate::geom::Rect::new(0, 0, 1280, 800));
        let px = |v: f32| (v * s).round() as i32;
        // Size the window to the longest page (Hotkeys has ~18 rows) so rows never run into
        // the status line and buttons; on a short screen the rows get tighter instead.
        let max_rows = PAGES.iter().map(|p| FIELDS.iter().filter(|f| f.page == *p).count()).max().unwrap_or(1) as i32;
        let (m, footer) = (px(12.0), px(126.0));
        let head = px(44.0);
        let mut row = px(40.0);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let frame = |cw: i32, ch: i32| {
            let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
            let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE::default(), (96.0 * s) as u32);
            (r.right - r.left, r.bottom - r.top)
        };
        let avail = (mon.h() as f32 * 0.92) as i32 - (frame(100, 100).1 - 100);
        if head + m + max_rows * row + footer > avail {
            row = ((avail - head - m - footer) / max_rows).max(px(34.0));
        }
        let (w, h) = frame(px(900.0), head + m + max_rows * row + footer);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            w!("Shelf — настройки"),
            style,
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
        dark_title_bar(hwnd);
        *SCALE.lock().unwrap() = s;
        let font = CreateFontW(-px(15.0), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let child = |class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, x: i32, y: i32, cw: i32, ch: i32, id: i32| -> HWND {
            let t = wide(text);
            let c = CreateWindowExW(ex, class, PCWSTR(t.as_ptr()), WS_CHILD | WS_VISIBLE | style, x, y, cw, ch, Some(hwnd), Some(HMENU(id as _)), Some(hinst.into()), None).unwrap_or_default();
            SendMessageW(c, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            c
        };
        let list_w = px(176.0);
        SIDE_W.store(list_w, std::sync::atomic::Ordering::Relaxed);
        ITEM_H.store(px(36.0), std::sync::atomic::Ordering::Relaxed);
        // Owner-drawn page list on the darker sidebar.
        let pages = child(w!("LISTBOX"), "", WINDOW_STYLE((LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT | WS_TABSTOP.0 as i32) as u32), WINDOW_EX_STYLE::default(), 0, m + px(8.0), list_w, rc.bottom - m - px(8.0), ID_PAGES);
        for p in PAGES {
            let t = wide(p);
            SendMessageW(pages, LB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        }
        let cfg = crate::config::current();
        let x0 = list_w + px(28.0);
        let label_w = px(400.0);
        let edit_w = rc.right - x0 - label_w - px(28.0);
        VALUE_X.store(label_w, std::sync::atomic::Ordering::Relaxed);
        let mut rows_on_page = std::collections::HashMap::<&str, i32>::new();
        let mut controls = Vec::new();
        for (i, fl) in FIELDS.iter().enumerate() {
            let r = rows_on_page.entry(fl.page).or_insert(0);
            let y = head + m + *r * row;
            *r += 1;
            let val = model::read(&cfg, fl);
            let id = ID_FIELD + i as i32;
            let (label, editor) = if fl.kind == Kind::Bool {
                let cb = child(w!("BUTTON"), fl.label, WINDOW_STYLE((BS_OWNERDRAW | WS_TABSTOP.0 as i32) as u32), WINDOW_EX_STYLE::default(), x0, y + (row - px(28.0)) / 2, label_w + edit_w, px(28.0), id);
                set_check(cb, val == "1");
                (HWND::default(), cb)
            } else {
                // One line with an ellipsis rather than a second line that the next row would cover.
                let l = child(w!("STATIC"), fl.label, WINDOW_STYLE(0x000C /* SS_LEFTNOWORDWRAP */ | 0x4000 /* SS_ENDELLIPSIS */), WINDOW_EX_STYLE::default(), x0, y + (row - px(20.0)) / 2, label_w - m, px(20.0), 0);
                // Borderless edit; the rounded frame around it is painted by the window.
                let eh = px(20.0);
                let e = child(w!("EDIT"), &val, WINDOW_STYLE((ES_AUTOHSCROLL | WS_TABSTOP.0 as i32) as u32), WINDOW_EX_STYLE::default(), x0 + label_w + px(10.0), y + (row - eh) / 2, edit_w - px(20.0), eh, id);
                (l, e)
            };
            controls.push((label, editor, val));
        }
        let by = rc.bottom - px(52.0);
        let (bw, bh, gap, rm) = (px(140.0), px(36.0), px(10.0), px(28.0));
        let owner = WINDOW_STYLE((BS_OWNERDRAW | WS_TABSTOP.0 as i32) as u32);
        child(w!("BUTTON"), "Закрыть", owner, WINDOW_EX_STYLE::default(), rc.right - rm - bw, by, bw, bh, ID_CLOSE);
        child(w!("BUTTON"), "Открыть config.toml", owner, WINDOW_EX_STYLE::default(), rc.right - rm - bw * 2 - gap - px(30.0), by, bw + px(30.0), bh, ID_OPEN);
        child(w!("BUTTON"), "Сохранить", owner, WINDOW_EX_STYLE::default(), rc.right - rm - bw * 3 - gap * 2 - px(30.0), by, bw, bh, ID_SAVE);
        let status = child(w!("STATIC"), "", WINDOW_STYLE(0), WINDOW_EX_STYLE::default(), x0, head + m + max_rows * row + px(8.0), rc.right - x0 - m, px(56.0), 0);
        STATUS.store(status.0 as isize, std::sync::atomic::Ordering::Relaxed);
        let title_font = CreateFontW(-px(24.0), 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let title = child(w!("STATIC"), PAGES[0], WINDOW_STYLE(0), WINDOW_EX_STYLE::default(), x0, m + px(10.0), rc.right - x0 - m, px(32.0), 0);
        SendMessageW(title, WM_SETFONT, Some(WPARAM(title_font.0 as usize)), Some(LPARAM(1)));
        TITLE.store(title.0 as isize, std::sync::atomic::Ordering::Relaxed);
        SETTINGS.with(|c| *c.borrow_mut() = Some(Settings { hwnd, pages, status, title, title_font, controls, font }));
        SendMessageW(pages, LB_SETCURSEL, Some(WPARAM(0)), None);
        show_page(0);
    }
}

fn set_status(text: &str) {
    let color = if text.starts_with("Сохранено") { theme::OK } else if text.is_empty() || text == "Нет изменений" { theme::MUTED } else { theme::WARN };
    STATUS_COLOR.store(color, std::sync::atomic::Ordering::Relaxed);
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
            let t = wide(name);
            unsafe {
                let _ = SetWindowTextW(s.title, PCWSTR(t.as_ptr()));
            }
            for (fl, (label, editor, _)) in FIELDS.iter().zip(&s.controls) {
                let cmd = if fl.page == name { SW_SHOW } else { SW_HIDE };
                unsafe {
                    if !label.is_invalid() {
                        let _ = ShowWindow(*label, cmd);
                    }
                    let _ = ShowWindow(*editor, cmd);
                }
            }
            unsafe {
                let _ = InvalidateRect(Some(s.hwnd), None, true);
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
                let on = get_check(*editor);
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
                    id if id >= ID_FIELD => {
                        let ctl = HWND(lp.0 as _);
                        let is_switch = FIELDS.get((id - ID_FIELD) as usize).is_some_and(|f| f.kind == Kind::Bool);
                        if is_switch && code == BN_CLICKED {
                            set_check(ctl, !get_check(ctl));
                        } else if code == EN_SETFOCUS || code == EN_KILLFOCUS {
                            // Focus ring on the field frame.
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                let dc = HDC(wp.0 as _);
                let ctl = lp.0;
                let c = if ctl == TITLE.load(std::sync::atomic::Ordering::Relaxed) {
                    theme::TEXT
                } else if ctl == STATUS.load(std::sync::atomic::Ordering::Relaxed) {
                    STATUS_COLOR.load(std::sync::atomic::Ordering::Relaxed)
                } else {
                    theme::LABEL
                };
                SetTextColor(dc, cref(c));
                SetBkColor(dc, cref(theme::BG));
                LRESULT(brushes()[0])
            }
            WM_CTLCOLOREDIT => {
                let dc = HDC(wp.0 as _);
                SetTextColor(dc, cref(theme::TEXT));
                SetBkColor(dc, cref(theme::FIELD));
                LRESULT(brushes()[2])
            }
            WM_CTLCOLORLISTBOX => LRESULT(brushes()[1]),
            WM_MEASUREITEM => {
                let mi = &mut *(lp.0 as *mut MEASUREITEMSTRUCT);
                mi.itemHeight = ITEM_H.load(std::sync::atomic::Ordering::Relaxed) as u32;
                LRESULT(1)
            }
            WM_DRAWITEM => {
                draw_item(&*(lp.0 as *const DRAWITEMSTRUCT));
                LRESULT(1)
            }
            WM_PAINT => {
                paint_window(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_DESTROY => {
                SETTINGS.with(|s| {
                    if let Some(s) = s.borrow_mut().take() {
                        let _ = DeleteObject(s.font.into());
                        let _ = DeleteObject(s.title_font.into());
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

// ! --- Dark theme (M16.2) --- !
// Native controls keep their behaviour (typing, Tab order, IME); only their colours change.
// Everything Shelf draws itself (sidebar, field frames, switches, buttons) goes through a
// Direct2D DC render target for antialiased rounded shapes.

mod theme {
    pub const BG: u32 = 0x1C1C1E;
    pub const SIDE: u32 = 0x161618;
    pub const FIELD: u32 = 0x2A2A2E;
    pub const BORDER: u32 = 0x3A3A3F;
    pub const TEXT: u32 = 0xEDEDF0;
    pub const LABEL: u32 = 0xC9C9CF;
    pub const MUTED: u32 = 0x8E8E93;
    pub const ACCENT: u32 = 0x2F8CFF;
    pub const OK: u32 = 0x4CD964;
    pub const WARN: u32 = 0xFFB340;
}

/// RGB (0xRRGGBB) → COLORREF (0x00BBGGRR).
fn cref(c: u32) -> COLORREF {
    COLORREF(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
}

/// GDI brushes for WM_CTLCOLOR*, created once (raw handles: the window lives as long as the app).
static BRUSHES: std::sync::OnceLock<[isize; 3]> = std::sync::OnceLock::new();

fn brushes() -> [isize; 3] {
    *BRUSHES.get_or_init(|| unsafe { [CreateSolidBrush(cref(theme::BG)).0 as isize, CreateSolidBrush(cref(theme::SIDE)).0 as isize, CreateSolidBrush(cref(theme::FIELD)).0 as isize] })
}

static STATUS: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static STATUS_COLOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(theme::MUTED);
static TITLE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Sidebar width and the item height of the page list, in pixels.
static SIDE_W: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static ITEM_H: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(30);
static SCALE: std::sync::Mutex<f32> = std::sync::Mutex::new(1.0);

struct Painter {
    factory: windows::Win32::Graphics::Direct2D::ID2D1Factory,
    rt: windows::Win32::Graphics::Direct2D::ID2D1DCRenderTarget,
    dw: windows::Win32::Graphics::DirectWrite::IDWriteFactory,
}

thread_local! {
    static PAINTER: RefCell<Option<Painter>> = const { RefCell::new(None) };
}

/// Runs `f` with a D2D target bound to `dc` over `rc` (client coordinates of that DC).
fn paint(dc: HDC, rc: &RECT, f: impl FnOnce(&windows::Win32::Graphics::Direct2D::ID2D1RenderTarget, &windows::Win32::Graphics::DirectWrite::IDWriteFactory)) {
    use windows::Win32::Graphics::Direct2D::Common::*;
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    PAINTER.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            unsafe {
                let Ok(factory) = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) else { return };
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                    ..Default::default()
                };
                let Ok(rt) = factory.CreateDCRenderTarget(&props) else { return };
                let Ok(dw) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return };
                *p = Some(Painter { factory, rt, dw });
            }
        }
        let Some(p) = p.as_ref() else { return };
        let _ = &p.factory;
        unsafe {
            if p.rt.BindDC(dc, rc).is_err() {
                return;
            }
            p.rt.BeginDraw();
            p.rt.SetTransform(&windows_numerics::Matrix3x2::translation(-rc.left as f32, -rc.top as f32));
            f(&p.rt, &p.dw);
            let _ = p.rt.EndDraw(None, None);
        }
    });
}

fn d2_rect(r: &RECT) -> windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
    windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: r.left as f32, top: r.top as f32, right: r.right as f32, bottom: r.bottom as f32 }
}

fn round_rect(rt: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget, r: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F, radius: f32, fill: Option<(u32, f32)>, stroke: Option<(u32, f32)>) {
    use windows::Win32::Graphics::Direct2D::*;
    let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
    unsafe {
        if let Some((c, a)) = fill {
            if let Ok(b) = rt.CreateSolidColorBrush(&crate::annot::rgba(c, a), None) {
                rt.FillRoundedRectangle(&rr, &b);
            }
        }
        if let Some((c, w)) = stroke {
            if let Ok(b) = rt.CreateSolidColorBrush(&crate::annot::rgba(c, 1.0), None) {
                let i = w / 2.0;
                let inner = D2D1_ROUNDED_RECT { rect: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: r.left + i, top: r.top + i, right: r.right - i, bottom: r.bottom - i }, radiusX: radius, radiusY: radius };
                rt.DrawRoundedRectangle(&inner, &b, w, None);
            }
        }
    }
}

fn text(rt: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget, dw: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, s: &str, r: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F, size: f32, color: u32, bold: bool, center: bool) {
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::DirectWrite::*;
    unsafe {
        let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let Ok(f) = dw.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("")) else { return };
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        if center {
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        if let Ok(b) = rt.CreateSolidColorBrush(&crate::annot::rgba(color, 1.0), None) {
            let u: Vec<u16> = s.encode_utf16().collect();
            rt.DrawText(&u, &f, &r, &b, D2D1_DRAW_TEXT_OPTIONS_CLIP, windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL);
        }
    }
}

fn scale() -> f32 {
    *SCALE.lock().unwrap()
}

/// Background, sidebar and the frames around the visible text fields.
fn paint_window(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let s = scale();
        let side = SIDE_W.load(std::sync::atomic::Ordering::Relaxed);
        let focus = GetFocus();
        // Visible text fields: their frames are drawn around the borderless EDIT controls.
        let fields: Vec<(RECT, bool)> = SETTINGS.with(|st| {
            st.try_borrow()
                .ok()
                .and_then(|st| {
                    st.as_ref().map(|st| {
                        st.controls
                            .iter()
                            .zip(FIELDS.iter())
                            .filter(|(c, fl)| fl.kind != Kind::Bool && IsWindowVisible(c.1).as_bool())
                            .map(|(c, _)| {
                                let mut r = RECT::default();
                                let _ = GetWindowRect(c.1, &mut r);
                                let mut pts = [POINT { x: r.left, y: r.top }, POINT { x: r.right, y: r.bottom }];
                                MapWindowPoints(None, Some(hwnd), &mut pts);
                                (RECT { left: pts[0].x, top: pts[0].y, right: pts[1].x, bottom: pts[1].y }, c.1 == focus)
                            })
                            .collect()
                    })
                })
                .unwrap_or_default()
        });
        paint(dc, &rc, |rt, _| {
            if let Ok(b) = rt.CreateSolidColorBrush(&crate::annot::rgba(theme::BG, 1.0), None) {
                rt.FillRectangle(&d2_rect(&rc), &b);
            }
            if let Ok(b) = rt.CreateSolidColorBrush(&crate::annot::rgba(theme::SIDE, 1.0), None) {
                rt.FillRectangle(&d2_rect(&RECT { left: 0, top: 0, right: side, bottom: rc.bottom }), &b);
            }
            for (r, focused) in &fields {
                let (px, py) = (10.0 * s, ((30.0 * s - (r.bottom - r.top) as f32) / 2.0).max(3.0 * s));
                let f = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: r.left as f32 - px, top: r.top as f32 - py, right: r.right as f32 + px, bottom: r.bottom as f32 + py };
                round_rect(rt, f, 7.0 * s, Some((theme::FIELD, 1.0)), Some((if *focused { theme::ACCENT } else { theme::BORDER }, if *focused { 1.6 * s } else { 1.0 })));
            }
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Owner-drawn page list items, switches and buttons.
fn draw_item(di: &DRAWITEMSTRUCT) {
    let s = scale();
    let rc = di.rcItem;
    let selected = di.itemState.0 & ODS_SELECTED.0 != 0;
    let id = di.CtlID as i32;
    paint(di.hDC, &rc, |rt, dw| {
        let r = d2_rect(&rc);
        if id == ID_PAGES {
            if let Ok(b) = unsafe { rt.CreateSolidColorBrush(&crate::annot::rgba(theme::SIDE, 1.0), None) } {
                unsafe { rt.FillRectangle(&r, &b) };
            }
            let Some(name) = PAGES.get(di.itemID as usize) else { return };
            let pill = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: r.left + 8.0 * s, top: r.top + 2.0 * s, right: r.right - 8.0 * s, bottom: r.bottom - 2.0 * s };
            if selected {
                round_rect(rt, pill, 7.0 * s, Some((theme::FIELD, 1.0)), None);
                round_rect(rt, windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: pill.left, top: pill.top + 8.0 * s, right: pill.left + 3.0 * s, bottom: pill.bottom - 8.0 * s }, 1.5 * s, Some((theme::ACCENT, 1.0)), None);
            }
            let tr = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: pill.left + 14.0 * s, ..pill };
            text(rt, dw, name, tr, 14.0 * s, if selected { theme::TEXT } else { theme::MUTED }, selected, false);
        } else if id == ID_SAVE || id == ID_OPEN || id == ID_CLOSE {
            let (bg, fg) = if id == ID_SAVE { (theme::ACCENT, 0xFFFFFF) } else { (theme::FIELD, theme::TEXT) };
            if let Ok(b) = unsafe { rt.CreateSolidColorBrush(&crate::annot::rgba(theme::BG, 1.0), None) } {
                unsafe { rt.FillRectangle(&r, &b) };
            }
            round_rect(rt, r, 8.0 * s, Some((bg, if selected { 0.75 } else { 1.0 })), if id == ID_SAVE { None } else { Some((theme::BORDER, 1.0)) });
            let label = match id {
                ID_SAVE => "Сохранить",
                ID_OPEN => "Открыть config.toml",
                _ => "Закрыть",
            };
            text(rt, dw, label, r, 14.0 * s, fg, id == ID_SAVE, true);
        } else if id >= ID_FIELD {
            // Switch row: label on the left, switch at the start of the value column.
            if let Ok(b) = unsafe { rt.CreateSolidColorBrush(&crate::annot::rgba(theme::BG, 1.0), None) } {
                unsafe { rt.FillRectangle(&r, &b) };
            }
            let on = unsafe { GetWindowLongPtrW(di.hwndItem, GWLP_USERDATA) } != 0;
            let label = FIELDS.get((id - ID_FIELD) as usize).map(|f| f.label).unwrap_or("");
            let value_x = r.left + VALUE_X.load(std::sync::atomic::Ordering::Relaxed) as f32;
            text(rt, dw, label, windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { right: value_x - 12.0 * s, ..r }, 15.0 * s, theme::LABEL, false, false);
            let (sw, sh) = (40.0 * s, 22.0 * s);
            let cy = (r.top + r.bottom) / 2.0;
            let track = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: value_x, top: cy - sh / 2.0, right: value_x + sw, bottom: cy + sh / 2.0 };
            round_rect(rt, track, sh / 2.0, Some((if on { theme::ACCENT } else { theme::BORDER }, 1.0)), None);
            let k = sh / 2.0 - 3.0 * s;
            let kx = if on { track.right - sh / 2.0 } else { track.left + sh / 2.0 };
            if let Ok(b) = unsafe { rt.CreateSolidColorBrush(&crate::annot::rgba(if on { 0xFFFFFF } else { 0xC8C8CC }, 1.0), None) } {
                let e = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: windows_numerics::Vector2::new(kx, cy), radiusX: k, radiusY: k };
                unsafe { rt.FillEllipse(&e, &b) };
            }
            text(rt, dw, if on { "Вкл" } else { "Выкл" }, windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: track.right + 10.0 * s, ..r }, 13.0 * s, theme::MUTED, false, false);
        }
    });
}

/// x of the value column inside a switch row (row starts at the label column).
static VALUE_X: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

fn get_check(h: HWND) -> bool {
    unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) != 0 }
}

fn set_check(h: HWND, on: bool) {
    unsafe {
        SetWindowLongPtrW(h, GWLP_USERDATA, on as isize);
        let _ = InvalidateRect(Some(h), None, false);
    }
}

fn dark_title_bar(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::*;
    unsafe {
        let on: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, 4);
        let cap = cref(theme::BG).0;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &cap as *const _ as _, 4);
        let border = cref(theme::BORDER).0;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
    }
}
