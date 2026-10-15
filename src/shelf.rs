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
    res: crate::annot::Res,
    /// Card under the cursor.
    hover: Option<usize>,
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
        round_corners(hwnd);
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
        let res_factory = factory.clone();
        let s = Shelf {
            hwnd,
            factory: res_factory,
            rt: None,
            dw,
            res: crate::annot::Res::new(&factory),
            hover: None,
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
        ((208.0 * self.scale) as i32, (132.0 * self.scale) as i32)
    }
    fn pad(&self) -> i32 {
        (12.0 * self.scale) as i32
    }
    fn header(&self) -> i32 {
        (46.0 * self.scale) as i32
    }
    fn footer(&self) -> i32 {
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
        let h = self.header() + ROWS * ch + (ROWS + 1) * self.pad() + self.footer();
        let x = mon.l + (mon.w() - w) / 2;
        let y = mon.t + (mon.h() - h) / 2;
        self.size = (w, h);
        self.items = history::list();
        self.selected.clear();
        self.scroll = 0;
        self.hover = None;
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
            let (Some(card), Some(accent), Some(text), Some(dim), Some(line)) =
                (brush(0x2A2A2E, 1.0), brush(crate::ui::ACCENT, 1.0), brush(0xF2F2F2, 1.0), brush(0x8E8E93, 1.0), brush(0xFFFFFF, 0.07))
            else {
                return;
            };
            let font = |size: f32, weight: DWRITE_FONT_WEIGHT| self.dw.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size * s, w!("")).ok();
            let fmt = font(12.0, DWRITE_FONT_WEIGHT_NORMAL);
            let tabf = font(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD);
            let draw = |t: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
                let u: Vec<u16> = t.encode_utf16().collect();
                rt.DrawText(&u, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            };
            let (w, h) = (self.size.0 as f32, self.size.1 as f32);
            rt.BeginDraw();
            rt.Clear(Some(&crate::annot::rgba(0x1C1C1E, 1.0)));
            // Header: tabs left, count right; hairline under it. Footer: hints for what is selected.
            if let (Some(tf), Some(f)) = (&tabf, &fmt) {
                self.render_tabs(&rt, tf, &accent, &text, &dim);
                let count = if self.text_tab { plural(self.texts.len(), "запись", "записи", "записей") } else { plural(self.items.len(), "снимок", "снимка", "снимков") };
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
                draw(&count, f, D2D_RECT_F { left: 0.0, top: 15.0 * s, right: w - 16.0 * s, bottom: self.header() as f32 }, &dim);
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                rt.FillRectangle(&D2D_RECT_F { left: 0.0, top: self.header() as f32 - 1.0, right: w, bottom: self.header() as f32 }, &line);
                let fy = h - self.footer() as f32;
                rt.FillRectangle(&D2D_RECT_F { left: 0.0, top: fy, right: w, bottom: fy + 1.0 }, &line);
                draw(&self.footer_hint(), f, D2D_RECT_F { left: 16.0 * s, top: fy + 8.0 * s, right: w - 16.0 * s, bottom: h }, &dim);
            }
            if self.text_tab {
                let small = font(11.0, DWRITE_FONT_WEIGHT_NORMAL);
                let big = font(13.0, DWRITE_FONT_WEIGHT_NORMAL);
                if let (Some(b), Some(sm), Some(selb)) = (big, small, brush(crate::ui::ACCENT, 0.22)) {
                    let _ = b.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    let _ = sm.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    self.render_text_tab(&rt, &b, &sm, &accent, &text, &dim, &selb);
                }
            } else if self.items.is_empty() {
                if let (Some(big), Some(f)) = (font(16.0, DWRITE_FONT_WEIGHT_SEMI_BOLD), &fmt) {
                    let _ = big.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let cy = h / 2.0;
                    draw("Здесь появятся снимки", &big, D2D_RECT_F { left: 0.0, top: cy - 24.0 * s, right: w, bottom: cy }, &text);
                    let key = crate::config::current().hotkeys.capture;
                    draw(&format!("Нажмите {key}, выделите область — снимок сразу окажется на полке"), f, D2D_RECT_F { left: 0.0, top: cy + 4.0 * s, right: w, bottom: cy + 28.0 * s }, &dim);
                    let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                }
            }
            let radius = 10.0 * s;
            for i in 0..if self.text_tab { 0 } else { self.items.len() } {
                if !self.visible_row(i) {
                    continue;
                }
                let r = self.item_rect(i);
                let rf = D2D_RECT_F { left: r.l as f32, top: r.t as f32, right: r.r as f32, bottom: r.b as f32 };
                let shape = D2D1_ROUNDED_RECT { rect: rf, radiusX: radius, radiusY: radius };
                let hovered = self.hover == Some(i);
                let selected = self.selected.contains(&i);
                // Content clipped to the rounded card.
                let geo = self.factory.CreateRoundedRectangleGeometry(&shape).ok();
                let params = D2D1_LAYER_PARAMETERS {
                    contentBounds: rf,
                    geometricMask: std::mem::ManuallyDrop::new(geo.map(|g| g.into())),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: windows_numerics::Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: std::mem::ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS_NONE,
                };
                rt.PushLayer(&params, None::<&ID2D1Layer>);
                rt.FillRectangle(&rf, &card);
                let file = self.items[i].file.clone();
                if let Some(b) = self.thumb(&rt, &file) {
                    // Cover the card (crop the overflow), centred.
                    let sz = b.GetSize();
                    let k = (rf.right - rf.left) / sz.width;
                    let k = k.max((rf.bottom - rf.top) / sz.height);
                    let (tw, th) = (sz.width * k, sz.height * k);
                    let cx = (rf.left + rf.right) / 2.0;
                    let cy = (rf.top + rf.bottom) / 2.0;
                    let d = D2D_RECT_F { left: cx - tw / 2.0, top: cy - th / 2.0, right: cx + tw / 2.0, bottom: cy + th / 2.0 };
                    rt.DrawBitmap(&b, Some(&d), if hovered || selected { 1.0 } else { 0.92 }, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
                if hovered {
                    // Meta strip: age · size · weight.
                    if let (Some(shade), Some(f)) = (brush(0, 0.62), &fmt) {
                        let strip = D2D_RECT_F { left: rf.left, top: rf.bottom - 26.0 * s, right: rf.right, bottom: rf.bottom };
                        rt.FillRectangle(&strip, &shade);
                        let e = &self.items[i];
                        let meta = format!("{}  ·  {}×{}  ·  {}", age(e.created), e.w, e.h, human_size(e.bytes));
                        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                        draw(&meta, f, D2D_RECT_F { left: strip.left + 10.0 * s, top: strip.top + 5.0 * s, right: strip.right - 6.0 * s, bottom: strip.bottom }, &text);
                    }
                }
                rt.PopLayer();
                let file = &self.items[i].file;
                let badge = if file.ends_with(".mp4") { Some("▶  MP4") } else if file.ends_with(".gif") { Some("▶  GIF") } else { None };
                if let Some(badge) = badge {
                    let y = if hovered { rf.bottom - 50.0 * s } else { rf.bottom - 28.0 * s };
                    crate::ui::pill(&rt, &self.res, badge, rf.left + 8.0 * s, y, s, 0xFFFFFF);
                }
                if self.items[i].pinned {
                    crate::ui::pill(&rt, &self.res, "★", rf.right - 30.0 * s, rf.top + 8.0 * s, s, 0xFFD54F);
                }
                if selected {
                    let ring = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: rf.left - 1.5 * s, top: rf.top - 1.5 * s, right: rf.right + 1.5 * s, bottom: rf.bottom + 1.5 * s }, radiusX: radius + 1.5 * s, radiusY: radius + 1.5 * s };
                    rt.DrawRoundedRectangle(&ring, &accent, 2.5 * s, None);
                } else if let Some(b) = brush(0xFFFFFF, if hovered { 0.22 } else { 0.06 }) {
                    rt.DrawRoundedRectangle(&shape, &b, 1.0, None);
                }
            }
            if rt.EndDraw(None, None).is_err() {
                self.rt = None;
            }
        }
    }

    /// Hints for the footer: what the keys do for the current selection.
    fn footer_hint(&self) -> String {
        if self.text_tab {
            return "Enter, Ctrl+C — копировать   ·   перетащите наружу   ·   Ctrl+P — закрепить   ·   Del — удалить   ·   Tab — снимки".into();
        }
        let sel = self.sorted_selection();
        let Some(&i) = sel.first() else {
            return "Выберите снимок или перетащите его в чат   ·   Tab — текст   ·   Esc — закрыть".into();
        };
        let f = &self.items[i].file;
        if sel.len() > 1 {
            format!("Выбрано: {}   ·   перетащите наружу   ·   Ctrl+Shift+C — пути   ·   P — закрепить   ·   Del — удалить", sel.len())
        } else if f.ends_with(".mp4") {
            "Ctrl+C — копировать файл   ·   G — сделать GIF   ·   Enter — открыть   ·   P — закрепить   ·   Del — удалить".into()
        } else if f.ends_with(".gif") {
            "Ctrl+C — копировать файл   ·   Enter — открыть   ·   O — в папке   ·   P — закрепить   ·   Del — удалить".into()
        } else {
            "Ctrl+C — копировать   ·   T — текст   ·   R — проверить личное   ·   Ctrl+Shift+C — путь   ·   P — закрепить   ·   Del — удалить".into()
        }
    }

    // ! --- Actions --- !

    fn copy_image(&self) {
        let Some(&i) = self.sorted_selection().first() else { return };
        let path = history::path_of(&self.items[i].file);
        // Videos and GIFs go to the clipboard as files: Ctrl+V in a chat attaches them.
        if !self.items[i].file.ends_with(".png") {
            clipboard::set_files(self.hwnd, &[path]);
            return;
        }
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

    /// Converts the selected recording to a GIF (M16): added to the shelf and copied as a file.
    fn make_gif(&self) {
        let Some(&i) = self.sorted_selection().first() else { return };
        if !self.items[i].file.ends_with(".mp4") {
            return;
        }
        let src = history::path_of(&self.items[i].file);
        let max_bytes = (crate::config::current().record.max_mb.max(0.0) * 1_000_000.0) as u64;
        std::thread::spawn(move || match crate::gifout::from_video(&src, crate::GIF_FPS, crate::GIF_MAX_W, max_bytes) {
            Ok((bytes, thumb)) => {
                let tmp = std::env::temp_dir().join(format!("shelf-gif-{}.gif", std::process::id()));
                if std::fs::write(&tmp, bytes).is_ok() {
                    if let Some(path) = history::add_file(&tmp, "gif", &thumb, "gif") {
                        crate::run_on_main(move || {
                            clipboard::set_files(crate::main_hwnd(), &[path]);
                        });
                    }
                }
            }
            Err(e) => log::error!("gif failed: {e}"),
        });
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
                with(|s| {
                    let h = if s.text_tab { None } else { s.hit(x, y) };
                    if h != s.hover {
                        s.hover = h;
                        s.redraw();
                    }
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: s.hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                });
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
            0x02A3 /* WM_MOUSELEAVE */ => {
                with(|s| {
                    if s.hover.take().is_some() {
                        s.redraw();
                    }
                });
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
                    VK_G => s.make_gif(),
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
        ((self.size.1 as f32 - self.header() as f32 - self.footer() as f32 - 40.0 * self.scale) / self.text_row_h()).max(1.0) as usize
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
        (Rect::new((10.0 * s) as i32, 0, (90.0 * s) as i32, h), Rect::new((94.0 * s) as i32, 0, (160.0 * s) as i32, h))
    }

    fn text_row_at(&self, y: i32) -> Option<usize> {
        let top = self.header() as f32 + 40.0 * self.scale;
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
            let rr = D2D_RECT_F { left: r.l as f32 + 6.0 * s, top: 13.0 * s, right: r.r as f32, bottom: r.b as f32 };
            unsafe {
                rt.DrawText(&t, fmt, &rr, if on { text } else { dim }, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                if on {
                    let bar = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: r.l as f32 + 6.0 * s, top: r.b as f32 - 3.0 * s, right: r.r as f32 - 10.0 * s, bottom: r.b as f32 }, radiusX: 1.5 * s, radiusY: 1.5 * s };
                    rt.FillRoundedRectangle(&bar, accent);
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
        // Search field.
        let field = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: 12.0 * s, top: top + 8.0 * s, right: w - 12.0 * s, bottom: top + 34.0 * s }, radiusX: 7.0 * s, radiusY: 7.0 * s };
        if let Ok(b) = unsafe { rt.CreateSolidColorBrush(&crate::annot::rgba(0x2A2A2E, 1.0), None) } {
            unsafe { rt.FillRoundedRectangle(&field, &b) };
        }
        let q = if self.query.is_empty() { "Поиск — просто начните печатать".to_string() } else { format!("{}▏", self.query) };
        draw(&q, fmt, D2D_RECT_F { left: 24.0 * s, top: top + 12.0 * s, right: w - 24.0 * s, bottom: top + 34.0 * s }, if self.query.is_empty() { dim } else { text });
        let y0 = top + 40.0 * s;
        if self.texts.is_empty() {
            draw(if self.query.is_empty() { "Скопируйте любой текст — он появится здесь" } else { "Ничего не найдено" }, fmt, D2D_RECT_F { left: 12.0 * s, top: y0 + 8.0 * s, right: w, bottom: y0 + 30.0 * s }, dim);
        }
        let rh = self.text_row_h();
        for (i, e) in self.texts.iter().enumerate().skip(self.ttop).take(self.text_rows()) {
            let y = y0 + (i - self.ttop) as f32 * rh;
            if i == self.tsel {
                let r = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: 8.0 * s, top: y + 1.0 * s, right: w - 8.0 * s, bottom: y + rh - 1.0 * s }, radiusX: 7.0 * s, radiusY: 7.0 * s };
                unsafe { rt.FillRoundedRectangle(&r, sel) };
            }
            let first = e.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            let pin = if e.pinned { "★ " } else { "" };
            draw(&format!("{pin}{}", crate::clipkind::clip_chars(first, 120)), fmt, D2D_RECT_F { left: 20.0 * s, top: y + 3.0 * s, right: w - 12.0 * s, bottom: y + 22.0 * s }, if e.pinned { accent } else { text });
            let mut meta = format!("{}  ·  {}  ·  {} симв.", age(e.time), if e.source.is_empty() { "—" } else { e.source.trim_end_matches(".exe") }, e.text.chars().count());
            if e.pinned && !crate::texthist::template_fields(&e.text).is_empty() {
                meta.push_str("  ·  шаблон");
            }
            draw(&meta, small, D2D_RECT_F { left: 20.0 * s, top: y + 21.0 * s, right: w - 12.0 * s, bottom: y + 38.0 * s }, dim);
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

/// Rounded corners and a hairline border from DWM (Windows 11).
fn round_corners(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::*;
    unsafe {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as _, size_of_val(&pref) as u32);
        let border: u32 = 0x003A3A3C; // COLORREF 0x00BBGGRR
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
    }
}

/// "только что", "5 мин", "3 ч", "2 дн", or a date.
pub fn age_text(created: u64, now: u64) -> String {
    let d = now.saturating_sub(created);
    match d {
        0..60 => "только что".into(),
        60..3600 => format!("{} мин", d / 60),
        3600..86400 => format!("{} ч", d / 3600),
        86400..604800 => format!("{} дн", d / 86400),
        _ => {
            let days = created / 86400;
            let (y, m, dd) = civil_from_days(days as i64);
            format!("{dd:02}.{m:02}.{y}")
        }
    }
}

fn age(created: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    age_text(created, now)
}

/// "1 снимок", "3 снимка", "11 снимков".
pub fn plural(n: usize, one: &str, few: &str, many: &str) -> String {
    let w = match (n % 10, n % 100) {
        (1, r) if r != 11 => one,
        (2..=4, r) if !(12..=14).contains(&r) => few,
        _ => many,
    };
    format!("{n} {w}")
}

/// "340 КБ", "1,2 МБ".
pub fn human_size(bytes: u64) -> String {
    if bytes < 1_000_000 {
        format!("{} КБ", (bytes + 999) / 1000)
    } else {
        format!("{:.1} МБ", bytes as f64 / 1e6).replace('.', ",")
    }
}

/// Days since 1970-01-01 → (year, month, day) in UTC (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_and_sizes() {
        let now = 1_800_000_000;
        assert_eq!(age_text(now - 5, now), "только что");
        assert_eq!(age_text(now - 125, now), "2 мин");
        assert_eq!(age_text(now - 7200, now), "2 ч");
        assert_eq!(age_text(now - 3 * 86400, now), "3 дн");
        // 2026-10-15 00:00 UTC = 1_792_022_400.
        assert_eq!(age_text(1_792_022_400, now), "15.10.2026");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(human_size(340_000), "340 КБ");
        assert_eq!(human_size(1), "1 КБ");
        assert_eq!(human_size(1_250_000), "1,2 МБ");
        assert_eq!(plural(1, "снимок", "снимка", "снимков"), "1 снимок");
        assert_eq!(plural(3, "снимок", "снимка", "снимков"), "3 снимка");
        assert_eq!(plural(11, "снимок", "снимка", "снимков"), "11 снимков");
        assert_eq!(plural(22, "снимок", "снимка", "снимков"), "22 снимка");
        assert_eq!(plural(25, "снимок", "снимка", "снимков"), "25 снимков");
    }
}
