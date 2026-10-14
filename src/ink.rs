//! Screen ink: a transparent, click-through, topmost layer over the whole virtual screen,
//! rendered with Direct2D into a DirectComposition swap chain (per-pixel alpha on the GPU).
//! It is a normal DWM-composed window, so screen sharing (Discord, OBS) sees it.

use crate::annot;
use crate::explain;
use crate::geom::Rect;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

const CLASS: PCWSTR = w!("Shelf.Ink");
const TIMER_ANIM: usize = 1;
const WM_APP_RENDER: u32 = WM_APP + 30;
const FADE_TIME: Duration = Duration::from_millis(600);

#[derive(Debug, Clone)]
pub struct Stroke {
    pub pts: Vec<(f32, f32)>,
    pub color: u32,
    pub width: f32,
    pub permanent: bool,
    /// When the stroke was finished; `None` while drawing.
    pub ended: Option<Instant>,
}

/// Opacity of a finished, non-permanent stroke `age` after it ended.
pub fn fade_alpha(age: Duration, delay: Duration, fade: Duration) -> f32 {
    if age <= delay {
        1.0
    } else if age >= delay + fade {
        0.0
    } else {
        1.0 - (age - delay).as_secs_f32() / fade.as_secs_f32()
    }
}

struct Gpu {
    swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    stroke_style: ID2D1StrokeStyle,
    res: annot::Res,
}

struct Zoom {
    bmp: ID2D1Bitmap1,
    /// Frozen frame bounds (virtual screen) and the monitor being zoomed.
    frame: Rect,
    mon: Rect,
    level: f32,
}

pub struct Ink {
    hwnd: HWND,
    gpu: Option<Gpu>,
    bounds: Rect,
    strokes: Vec<Stroke>,
    current: Option<Stroke>,
    pub color: u32,
    pub width: f32,
    pub fade_delay: Duration,
    shown: bool,
    animating: bool,
    /// Sticky mode: a thin frame around every monitor shows that clicks draw.
    pub sticky: bool,
    render_pending: bool,
    // Explanation tools (M6)
    cursor: (i32, i32),
    laser: bool,
    trail: Vec<(f32, f32, Instant)>,
    spot: Option<f32>,
    zoom: Option<Zoom>,
    markers: Vec<annot::Shape>,
    rings: Vec<(f32, f32, Instant)>,
    keys: explain::KeyLog,
    pub rings_on: bool,
    pub keys_on: bool,
}

thread_local! {
    static INK: RefCell<Option<Ink>> = const { RefCell::new(None) };
}

pub fn with<R>(f: impl FnOnce(&mut Ink) -> R) -> Option<R> {
    INK.with(|i| i.try_borrow_mut().ok().and_then(|mut i| i.as_mut().map(f)))
}

pub fn init() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let vs = crate::capture::virtual_screen();
        // Layered + transparent = click-through; no redirection bitmap, content comes from DComp.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            CLASS,
            w!("Shelf ink"),
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
        // Layered + transparent makes the window click-through for other processes.
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        let ink = Ink {
            hwnd,
            gpu: None,
            bounds: vs,
            strokes: Vec::new(),
            current: None,
            color: annot::PALETTE[0],
            width: 4.0,
            fade_delay: Duration::from_millis(2500),
            shown: false,
            animating: false,
            sticky: false,
            render_pending: false,
            cursor: (0, 0),
            laser: false,
            trail: Vec::new(),
            spot: None,
            zoom: None,
            markers: Vec::new(),
            rings: Vec::new(),
            keys: explain::KeyLog::default(),
            rings_on: false,
            keys_on: false,
        };
        INK.with(|i| *i.borrow_mut() = Some(ink));
    }
}

fn create_gpu(hwnd: HWND, w: u32, h: u32) -> Result<Gpu> {
    unsafe {
        let mut dev: Option<ID3D11Device> = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None)?;
        let dev = dev.ok_or_else(|| Error::from(E_FAIL))?;
        let dxgi: IDXGIDevice = dev.cast()?;
        let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        let swap = factory.CreateSwapChainForComposition(&dxgi, &desc, None)?;
        let d2f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2dev = d2f.CreateDevice(&dxgi)?;
        let dc = d2dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let surface: IDXGISurface = swap.GetBuffer(0)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        let target_bmp = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
        dc.SetTarget(&target_bmp);
        dc.SetDpi(96.0, 96.0);
        let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
        let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
        let visual = dcomp.CreateVisual()?;
        visual.SetContent(&swap)?;
        target.SetRoot(&visual)?;
        dcomp.Commit()?;
        let sp = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            ..Default::default()
        };
        let base: ID2D1Factory = d2f.cast()?;
        let stroke_style = base.CreateStrokeStyle(&sp, None)?;
        let res = annot::Res::new(&base);
        Ok(Gpu { swap, dc, _dcomp: dcomp, _target: target, _visual: visual, stroke_style, res })
    }
}

impl Ink {
    fn ensure_gpu(&mut self) -> bool {
        let vs = crate::capture::virtual_screen();
        if vs != self.bounds {
            self.bounds = vs;
            self.gpu = None;
            unsafe {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), vs.l, vs.t, vs.w(), vs.h(), SWP_NOACTIVATE);
            }
        }
        if self.gpu.is_none() {
            match create_gpu(self.hwnd, vs.w() as u32, vs.h() as u32) {
                Ok(g) => self.gpu = Some(g),
                Err(e) => {
                    log::error!("ink: GPU init failed: {e}");
                    return false;
                }
            }
        }
        true
    }

    fn set_shown(&mut self, on: bool) {
        if on != self.shown {
            self.shown = on;
            unsafe {
                let _ = ShowWindow(self.hwnd, if on { SW_SHOWNOACTIVATE } else { SW_HIDE });
                if on {
                    let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
        }
    }

    pub fn begin(&mut self, x: i32, y: i32, permanent: bool) {
        self.end();
        self.current = Some(Stroke { pts: vec![(x as f32, y as f32)], color: self.color, width: self.width, permanent, ended: None });
        self.request_render();
    }

    pub fn add_point(&mut self, x: i32, y: i32) {
        let Some(s) = self.current.as_mut() else { return };
        let p = (x as f32, y as f32);
        if s.pts.last() == Some(&p) {
            return;
        }
        s.pts.push(p);
        self.request_render();
    }

    /// Finishes the current stroke. `permanent` upgrades it (modifier held at release).
    pub fn end_with(&mut self, permanent: bool) {
        if let Some(mut s) = self.current.take() {
            s.permanent |= permanent;
            s.ended = Some(Instant::now());
            self.strokes.push(s);
            self.request_render();
        }
    }

    pub fn end(&mut self) {
        self.end_with(false);
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.current = None;
        self.markers.clear();
        self.request_render();
    }

    // ! --- Explanation tools --- !

    /// Every mouse move from the hook while any ink feature listens.
    pub fn on_move(&mut self, x: i32, y: i32) {
        self.cursor = (x, y);
        if self.laser {
            self.trail.push((x as f32, y as f32, Instant::now()));
        }
        if self.current.is_some() {
            self.add_point(x, y);
        } else if self.laser || self.spot.is_some() || self.zoom.is_some() {
            self.request_render();
        }
    }

    pub fn laser(&mut self, on: bool, x: i32, y: i32) {
        self.laser = on;
        self.cursor = (x, y);
        if on {
            self.trail.push((x as f32, y as f32, Instant::now()));
        }
        self.request_render();
    }

    pub fn toggle_spotlight(&mut self, x: i32, y: i32) -> bool {
        self.cursor = (x, y);
        self.spot = if self.spot.is_some() { None } else { Some(160.0 * self.scale_at(x, y)) };
        self.request_render();
        self.spot.is_some()
    }

    pub fn toggle_zoom(&mut self, x: i32, y: i32) -> bool {
        self.cursor = (x, y);
        if self.zoom.take().is_some() {
            self.request_render();
            return false;
        }
        if !self.ensure_gpu() {
            return false;
        }
        // Freeze the screen (without the ink layer, which GDI does not see) and zoom into it.
        let mut frame = None;
        if !crate::capture::grab(&mut frame) {
            return false;
        }
        let f = frame.unwrap();
        let mon = crate::capture::monitors().into_iter().map(|m| m.0).find(|r| r.contains(x, y)).unwrap_or(f.bounds);
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let g = self.gpu.as_ref().unwrap();
        let size = D2D_SIZE_U { width: f.bounds.w() as u32, height: f.bounds.h() as u32 };
        let bmp = unsafe { g.dc.CreateBitmap(size, Some(f.pixels as _), f.stride() as u32, &props) };
        match bmp {
            Ok(bmp) => self.zoom = Some(Zoom { bmp, frame: f.bounds, mon, level: 2.0 }),
            Err(e) => log::error!("zoom bitmap: {e}"),
        }
        self.request_render();
        self.zoom.is_some()
    }

    pub fn wheel(&mut self, up: bool) {
        if let Some(z) = self.zoom.as_mut() {
            z.level = (z.level * if up { 1.25 } else { 0.8 }).clamp(1.25, 12.0);
        } else if let Some(r) = self.spot.as_mut() {
            *r = (*r * if up { 1.15 } else { 0.87 }).clamp(30.0, 2000.0);
        }
        self.request_render();
    }

    pub fn marker(&mut self, x: i32, y: i32) {
        let n = self.markers.len() as u32 + 1;
        let width = self.width;
        self.markers.push(annot::Shape { kind: annot::Kind::Number((x as f32, y as f32), n), color: self.color, width });
        self.request_render();
    }

    pub fn ring(&mut self, x: i32, y: i32) {
        self.rings.push((x as f32, y as f32, Instant::now()));
        self.request_render();
    }

    pub fn key(&mut self, combo: String) {
        self.keys.push(combo, Instant::now());
        self.request_render();
    }

    pub fn exit_modes(&mut self) {
        self.spot = None;
        self.zoom = None;
        self.request_render();
    }

    pub fn feature_bits(&self) -> u32 {
        use crate::hook::{F_KEYS, F_RINGS, F_SPOT, F_ZOOM};
        let mut b = 0;
        if self.spot.is_some() {
            b |= F_SPOT;
        }
        if self.zoom.is_some() {
            b |= F_ZOOM;
        }
        if self.rings_on {
            b |= F_RINGS;
        }
        if self.keys_on {
            b |= F_KEYS;
        }
        b
    }

    fn scale_at(&self, x: i32, y: i32) -> f32 {
        crate::geom::scale_at(&crate::capture::monitors(), x, y)
    }

    pub fn set_color(&mut self, c: u32) {
        self.color = c;
        if let Some(s) = self.current.as_mut() {
            s.color = c;
            self.request_render();
        }
    }

    /// Drops fully faded strokes; returns whether anything is still fading.
    fn prune(&mut self) -> bool {
        let now = Instant::now();
        let delay = self.fade_delay;
        self.strokes.retain(|s| s.permanent || s.ended.is_none_or(|e| fade_alpha(now - e, delay, FADE_TIME) > 0.0));
        self.strokes.iter().any(|s| !s.permanent)
    }

    /// Coalesces redraws: the mouse hook delivers up to ~1000 points/s, but we present
    /// at most once per queued render message, after all pending points are added.
    pub fn request_render(&mut self) {
        if !self.render_pending {
            self.render_pending = true;
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_APP_RENDER, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn render(&mut self) {
        self.render_pending = false;
        let now = Instant::now();
        let fading = self.prune();
        explain::trim_trail(&mut self.trail, now);
        self.rings.retain(|r| now.duration_since(r.2) < explain::RING_LIFE);
        self.keys.prune(now);
        let animating = fading || self.laser || !self.trail.is_empty() || !self.rings.is_empty() || !self.keys.entries.is_empty();
        let empty = self.strokes.is_empty()
            && self.current.is_none()
            && !self.sticky
            && self.markers.is_empty()
            && self.trail.is_empty()
            && !self.laser
            && self.spot.is_none()
            && self.zoom.is_none()
            && self.rings.is_empty()
            && self.keys.entries.is_empty();
        self.set_animating(animating);
        if empty {
            self.set_shown(false);
            return;
        }
        if !self.ensure_gpu() {
            return;
        }
        let g = self.gpu.as_ref().unwrap();
        let o = (self.bounds.l as f32, self.bounds.t as f32);
        let lr = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F { left: l - o.0, top: t - o.1, right: r - o.0, bottom: b - o.1 };
        unsafe {
            let dc = &g.dc;
            let brush = |c: u32, a: f32| dc.CreateSolidColorBrush(&annot::rgba(c, a), None).ok();
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            // Zoom: the frozen frame, magnified onto its monitor.
            if let Some(z) = &self.zoom {
                let src = explain::zoom_src(&z.mon, self.cursor, z.level);
                let s = D2D_RECT_F { left: src.0 - z.frame.l as f32, top: src.1 - z.frame.t as f32, right: src.2 - z.frame.l as f32, bottom: src.3 - z.frame.t as f32 };
                let d = lr(z.mon.l as f32, z.mon.t as f32, z.mon.r as f32, z.mon.b as f32);
                dc.DrawBitmap(&z.bmp, Some(&d), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, Some(&s), None);
            }
            // Spotlight: dim everything except a circle around the cursor.
            if let (Some(r), Ok(f)) = (self.spot, dc.GetFactory()) {
                let full = D2D_RECT_F { left: 0.0, top: 0.0, right: self.bounds.w() as f32, bottom: self.bounds.h() as f32 };
                let c = Vector2::new(self.cursor.0 as f32 - o.0, self.cursor.1 as f32 - o.1);
                if let (Ok(rect), Ok(circle), Some(b)) =
                    (f.CreateRectangleGeometry(&full), f.CreateEllipseGeometry(&D2D1_ELLIPSE { point: c, radiusX: r, radiusY: r }), brush(0, 0.65))
                {
                    let parts = [Some(rect.cast::<ID2D1Geometry>().unwrap()), Some(circle.cast::<ID2D1Geometry>().unwrap())];
                    if let Ok(group) = f.CreateGeometryGroup(D2D1_FILL_MODE_ALTERNATE, &parts) {
                        dc.FillGeometry(&group, &b, None);
                    }
                }
            }
            if self.sticky {
                if let Some(b) = brush(self.color, 0.9) {
                    for (m, _) in crate::capture::monitors() {
                        let r = lr(m.l as f32 + 2.0, m.t as f32 + 2.0, m.r as f32 - 2.0, m.b as f32 - 2.0);
                        dc.DrawRectangle(&r, &b, 4.0, None);
                    }
                }
            }
            for s in self.strokes.iter().chain(self.current.iter()) {
                let a = match (s.permanent, s.ended) {
                    (false, Some(e)) => fade_alpha(now - e, self.fade_delay, FADE_TIME),
                    _ => 1.0,
                };
                draw_stroke(dc, &g.stroke_style, s, a, o);
            }
            annot::draw(dc, &g.res, &self.markers, o);
            // Click rings: grow and fade.
            for (x, y, t) in &self.rings {
                let k = now.duration_since(*t).as_secs_f32() / explain::RING_LIFE.as_secs_f32();
                let scale = self.scale_at(*x as i32, *y as i32);
                if let Some(b) = brush(0xFFC107, 1.0 - k) {
                    let e = D2D1_ELLIPSE { point: Vector2::new(x - o.0, y - o.1), radiusX: (10.0 + 25.0 * k) * scale, radiusY: (10.0 + 25.0 * k) * scale };
                    dc.DrawEllipse(&e, &b, 3.0 * scale, None);
                }
            }
            // Laser: a fading red trail with a bright head.
            if let Some(last) = self.trail.last().copied() {
                for w in self.trail.windows(2) {
                    let age = now.duration_since(w[1].2).as_secs_f32() / explain::LASER_LIFE.as_secs_f32();
                    if let Some(b) = brush(0xFF1744, (1.0 - age).clamp(0.0, 1.0) * 0.85) {
                        dc.DrawLine(Vector2::new(w[0].0 - o.0, w[0].1 - o.1), Vector2::new(w[1].0 - o.0, w[1].1 - o.1), &b, 6.0 * (1.0 - age * 0.6), &g.stroke_style);
                    }
                }
                if self.laser {
                    let p = Vector2::new(last.0 - o.0, last.1 - o.1);
                    if let (Some(glow), Some(core)) = (brush(0xFF1744, 0.35), brush(0xFF5252, 1.0)) {
                        dc.FillEllipse(&D2D1_ELLIPSE { point: p, radiusX: 14.0, radiusY: 14.0 }, &glow);
                        dc.FillEllipse(&D2D1_ELLIPSE { point: p, radiusX: 6.0, radiusY: 6.0 }, &core);
                    }
                }
            }
            // Pressed keys: a plate in the bottom-right corner of the primary monitor.
            if !self.keys.entries.is_empty() {
                let mons = crate::capture::monitors();
                let m = mons.iter().map(|m| m.0).find(|r| r.contains(0, 0)).unwrap_or(self.bounds);
                let scale = self.scale_at(m.l, m.t);
                if let (Some(fmt), Some(bg), Some(fg)) = (g.res.text_format(18.0 * scale), brush(0x202020, 0.85), brush(0xFFFFFF, 1.0)) {
                    let h = 34.0 * scale;
                    let gap = 6.0 * scale;
                    let n = self.keys.entries.len();
                    for (i, e) in self.keys.entries.iter().enumerate() {
                        let text = explain::KeyLog::label(e);
                        let w = (12.0 * text.chars().count() as f32 + 24.0) * scale;
                        let b = m.b as f32 - 40.0 * scale - (n - 1 - i) as f32 * (h + gap);
                        let r = lr(m.r as f32 - 40.0 * scale - w, b - h, m.r as f32 - 40.0 * scale, b);
                        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: 6.0 * scale, radiusY: 6.0 * scale };
                        dc.FillRoundedRectangle(&rr, &bg);
                        let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                        let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                        let t: Vec<u16> = text.encode_utf16().collect();
                        dc.DrawText(&t, &fmt, &r, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                }
            }
            if dc.EndDraw(None, None).is_err() {
                self.gpu = None;
                self.zoom = None; // its bitmap belonged to the lost device
                return;
            }
            // No vsync wait: the UI thread must keep draining mouse points; DWM composes at refresh rate anyway.
            let _ = g.swap.Present(0, DXGI_PRESENT(0));
        }
        self.set_shown(true);
    }

    fn set_animating(&mut self, on: bool) {
        if on != self.animating {
            self.animating = on;
            unsafe {
                if on {
                    SetTimer(Some(self.hwnd), TIMER_ANIM, 16, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
                }
            }
        }
    }
}

unsafe fn draw_stroke(dc: &ID2D1DeviceContext, style: &ID2D1StrokeStyle, s: &Stroke, alpha: f32, o: (f32, f32)) {
    unsafe {
        let Ok(brush) = dc.CreateSolidColorBrush(&annot::rgba(s.color, alpha), None) else { return };
        let v = |p: (f32, f32)| Vector2::new(p.0 - o.0, p.1 - o.1);
        if s.pts.len() == 1 {
            let e = D2D1_ELLIPSE { point: v(s.pts[0]), radiusX: s.width / 2.0, radiusY: s.width / 2.0 };
            dc.FillEllipse(&e, &brush);
            return;
        }
        let Ok(f) = dc.GetFactory() else { return };
        let Ok(path) = f.CreatePathGeometry() else { return };
        if let Ok(sink) = path.Open() {
            sink.BeginFigure(v(s.pts[0]), D2D1_FIGURE_BEGIN_HOLLOW);
            for p in &s.pts[1..] {
                sink.AddLine(v(*p));
            }
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
        }
        dc.DrawGeometry(&path, &brush, s.width, style);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_RENDER => {
                with(|i| i.render());
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_ANIM => {
                with(|i| i.render());
                LRESULT(0)
            }
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DPICHANGED => LRESULT(0),
            WM_DISPLAYCHANGE => {
                with(|i| i.gpu = None);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_curve() {
        let d = Duration::from_millis(2500);
        let f = Duration::from_millis(500);
        assert_eq!(fade_alpha(Duration::ZERO, d, f), 1.0);
        assert_eq!(fade_alpha(d, d, f), 1.0);
        assert!((fade_alpha(d + Duration::from_millis(250), d, f) - 0.5).abs() < 1e-4);
        assert_eq!(fade_alpha(d + f, d, f), 0.0);
        assert_eq!(fade_alpha(Duration::from_secs(60), d, f), 0.0);
    }
}
