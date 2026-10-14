//! Pure geometry for the capture overlay. All coordinates are physical pixels in
//! virtual-screen space (process is per-monitor DPI aware v2).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub const fn new(l: i32, t: i32, r: i32, b: i32) -> Self {
        Self { l, t, r, b }
    }
    pub fn from_points(a: (i32, i32), b: (i32, i32)) -> Self {
        Self::new(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
    }
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
    pub fn is_empty(&self) -> bool {
        self.w() <= 0 || self.h() <= 0
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let r = Rect::new(self.l.max(o.l), self.t.max(o.t), self.r.min(o.r), self.b.min(o.b));
        if r.is_empty() { Rect::default() } else { r }
    }
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.l + dx, self.t + dy, self.r + dx, self.b + dy)
    }
    /// Moves (without resizing) so the rect lies inside `bounds` as far as possible.
    pub fn clamp_move(&self, bounds: &Rect) -> Rect {
        let dx = if self.l < bounds.l {
            bounds.l - self.l
        } else if self.r > bounds.r {
            (bounds.r - self.r).max(bounds.l - self.l)
        } else {
            0
        };
        let dy = if self.t < bounds.t {
            bounds.t - self.t
        } else if self.b > bounds.b {
            (bounds.b - self.b).max(bounds.t - self.t)
        } else {
            0
        };
        self.offset(dx, dy)
    }
}

/// Which part of a selection the cursor grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grip {
    Move,
    Edge { l: bool, t: bool, r: bool, b: bool },
}

/// Hit-test the selection border with a tolerance of `tol` pixels.
pub fn hit_test(sel: &Rect, x: i32, y: i32, tol: i32) -> Option<Grip> {
    let outer = Rect::new(sel.l - tol, sel.t - tol, sel.r + tol, sel.b + tol);
    if !outer.contains(x, y) {
        return None;
    }
    let l = (x - sel.l).abs() <= tol;
    let r = !l && (x - sel.r).abs() <= tol;
    let t = (y - sel.t).abs() <= tol;
    let b = !t && (y - sel.b).abs() <= tol;
    if l || r || t || b {
        Some(Grip::Edge { l, t, r, b })
    } else if sel.contains(x, y) {
        Some(Grip::Move)
    } else {
        None
    }
}

/// Applies a drag of (dx, dy) to `orig` with grip `g`. The result is normalised,
/// so dragging an edge past the opposite one flips the selection.
pub fn apply_grip(orig: &Rect, g: Grip, dx: i32, dy: i32, bounds: &Rect) -> Rect {
    match g {
        Grip::Move => orig.offset(dx, dy).clamp_move(bounds),
        Grip::Edge { l, t, r, b } => {
            let mut n = *orig;
            if l {
                n.l += dx;
            }
            if r {
                n.r += dx;
            }
            if t {
                n.t += dy;
            }
            if b {
                n.b += dy;
            }
            Rect::from_points((n.l, n.t), (n.r, n.b)).intersect(bounds)
        }
    }
}

/// UI scale (1.0 at 96 DPI) for the monitor containing (x, y); falls back to the nearest monitor.
pub fn scale_at(monitors: &[(Rect, u32)], x: i32, y: i32) -> f32 {
    let dpi = monitors
        .iter()
        .find(|(r, _)| r.contains(x, y))
        .or_else(|| {
            monitors.iter().min_by_key(|(r, _)| {
                let dx = (r.l - x).max(x - r.r + 1).max(0) as i64;
                let dy = (r.t - y).max(y - r.b + 1).max(0) as i64;
                dx * dx + dy * dy
            })
        })
        .map(|m| m.1)
        .unwrap_or(96);
    dpi as f32 / 96.0
}

/// Places a box of size (w, h) near the cursor, offset by `gap`, flipping to stay inside `mon`.
pub fn place_near(x: i32, y: i32, w: i32, h: i32, gap: i32, mon: &Rect) -> (i32, i32) {
    let mut px = x + gap;
    let mut py = y + gap;
    if px + w > mon.r {
        px = x - gap - w;
    }
    if py + h > mon.b {
        py = y - gap - h;
    }
    (px.max(mon.l), py.max(mon.t))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(-1920, 0, 2560, 1440);

    #[test]
    fn from_points_normalises() {
        assert_eq!(Rect::from_points((10, 20), (5, 2)), Rect::new(5, 2, 10, 20));
    }

    #[test]
    fn hit_edges_and_corners() {
        let s = Rect::new(100, 100, 200, 200);
        assert_eq!(hit_test(&s, 150, 150, 4), Some(Grip::Move));
        assert_eq!(hit_test(&s, 98, 150, 4), Some(Grip::Edge { l: true, t: false, r: false, b: false }));
        assert_eq!(hit_test(&s, 202, 203, 4), Some(Grip::Edge { l: false, t: false, r: true, b: true }));
        assert_eq!(hit_test(&s, 50, 50, 4), None);
    }

    #[test]
    fn tiny_selection_prefers_left_top() {
        // On a 2px wide selection both edges are in range; must not set both l and r.
        let s = Rect::new(100, 100, 102, 102);
        assert_eq!(hit_test(&s, 101, 101, 4), Some(Grip::Edge { l: true, t: true, r: false, b: false }));
    }

    #[test]
    fn resize_flips_past_opposite_edge() {
        let s = Rect::new(100, 100, 200, 200);
        let g = Grip::Edge { l: true, t: false, r: false, b: false };
        assert_eq!(apply_grip(&s, g, 150, 0, &SCREEN), Rect::new(200, 100, 250, 200));
    }

    #[test]
    fn move_is_clamped_on_negative_monitor() {
        let s = Rect::new(-1900, 10, -1800, 110);
        assert_eq!(apply_grip(&s, Grip::Move, -500, -50, &SCREEN), Rect::new(-1920, 0, -1820, 100));
        let big = Rect::new(0, 0, 5000, 100);
        assert_eq!(big.clamp_move(&SCREEN).l, -1920);
    }

    #[test]
    fn scale_per_monitor() {
        let mons = [(Rect::new(-1920, 0, 0, 1080), 96), (Rect::new(0, 0, 2560, 1440), 144)];
        assert_eq!(scale_at(&mons, -5, 500), 1.0);
        assert_eq!(scale_at(&mons, 0, 500), 1.5);
        // Gap below the shorter monitor: nearest is the left one.
        assert_eq!(scale_at(&mons, -1000, 1200), 1.0);
        assert_eq!(scale_at(&[], 0, 0), 1.0);
    }

    #[test]
    fn place_near_flips_at_edges() {
        let m = Rect::new(0, 0, 1000, 800);
        assert_eq!(place_near(100, 100, 50, 50, 20, &m), (120, 120));
        assert_eq!(place_near(990, 790, 50, 50, 20, &m), (920, 720));
    }
}
