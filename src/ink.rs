//! Screen ink: a transparent, click-through, topmost layer over the whole virtual screen,
//! rendered with Direct2D into a DirectComposition swap chain (per-pixel alpha on the GPU).
//! It is a normal DWM-composed window, so screen sharing (Discord, OBS) sees it.

use crate::annot;
use crate::geom::Rect;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
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
        Ok(Gpu { swap, dc, _dcomp: dcomp, _target: target, _visual: visual, stroke_style })
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
        self.request_render();
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
        let fading = self.prune();
        let empty = self.strokes.is_empty() && self.current.is_none() && !self.sticky;
        self.set_animating(fading);
        if empty {
            self.set_shown(false);
            return;
        }
        if !self.ensure_gpu() {
            return;
        }
        let g = self.gpu.as_ref().unwrap();
        let now = Instant::now();
        let o = (self.bounds.l as f32, self.bounds.t as f32);
        unsafe {
            g.dc.BeginDraw();
            g.dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            if self.sticky {
                if let Ok(b) = g.dc.CreateSolidColorBrush(&annot::rgba(self.color, 0.9), None) {
                    for (m, _) in crate::capture::monitors() {
                        let r = D2D_RECT_F { left: (m.l - self.bounds.l) as f32 + 2.0, top: (m.t - self.bounds.t) as f32 + 2.0, right: (m.r - self.bounds.l) as f32 - 2.0, bottom: (m.b - self.bounds.t) as f32 - 2.0 };
                        g.dc.DrawRectangle(&r, &b, 4.0, None);
                    }
                }
            }
            for s in self.strokes.iter().chain(self.current.iter()) {
                let a = match (s.permanent, s.ended) {
                    (false, Some(e)) => fade_alpha(now - e, self.fade_delay, FADE_TIME),
                    _ => 1.0,
                };
                draw_stroke(&g.dc, &g.stroke_style, s, a, o);
            }
            if g.dc.EndDraw(None, None).is_err() {
                self.gpu = None;
                return;
            }
            // No vsync wait: the UI thread must keep draining mouse points; DWM composes at refresh rate anyway.
            let hr = g.swap.Present(0, DXGI_PRESENT(0));
            log::debug!("ink present: {} strokes, {hr:?}", self.strokes.len() + self.current.is_some() as usize);
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
