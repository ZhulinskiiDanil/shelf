//! Shared look of Shelf's own surfaces (capture toolbar, board, labels): one dark panel
//! style, vector icons drawn with Direct2D (crisp at any scale, no icon font needed),
//! hover tooltips. Everything here is cheap to draw: a few dozen primitives per frame.

use crate::annot::{self, Res, Tool};
use crate::geom::Rect;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows_numerics::Vector2;

pub const PANEL: u32 = 0x1C1C1E;
pub const ACCENT: u32 = 0x2F8CFF;
pub const TEXT: u32 = 0xF2F2F2;
pub const MUTED: u32 = 0x9A9AA0;

pub fn brush(rt: &ID2D1RenderTarget, c: u32, a: f32) -> Option<ID2D1SolidColorBrush> {
    unsafe { rt.CreateSolidColorBrush(&annot::rgba(c, a), None).ok() }
}

fn rr(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

/// Dark rounded panel with a soft shadow and a hairline border.
pub fn panel(rt: &ID2D1RenderTarget, r: D2D_RECT_F, radius: f32, scale: f32) {
    unsafe {
        // Shadow: a few widening, fading rounded rects (no effects pipeline needed).
        for i in 1..=4 {
            let d = i as f32 * 1.5 * scale;
            if let Some(b) = brush(rt, 0, 0.10) {
                let s = D2D_RECT_F { left: r.left - d, top: r.top - d + 2.0 * scale, right: r.right + d, bottom: r.bottom + d + 2.0 * scale };
                rt.FillRoundedRectangle(&rr(s, radius + d), &b);
            }
        }
        if let Some(b) = brush(rt, PANEL, 0.98) {
            rt.FillRoundedRectangle(&rr(r, radius), &b);
        }
        if let Some(b) = brush(rt, 0xFFFFFF, 0.09) {
            rt.DrawRoundedRectangle(&rr(r, radius), &b, 1.0, None);
        }
    }
}

/// A small pill with text (size label, coordinates, tooltips). `x`,`y` = top-left.
/// Returns the pill rect.
pub fn pill(rt: &ID2D1RenderTarget, res: &Res, text: &str, x: f32, y: f32, scale: f32, fg: u32) -> D2D_RECT_F {
    let w16: Vec<u16> = text.encode_utf16().collect();
    let size = 12.0 * scale;
    let (tw, th) = measure(res, text, size);
    let (px, py) = (8.0 * scale, 3.0 * scale);
    let r = D2D_RECT_F { left: x, top: y, right: x + tw + 2.0 * px, bottom: y + th + 2.0 * py };
    unsafe {
        if let Some(b) = brush(rt, 0x111113, 0.88) {
            rt.FillRoundedRectangle(&rr(r, (r.bottom - r.top) / 2.0), &b);
        }
        if let (Some(f), Some(b)) = (res.text_format(size), brush(rt, fg, 1.0)) {
            let tr = D2D_RECT_F { left: x + px, top: y + py, right: r.right, bottom: r.bottom };
            rt.DrawText(&w16, &f, &tr, &b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }
    r
}

pub fn pill_size(res: &Res, text: &str, scale: f32) -> (f32, f32) {
    let (tw, th) = measure(res, text, 12.0 * scale);
    (tw + 16.0 * scale, th + 6.0 * scale)
}

fn measure(res: &Res, text: &str, size: f32) -> (f32, f32) {
    let w16: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        res.text_format(size)
            .and_then(|f| res.dw.CreateTextLayout(&w16, &f, 4000.0, 1000.0).ok())
            .and_then(|l| {
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m).ok().map(|_| (m.widthIncludingTrailingWhitespace.ceil(), m.height.ceil()))
            })
            .unwrap_or((text.chars().count() as f32 * size * 0.55, size * 1.3))
    }
}

pub fn tool_name(t: Tool) -> &'static str {
    match t {
        Tool::Pen => "Карандаш",
        Tool::Line => "Линия",
        Tool::Arrow => "Стрелка",
        Tool::Rect => "Прямоугольник",
        Tool::Ellipse => "Овал",
        Tool::Marker => "Маркер",
        Tool::Text => "Текст",
        Tool::Blur => "Пикселизация",
        Tool::Number => "Нумерация",
    }
}

pub fn tool_key(t: Tool) -> String {
    let k = crate::config::current().tools;
    match t {
        Tool::Pen => k.pen,
        Tool::Line => k.line,
        Tool::Arrow => k.arrow,
        Tool::Rect => k.rect,
        Tool::Ellipse => k.ellipse,
        Tool::Marker => k.marker,
        Tool::Text => k.text,
        Tool::Blur => k.blur,
        Tool::Number => k.number,
    }
}

/// Vector icon in a 16×16 design box centred in `r`.
pub fn icon(rt: &ID2D1RenderTarget, res: &Res, tool: Tool, r: D2D_RECT_F, fg: &ID2D1SolidColorBrush, scale: f32) {
    let k = scale * 1.15;
    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    let p = |x: f32, y: f32| Vector2::new(cx + (x - 8.0) * k, cy + (y - 8.0) * k);
    let w = 1.6 * scale;
    let st = &res.stroke_round;
    unsafe {
        match tool {
            Tool::Pen => {
                // Freehand squiggle.
                let pts: Vec<Vector2> = (0..=12).map(|i| { let t = i as f32 / 12.0; p(2.5 + 11.0 * t, 9.5 - 4.0 * t + 2.6 * (t * 9.0).sin()) }).collect();
                for s in pts.windows(2) {
                    rt.DrawLine(s[0], s[1], fg, w, st);
                }
            }
            Tool::Line => rt.DrawLine(p(3.0, 13.0), p(13.0, 3.0), fg, w, st),
            Tool::Arrow => {
                rt.DrawLine(p(3.0, 13.0), p(13.0, 3.0), fg, w, st);
                rt.DrawLine(p(7.5, 3.0), p(13.0, 3.0), fg, w, st);
                rt.DrawLine(p(13.0, 3.0), p(13.0, 8.5), fg, w, st);
            }
            Tool::Rect => {
                let a = p(2.5, 4.0);
                let b = p(13.5, 12.0);
                rt.DrawRoundedRectangle(&rr(D2D_RECT_F { left: a.X, top: a.Y, right: b.X, bottom: b.Y }, 1.5 * scale), fg, w, None);
            }
            Tool::Ellipse => {
                let e = D2D1_ELLIPSE { point: p(8.0, 8.0), radiusX: 5.8 * k, radiusY: 4.6 * k };
                rt.DrawEllipse(&e, fg, w, None);
            }
            Tool::Marker => {
                if let Some(b) = brush(rt, 0xFDD835, 0.75) {
                    rt.DrawLine(p(3.0, 11.0), p(13.0, 6.0), &b, 4.2 * scale, st);
                }
            }
            Tool::Text => {
                rt.DrawLine(p(3.5, 3.5), p(12.5, 3.5), fg, w, st);
                rt.DrawLine(p(8.0, 3.5), p(8.0, 13.0), fg, w, st);
            }
            Tool::Blur => {
                for (i, j) in [(0, 0), (1, 1), (2, 0), (0, 2), (2, 2)] {
                    let a = p(3.0 + i as f32 * 3.6, 3.0 + j as f32 * 3.6);
                    let b = p(6.2 + i as f32 * 3.6, 6.2 + j as f32 * 3.6);
                    rt.FillRectangle(&D2D_RECT_F { left: a.X, top: a.Y, right: b.X, bottom: b.Y }, fg);
                }
            }
            Tool::Number => {
                let e = D2D1_ELLIPSE { point: p(8.0, 8.0), radiusX: 5.8 * k, radiusY: 5.8 * k };
                rt.DrawEllipse(&e, fg, w, None);
                if let Some(f) = res.text_format(9.0 * k) {
                    let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    let a = p(2.0, 2.0);
                    let b = p(14.0, 14.5);
                    let t: Vec<u16> = "1".encode_utf16().collect();
                    rt.DrawText(&t, &f, &D2D_RECT_F { left: a.X, top: a.Y, right: b.X, bottom: b.Y }, fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
        }
    }
}

/// The markup toolbar: tools, a divider, colour dots. `rects` from `annot::toolbar_layout`
/// (screen coordinates, shifted by `o`); `hover` = cursor in the same coordinates.
#[allow(clippy::too_many_arguments)]
pub fn toolbar(rt: &ID2D1RenderTarget, res: &Res, rects: &[Rect], o: (i32, i32), active: Option<Tool>, color: u32, hover: (i32, i32), scale: f32, dimmed: &[Tool]) {
    let (Some(a), Some(z)) = (rects.first(), rects.last()) else { return };
    let f = |r: &Rect| D2D_RECT_F { left: (r.l - o.0) as f32, top: (r.t - o.1) as f32, right: (r.r - o.0) as f32, bottom: (r.b - o.1) as f32 };
    let pad = 4.0 * scale;
    let all = f(&Rect::new(a.l, a.t, z.r, z.b));
    panel(rt, D2D_RECT_F { left: all.left - pad, top: all.top - pad, right: all.right + pad, bottom: all.bottom + pad }, 10.0 * scale, scale);
    let (Some(white), Some(muted), Some(accent), Some(hov)) = (brush(rt, TEXT, 1.0), brush(rt, MUTED, 0.55), brush(rt, ACCENT, 1.0), brush(rt, 0xFFFFFF, 0.10)) else { return };
    let mut tip: Option<(String, D2D_RECT_F)> = None;
    unsafe {
        for (i, r) in rects.iter().enumerate() {
            let d = f(r);
            let inner = D2D_RECT_F { left: d.left + 2.0 * scale, top: d.top + 2.0 * scale, right: d.right - 2.0 * scale, bottom: d.bottom - 2.0 * scale };
            let hovered = r.contains(hover.0, hover.1);
            if let Some(&tool) = annot::TOOLS.get(i) {
                if active == Some(tool) {
                    rt.FillRoundedRectangle(&rr(inner, 7.0 * scale), &accent);
                } else if hovered {
                    rt.FillRoundedRectangle(&rr(inner, 7.0 * scale), &hov);
                }
                let fg = if dimmed.contains(&tool) { &muted } else { &white };
                icon(rt, res, tool, d, fg, scale);
                if hovered {
                    tip = Some((format!("{}  ·  {}", tool_name(tool), tool_key(tool)), d));
                }
            } else {
                if i == annot::TOOLS.len() {
                    // Divider between tools and colours.
                    let x = d.left - 3.0 * scale;
                    rt.DrawLine(Vector2::new(x, d.top + 7.0 * scale), Vector2::new(x, d.bottom - 7.0 * scale), &hov, 1.0 * scale, None);
                }
                let c = annot::PALETTE[i - annot::TOOLS.len()];
                let center = Vector2::new((d.left + d.right) / 2.0, (d.top + d.bottom) / 2.0);
                let rad = if hovered { 7.5 } else { 6.5 } * scale;
                if c == color {
                    let ring = D2D1_ELLIPSE { point: center, radiusX: 10.0 * scale, radiusY: 10.0 * scale };
                    rt.DrawEllipse(&ring, &white, 1.6 * scale, None);
                }
                if let Some(b) = brush(rt, c, 1.0) {
                    rt.FillEllipse(&D2D1_ELLIPSE { point: center, radiusX: rad, radiusY: rad }, &b);
                }
                // Hairline so black and white dots read on any panel.
                if let Some(b) = brush(rt, 0xFFFFFF, 0.28) {
                    rt.DrawEllipse(&D2D1_ELLIPSE { point: center, radiusX: rad, radiusY: rad }, &b, 1.0, None);
                }
            }
        }
    }
    // Tooltip above the bar (below if the bar is near the top of the screen).
    if let Some((text, d)) = tip {
        let (tw, th) = pill_size(res, &text, scale);
        let x = ((d.left + d.right) / 2.0 - tw / 2.0).max(all.left - pad);
        let above = all.top - pad - th - 6.0 * scale;
        let y = if above > 0.0 { above } else { all.bottom + pad + 6.0 * scale };
        pill(rt, res, &text, x, y, scale, TEXT);
    }
}

/// Selection handle: white dot with an accent ring.
pub fn handle(rt: &ID2D1RenderTarget, x: f32, y: f32, scale: f32) {
    let r = 4.5 * scale;
    unsafe {
        if let Some(b) = brush(rt, 0, 0.35) {
            rt.FillEllipse(&D2D1_ELLIPSE { point: Vector2::new(x, y + 0.8 * scale), radiusX: r + 1.0, radiusY: r + 1.0 }, &b);
        }
        if let Some(b) = brush(rt, 0xFFFFFF, 1.0) {
            rt.FillEllipse(&D2D1_ELLIPSE { point: Vector2::new(x, y), radiusX: r, radiusY: r }, &b);
        }
        if let Some(b) = brush(rt, ACCENT, 1.0) {
            rt.DrawEllipse(&D2D1_ELLIPSE { point: Vector2::new(x, y), radiusX: r, radiusY: r }, &b, 1.5 * scale, None);
        }
    }
}
