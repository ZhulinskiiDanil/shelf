//! The shelf: a grid of recent captures. Native window + Direct2D, pre-created and
//! only shown/hidden, like the capture overlay.

use crate::clipboard;
use crate::geom::{self, Rect};
use crate::history::{self, Entry};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const CLASS: PCWSTR = w!("Shelf.Shelf");
const WM_APP_REFRESH: u32 = WM_APP + 20;
const COLS: i32 = 5;
const ROWS: i32 = 3;

const CMD_COPY: usize = 1;
const CMD_PATH: usize = 2;
const CMD_FOLDER: usize = 3;
const CMD_PIN: usize = 4;
const CMD_DELETE: usize = 5;
const CMD_OPEN: usize = 6;
const CMD_TEXT: usize = 7;
const CMD_PRIVACY: usize = 8;

struct Shelf {
    hwnd: HWND,
    factory: ID2D1Factory,
    rt: Option<ID2D1HwndRenderTarget>,
    dw: IDWriteFactory,
    items: Vec<Entry>,
    thumbs: HashMap<String, Option<ID2D1Bitmap>>,
    selected: HashSet<usize>,
    anchor: usize,
    scroll: i32,
    scale: f32,
    size: (i32, i32),
    press: Option<(i32, i32)>,
    /// Menu or drag loop running: ignore deactivation.
    modal: bool,
    visible: bool,
    // M13 "Текст" tab
    text_tab: bool,
    query: String,
    texts: Vec<crate::texthist::Entry>,
    tsel: usize,
    ttop: usize,
}

thread_local! {
    static SHELF: RefCell<Option<Shelf>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Shelf) -> R) -> Option<R> {
    SHELF.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

pub fn init() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, CLASS, w!("Shelf"), WS_POPUP, 0, 0, 100, 100, None, None, Some(hinst.into()), None).unwrap();
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
        let s = Shelf {
            hwnd,
            factory,
            rt: None,
            dw,
            items: Vec::new(),
            thumbs: HashMap::new(),
            selected: HashSet::new(),
            anchor: 0,
            scroll: 0,
            scale: 1.0,
            size: (0, 0),
            press: None,
            modal: false,
            visible: false,
            text_tab: false,
            query: String::new(),
            texts: Vec::new(),
            tsel: 0,
            ttop: 0,
        };
        SHELF.with(|c| *c.borrow_mut() = Some(s));
        let raw = hwnd.0 as isize;
        history::on_change(move || {
            let _ = PostMessageW(Some(HWND(raw as _)), WM_APP_REFRESH, WPARAM(0), LPARAM(0));
        });
    }
}

pub fn toggle() {
    if with(|s| s.visible) == Some(true) {
        with(|s| s.hide());
    } else {
        let t0 = std::time::Instant::now();
        with(|s| s.show());
        log::info!("metric: shelf open {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
    }
}

impl Shelf {
    fn cell(&self) -> (i32, i32) {
        ((200.0 * self.scale) as i32, (140.0 * self.scale) as i32)
    }
    fn pad(&self) -> i32 {
        (8.0 * self.scale) as i32
    }
    fn header(&self) -> i32 {
        (34.0 * self.scale) as i32
    }

    fn show(&mut self) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let mons = crate::capture::monitors();
        self.scale = geom::scale_at(&mons, pt.x, pt.y);
        let mon = mons.iter().map(|m| m.0).find(|r| r.contains(pt.x, pt.y)).unwrap_or(Rect::new(0, 0, 1920, 1080));
        let (cw, ch) = self.cell();
        let w = COLS * cw + (COLS + 1) * self.pad();
        let h = self.header() + ROWS * ch + (ROWS + 1) * self.pad();
        let x = mon.l + (mon.w() - w) / 2;
        let y = mon.t + (mon.h() - h) / 2;
        self.size = (w, h);
        self.items = history::list();
        self.selected.clear();
        self.scroll = 0;
        if self.rt.is_some() {
            unsafe {
                let _ = self.rt.as_ref().unwrap().Resize(&D2D_SIZE_U { width: w as u32, height: h as u32 });
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
        self.press = None;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        // Free GPU bitmaps; thumbnails reload quickly from disk.
        self.thumbs.clear();
    }

    fn refresh(&mut self) {
        if !self.visible {
            return;
        }
        let sel: HashSet<String> = self.selected.iter().filter_map(|&i| self.items.get(i)).map(|e| e.file.clone()).collect();
        self.items = history::list();
        self.selected = self.items.iter().enumerate().filter(|(_, e)| sel.contains(&e.file)).map(|(i, _)| i).collect();
        self.clamp_scroll();
        self.redraw();
    }

    fn rows_total(&self) -> i32 {
        (self.items.len() as i32 + COLS - 1) / COLS
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0, (self.rows_total() - ROWS).max(0));
    }

    fn item_rect(&self, i: usize) -> Rect {
        let (cw, ch) = self.cell();
        let p = self.pad();
        let col = i as i32 % COLS;
        let row = i as i32 / COLS - self.scroll;
        let l = p + col * (cw + p);
        let t = self.header() + p + row * (ch + p);
        Rect::new(l, t, l + cw, t + ch)
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.items.len()).find(|&i| self.item_rect(i).contains(x, y) && self.visible_row(i))
    }

    fn visible_row(&self, i: usize) -> bool {
        let row = i as i32 / COLS - self.scroll;
        (0..ROWS).contains(&row)
    }

    fn sorted_selection(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.selected.iter().copied().filter(|&i| i < self.items.len()).collect();
        v.sort_unstable();
        v
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.sorted_selection().iter().map(|&i| history::path_of(&self.items[i].file)).collect()
    }

    fn selected_files(&self) -> Vec<String> {
        self.sorted_selection().iter().map(|&i| self.items[i].file.clone()).collect()
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    // ! --- Rendering --- !

    fn ensure_rt(&mut self) -> Option<ID2D1HwndRenderTarget> {
        if self.rt.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U { width: self.size.0 as u32, height: self.size.1 as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
            };
            self.rt = unsafe { self.factory.CreateHwndRenderTarget(&props, &hprops).ok() };
            self.thumbs.clear();
        }
        self.rt.clone()
    }

    fn thumb(&mut self, rt: &ID2D1HwndRenderTarget, file: &str) -> Option<ID2D1Bitmap> {
        if let Some(b) = self.thumbs.get(file) {
            return b.clone();
        }
        let bmp = history::load_thumb(file).and_then(|img| unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            rt.CreateBitmap(D2D_SIZE_U { width: img.w, height: img.h }, Some(img.bgra.as_ptr() as _), img.w * 4, &props).ok()
        });
        self.thumbs.insert(file.to_string(), bmp.clone());
        bmp
    }

    fn render(&mut self) {
        let Some(rt) = self.ensure_rt() else { return };
        let s = self.scale;
        unsafe {
            let brush = |c: u32, a: f32| rt.CreateSolidColorBrush(&crate::annot::rgba(c, a), None).ok();
            let (Some(bg), Some(card), Some(accent), Some(text), Some(dim)) =
                (brush(0x1E1E1E, 1.0), brush(0x2D2D2D, 1.0), brush(0x2F8CFF, 1.0), brush(0xF0F0F0, 1.0), brush(0x9A9A9A, 1.0))
            else {
                return;
            };
            let fmt = self
                .dw
                .CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 12.0 * s, w!(""))
                .ok();
            rt.BeginDraw();
            rt.Clear(Some(&crate::annot::rgba(0x1E1E1E, 1.0)));
            let full = D2D_RECT_F { left: 0.0, top: 0.0, right: self.size.0 as f32, bottom: self.size.1 as f32 };
            rt.FillRectangle(&full, &bg);
            rt.DrawRectangle(&full, &card, 2.0, None);
            if let Some(f) = &fmt {
                self.render_tabs(&rt, f, &accent, &text, &dim);
                let hint = if self.text_tab {
                    "Enter, Ctrl+C — копировать   ·   перетащите наружу   ·   Ctrl+P — закрепить   ·   Del — удалить   ·   Tab — снимки".to_string()
                } else if self.items.is_empty() {
                    "Shelf is empty — captures and copied images appear here".to_string()
                } else {
                    format!(
                        "{} items   ·   drag out   ·   Ctrl+C image   ·   Ctrl+Shift+C path   ·   T text   ·   R privacy   ·   P pin   ·   Del delete   ·   O folder",
                        self.items.len()
                    )
                };
                let t: Vec<u16> = hint.encode_utf16().collect();
                let r = D2D_RECT_F { left: 160.0 * s, top: 9.0 * s, right: self.size.0 as f32, bottom: self.header() as f32 };
                rt.DrawText(&t, f, &r, &dim, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
            if self.text_tab {
                let small = self.dw.CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 11.0 * s, w!("")).ok();
                let big = self.dw.CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 13.0 * s, w!("")).ok();
                if let (Some(b), Some(sm), Some(selb)) = (big, small, brush(0x2F8CFF, 0.25)) {
                    let _ = b.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    let _ = sm.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    self.render_text_tab(&rt, &b, &sm, &accent, &text, &dim, &selb);
                }
            }
            for i in 0..if self.text_tab { 0 } else { self.items.len() } {
                if !self.visible_row(i) {
                    continue;
                }
                let r = self.item_rect(i);
                let rf = D2D_RECT_F { left: r.l as f32, top: r.t as f32, right: r.r as f32, bottom: r.b as f32 };
                rt.FillRectangle(&rf, &card);
                let file = self.items[i].file.clone();
                if let Some(b) = self.thumb(&rt, &file) {
                    let sz = b.GetSize();
                    let k = ((r.w() - 8) as f32 / sz.width).min((r.h() - 8) as f32 / sz.height).min(1.0 * s.max(1.0));
                    let (w, h) = (sz.width * k, sz.height * k);
                    let cx = (r.l + r.r) as f32 / 2.0;
                    let cy = (r.t + r.b) as f32 / 2.0;
                    let d = D2D_RECT_F { left: cx - w / 2.0, top: cy - h / 2.0, right: cx + w / 2.0, bottom: cy + h / 2.0 };
                    rt.DrawBitmap(&b, Some(&d), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
                if self.items[i].file.ends_with(".mp4") {
                    if let (Some(f), Some(bg)) = (&fmt, brush(0x000000, 0.7)) {
                        let t: Vec<u16> = "▶ MP4".encode_utf16().collect();
                        let lr = D2D_RECT_F { left: rf.right - 58.0 * s, top: rf.bottom - 22.0 * s, right: rf.right - 4.0 * s, bottom: rf.bottom - 4.0 * s };
                        rt.FillRectangle(&lr, &bg);
                        let tr = D2D_RECT_F { left: lr.left + 6.0 * s, top: lr.top + 1.0 * s, right: lr.right, bottom: lr.bottom };
                        rt.DrawText(&t, f, &tr, &text, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                }
                if self.items[i].pinned {
                    if let Some(f) = &fmt {
                        let t: Vec<u16> = "★".encode_utf16().collect();
                        let pr = D2D_RECT_F { left: rf.left + 4.0 * s, top: rf.top + 2.0 * s, right: rf.right, bottom: rf.bottom };
                        rt.DrawText(&t, f, &pr, &accent, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                }
                if self.selected.contains(&i) {
                    rt.DrawRectangle(&rf, &accent, 3.0 * s, None);
                }
            }
            if rt.EndDraw(None, None).is_err() {
                self.rt = None;
            }
        }
    }

    // ! --- Actions --- !

    fn copy_image(&self) {
        let Some(&i) = self.sorted_selection().first() else { return };
        let path = history::path_of(&self.items[i].file);
        if let Some(img) = std::fs::read(&path).ok().and_then(|b| crate::imageio::decode_png(&b)) {
            clipboard::set_image(self.hwnd, std::sync::Arc::new(img));
        }
    }

    fn copy_text(&self) {
        let Some(&i) = self.sorted_selection().first() else { return };
        let path = history::path_of(&self.items[i].file);
        if let Some(img) = std::fs::read(&path).ok().and_then(|b| crate::imageio::decode_png(&b)) {
            crate::ocr_to_clipboard(std::sync::Arc::new(img));
        }
    }

    /// Opens the selected capture in markup and runs the personal-data check (M15).
    fn check_privacy(&mut self) {
        let Some(&i) = self.sorted_selection().first() else { return };
        let path = history::path_of(&self.items[i].file);
        let Some(img) = std::fs::read(&path).ok().and_then(|b| crate::imageio::decode_png(&b)) else { return };
        self.hide();
        crate::overlay::show_image(std::sync::Arc::new(img));
        crate::overlay::start_redaction();
    }

    fn copy_paths(&self) {
        let text = self.selected_paths().iter().map(|p| format!("\"{}\"", p.display())).collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            clipboard::set_text(self.hwnd, &text);
        }
    }

    fn open_folder(&self) {
        let target = self.selected_paths().into_iter().next();
        let args = match target {
            Some(p) => format!("/select,\"{}\"", p.display()),
            None => format!("\"{}\"", history::dir().display()),
        };
        let a: Vec<u16> = args.encode_utf16().chain(Some(0)).collect();
        unsafe {
            ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(a.as_ptr()), None, SW_SHOWNORMAL);
        }
    }

    fn open_file(&self) {
        if let Some(p) = self.selected_paths().first() {
            crate::shell_open(p);
        }
    }

    fn toggle_pin(&mut self) {
        let files = self.selected_files();
        if files.is_empty() {
            return;
        }
        let pin = !self.sorted_selection().iter().all(|&i| self.items[i].pinned);
        history::set_pinned(&files, pin);
    }

    fn delete(&mut self) {
        let files = self.selected_files();
        if !files.is_empty() {
            history::delete(&files);
            self.selected.clear();
        }
    }

    fn click(&mut self, i: usize, ctrl: bool, shift: bool) {
        if shift {
            let (a, b) = (self.anchor.min(i), self.anchor.max(i));
            if !ctrl {
                self.selected.clear();
            }
            self.selected.extend(a..=b);
        } else if ctrl {
            if !self.selected.remove(&i) {
                self.selected.insert(i);
            }
            self.anchor = i;
        } else {
            self.selected.clear();
            self.selected.insert(i);
            self.anchor = i;
        }
        self.redraw();
    }

    fn move_sel(&mut self, d: i32) {
        if self.items.is_empty() {
            return;
        }
        let cur = self.sorted_selection().first().copied().map(|i| i as i32).unwrap_or(-d.signum());
        let n = (cur + d).clamp(0, self.items.len() as i32 - 1) as usize;
        self.selected.clear();
        self.selected.insert(n);
        self.anchor = n;
        let row = n as i32 / COLS;
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + ROWS {
            self.scroll = row - ROWS + 1;
        }
        self.redraw();
    }
}

/// Runs an action that may pump messages (menu, drag loop) outside the shelf borrow.
fn modal(f: impl FnOnce()) {
    with(|s| s.modal = true);
    f();
    with(|s| s.modal = false);
}

fn start_drag() {
    let Some(paths) = with(|s| s.selected_paths()) else { return };
    if paths.is_empty() {
        return;
    }
    let mut dropped = false;
    modal(|| dropped = crate::dragdrop::drag_files(&paths, None, false));
    if dropped {
        with(|s| s.hide());
    }
}

fn context_menu(x: i32, y: i32) {
    let Some(pinned) = with(|s| !s.sorted_selection().is_empty() && s.sorted_selection().iter().all(|&i| s.items[i].pinned)) else { return };
    let hwnd = with(|s| s.hwnd).unwrap();
    let cmd = unsafe {
        let m = CreatePopupMenu().unwrap();
        let _ = AppendMenuW(m, MF_STRING, CMD_OPEN, w!("Open\tEnter"));
        let _ = AppendMenuW(m, MF_STRING, CMD_COPY, w!("Copy image\tCtrl+C"));
        let _ = AppendMenuW(m, MF_STRING, CMD_PATH, w!("Copy path\tCtrl+Shift+C"));
        let _ = AppendMenuW(m, MF_STRING, CMD_FOLDER, w!("Show in folder\tO"));
        let _ = AppendMenuW(m, MF_STRING, CMD_PIN, if pinned { w!("Unpin\tP") } else { w!("Pin\tP") });
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(m, MF_STRING, CMD_DELETE, w!("Delete\tDel"));
        let mut c = 0;
        modal(|| c = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, None, hwnd, None).0);
        let _ = DestroyMenu(m);
        c as usize
    };
    with(|s| match cmd {
        CMD_OPEN => s.open_file(),
        CMD_COPY => s.copy_image(),
        CMD_PATH => s.copy_paths(),
        CMD_TEXT => s.copy_text(),
        CMD_PRIVACY => s.check_privacy(),
        CMD_FOLDER => s.open_folder(),
        CMD_PIN => s.toggle_pin(),
        CMD_DELETE => s.delete(),
        _ => {}
    });
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

fn lp_xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with(|s| s.render());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_APP_REFRESH => {
                with(|s| s.refresh());
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE && with(|s| !s.modal) == Some(true) {
                    with(|s| s.hide());
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = lp_xy(lp);
                let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
                with(|s| {
                    let (shots, texts) = s.tab_rects();
                    if shots.contains(x, y) || texts.contains(x, y) {
                        s.set_tab(texts.contains(x, y));
                        return;
                    }
                    if s.text_tab {
                        if let Some(i) = s.text_row_at(y) {
                            s.tsel = i;
                            s.redraw();
                            s.press = Some((x, y));
                        }
                        return;
                    }
                    match s.hit(x, y) {
                        Some(i) if s.selected.contains(&i) && !ctrl && !shift => {}
                        Some(i) => s.click(i, ctrl, shift),
                        None => {
                            s.selected.clear();
                            s.redraw();
                        }
                    }
                    s.press = Some((x, y));
                });
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = lp_xy(lp);
                let drag = with(|s| match s.press {
                    Some((px, py)) if (x - px).abs() > 4 || (y - py).abs() > 4 => {
                        s.press = None;
                        if s.text_tab { s.text_row_at(py).is_some() } else { s.hit(px, py).is_some() }
                    }
                    _ => false,
                });
                if drag == Some(true) {
                    let _ = ReleaseCapture();
                    if with(|s| s.text_tab) == Some(true) {
                        start_text_drag();
                    } else {
                        start_drag();
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                let (x, y) = lp_xy(lp);
                let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
                with(|s| {
                    if s.press.take().is_some() && !ctrl && !shift {
                        if let Some(i) = s.hit(x, y) {
                            s.click(i, false, false);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                with(|s| if s.text_tab { s.text_copy() } else { s.open_file() });
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                if with(|s| s.text_tab) == Some(true) {
                    return LRESULT(0);
                }
                let (x, y) = lp_xy(lp);
                with(|s| {
                    if let Some(i) = s.hit(x, y) {
                        if !s.selected.contains(&i) {
                            s.click(i, false, false);
                        }
                    }
                });
                let mut pt = POINT { x, y };
                let _ = ClientToScreen(hwnd, &mut pt);
                context_menu(pt.x, pt.y);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = (wp.0 >> 16) as u16 as i16;
                with(|s| {
                    if s.text_tab {
                        s.text_move(if delta > 0 { -3 } else { 3 });
                        return;
                    }
                    s.scroll += if delta > 0 { -1 } else { 1 };
                    s.clamp_scroll();
                    s.redraw();
                });
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
                if with(|s| s.text_tab) == Some(true) {
                    with(|s| s.text_key(vk, ctrl));
                    return LRESULT(0);
                }
                if vk == VK_TAB {
                    with(|s| s.set_tab(true));
                    return LRESULT(0);
                }
                with(|s| match vk {
                    VK_ESCAPE => s.hide(),
                    VK_A if ctrl => {
                        s.selected = (0..s.items.len()).collect();
                        s.redraw();
                    }
                    VK_C if ctrl && shift => s.copy_paths(),
                    VK_C if ctrl => s.copy_image(),
                    VK_DELETE => s.delete(),
                    VK_P => s.toggle_pin(),
                    VK_T => s.copy_text(),
                    VK_R => s.check_privacy(),
                    VK_O => s.open_folder(),
                    VK_RETURN => s.open_file(),
                    VK_LEFT => s.move_sel(-1),
                    VK_RIGHT => s.move_sel(1),
                    VK_UP => s.move_sel(-COLS),
                    VK_DOWN => s.move_sel(COLS),
                    _ => {}
                });
                LRESULT(0)
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    // Printable characters and Backspace edit the search; Tab/Enter/Esc are keys.
                    if !key_down(VK_CONTROL) && !c.is_control() || c == '\u{8}' {
                        with(|s| if s.text_tab { s.text_char(c) });
                    }
                }
                LRESULT(0)
            }
            WM_DPICHANGED => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

// ! --- Text tab (M13) --- !

impl Shelf {
    fn text_row_h(&self) -> f32 {
        40.0 * self.scale
    }

    /// Rows that fit under the header and the search line.
    fn text_rows(&self) -> usize {
        ((self.size.1 as f32 - self.header() as f32 - 34.0 * self.scale) / self.text_row_h()).max(1.0) as usize
    }

    fn refresh_texts(&mut self) {
        self.texts = crate::texthist::view(&self.query);
        self.tsel = self.tsel.min(self.texts.len().saturating_sub(1));
        self.ttop = self.ttop.min(self.tsel);
    }

    fn set_tab(&mut self, text: bool) {
        self.text_tab = text;
        if text {
            self.refresh_texts();
        }
        self.redraw();
    }

    /// Header tab rectangles: (shots, text), in client pixels.
    fn tab_rects(&self) -> (Rect, Rect) {
        let s = self.scale;
        let h = self.header();
        (Rect::new((8.0 * s) as i32, 0, (80.0 * s) as i32, h), Rect::new((84.0 * s) as i32, 0, (150.0 * s) as i32, h))
    }

    fn text_row_at(&self, y: i32) -> Option<usize> {
        let top = self.header() as f32 + 34.0 * self.scale;
        if (y as f32) < top {
            return None;
        }
        let i = self.ttop + ((y as f32 - top) / self.text_row_h()) as usize;
        (i < self.texts.len() && i < self.ttop + self.text_rows()).then_some(i)
    }

    fn text_move(&mut self, d: i32) {
        if self.texts.is_empty() {
            return;
        }
        self.tsel = (self.tsel as i32 + d).clamp(0, self.texts.len() as i32 - 1) as usize;
        let rows = self.text_rows();
        if self.tsel < self.ttop {
            self.ttop = self.tsel;
        } else if self.tsel >= self.ttop + rows {
            self.ttop = self.tsel + 1 - rows;
        }
        self.redraw();
    }

    fn text_copy(&self) {
        if let Some(e) = self.texts.get(self.tsel) {
            crate::texthist::touch(&e.text);
            clipboard::set_text(self.hwnd, &e.text);
        }
    }

    fn text_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool) {
        match vk {
            VK_ESCAPE => self.hide(),
            VK_TAB => self.set_tab(false),
            VK_UP => self.text_move(-1),
            VK_DOWN => self.text_move(1),
            VK_PRIOR => self.text_move(-(self.text_rows() as i32)),
            VK_NEXT => self.text_move(self.text_rows() as i32),
            VK_RETURN => self.text_copy(),
            VK_C if ctrl => self.text_copy(),
            VK_P if ctrl => {
                if let Some(e) = self.texts.get(self.tsel) {
                    crate::texthist::set_pinned(&e.text, !e.pinned);
                    self.refresh_texts();
                    self.redraw();
                }
            }
            VK_DELETE => {
                if let Some(e) = self.texts.get(self.tsel) {
                    crate::texthist::delete(&e.text);
                    self.refresh_texts();
                    self.redraw();
                }
            }
            _ => {}
        }
    }

    fn text_char(&mut self, c: char) {
        match c {
            '\u{8}' => {
                self.query.pop();
            }
            c if !c.is_control() => self.query.push(c),
            _ => return,
        }
        self.tsel = 0;
        self.ttop = 0;
        self.refresh_texts();
        self.redraw();
    }

    unsafe fn render_tabs(&self, rt: &ID2D1HwndRenderTarget, fmt: &IDWriteTextFormat, accent: &ID2D1SolidColorBrush, text: &ID2D1SolidColorBrush, dim: &ID2D1SolidColorBrush) {
        let (a, b) = self.tab_rects();
        let s = self.scale;
        for (r, label, on) in [(a, "Снимки", !self.text_tab), (b, "Текст", self.text_tab)] {
            let t: Vec<u16> = label.encode_utf16().collect();
            let rr = D2D_RECT_F { left: r.l as f32 + 4.0 * s, top: 9.0 * s, right: r.r as f32, bottom: r.b as f32 };
            unsafe {
                rt.DrawText(&t, fmt, &rr, if on { text } else { dim }, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                if on {
                    rt.FillRectangle(&D2D_RECT_F { left: r.l as f32 + 4.0 * s, top: r.b as f32 - 3.0 * s, right: r.r as f32 - 8.0 * s, bottom: r.b as f32 - 1.0 * s }, accent);
                }
            }
        }
    }

    unsafe fn render_text_tab(&self, rt: &ID2D1HwndRenderTarget, fmt: &IDWriteTextFormat, small: &IDWriteTextFormat, accent: &ID2D1SolidColorBrush, text: &ID2D1SolidColorBrush, dim: &ID2D1SolidColorBrush, sel: &ID2D1SolidColorBrush) {
        let s = self.scale;
        let w = self.size.0 as f32;
        let top = self.header() as f32;
        let draw = |t: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
            let u: Vec<u16> = t.encode_utf16().collect();
            unsafe { rt.DrawText(&u, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL) };
        };
        let q = if self.query.is_empty() { "Поиск: начните печатать…".to_string() } else { format!("Поиск: {}▏", self.query) };
        draw(&q, fmt, D2D_RECT_F { left: 12.0 * s, top: top + 6.0 * s, right: w, bottom: top + 30.0 * s }, if self.query.is_empty() { dim } else { text });
        let y0 = top + 34.0 * s;
        if self.texts.is_empty() {
            draw("Нет записей", fmt, D2D_RECT_F { left: 12.0 * s, top: y0 + 8.0 * s, right: w, bottom: y0 + 30.0 * s }, dim);
        }
        let rh = self.text_row_h();
        for (i, e) in self.texts.iter().enumerate().skip(self.ttop).take(self.text_rows()) {
            let y = y0 + (i - self.ttop) as f32 * rh;
            if i == self.tsel {
                unsafe { rt.FillRectangle(&D2D_RECT_F { left: 2.0, top: y, right: w - 2.0, bottom: y + rh }, sel) };
            }
            let first = e.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            let pin = if e.pinned { "★ " } else { "" };
            draw(&format!("{pin}{}", crate::clipkind::clip_chars(first, 120)), fmt, D2D_RECT_F { left: 12.0 * s, top: y + 3.0 * s, right: w - 12.0 * s, bottom: y + 22.0 * s }, if e.pinned { accent } else { text });
            let meta = format!("{} · {} симв. · {}", if e.source.is_empty() { "—" } else { &e.source }, e.text.chars().count(), if e.pinned && !crate::texthist::template_fields(&e.text).is_empty() { "шаблон" } else { "" });
            draw(&meta, small, D2D_RECT_F { left: 12.0 * s, top: y + 21.0 * s, right: w - 12.0 * s, bottom: y + 38.0 * s }, dim);
        }
    }
}

fn start_text_drag() {
    let Some(text) = with(|s| s.texts.get(s.tsel).map(|e| e.text.clone())).flatten() else { return };
    let mut dropped = false;
    modal(|| dropped = crate::dragdrop::drag_text(&text));
    if dropped {
        with(|s| s.hide());
    }
}
