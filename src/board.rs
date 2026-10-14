//! Thinking board: a semi-transparent canvas over the monitor under the cursor with the
//! M3 markup tools and infinite panning. Saved to history as PNG on close.

use crate::annot::{self, Kind, Shape, Tool};
use crate::geom::Rect;
use crate::ink::{Gpu, create_gpu};
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const CLASS: PCWSTR = w!("Shelf.Board");

struct Board {
    hwnd: HWND,
    gpu: Option<Gpu>,
    mon: Rect,
    scale: f32,
    /// World coordinate shown at the window's top-left corner.
    cam: (f32, f32),
    doc: annot::Doc,
    tool: Tool,
    color: u32,
    width: f32,
    pending: Option<Shape>,
    pan_from: Option<(i32, i32, (f32, f32))>,
    space: bool,
    visible: bool,
    bg: u32,
    opacity: f32,
}

thread_local! {
    static BOARD: RefCell<Option<Board>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Board) -> R) -> Option<R> {
    BOARD.with(|b| b.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

pub fn init() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP, CLASS, w!("Shelf board"), WS_POPUP, 0, 0, 100, 100, None, None, Some(hinst.into()), None).unwrap();
        let b = Board {
            hwnd,
            gpu: None,
            mon: Rect::default(),
            scale: 1.0,
            cam: (0.0, 0.0),
            doc: annot::Doc::default(),
            tool: Tool::Pen,
            color: annot::PALETTE[6],
            width: 3.0,
            pending: None,
            pan_from: None,
            space: false,
            visible: false,
            bg: 0x1E1E1E,
            opacity: 0.85,
        };
        BOARD.with(|c| *c.borrow_mut() = Some(b));
    }
}

pub fn configure(opacity: f32, bg: u32) {
    with(|b| {
        b.opacity = opacity.clamp(0.1, 1.0);
        b.bg = bg;
    });
}

pub fn toggle() {
    if with(|b| b.visible) == Some(true) {
        close();
    } else {
        with(|b| b.open());
    }
}

/// Hides the board and saves it to history if anything was drawn.
fn close() {
    let Some(png) = with(|b| {
        b.commit_text();
        b.visible = false;
        unsafe {
            let _ = ShowWindow(b.hwnd, SW_HIDE);
        }
        let out = b.export();
        b.doc.clear();
        b.gpu = None; // free the swap chain while closed
        out
    }) else {
        return;
    };
    if let Some(img) = png {
        crate::history::add_async(std::sync::Arc::new(img), "board");
    }
}

impl Board {
    fn open(&mut self) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let mons = crate::capture::monitors();
        self.mon = mons.iter().map(|m| m.0).find(|r| r.contains(pt.x, pt.y)).unwrap_or(crate::capture::virtual_screen());
        self.scale = crate::geom::scale_at(&mons, pt.x, pt.y);
        self.cam = (self.mon.l as f32, self.mon.t as f32);
        self.width = 3.0 * self.scale;
        let m = self.mon;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), m.l, m.t, m.w(), m.h(), SWP_SHOWWINDOW);
            crate::overlay::force_foreground(self.hwnd);
        }
        match create_gpu(self.hwnd, m.w() as u32, m.h() as u32) {
            Ok(g) => self.gpu = Some(g),
            Err(e) => log::error!("board: GPU init failed: {e}"),
        }
        self.visible = true;
        self.render();
    }

    fn world(&self, x: i32, y: i32) -> (f32, f32) {
        (x as f32 + self.cam.0, y as f32 + self.cam.1)
    }

    fn toolbar(&self) -> Vec<Rect> {
        // Laid out in window coordinates along the bottom edge.
        let w = Rect::new(0, 0, self.mon.w(), self.mon.h());
        let anchor = Rect::new(w.r - 1, w.b - 1, w.r, w.b);
        annot::toolbar_layout(&anchor, &w, self.scale)
    }

    // ! --- Editing --- !

    fn start(&mut self, x: i32, y: i32) {
        self.commit_text();
        let p = self.world(x, y);
        let r = Rect::new(p.0 as i32, p.1 as i32, p.0 as i32, p.1 as i32);
        let kind = match self.tool {
            Tool::Pen => Kind::Pen(vec![p]),
            Tool::Marker => Kind::Marker(vec![p]),
            Tool::Line => Kind::Line(p, p),
            Tool::Arrow => Kind::Arrow(p, p),
            Tool::Rect => Kind::Rect(r),
            Tool::Ellipse => Kind::Ellipse(r),
            Tool::Blur => return, // nothing to pixelate on a blank board
            Tool::Number => {
                let n = self.doc.next_number();
                self.doc.push(Shape { kind: Kind::Number(p, n), color: self.color, width: self.width });
                self.render();
                return;
            }
            Tool::Text => Kind::Text(p, String::new()),
        };
        self.pending = Some(Shape { kind, color: self.color, width: self.width });
        self.render();
    }

    fn extend(&mut self, x: i32, y: i32) {
        let p = self.world(x, y);
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let Some(s) = self.pending.as_mut() else { return };
        match &mut s.kind {
            Kind::Pen(pts) | Kind::Marker(pts) => pts.push(p),
            Kind::Line(a, b) | Kind::Arrow(a, b) => {
                *b = p;
                if shift {
                    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
                    let ang = (dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
                    let len = (dx * dx + dy * dy).sqrt();
                    *b = (a.0 + len * ang.cos(), a.1 + len * ang.sin());
                }
            }
            Kind::Rect(r) | Kind::Ellipse(r) => {
                r.r = p.0 as i32;
                r.b = p.1 as i32;
                if shift {
                    let d = (r.r - r.l).abs().max((r.b - r.t).abs());
                    r.r = r.l + d * (r.r - r.l).signum();
                    r.b = r.t + d * (r.b - r.t).signum();
                }
            }
            _ => return,
        }
        self.render();
    }

    fn finish(&mut self) {
        if matches!(self.pending.as_ref().map(|s| &s.kind), Some(Kind::Text(..))) {
            return; // text stays open for typing
        }
        if let Some(mut s) = self.pending.take() {
            if let Kind::Rect(r) | Kind::Ellipse(r) = &mut s.kind {
                *r = Rect::from_points((r.l, r.t), (r.r, r.b));
                if r.is_empty() {
                    return;
                }
            }
            self.doc.push(s);
        }
        self.render();
    }

    fn commit_text(&mut self) {
        if self.typing() {
            let s = self.pending.take().unwrap();
            if matches!(&s.kind, Kind::Text(_, t) if !t.is_empty()) {
                self.doc.push(s);
            }
        }
    }

    fn typing(&self) -> bool {
        matches!(self.pending.as_ref().map(|s| &s.kind), Some(Kind::Text(..)))
    }

    fn type_char(&mut self, c: char) {
        if let Some(Shape { kind: Kind::Text(_, t), .. }) = self.pending.as_mut() {
            match c {
                '\u{8}' => {
                    t.pop();
                }
                '\r' => t.push('\n'),
                c if !c.is_control() => t.push(c),
                _ => {}
            }
            self.render();
        }
    }

    /// Changes the width, including the shape being drawn right now (as in Lightshot).
    fn set_width(&mut self, delta: f32) {
        self.width = (self.width + delta).clamp(1.0, 40.0);
        if let Some(p) = self.pending.as_mut() {
            p.width = self.width;
        }
        self.render();
    }

    /// Single-key tool, color and width shortcuts (same as capture markup).
    fn key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let keys = crate::config::current().tools.tool_keys();
        if let Some(i) = keys.iter().position(|&k| k == vk.0 as u32) {
            self.commit_text();
            self.tool = annot::TOOLS[i];
        } else if (0x31..=0x38).contains(&vk.0) {
            self.color = annot::PALETTE[(vk.0 - 0x31) as usize];
        } else if vk == VK_OEM_4 || vk == VK_OEM_MINUS {
            self.set_width(-1.0);
        } else if vk == VK_OEM_6 || vk == VK_OEM_PLUS {
            self.set_width(1.0);
        } else {
            return false;
        }
        self.render();
        true
    }

    // ! --- Output --- !

    /// The drawing cropped to its content, on an opaque background.
    fn export(&self) -> Option<crate::clipboard::Image> {
        if self.doc.shapes.is_empty() {
            return None;
        }
        let pad = 32.0;
        let (mut l, mut t, mut r, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for s in &self.doc.shapes {
            let bb = annot::shape_bounds(s);
            l = l.min(bb.0);
            t = t.min(bb.1);
            r = r.max(bb.2);
            b = b.max(bb.3);
        }
        let (l, t) = ((l - pad).floor(), (t - pad).floor());
        let w = ((r + pad).ceil() - l).clamp(1.0, 16000.0) as i32;
        let h = ((b + pad).ceil() - t).clamp(1.0, 16000.0) as i32;
        let px: Vec<u8> = [(self.bg & 0xFF) as u8, (self.bg >> 8 & 0xFF) as u8, (self.bg >> 16 & 0xFF) as u8, 255].repeat((w * h) as usize);
        let factory: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()? };
        let res = annot::Res::new(&factory);
        match crate::overlay::wic_render(&factory, &res, &px, w, h, &self.doc.shapes, (l, t), 1.0) {
            Ok(bgra) => Some(crate::clipboard::Image { w: w as u32, h: h as u32, bgra }),
            Err(e) => {
                log::error!("board export failed: {e}");
                None
            }
        }
    }

    fn render(&mut self) {
        let Some(g) = self.gpu.as_ref() else { return };
        let s = self.scale;
        unsafe {
            let dc = &g.dc;
            dc.BeginDraw();
            let mut bg = annot::rgba(self.bg, self.opacity);
            // Premultiplied alpha.
            bg.r *= bg.a;
            bg.g *= bg.a;
            bg.b *= bg.a;
            dc.Clear(Some(&bg));
            annot::draw(dc, &g.res, &self.doc.shapes, self.cam);
            if let Some(p) = &self.pending {
                annot::draw(dc, &g.res, std::slice::from_ref(p), self.cam);
            }
            // Toolbar and hint.
            let brush = |c: u32, a: f32| dc.CreateSolidColorBrush(&annot::rgba(c, a), None).ok();
            let fmt = g.res.text_format(15.0 * s);
            if let (Some(black), Some(white), Some(accent), Some(dim)) = (brush(0, 0.75), brush(0xFFFFFF, 1.0), brush(0x2F8CFF, 1.0), brush(0xFFFFFF, 0.35)) {
                for (i, r) in self.toolbar().iter().enumerate() {
                    let d = D2D_RECT_F { left: r.l as f32, top: r.t as f32, right: r.r as f32, bottom: r.b as f32 };
                    dc.FillRectangle(&d, &black);
                    if let Some(&tool) = annot::TOOLS.get(i) {
                        if let Some(f) = &fmt {
                            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                            let t: Vec<u16> = tool.glyph().encode_utf16().collect();
                            let fg = if tool == Tool::Blur { &dim } else { &white };
                            dc.DrawText(&t, f, &d, fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                        }
                        if tool == self.tool {
                            dc.DrawRectangle(&d, &accent, 2.0 * s, None);
                        }
                    } else {
                        let c = annot::PALETTE[i - annot::TOOLS.len()];
                        let inset = 6.0 * s;
                        if let Some(b) = brush(c, 1.0) {
                            dc.FillRectangle(&D2D_RECT_F { left: d.left + inset, top: d.top + inset, right: d.right - inset, bottom: d.bottom - inset }, &b);
                        }
                        if c == self.color {
                            dc.DrawRectangle(&d, &white, 2.0 * s, None);
                        }
                    }
                }
                if let Some(f) = g.res.text_format(12.0 * s) {
                    let hint = "Esc — save to shelf and close   ·   middle button or Space+drag — pan   ·   Ctrl+Z / Ctrl+Y";
                    let t: Vec<u16> = hint.encode_utf16().collect();
                    let r = D2D_RECT_F { left: 12.0 * s, top: 10.0 * s, right: self.mon.w() as f32, bottom: 40.0 * s };
                    dc.DrawText(&t, &f, &r, &dim, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
            if dc.EndDraw(None, None).is_err() {
                self.gpu = None;
                return;
            }
            let _ = g.swap.Present(0, DXGI_PRESENT(0));
        }
    }
}

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_LBUTTONDOWN => {
                let (x, y) = xy(lp);
                SetCapture(hwnd);
                with(|b| {
                    if b.space {
                        b.pan_from = Some((x, y, b.cam));
                    } else if let Some(i) = b.toolbar().iter().position(|r| r.contains(x, y)) {
                        b.commit_text();
                        match annot::TOOLS.get(i) {
                            Some(&t) => b.tool = t,
                            None => b.color = annot::PALETTE[i - annot::TOOLS.len()],
                        }
                        b.render();
                    } else {
                        b.start(x, y);
                    }
                });
                LRESULT(0)
            }
            WM_MBUTTONDOWN => {
                let (x, y) = xy(lp);
                SetCapture(hwnd);
                with(|b| b.pan_from = Some((x, y, b.cam)));
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = xy(lp);
                with(|b| {
                    if let Some((px, py, cam)) = b.pan_from {
                        b.cam = (cam.0 - (x - px) as f32, cam.1 - (y - py) as f32);
                        b.render();
                    } else if b.pending.is_some() {
                        b.extend(x, y);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP | WM_MBUTTONUP => {
                let _ = ReleaseCapture();
                with(|b| {
                    if b.pan_from.take().is_none() {
                        b.finish();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let up = (wp.0 >> 16) as u16 as i16 > 0;
                with(|b| b.set_width(if up { 1.0 } else { -1.0 }));
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                if with(|b| b.typing()).unwrap_or(false) && !ctrl {
                    if vk == VK_ESCAPE || (vk == VK_RETURN && !shift) {
                        with(|b| {
                            b.commit_text();
                            b.render();
                        });
                    }
                    return LRESULT(0);
                }
                match vk {
                    VK_ESCAPE => close(),
                    VK_Z if ctrl => {
                        with(|b| {
                            b.doc.undo();
                            b.render();
                        });
                    }
                    VK_Y if ctrl => {
                        with(|b| {
                            b.doc.redo();
                            b.render();
                        });
                    }
                    VK_SPACE => {
                        with(|b| b.space = true);
                    }
                    _ if !ctrl => {
                        with(|b| b.key(vk));
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_KEYUP if wp.0 as u16 == VK_SPACE.0 => {
                with(|b| b.space = false);
                LRESULT(0)
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    if !(c == '\r' && !shift) {
                        with(|b| b.type_char(c));
                    }
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let pan = with(|b| b.space || b.pan_from.is_some()).unwrap_or(false);
                SetCursor(LoadCursorW(None, if pan { IDC_SIZEALL } else { IDC_CROSS }).ok());
                LRESULT(1)
            }
            WM_DPICHANGED => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
