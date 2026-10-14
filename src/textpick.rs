//! M13: compact text-history picker near the cursor (like Win+V), with a small form for
//! templates. Pre-created hidden window, Direct2D drawing, no animations.

use crate::texthist::{self, Entry};
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const CLASS: PCWSTR = w!("Shelf.TextPick");
const ROWS: usize = 8;

enum Mode {
    List,
    /// Filling `{fields}` of a template before pasting.
    Form { template: String, fields: Vec<(String, String)>, cur: usize },
}

struct Picker {
    hwnd: HWND,
    factory: ID2D1Factory,
    dw: IDWriteFactory,
    rt: Option<ID2D1HwndRenderTarget>,
    size: (i32, i32),
    scale: f32,
    /// Window that was active before the picker opened: Enter pastes into it.
    prev: HWND,
    query: String,
    items: Vec<Entry>,
    sel: usize,
    top: usize,
    mode: Mode,
    visible: bool,
}

thread_local! {
    static PICK: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICK.with(|p| p.try_borrow_mut().ok().and_then(|mut p| p.as_mut().map(f)))
}

pub fn init() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, CLASS, w!("Shelf text"), WS_POPUP, 0, 0, 10, 10, None, None, Some(hinst.into()), None).unwrap();
        let p = Picker {
            hwnd,
            factory: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap(),
            dw: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap(),
            rt: None,
            size: (0, 0),
            scale: 1.0,
            prev: HWND::default(),
            query: String::new(),
            items: Vec::new(),
            sel: 0,
            top: 0,
            mode: Mode::List,
            visible: false,
        };
        PICK.with(|c| *c.borrow_mut() = Some(p));
        // Warm up the render target and fonts while hidden, so the first open is instant.
        with(|p| {
            let mons = crate::capture::monitors();
            let s = crate::geom::scale_at(&mons, 0, 0);
            p.scale = s;
            p.size = ((480.0 * s) as i32, (44.0 * s + ROWS as f32 * 42.0 * s + 28.0 * s) as i32);
            p.render();
        });
        // Decrypting the history file takes a few ms: do it now, off the UI thread.
        crate::history::run_async(|| {
            let _ = texthist::view("");
        });
    }
}

pub fn toggle() {
    if with(|p| p.visible) == Some(true) {
        with(|p| p.hide());
        return;
    }
    let t0 = std::time::Instant::now();
    with(|p| p.open());
    log::info!("metric: text picker open {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
}

fn ago(t: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let d = now.saturating_sub(t);
    match d {
        0..60 => "сейчас".into(),
        60..3600 => format!("{} мин", d / 60),
        3600..86400 => format!("{} ч", d / 3600),
        _ => format!("{} дн", d / 86400),
    }
}

impl Picker {
    fn open(&mut self) {
        self.prev = unsafe { GetForegroundWindow() };
        self.query.clear();
        self.mode = Mode::List;
        self.refresh();
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let mons = crate::capture::monitors();
        self.scale = crate::geom::scale_at(&mons, pt.x, pt.y);
        let s = self.scale;
        let (w, h) = ((480.0 * s) as i32, (44.0 * s + ROWS as f32 * 42.0 * s + 28.0 * s) as i32);
        let mon = mons.iter().map(|m| m.0).find(|r| r.contains(pt.x, pt.y)).unwrap_or(crate::geom::Rect::new(0, 0, 1920, 1080));
        let (x, y) = crate::geom::place_near(pt.x, pt.y, w, h, (12.0 * s) as i32, &mon);
        if self.size != (w, h) {
            self.size = (w, h);
            if let Some(rt) = &self.rt {
                unsafe {
                    let _ = rt.Resize(&D2D_SIZE_U { width: w as u32, height: h as u32 });
                }
            }
        }
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
            crate::overlay::force_foreground(self.hwnd);
        }
        self.visible = true;
        self.render();
    }

    fn hide(&mut self) {
        self.visible = false;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn refresh(&mut self) {
        self.items = texthist::view(&self.query);
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
        self.top = self.top.min(self.sel);
    }

    fn move_sel(&mut self, d: i32) {
        if self.items.is_empty() {
            return;
        }
        self.sel = (self.sel as i32 + d).clamp(0, self.items.len() as i32 - 1) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + ROWS {
            self.top = self.sel + 1 - ROWS;
        }
        self.render();
    }

    /// Enter on an entry: templates go to the form, plain text is delivered.
    fn choose(&mut self, paste: bool) {
        let Some(e) = self.items.get(self.sel).cloned() else { return };
        let fields = texthist::template_fields(&e.text);
        if e.pinned && !fields.is_empty() {
            self.mode = Mode::Form { template: e.text, fields: fields.into_iter().map(|f| (f, String::new())).collect(), cur: 0 };
            self.render();
            return;
        }
        self.deliver(e.text, paste);
    }

    fn deliver(&mut self, text: String, paste: bool) {
        let prev = self.prev;
        self.hide();
        texthist::touch(&text);
        crate::clipboard::set_text(crate::main_hwnd(), &text);
        if paste && !prev.is_invalid() {
            unsafe {
                crate::overlay::force_foreground(prev);
            }
            // Give the target window a moment to take focus before Ctrl+V.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(60));
                crate::run_on_main(crate::send_paste);
            });
        }
    }

    fn type_char(&mut self, c: char) {
        match &mut self.mode {
            Mode::List => {
                match c {
                    '\u{8}' => {
                        self.query.pop();
                    }
                    c if !c.is_control() => self.query.push(c),
                    _ => return,
                }
                self.sel = 0;
                self.top = 0;
                self.refresh();
            }
            Mode::Form { fields, cur, .. } => {
                let v = &mut fields[*cur].1;
                match c {
                    '\u{8}' => {
                        v.pop();
                    }
                    c if !c.is_control() => v.push(c),
                    _ => return,
                }
            }
        }
        self.render();
    }

    fn key(&mut self, vk: VIRTUAL_KEY, shift: bool, ctrl: bool) {
        if let Mode::Form { template, fields, cur } = &mut self.mode {
            match vk {
                VK_ESCAPE => self.mode = Mode::List,
                VK_TAB | VK_DOWN if !(vk == VK_TAB && shift) => *cur = (*cur + 1) % fields.len(),
                VK_TAB | VK_UP => *cur = (*cur + fields.len() - 1) % fields.len(),
                VK_RETURN => {
                    let text = texthist::fill_template(template, fields);
                    self.mode = Mode::List;
                    self.deliver(text, !shift);
                    return;
                }
                _ => return,
            }
            self.render();
            return;
        }
        match vk {
            VK_ESCAPE => self.hide(),
            VK_UP => self.move_sel(-1),
            VK_DOWN => self.move_sel(1),
            VK_PRIOR => self.move_sel(-(ROWS as i32)),
            VK_NEXT => self.move_sel(ROWS as i32),
            VK_RETURN => self.choose(!shift),
            VK_P if ctrl => {
                if let Some(e) = self.items.get(self.sel) {
                    texthist::set_pinned(&e.text, !e.pinned);
                    self.refresh();
                    self.render();
                }
            }
            VK_DELETE => {
                if let Some(e) = self.items.get(self.sel) {
                    texthist::delete(&e.text);
                    self.refresh();
                    self.render();
                }
            }
            _ => {}
        }
    }

    fn row_at(&self, y: i32) -> Option<usize> {
        let s = self.scale;
        let top = 44.0 * s;
        if (y as f32) < top {
            return None;
        }
        let i = self.top + ((y as f32 - top) / (42.0 * s)) as usize;
        (i < self.items.len() && i < self.top + ROWS).then_some(i)
    }

    // ! --- Drawing --- !

    fn render(&mut self) {
        if self.rt.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let hp = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd: self.hwnd, pixelSize: D2D_SIZE_U { width: self.size.0 as u32, height: self.size.1 as u32 }, presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY };
            self.rt = unsafe { self.factory.CreateHwndRenderTarget(&props, &hp).ok() };
        }
        let Some(rt) = self.rt.clone() else { return };
        let s = self.scale;
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        unsafe {
            let brush = |c: u32, a: f32| rt.CreateSolidColorBrush(&crate::annot::rgba(c, a), None).ok();
            let fmt = |size: f32, mono: bool| {
                self.dw
                    .CreateTextFormat(if mono { w!("Consolas") } else { w!("Segoe UI") }, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size * s, w!(""))
                    .ok()
                    .inspect(|f| {
                        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    })
            };
            let text = |t: &str, f: &Option<IDWriteTextFormat>, r: D2D_RECT_F, b: &Option<ID2D1SolidColorBrush>| {
                if let (Some(f), Some(b)) = (f, b) {
                    let u: Vec<u16> = t.encode_utf16().collect();
                    rt.DrawText(&u, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, DWRITE_MEASURING_MODE_NATURAL);
                }
            };
            let (main, dim, accent, sel_bg) = (brush(0xF0F0F0, 1.0), brush(0x9A9A9A, 1.0), brush(0x2F8CFF, 1.0), brush(0x2F8CFF, 0.25));
            let (f13, f11) = (fmt(13.0, false), fmt(11.0, false));
            rt.BeginDraw();
            rt.Clear(Some(&crate::annot::rgba(0x1F1F1F, 1.0)));
            if let Some(b) = brush(0x3A3A3A, 1.0) {
                rt.DrawRectangle(&D2D_RECT_F { left: 0.5, top: 0.5, right: w - 0.5, bottom: h - 0.5 }, &b, 1.0, None);
            }
            let pad = 12.0 * s;
            match &self.mode {
                Mode::List => {
                    let q = if self.query.is_empty() { "Поиск…".to_string() } else { format!("{}▏", self.query) };
                    text(&q, &fmt(14.0, false), D2D_RECT_F { left: pad, top: 11.0 * s, right: w - pad, bottom: 40.0 * s }, if self.query.is_empty() { &dim } else { &main });
                    if let Some(b) = brush(0x3A3A3A, 1.0) {
                        rt.DrawLine(windows_numerics::Vector2::new(0.0, 43.0 * s), windows_numerics::Vector2::new(w, 43.0 * s), &b, 1.0, None);
                    }
                    if self.items.is_empty() {
                        text(if self.query.is_empty() { "История текста пуста" } else { "Ничего не найдено" }, &f13, D2D_RECT_F { left: pad, top: 56.0 * s, right: w, bottom: 80.0 * s }, &dim);
                    }
                    for (row, e) in self.items.iter().enumerate().skip(self.top).take(ROWS) {
                        let y = 44.0 * s + (row - self.top) as f32 * 42.0 * s;
                        if row == self.sel {
                            if let Some(b) = &sel_bg {
                                rt.FillRectangle(&D2D_RECT_F { left: 0.0, top: y, right: w, bottom: y + 42.0 * s }, b);
                            }
                        }
                        let first = e.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                        let pin = if e.pinned { "★ " } else { "" };
                        let fields = if e.pinned && !texthist::template_fields(&e.text).is_empty() { " · шаблон" } else { "" };
                        text(&format!("{pin}{}", crate::clipkind::clip_chars(first, 70)), &f13, D2D_RECT_F { left: pad, top: y + 4.0 * s, right: w - pad, bottom: y + 24.0 * s }, if e.pinned { &accent } else { &main });
                        let lines = e.text.lines().count();
                        let meta = format!("{} · {} · {} симв.{}{fields}", ago(e.time), if e.source.is_empty() { "—" } else { &e.source }, e.text.chars().count(), if lines > 1 { format!(" · {lines} стр.") } else { String::new() });
                        text(&meta, &f11, D2D_RECT_F { left: pad, top: y + 23.0 * s, right: w - pad, bottom: y + 40.0 * s }, &dim);
                    }
                    text("Enter — вставить · Shift+Enter — в буфер · Ctrl+P — закрепить · Del — удалить", &f11, D2D_RECT_F { left: pad, top: h - 22.0 * s, right: w, bottom: h }, &dim);
                }
                Mode::Form { template, fields, cur } => {
                    text("Шаблон — заполните поля", &fmt(14.0, false), D2D_RECT_F { left: pad, top: 11.0 * s, right: w - pad, bottom: 40.0 * s }, &main);
                    for (i, (name, val)) in fields.iter().enumerate().take(ROWS - 2) {
                        let y = 48.0 * s + i as f32 * 34.0 * s;
                        let box_r = D2D_RECT_F { left: pad + 120.0 * s, top: y, right: w - pad, bottom: y + 28.0 * s };
                        text(name, &f13, D2D_RECT_F { left: pad, top: y + 5.0 * s, right: pad + 115.0 * s, bottom: y + 28.0 * s }, &dim);
                        if let Some(b) = brush(if i == *cur { 0x2F8CFF } else { 0x3A3A3A }, 1.0) {
                            rt.DrawRectangle(&box_r, &b, 1.0, None);
                        }
                        let shown = if i == *cur { format!("{val}▏") } else { val.clone() };
                        text(&shown, &f13, D2D_RECT_F { left: box_r.left + 6.0 * s, top: y + 5.0 * s, right: box_r.right - 4.0 * s, bottom: y + 26.0 * s }, &main);
                    }
                    let preview = texthist::fill_template(template, fields);
                    let first = preview.lines().next().unwrap_or("");
                    text(&crate::clipkind::clip_chars(first, 80), &fmt(12.0, true), D2D_RECT_F { left: pad, top: h - 48.0 * s, right: w - pad, bottom: h - 26.0 * s }, &dim);
                    text("Tab — следующее поле · Enter — вставить · Shift+Enter — в буфер · Esc — назад", &f11, D2D_RECT_F { left: pad, top: h - 22.0 * s, right: w, bottom: h }, &dim);
                }
            }
            if rt.EndDraw(None, None).is_err() {
                self.rt = None;
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with(|p| p.render());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_ACTIVATE => {
                if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    with(|p| p.hide());
                }
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                with(|p| p.key(VIRTUAL_KEY(wp.0 as u16), shift, ctrl));
                LRESULT(0)
            }
            WM_CHAR => {
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    if !ctrl && c != '\r' && c != '\t' && c != '\u{1b}' {
                        with(|p| p.type_char(c));
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
                with(|p| {
                    if let (Mode::List, Some(i)) = (&p.mode, p.row_at(y)) {
                        p.sel = i;
                        p.choose(true);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let up = (wp.0 >> 16) as u16 as i16 > 0;
                with(|p| p.move_sel(if up { -1 } else { 1 }));
                LRESULT(0)
            }
            WM_DPICHANGED => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
