//! Markup in capture mode: shapes, undo/redo, toolbar layout and D2D drawing.
//! Shapes live in virtual-screen pixel coordinates.

use crate::geom::Rect;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::*;
use windows_numerics::Vector2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Marker,
    Text,
    Blur,
    Number,
}

pub const TOOLS: [Tool; 9] = [Tool::Pen, Tool::Line, Tool::Arrow, Tool::Rect, Tool::Ellipse, Tool::Marker, Tool::Text, Tool::Blur, Tool::Number];

impl Tool {
    pub fn glyph(self) -> &'static str {
        match self {
            Tool::Pen => "✎",
            Tool::Line => "╱",
            Tool::Arrow => "↗",
            Tool::Rect => "▭",
            Tool::Ellipse => "◯",
            Tool::Marker => "▌",
            Tool::Text => "T",
            Tool::Blur => "▦",
            Tool::Number => "①",
        }
    }
}

pub const PALETTE: [u32; 8] = [0xE53935, 0xFB8C00, 0xFDD835, 0x43A047, 0x1E88E5, 0x8E24AA, 0xFFFFFF, 0x000000];

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Pen(Vec<(f32, f32)>),
    Marker(Vec<(f32, f32)>),
    Line((f32, f32), (f32, f32)),
    Arrow((f32, f32), (f32, f32)),
    Rect(Rect),
    Ellipse(Rect),
    Text((f32, f32), String),
    Blur(Rect),
    Number((f32, f32), u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    pub color: u32,
    pub width: f32,
}

#[derive(Default)]
pub struct Doc {
    pub shapes: Vec<Shape>,
    redo: Vec<Shape>,
}

impl Doc {
    pub fn push(&mut self, s: Shape) {
        self.shapes.push(s);
        self.redo.clear();
    }
    pub fn undo(&mut self) -> bool {
        self.shapes.pop().map(|s| self.redo.push(s)).is_some()
    }
    pub fn redo(&mut self) -> bool {
        self.redo.pop().map(|s| self.shapes.push(s)).is_some()
    }
    pub fn clear(&mut self) {
        self.shapes.clear();
        self.redo.clear();
    }
    pub fn next_number(&self) -> u32 {
        self.shapes.iter().filter(|s| matches!(s.kind, Kind::Number(..))).count() as u32 + 1
    }
}

/// Text size for a stroke width.
pub fn font_size(width: f32) -> f32 {
    10.0 + width * 3.0
}

// ! --- Pure helpers --- !

/// Pixelates `area` (absolute coords) inside a BGRA buffer whose top-left is at `origin`.
pub fn pixelate(buf: &mut [u8], stride: usize, origin: (i32, i32), size: (i32, i32), area: &Rect, block: i32) {
    let bounds = Rect::new(origin.0, origin.1, origin.0 + size.0, origin.1 + size.1);
    let a = area.intersect(&bounds);
    if a.is_empty() || block < 1 {
        return;
    }
    let mut by = a.t;
    while by < a.b {
        let bh = block.min(a.b - by);
        let mut bx = a.l;
        while bx < a.r {
            let bw = block.min(a.r - bx);
            let mut sum = [0u32; 4];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let i = (y - origin.1) as usize * stride + (x - origin.0) as usize * 4;
                    for c in 0..4 {
                        sum[c] += buf[i + c] as u32;
                    }
                }
            }
            let n = (bw * bh) as u32;
            let avg = sum.map(|s| ((s + n / 2) / n) as u8);
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let i = (y - origin.1) as usize * stride + (x - origin.0) as usize * 4;
                    buf[i..i + 4].copy_from_slice(&avg);
                }
            }
            bx += block;
        }
        by += block;
    }
}

/// Toolbar: one row of square buttons under the selection (above it, or inside, when there is no room).
/// Returns button rects in order: TOOLS, then the 8 palette swatches.
pub fn toolbar_layout(sel: &Rect, mon: &Rect, scale: f32) -> Vec<Rect> {
    let b = (28.0 * scale).round() as i32;
    let gap = (4.0 * scale).round() as i32;
    let n = (TOOLS.len() + PALETTE.len()) as i32;
    let width = n * b + gap; // extra gap between tools and palette
    let mut x = (sel.r - width).max(mon.l).min(mon.r - width);
    let y = if sel.b + gap + b <= mon.b {
        sel.b + gap
    } else if sel.t - gap - b >= mon.t {
        sel.t - gap - b
    } else {
        sel.b - gap - b
    };
    x = x.max(mon.l);
    (0..n)
        .map(|i| {
            let extra = if i >= TOOLS.len() as i32 { gap } else { 0 };
            let l = x + i * b + extra;
            Rect::new(l, y, l + b, y + b)
        })
        .collect()
}

// ! --- Drawing --- !

pub struct Res {
    pub stroke_round: ID2D1StrokeStyle,
    pub dw: IDWriteFactory,
}

impl Res {
    pub fn new(factory: &ID2D1Factory) -> Self {
        unsafe {
            let props = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                ..Default::default()
            };
            Res {
                stroke_round: factory.CreateStrokeStyle(&props, None).unwrap(),
                dw: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap(),
            }
        }
    }

    pub fn text_format(&self, size: f32) -> Option<IDWriteTextFormat> {
        unsafe {
            self.dw
                .CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!(""))
                .ok()
        }
    }
}

pub fn rgba(c: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((c >> 16) & 0xFF) as f32 / 255.0, g: ((c >> 8) & 0xFF) as f32 / 255.0, b: (c & 0xFF) as f32 / 255.0, a }
}

fn v(p: (f32, f32), o: (f32, f32)) -> Vector2 {
    Vector2::new(p.0 - o.0, p.1 - o.1)
}

/// Draws shapes onto `rt`, translating absolute coordinates by `-o`.
/// Blur shapes are skipped: they are applied to pixels separately.
pub fn draw(rt: &ID2D1RenderTarget, res: &Res, shapes: &[Shape], o: (f32, f32)) {
    unsafe {
        for s in shapes {
            let alpha = if matches!(s.kind, Kind::Marker(_)) { 0.4 } else { 1.0 };
            let Ok(brush) = rt.CreateSolidColorBrush(&rgba(s.color, alpha), None) else { continue };
            let w = s.width;
            match &s.kind {
                Kind::Pen(pts) | Kind::Marker(pts) => {
                    let w = if matches!(s.kind, Kind::Marker(_)) { w * 4.0 } else { w };
                    if pts.len() == 1 {
                        let e = D2D1_ELLIPSE { point: v(pts[0], o), radiusX: w / 2.0, radiusY: w / 2.0 };
                        rt.FillEllipse(&e, &brush);
                    }
                    // A path keeps translucent marker strokes from darkening at joints.
                    let Ok(fac) = rt.GetFactory() else { continue };
                    let Ok(path) = fac.CreatePathGeometry() else { continue };
                    if let Ok(sink) = path.Open() {
                        if let Some(first) = pts.first() {
                            sink.BeginFigure(v(*first, o), D2D1_FIGURE_BEGIN_HOLLOW);
                            for p in &pts[1..] {
                                sink.AddLine(v(*p, o));
                            }
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        let _ = sink.Close();
                    }
                    rt.DrawGeometry(&path, &brush, w, &res.stroke_round);
                }
                Kind::Line(a, b) => rt.DrawLine(v(*a, o), v(*b, o), &brush, w, &res.stroke_round),
                Kind::Arrow(a, b) => {
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    let len = (dx * dx + dy * dy).sqrt().max(1.0);
                    let (ux, uy) = (dx / len, dy / len);
                    let head = (w * 4.0 + 6.0).min(len);
                    let back = (b.0 - ux * head, b.1 - uy * head);
                    let l = (back.0 - uy * head * 0.5, back.1 + ux * head * 0.5);
                    let r = (back.0 + uy * head * 0.5, back.1 - ux * head * 0.5);
                    rt.DrawLine(v(*a, o), v((b.0 - ux * head * 0.6, b.1 - uy * head * 0.6), o), &brush, w, &res.stroke_round);
                    let Ok(fac) = rt.GetFactory() else { continue };
                    let Ok(path) = fac.CreatePathGeometry() else { continue };
                    if let Ok(sink) = path.Open() {
                        sink.BeginFigure(v(*b, o), D2D1_FIGURE_BEGIN_FILLED);
                        sink.AddLine(v(l, o));
                        sink.AddLine(v(r, o));
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        let _ = sink.Close();
                    }
                    rt.FillGeometry(&path, &brush, None);
                }
                Kind::Rect(r) => {
                    let f = D2D_RECT_F { left: r.l as f32 - o.0, top: r.t as f32 - o.1, right: r.r as f32 - o.0, bottom: r.b as f32 - o.1 };
                    rt.DrawRectangle(&f, &brush, w, None);
                }
                Kind::Ellipse(r) => {
                    let e = D2D1_ELLIPSE {
                        point: Vector2::new((r.l + r.r) as f32 / 2.0 - o.0, (r.t + r.b) as f32 / 2.0 - o.1),
                        radiusX: r.w() as f32 / 2.0,
                        radiusY: r.h() as f32 / 2.0,
                    };
                    rt.DrawEllipse(&e, &brush, w, None);
                }
                Kind::Text(p, text) => {
                    let Some(fmt) = res.text_format(font_size(w)) else { continue };
                    let t: Vec<u16> = text.encode_utf16().collect();
                    let lr = D2D_RECT_F { left: p.0 - o.0, top: p.1 - o.1, right: p.0 - o.0 + 4000.0, bottom: p.1 - o.1 + 4000.0 };
                    rt.DrawText(&t, &fmt, &lr, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
                Kind::Number(p, n) => {
                    let rad = font_size(w) * 0.85;
                    let e = D2D1_ELLIPSE { point: v(*p, o), radiusX: rad, radiusY: rad };
                    rt.FillEllipse(&e, &brush);
                    let light = (s.color >> 16 & 0xFF) + (s.color >> 8 & 0xFF) + (s.color & 0xFF) > 600;
                    let Ok(fg) = rt.CreateSolidColorBrush(&rgba(if light { 0 } else { 0xFFFFFF }, 1.0), None) else { continue };
                    let Some(fmt) = res.text_format(font_size(w)) else { continue };
                    let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                    let t: Vec<u16> = n.to_string().encode_utf16().collect();
                    let lr = D2D_RECT_F { left: p.0 - o.0 - rad, top: p.1 - o.1 - rad, right: p.0 - o.0 + rad, bottom: p.1 - o.1 + rad };
                    rt.DrawText(&t, &fmt, &lr, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
                Kind::Blur(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixelate_averages_blocks_and_clips() {
        // 4x2 image at origin (10, 10); pixelate 2x2 blocks over x 10..14, y 10..12.
        let stride = 16;
        let mut buf: Vec<u8> = (0..32).map(|i| if (i / 4) % 2 == 0 { 0 } else { 100 }).collect();
        pixelate(&mut buf, stride, (10, 10), (4, 2), &Rect::new(5, 5, 100, 100), 2);
        assert!(buf.chunks(4).all(|p| p == [50, 50, 50, 50]));
    }

    #[test]
    fn pixelate_outside_is_noop() {
        let mut buf = vec![7u8; 16];
        pixelate(&mut buf, 8, (0, 0), (2, 2), &Rect::new(5, 5, 9, 9), 4);
        assert!(buf.iter().all(|&b| b == 7));
    }

    #[test]
    fn undo_redo() {
        let mut d = Doc::default();
        let s = |n| Shape { kind: Kind::Number((0.0, 0.0), n), color: 0, width: 1.0 };
        d.push(s(1));
        d.push(s(2));
        assert!(d.undo());
        assert_eq!(d.next_number(), 2);
        assert!(d.redo());
        assert!(!d.redo());
        d.undo();
        d.push(s(9));
        assert!(!d.redo(), "new shape clears redo");
        assert_eq!(d.shapes.len(), 2);
    }

    #[test]
    fn toolbar_stays_on_monitor() {
        let mon = Rect::new(0, 0, 1920, 1080);
        let bar = toolbar_layout(&Rect::new(10, 10, 100, 100), &mon, 1.0);
        assert_eq!(bar.len(), TOOLS.len() + PALETTE.len());
        assert!(bar.iter().all(|r| r.l >= 0 && r.r <= 1920 && r.t == 104));
        // Selection touching the bottom: bar goes above.
        let bar = toolbar_layout(&Rect::new(500, 600, 1500, 1080), &mon, 1.0);
        assert!(bar[0].b <= 600);
        // Full-screen selection: bar inside, bottom edge.
        let bar = toolbar_layout(&mon, &mon, 1.0);
        assert!(bar[0].b <= 1080 && bar[0].t >= 0);
    }
}
