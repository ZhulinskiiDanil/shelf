//! Capture overlay: one pre-created topmost window spanning the virtual screen.
//! It is only shown/hidden; while hidden it is also DWM-cloaked so that the new frame
//! can be rendered before the window becomes visible (no flash of the previous shot).

use crate::capture::{self, Frame};
use crate::clipboard::{self, Image};
use crate::geom::{self, Grip, Rect};
use crate::annot::{self, Kind, Shape, Tool};
use crate::{config, history};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use std::os::windows::ffi::OsStrExt;
use windows_numerics::Vector2;

const CLASS: PCWSTR = w!("Shelf.Capture");
const DRAG_THRESHOLD: i32 = 3;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// No selection; hovering highlights the window under the cursor.
    Hover,
    /// Mouse is down, not yet moved past the drag threshold.
    Pressing { at: (i32, i32) },
    Drawing { anchor: (i32, i32) },
    Selected,
    Grabbing { grip: Grip, orig: Rect, at: (i32, i32) },
    /// Drawing a markup shape with the mouse.
    Annotating,
}

pub enum Finish {
    Copy,
    CopyPath,
    Save,
    SaveAs,
    Drag,
    Ocr,
}

struct Gfx {
    rt: ID2D1HwndRenderTarget,
    bmp: Option<(ID2D1Bitmap, Rect)>,
    white: ID2D1SolidColorBrush,
    black: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    text: IDWriteTextFormat,
}

pub struct Overlay {
    hwnd: HWND,
    owner: HWND,
    factory: ID2D1Factory,
    gfx: Option<Gfx>,
    frame: Option<Frame>,
    bounds: Rect,
    monitors: Vec<(Rect, u32)>,
    windows: Vec<Rect>,
    mode: Mode,
    sel: Rect,
    cursor: (i32, i32),
    visible: bool,
    pub magnifier: bool,
    // Markup
    res: annot::Res,
    doc: annot::Doc,
    tool: Option<annot::Tool>,
    color: u32,
    width: f32,
    /// Shape being drawn with the mouse, or text being typed.
    pending: Option<annot::Shape>,
    /// Pixelated patches for on-screen preview of blur shapes.
    blur_bmps: Vec<(Rect, ID2D1Bitmap)>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OVERLAY.with(|o| o.try_borrow_mut().ok().and_then(|mut o| o.as_mut().map(f)))
}

fn rf(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.l as f32, top: r.t as f32, right: r.r as f32, bottom: r.b as f32 }
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

pub fn init(owner: HWND, magnifier: bool) {
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
        let vs = capture::virtual_screen();
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("Shelf capture"),
            WS_POPUP,
            vs.l,
            vs.t,
            vs.w(),
            vs.h(),
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();
        set_cloak(hwnd, true);
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let mut ov = Overlay {
            hwnd,
            owner,
            factory: factory.clone(),
            gfx: None,
            frame: None,
            bounds: vs,
            monitors: Vec::new(),
            windows: Vec::new(),
            mode: Mode::Hover,
            sel: Rect::default(),
            cursor: (0, 0),
            visible: false,
            magnifier,
            res: annot::Res::new(&factory),
            doc: annot::Doc::default(),
            tool: None,
            color: annot::PALETTE[0],
            width: 3.0,
            pending: None,
            blur_bmps: Vec::new(),
        };
        // Warm up: device, brushes and the screen buffer are allocated now, not on first PrtScrn.
        ov.ensure_gfx();
        capture::grab(&mut ov.frame);
        ov.upload();
        // First present pays for shader/swap-chain setup; do it once while cloaked.
        let _ = ShowWindow(hwnd, SW_SHOWNA);
        ov.render();
        let _ = ShowWindow(hwnd, SW_HIDE);
        OVERLAY.with(|o| *o.borrow_mut() = Some(ov));
    }
}

pub fn set_magnifier(on: bool) {
    with(|o| o.magnifier = on);
}

fn set_cloak(hwnd: HWND, on: bool) {
    let v: BOOL = on.into();
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const _ as _, 4);
    }
}

/// Entry point from the hotkey hook. `t0` is when the key was pressed.
pub fn show(t0: Instant) {
    with(|o| o.show(t0));
}

impl Overlay {
    fn ensure_gfx(&mut self) {
        if self.gfx.is_some() {
            return;
        }
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U { width: self.bounds.w() as u32, height: self.bounds.h() as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
            };
            let Ok(rt) = self.factory.CreateHwndRenderTarget(&props, &hprops) else {
                log::error!("CreateHwndRenderTarget failed");
                return;
            };
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
            let text = dw
                .CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 12.0, w!(""))
                .unwrap();
            self.gfx = Some(Gfx {
                white: rt.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 1.0), None).unwrap(),
                black: rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.75), None).unwrap(),
                accent: rt.CreateSolidColorBrush(&color(0.15, 0.55, 1.0, 1.0), None).unwrap(),
                dim: rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.45), None).unwrap(),
                rt,
                bmp: None,
                text,
            });
        }
    }

    /// Copies the frozen frame to the GPU bitmap, (re)creating it on size change.
    fn upload(&mut self) {
        let (Some(g), Some(f)) = (self.gfx.as_mut(), self.frame.as_ref()) else { return };
        unsafe {
            if g.bmp.as_ref().map(|b| b.1) != Some(f.bounds) {
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let size = D2D_SIZE_U { width: f.bounds.w() as u32, height: f.bounds.h() as u32 };
                g.bmp = g.rt.CreateBitmap(size, None, 0, &props).ok().map(|b| (b, f.bounds));
            }
            if let Some((b, _)) = &g.bmp {
                let _ = b.CopyFromMemory(None, f.pixels as _, f.stride() as u32);
            }
        }
    }

    fn show(&mut self, t0: Instant) {
        if self.visible {
            return;
        }
        // The grab (GDI readback, the slowest step) runs in parallel with showing the
        // still-cloaked window and taking focus. The cloaked window is not in the capture.
        struct SendFrame(Option<Frame>);
        unsafe impl Send for SendFrame {}
        let mut slot = SendFrame(self.frame.take());
        let hwnd = self.hwnd;
        let (ok, t_grab, t_fg) = std::thread::scope(|s| {
            let h = s.spawn(|| {
                let slot = &mut slot;
                let ok = capture::grab(&mut slot.0);
                (ok, t0.elapsed())
            });
            self.monitors = capture::monitors();
            self.windows = capture::window_rects(hwnd);
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
                force_foreground(hwnd);
            }
            let t_fg = t0.elapsed();
            let (ok, t_grab) = h.join().unwrap();
            (ok, t_grab, t_fg)
        });
        self.frame = slot.0;
        if !ok {
            log::error!("screen grab failed");
            self.visible = true;
            self.hide();
            return;
        }
        let vs = self.frame.as_ref().unwrap().bounds;
        if vs != self.bounds {
            self.bounds = vs;
            unsafe {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), vs.l, vs.t, vs.w(), vs.h(), SWP_NOACTIVATE);
            }
            if let Some(g) = &self.gfx {
                unsafe {
                    let _ = g.rt.Resize(&D2D_SIZE_U { width: vs.w() as u32, height: vs.h() as u32 });
                }
            }
        }
        self.ensure_gfx();
        self.upload();
        let t_up = t0.elapsed();
        self.mode = Mode::Hover;
        self.sel = Rect::default();
        self.doc.clear();
        self.tool = None;
        self.pending = None;
        self.blur_bmps.clear();
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        self.cursor = (pt.x, pt.y);
        self.render();
        let t_render = t0.elapsed();
        set_cloak(self.hwnd, false);
        self.visible = true;
        unsafe {
            let _ = ValidateRect(Some(self.hwnd), None);
        }
        log::info!(
            "metric: hotkey->overlay {:.1} ms (grab {:.1} || show+focus {:.1}, upload {:.1}, render {:.1}; {}x{})",
            t0.elapsed().as_secs_f64() * 1000.0,
            t_grab.as_secs_f64() * 1000.0,
            t_fg.as_secs_f64() * 1000.0,
            t_up.as_secs_f64() * 1000.0,
            t_render.as_secs_f64() * 1000.0,
            vs.w(),
            vs.h()
        );
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        set_cloak(self.hwnd, true);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn hovered_window(&self) -> Rect {
        let (x, y) = self.cursor;
        self.windows
            .iter()
            .find(|r| r.contains(x, y))
            .copied()
            .or_else(|| self.monitors.iter().map(|m| m.0).find(|r| r.contains(x, y)))
            .unwrap_or(self.bounds)
            .intersect(&self.bounds)
    }

    /// The rect that would be captured right now.
    fn target(&self) -> Rect {
        match self.mode {
            Mode::Hover | Mode::Pressing { .. } => self.hovered_window(),
            _ => self.sel,
        }
    }

    fn scale(&self) -> f32 {
        geom::scale_at(&self.monitors, self.cursor.0, self.cursor.1)
    }

    fn monitor_at(&self, x: i32, y: i32) -> Rect {
        self.monitors.iter().map(|m| m.0).find(|r| r.contains(x, y)).unwrap_or(self.bounds)
    }

    // ! --- Rendering --- !

    fn render(&mut self) {
        let Some(g) = self.gfx.as_ref() else { return };
        let Some((bmp, _)) = g.bmp.as_ref() else { return };
        let o = (self.bounds.l, self.bounds.t);
        let local = |r: &Rect| rf(&r.offset(-o.0, -o.1));
        let full = local(&self.bounds);
        let target = self.target();
        let scale = self.scale();
        unsafe {
            g.rt.BeginDraw();
            g.rt.DrawBitmap(bmp, Some(&full), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
            g.rt.FillRectangle(&full, &g.dim);
            if !target.is_empty() {
                let t = local(&target);
                g.rt.DrawBitmap(bmp, Some(&t), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, Some(&t));
                self.draw_markup(g, &t);
                let bw = (scale).max(1.0).round();
                let border = D2D_RECT_F { left: t.left - bw / 2.0, top: t.top - bw / 2.0, right: t.right + bw / 2.0, bottom: t.bottom + bw / 2.0 };
                g.rt.DrawRectangle(&border, &g.accent, bw, None);
                if matches!(self.mode, Mode::Selected | Mode::Grabbing { .. } | Mode::Annotating) {
                    self.draw_handles(g, &t, scale);
                }
                self.draw_size_label(g, &target, scale);
                if matches!(self.mode, Mode::Selected | Mode::Grabbing { .. } | Mode::Annotating) {
                    self.draw_toolbar(g, scale);
                }
            }
            if self.magnifier && !matches!(self.mode, Mode::Selected | Mode::Annotating | Mode::Grabbing { grip: Grip::Move, .. }) {
                self.draw_magnifier(g, bmp, scale);
            }
            if g.rt.EndDraw(None, None).is_err() {
                // Device lost: rebuild on next show.
                self.gfx = None;
            }
        }
    }

    fn draw_markup(&self, g: &Gfx, clip: &D2D_RECT_F) {
        if self.doc.shapes.is_empty() && self.pending.is_none() {
            return;
        }
        let o = (self.bounds.l as f32, self.bounds.t as f32);
        unsafe {
            g.rt.PushAxisAlignedClip(clip, D2D1_ANTIALIAS_MODE_ALIASED);
            for s in &self.doc.shapes {
                if let Kind::Blur(r) = &s.kind {
                    if let Some((_, b)) = self.blur_bmps.iter().find(|(br, _)| br == r) {
                        let d = rf(&r.offset(-self.bounds.l, -self.bounds.t));
                        g.rt.DrawBitmap(b, Some(&d), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                    }
                }
            }
            annot::draw(&g.rt, &self.res, &self.doc.shapes, o);
            if let Some(p) = &self.pending {
                if let Kind::Blur(r) = &p.kind {
                    g.rt.FillRectangle(&rf(&r.offset(-self.bounds.l, -self.bounds.t)), &g.black);
                } else {
                    annot::draw(&g.rt, &self.res, std::slice::from_ref(p), o);
                    if let Kind::Text(pos, _) = &p.kind {
                        // Caret hint: a thin box where typing goes.
                        let h = annot::font_size(p.width) * 1.3;
                        let r = D2D_RECT_F { left: pos.0 - o.0 - 2.0, top: pos.1 - o.1, right: pos.0 - o.0 - 1.0, bottom: pos.1 - o.1 + h };
                        g.rt.FillRectangle(&r, &g.white);
                    }
                }
            }
            g.rt.PopAxisAlignedClip();
        }
    }

    fn toolbar(&self) -> Vec<Rect> {
        let c = ((self.sel.l + self.sel.r) / 2, (self.sel.t + self.sel.b) / 2);
        let mon = self.monitor_at(c.0, c.1);
        annot::toolbar_layout(&self.sel, &mon, geom::scale_at(&self.monitors, c.0, c.1))
    }

    fn draw_toolbar(&self, g: &Gfx, scale: f32) {
        let fmt = self.res.text_format(15.0 * scale);
        unsafe {
            if let Some(f) = &fmt {
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            }
            for (i, r) in self.toolbar().iter().enumerate() {
                let d = rf(&r.offset(-self.bounds.l, -self.bounds.t));
                g.rt.FillRectangle(&d, &g.black);
                if let Some(&tool) = annot::TOOLS.get(i) {
                    if let Some(f) = &fmt {
                        let t: Vec<u16> = tool.glyph().encode_utf16().collect();
                        g.rt.DrawText(&t, f, &d, &g.white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                    if self.tool == Some(tool) {
                        g.rt.DrawRectangle(&d, &g.accent, 2.0 * scale, None);
                    }
                } else {
                    let c = annot::PALETTE[i - annot::TOOLS.len()];
                    let inset = 6.0 * scale;
                    let sw = D2D_RECT_F { left: d.left + inset, top: d.top + inset, right: d.right - inset, bottom: d.bottom - inset };
                    if let Ok(b) = g.rt.CreateSolidColorBrush(&annot::rgba(c, 1.0), None) {
                        g.rt.FillRectangle(&sw, &b);
                    }
                    if c == self.color {
                        g.rt.DrawRectangle(&d, &g.white, 2.0 * scale, None);
                    }
                }
            }
        }
    }

    unsafe fn draw_handles(&self, g: &Gfx, t: &D2D_RECT_F, scale: f32) {
        let s = 3.0 * scale;
        let mx = (t.left + t.right) / 2.0;
        let my = (t.top + t.bottom) / 2.0;
        for (x, y) in [(t.left, t.top), (mx, t.top), (t.right, t.top), (t.left, my), (t.right, my), (t.left, t.bottom), (mx, t.bottom), (t.right, t.bottom)] {
            let r = D2D_RECT_F { left: x - s, top: y - s, right: x + s, bottom: y + s };
            unsafe {
                g.rt.FillRectangle(&r, &g.white);
                g.rt.DrawRectangle(&r, &g.accent, 1.0, None);
            }
        }
    }

    fn label(&self, g: &Gfx, text: &str, x: f32, y: f32, scale: f32) {
        let w16: Vec<u16> = text.encode_utf16().collect();
        let w = (7.0 * text.chars().count() as f32 + 10.0) * scale;
        let h = 18.0 * scale;
        let bg = D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h };
        unsafe {
            g.rt.FillRectangle(&bg, &g.black);
            g.rt.SetTransform(&windows_numerics::Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: x + 5.0 * scale, M32: y + 1.0 * scale });
            let lr = D2D_RECT_F { left: 0.0, top: 0.0, right: w / scale, bottom: h / scale };
            g.rt.DrawText(&w16, &g.text, &lr, &g.white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            g.rt.SetTransform(&windows_numerics::Matrix3x2::identity());
        }
    }

    fn draw_size_label(&self, g: &Gfx, target: &Rect, scale: f32) {
        let text = format!("{} × {}", target.w(), target.h());
        let h = (18.0 * scale) as i32;
        let mon = self.monitor_at(target.l, target.t);
        let mut y = target.t - h - (4.0 * scale) as i32;
        if y < mon.t {
            y = target.t + (4.0 * scale) as i32;
        }
        self.label(g, &text, (target.l.max(mon.l) - self.bounds.l) as f32, (y - self.bounds.t) as f32, scale);
    }

    fn draw_magnifier(&self, g: &Gfx, bmp: &ID2D1Bitmap, scale: f32) {
        const SRC: i32 = 15;
        let zoom = (8.0 * scale).round() as i32;
        let size = SRC * zoom;
        let (cx, cy) = self.cursor;
        let mon = self.monitor_at(cx, cy);
        let label_h = (18.0 * scale) as i32;
        let (px, py) = geom::place_near(cx, cy, size, size + label_h, (20.0 * scale) as i32, &mon);
        let (lx, ly) = ((px - self.bounds.l) as f32, (py - self.bounds.t) as f32);
        let dst = D2D_RECT_F { left: lx, top: ly, right: lx + size as f32, bottom: ly + size as f32 };
        let sx = (cx - self.bounds.l - SRC / 2) as f32;
        let sy = (cy - self.bounds.t - SRC / 2) as f32;
        let src = D2D_RECT_F { left: sx, top: sy, right: sx + SRC as f32, bottom: sy + SRC as f32 };
        unsafe {
            g.rt.FillRectangle(&dst, &g.black);
            g.rt.DrawBitmap(bmp, Some(&dst), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, Some(&src));
            // Crosshair around the centre pixel.
            let c0 = lx + (SRC / 2 * zoom) as f32;
            let c1 = ly + (SRC / 2 * zoom) as f32;
            let px_rect = D2D_RECT_F { left: c0, top: c1, right: c0 + zoom as f32, bottom: c1 + zoom as f32 };
            g.rt.DrawLine(Vector2::new(lx, c1 + zoom as f32 / 2.0), Vector2::new(c0, c1 + zoom as f32 / 2.0), &g.dim, 1.0, None);
            g.rt.DrawLine(Vector2::new(c0 + zoom as f32, c1 + zoom as f32 / 2.0), Vector2::new(dst.right, c1 + zoom as f32 / 2.0), &g.dim, 1.0, None);
            g.rt.DrawLine(Vector2::new(c0 + zoom as f32 / 2.0, ly), Vector2::new(c0 + zoom as f32 / 2.0, c1), &g.dim, 1.0, None);
            g.rt.DrawLine(Vector2::new(c0 + zoom as f32 / 2.0, c1 + zoom as f32), Vector2::new(c0 + zoom as f32 / 2.0, dst.bottom), &g.dim, 1.0, None);
            g.rt.DrawRectangle(&px_rect, &g.white, 1.0, None);
            g.rt.DrawRectangle(&dst, &g.white, 1.0, None);
        }
        let color = self.pixel(cx, cy).map(|(r, gg, b)| format!("  #{r:02X}{gg:02X}{b:02X}")).unwrap_or_default();
        self.label(g, &format!("{}, {}{}", cx, cy, color), lx, ly + size as f32, scale);
    }

    fn pixel(&self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        let f = self.frame.as_ref()?;
        if !f.bounds.contains(x, y) {
            return None;
        }
        let i = (y - f.bounds.t) as usize * f.stride() + (x - f.bounds.l) as usize * 4;
        let d = f.data();
        Some((d[i + 2], d[i + 1], d[i]))
    }

    // ! --- Input --- !

    fn grip_tol(&self) -> i32 {
        (6.0 * self.scale()).round() as i32
    }

    fn on_mouse_move(&mut self, x: i32, y: i32) {
        self.cursor = (x, y);
        match self.mode {
            Mode::Pressing { at } if (x - at.0).abs() > DRAG_THRESHOLD || (y - at.1).abs() > DRAG_THRESHOLD => {
                self.mode = Mode::Drawing { anchor: at };
                self.sel = Rect::from_points(at, (x, y)).intersect(&self.bounds);
            }
            Mode::Drawing { anchor } => self.sel = Rect::from_points(anchor, (x + 1, y + 1)).intersect(&self.bounds),
            Mode::Grabbing { grip, orig, at } => self.sel = geom::apply_grip(&orig, grip, x - at.0, y - at.1, &self.bounds),
            Mode::Annotating => self.extend_pending(x, y),
            _ => {}
        }
        self.redraw();
    }

    fn on_lbutton_down(&mut self, x: i32, y: i32) {
        self.cursor = (x, y);
        if self.mode == Mode::Selected {
            if let Some(i) = self.toolbar().iter().position(|r| r.contains(x, y)) {
                self.commit_text();
                match annot::TOOLS.get(i) {
                    Some(&t) => self.tool = if self.tool == Some(t) { None } else { Some(t) },
                    None => self.color = annot::PALETTE[i - annot::TOOLS.len()],
                }
                self.redraw();
                return;
            }
            let grip = geom::hit_test(&self.sel, x, y, self.grip_tol());
            if let (Some(tool), Some(Grip::Move)) = (self.tool, grip) {
                self.start_shape(tool, x, y);
                return;
            }
            self.commit_text();
            if let Some(grip) = grip {
                self.mode = Mode::Grabbing { grip, orig: self.sel, at: (x, y) };
                return;
            }
        }
        self.mode = Mode::Pressing { at: (x, y) };
        self.redraw();
    }

    fn on_lbutton_up(&mut self) {
        match self.mode {
            Mode::Pressing { .. } => {
                // Click without drag: take the highlighted window.
                self.sel = self.hovered_window();
                self.mode = Mode::Selected;
            }
            Mode::Drawing { .. } | Mode::Grabbing { .. } => {
                self.mode = if self.sel.is_empty() { Mode::Hover } else { Mode::Selected };
            }
            Mode::Annotating => {
                self.mode = Mode::Selected;
                if let Some(s) = self.pending.take() {
                    self.commit_shape(s);
                }
            }
            _ => {}
        }
        self.redraw();
    }

    // ! --- Markup --- !

    fn start_shape(&mut self, tool: Tool, x: i32, y: i32) {
        let p = (x as f32 + 0.5, y as f32 + 0.5);
        let kind = match tool {
            Tool::Pen => Kind::Pen(vec![p]),
            Tool::Marker => Kind::Marker(vec![p]),
            Tool::Line => Kind::Line(p, p),
            Tool::Arrow => Kind::Arrow(p, p),
            Tool::Rect => Kind::Rect(Rect::new(x, y, x, y)),
            Tool::Ellipse => Kind::Ellipse(Rect::new(x, y, x, y)),
            Tool::Blur => Kind::Blur(Rect::new(x, y, x, y)),
            Tool::Number => {
                let n = self.doc.next_number();
                self.doc.push(Shape { kind: Kind::Number(p, n), color: self.color, width: self.width });
                self.redraw();
                return;
            }
            Tool::Text => {
                self.commit_text();
                self.pending = Some(Shape { kind: Kind::Text(p, String::new()), color: self.color, width: self.width });
                self.redraw();
                return;
            }
        };
        self.commit_text();
        self.pending = Some(Shape { kind, color: self.color, width: self.width });
        self.mode = Mode::Annotating;
        unsafe {
            SetCapture(self.hwnd);
        }
        self.redraw();
    }

    fn extend_pending(&mut self, x: i32, y: i32) {
        let Some(s) = self.pending.as_mut() else { return };
        let p = (x as f32 + 0.5, y as f32 + 0.5);
        let shift = key_down(VK_SHIFT);
        let is_blur = matches!(s.kind, Kind::Blur(_));
        match &mut s.kind {
            Kind::Pen(pts) | Kind::Marker(pts) => pts.push(p),
            Kind::Line(a, b) | Kind::Arrow(a, b) => {
                *b = p;
                if shift {
                    // Snap to 45° steps.
                    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
                    let ang = (dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
                    let len = (dx * dx + dy * dy).sqrt();
                    *b = (a.0 + len * ang.cos(), a.1 + len * ang.sin());
                }
            }
            Kind::Rect(r) | Kind::Ellipse(r) | Kind::Blur(r) => {
                // While dragging, l/t hold the start corner; normalised on commit.
                r.r = x;
                r.b = y;
                if shift && !is_blur {
                    let d = (r.r - r.l).abs().max((r.b - r.t).abs());
                    r.r = r.l + d * (r.r - r.l).signum();
                    r.b = r.t + d * (r.b - r.t).signum();
                }
            }
            _ => {}
        }
    }

    fn commit_shape(&mut self, mut s: Shape) {
        match &mut s.kind {
            Kind::Rect(r) | Kind::Ellipse(r) | Kind::Blur(r) => {
                *r = Rect::from_points((r.l, r.t), (r.r, r.b));
                if r.is_empty() {
                    return;
                }
            }
            Kind::Text(_, t) if t.is_empty() => return,
            _ => {}
        }
        if let Kind::Blur(r) = &s.kind {
            self.make_blur_bitmap(*r);
        }
        self.doc.push(s);
    }

    fn commit_text(&mut self) {
        if matches!(self.pending.as_ref().map(|s| &s.kind), Some(Kind::Text(..))) {
            let s = self.pending.take().unwrap();
            self.commit_shape(s);
            self.redraw();
        }
    }

    fn is_typing(&self) -> bool {
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
            self.redraw();
        }
    }

    fn block_size(r: &Rect) -> i32 {
        (r.w().min(r.h()) / 6).clamp(6, 16)
    }

    fn make_blur_bitmap(&mut self, r: Rect) {
        let (Some(f), Some(g)) = (self.frame.as_ref(), self.gfx.as_ref()) else { return };
        let r = r.intersect(&f.bounds);
        if r.is_empty() {
            return;
        }
        let mut patch = f.crop(&r);
        annot::pixelate(&mut patch, r.w() as usize * 4, (r.l, r.t), (r.w(), r.h()), &r, Self::block_size(&r));
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            let size = D2D_SIZE_U { width: r.w() as u32, height: r.h() as u32 };
            if let Ok(b) = g.rt.CreateBitmap(size, Some(patch.as_ptr() as _), r.w() as u32 * 4, &props) {
                self.blur_bmps.push((r, b));
            }
        }
    }

    fn set_width(&mut self, delta: f32) {
        self.width = (self.width + delta).clamp(1.0, 40.0);
        self.redraw();
    }

    /// Returns true if the key was used by markup.
    fn markup_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool) -> bool {
        if ctrl {
            let done = match vk {
                VK_Z => {
                    self.commit_text();
                    self.doc.undo()
                }
                VK_Y => self.doc.redo(),
                _ => return false,
            };
            if done {
                self.redraw();
            }
            return true;
        }
        if self.mode != Mode::Selected || self.is_typing() {
            return false;
        }
        let keys = &config::current().tools;
        if let Some(i) = keys.tool_keys().iter().position(|&k| k == vk.0 as u32) {
            let t = annot::TOOLS[i];
            self.tool = if self.tool == Some(t) { None } else { Some(t) };
        } else if (0x31..=0x38).contains(&vk.0) {
            self.color = annot::PALETTE[(vk.0 - 0x31) as usize];
        } else if vk == VK_OEM_4 || vk == VK_OEM_MINUS || vk == VK_SUBTRACT {
            self.set_width(-1.0);
        } else if vk == VK_OEM_6 || vk == VK_OEM_PLUS || vk == VK_ADD {
            self.set_width(1.0);
        } else {
            return false;
        }
        self.redraw();
        true
    }

    /// Selection pixels with markup burned in. Without markup this is a plain crop (the M1 path).
    fn render_output(&self, target: &Rect) -> Option<Vec<u8>> {
        let f = self.frame.as_ref()?;
        let mut px = f.crop(target);
        if self.doc.shapes.is_empty() {
            return Some(px);
        }
        let (w, h) = (target.w(), target.h());
        for s in &self.doc.shapes {
            if let Kind::Blur(full) = &s.kind {
                // Same blocks as the preview: pixelate the whole patch, then copy the visible part.
                let r = full.intersect(target);
                if r.is_empty() {
                    continue;
                }
                let mut patch = f.crop(full);
                let fr = full.intersect(&f.bounds);
                annot::pixelate(&mut patch, fr.w() as usize * 4, (fr.l, fr.t), (fr.w(), fr.h()), &fr, Self::block_size(&fr));
                for y in r.t..r.b {
                    let src = (y - fr.t) as usize * fr.w() as usize * 4 + (r.l - fr.l) as usize * 4;
                    let dst = (y - target.t) as usize * w as usize * 4 + (r.l - target.l) as usize * 4;
                    let n = r.w() as usize * 4;
                    px[dst..dst + n].copy_from_slice(&patch[src..src + n]);
                }
            }
        }
        if self.doc.shapes.iter().all(|s| matches!(s.kind, Kind::Blur(_))) {
            return Some(px);
        }
        match wic_render(&self.factory, &self.res, &px, w, h, &self.doc.shapes, (target.l as f32, target.t as f32)) {
            Ok(out) => Some(out),
            Err(e) => {
                log::error!("markup render failed: {e}");
                Some(px)
            }
        }
    }

    fn nudge(&mut self, dx: i32, dy: i32) {
        if self.mode == Mode::Selected {
            self.sel = self.sel.offset(dx, dy).clamp_move(&self.bounds);
        } else {
            // No selection yet: arrows move the cursor for pixel-precise starts.
            let (x, y) = (self.cursor.0 + dx, self.cursor.1 + dy);
            unsafe {
                let _ = SetCursorPos(x, y);
            }
            self.cursor = (x, y);
        }
        self.redraw();
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn cursor_for(&self, x: i32, y: i32) -> PCWSTR {
        if !matches!(self.mode, Mode::Selected) {
            return IDC_CROSS;
        }
        match geom::hit_test(&self.sel, x, y, self.grip_tol()) {
            Some(Grip::Move) => IDC_SIZEALL,
            Some(Grip::Edge { l, t, r, b }) => match (l || r, t || b) {
                (true, true) if (l && t) || (r && b) => IDC_SIZENWSE,
                (true, true) => IDC_SIZENESW,
                (true, false) => IDC_SIZEWE,
                _ => IDC_SIZENS,
            },
            None => IDC_CROSS,
        }
    }

    /// Hides the overlay and returns the follow-up work, which the caller runs after
    /// releasing the overlay borrow (dialogs and drag loops pump messages).
    fn finish(&mut self, action: Finish, t0: Instant) -> Option<Box<dyn FnOnce()>> {
        let target = self.target();
        if target.is_empty() {
            return None;
        }
        self.commit_text();
        let bgra = self.render_output(&target)?;
        let img = Arc::new(Image { w: target.w() as u32, h: target.h() as u32, bgra });
        self.hide();
        let owner = self.owner;
        match action {
            Finish::Copy => {
                let ok = clipboard::set_image(owner, img.clone());
                log::info!("metric: ctrl+c->clipboard {:.1} ms ({}x{}, ok={ok})", t0.elapsed().as_secs_f64() * 1000.0, target.w(), target.h());
                history::add_async(img, "capture");
                None
            }
            Finish::CopyPath => Some(Box::new(move || {
                if let Some(p) = history::add(&img, "capture") {
                    clipboard::set_text(owner, &format!("\"{}\"", p.display()));
                }
            })),
            Finish::Save => {
                history::run_async(move || {
                    if let Some(p) = history::add(&img, "capture") {
                        let dir = config::current().save_dir();
                        match history::copy_to(&p, &dir) {
                            Some(d) => log::info!("saved {}", d.display()),
                            None => log::error!("save to {} failed", dir.display()),
                        }
                    }
                });
                None
            }
            Finish::SaveAs => Some(Box::new(move || {
                if let Some(path) = save_dialog(owner) {
                    let png = crate::imageio::encode_png(&img, png::Compression::Balanced);
                    if let Err(e) = std::fs::write(&path, png) {
                        log::error!("save {} failed: {e}", path.display());
                    }
                }
                history::add_async(img, "capture");
            })),
            Finish::Ocr => {
                crate::ocr_to_clipboard(img.clone());
                history::add_async(img, "capture");
                None
            }
            Finish::Drag => Some(Box::new(move || {
                if let Some(p) = history::add(&img, "capture") {
                    crate::dragdrop::drag_files(&[p], Some(&img), true);
                }
            })),
        }
    }
}

/// SetForegroundWindow is refused unless we own the last input; attach to the
/// foreground thread's input queue for the call.
pub unsafe fn force_foreground(hwnd: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
}

fn lparam_pt(hwnd: HWND, lp: LPARAM) -> (i32, i32) {
    let _ = hwnd;
    let x = (lp.0 & 0xFFFF) as i16 as i32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
    let b = with(|o| (o.bounds.l, o.bounds.t)).unwrap_or((0, 0));
    (x + b.0, y + b.1)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with(|o| o.render());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                let (x, y) = lparam_pt(hwnd, lp);
                with(|o| o.on_mouse_move(x, y));
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                SetCapture(hwnd);
                let (x, y) = lparam_pt(hwnd, lp);
                with(|o| o.on_lbutton_down(x, y));
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                with(|o| o.on_lbutton_up());
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                with(|o| o.hide());
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let c = with(|o| o.cursor_for(pt.x, pt.y)).unwrap_or(IDC_CROSS);
                SetCursor(LoadCursorW(None, c).ok());
                LRESULT(1)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let t0 = Instant::now();
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let ctrl = key_down(VK_CONTROL);
                let shift = key_down(VK_SHIFT);
                let step = if shift { 10 } else { 1 };
                let typing = with(|o| o.is_typing()).unwrap_or(false);
                if typing && !ctrl {
                    // Text tool: Esc and Enter finish the text; the rest arrives as WM_CHAR.
                    if vk == VK_ESCAPE || (vk == VK_RETURN && !shift) {
                        with(|o| o.commit_text());
                    }
                    return LRESULT(0);
                }
                if with(|o| o.markup_key(vk, ctrl)).unwrap_or(false) {
                    return LRESULT(0);
                }
                match vk {
                    VK_ESCAPE => {
                        with(|o| o.hide());
                    }
                    VK_LEFT => {
                        with(|o| o.nudge(-step, 0));
                    }
                    VK_RIGHT => {
                        with(|o| o.nudge(step, 0));
                    }
                    VK_UP => {
                        with(|o| o.nudge(0, -step));
                    }
                    VK_DOWN => {
                        with(|o| o.nudge(0, step));
                    }
                    VK_C if ctrl => finish(if shift { Finish::CopyPath } else { Finish::Copy }, t0),
                    VK_S if ctrl => finish(if shift { Finish::SaveAs } else { Finish::Save }, t0),
                    VK_D if ctrl => finish(Finish::Drag, t0),
                    VK_T if ctrl => finish(Finish::Ocr, t0),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    // Enter without Shift is handled in WM_KEYDOWN as "finish text".
                    if !(c == '\r' && !key_down(VK_SHIFT)) {
                        with(|o| o.type_char(c));
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = (wp.0 >> 16) as u16 as i16;
                with(|o| o.set_width(if delta > 0 { 1.0 } else { -1.0 }));
                LRESULT(0)
            }
            WM_KILLFOCUS => LRESULT(0),
            WM_DPICHANGED => LRESULT(0), // keep spanning the virtual screen in physical pixels
            WM_DISPLAYCHANGE => {
                // Recreate the render target at the new size on next show.
                with(|o| o.gfx = None);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn finish(action: Finish, t0: Instant) {
    if let Some(Some(next)) = with(|o| o.finish(action, t0)) {
        next();
    }
}

fn save_dialog(owner: HWND) -> Option<std::path::PathBuf> {
    use windows::Win32::UI::Controls::Dialogs::*;
    let stem = crate::names::format_stem(&config::current().save.filename, &crate::names::now());
    let mut buf = [0u16; 1024];
    for (i, c) in format!("{stem}.png").encode_utf16().take(1000).enumerate() {
        buf[i] = c;
    }
    let dir: Vec<u16> = config::current().save_dir().as_os_str().encode_wide().chain(Some(0)).collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: w!("PNG\0*.png\0"),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrInitialDir: PCWSTR(dir.as_ptr()),
        lpstrDefExt: w!("png"),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    unsafe {
        let _ = SetForegroundWindow(owner);
        if !GetSaveFileNameW(&mut ofn).as_bool() {
            return None;
        }
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// Renders `px` (top-down BGRA, w×h) plus vector markup into a new buffer, using a WIC
/// render target so the output matches the on-screen drawing.
pub(crate) fn wic_render(factory: &ID2D1Factory, res: &annot::Res, px: &[u8], w: i32, h: i32, shapes: &[Shape], origin: (f32, f32)) -> Result<Vec<u8>> {
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::*;
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bmp = wic.CreateBitmap(w as u32, h as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = factory.CreateWicBitmapRenderTarget(&bmp, &props)?;
        let bprops = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let src = rt.CreateBitmap(D2D_SIZE_U { width: w as u32, height: h as u32 }, Some(px.as_ptr() as _), w as u32 * 4, &bprops)?;
        rt.BeginDraw();
        rt.DrawBitmap(&src, None, 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
        annot::draw(&rt, res, shapes, origin);
        rt.EndDraw(None, None)?;
        let mut out = vec![0u8; (w * h * 4) as usize];
        bmp.CopyPixels(std::ptr::null(), w as u32 * 4, &mut out)?;
        Ok(out)
    }
}
