//! M14: pure stitching for scrolling capture. Frames are compared row by row using
//! quantized-grayscale row hashes; the offset is measured, never assumed from the
//! scroll step. Fixed headers/footers are detected and emitted once.

/// Per-row signature: hash of quantized gray values, and whether the row is a single colour
/// (blank rows match anything and must not drive the offset decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub hash: u64,
    pub uniform: bool,
}

/// Signatures for a top-down BGRA frame. `skip_right` columns (scrollbar) are ignored.
pub fn rows(bgra: &[u8], w: usize, h: usize, skip_right: usize) -> Vec<Row> {
    let cols = w.saturating_sub(skip_right).max(1);
    (0..h)
        .map(|y| {
            let line = &bgra[y * w * 4..y * w * 4 + cols * 4];
            let mut hash: u64 = 0xcbf29ce484222325;
            let mut first = None;
            let mut uniform = true;
            for p in line.chunks_exact(4) {
                // Rec.601 luma, quantized to 6 bits so tiny rendering noise does not break matches.
                let g = ((p[2] as u32 * 299 + p[1] as u32 * 587 + p[0] as u32 * 114) / 1000) as u8 >> 2;
                hash = (hash ^ g as u64).wrapping_mul(0x100000001b3);
                match first {
                    None => first = Some(g),
                    Some(f) if f != g => uniform = false,
                    _ => {}
                }
            }
            Row { hash, uniform }
        })
        .collect()
}

/// Header/footer heights: rows identical (same position) at the top and bottom of two
/// consecutive frames whose middle moved. Each is capped at 40 % of the frame.
pub fn fixed_bands(a: &[Row], b: &[Row]) -> (usize, usize) {
    let n = a.len().min(b.len());
    let cap = n * 2 / 5;
    let header = (0..n).take_while(|&i| a[i].hash == b[i].hash).count().min(cap);
    let footer = (0..n).take_while(|&i| a[n - 1 - i].hash == b[n - 1 - i].hash).count().min(cap);
    (header, footer)
}

/// How far the content moved up between `prev` and `cur` (content rows only, same length):
/// `Some(0)` = identical, `Some(dy)` = scrolled by dy rows, `None` = no reliable match.
pub fn find_offset(prev: &[Row], cur: &[Row]) -> Option<usize> {
    let n = prev.len().min(cur.len());
    if n == 0 {
        return None;
    }
    if prev[..n] == cur[..n] {
        return Some(0);
    }
    let mut best: Option<(usize, usize)> = None; // (dy, matched informative rows)
    for dy in 1..n {
        let overlap = n - dy;
        let (mut informative, mut matched) = (0usize, 0usize);
        for i in 0..overlap {
            let (p, c) = (prev[dy + i], cur[i]);
            if p.uniform && c.uniform {
                continue;
            }
            informative += 1;
            if p.hash == c.hash {
                matched += 1;
            }
        }
        if informative < 8 || matched * 100 < informative * 97 {
            continue;
        }
        // The true offset keeps the largest number of matching informative rows.
        if best.is_none_or(|b| matched > b.1) {
            best = Some((dy, matched));
        }
    }
    best.map(|b| b.0)
}

/// Incremental stitcher: feed frames in order; header from the first frame, footer from the last.
pub struct Stitcher {
    pub w: usize,
    h: usize,
    skip_right: usize,
    header: usize,
    footer: usize,
    bands_known: bool,
    /// Accumulated rows: header + content (footer is added at the end).
    out: Vec<u8>,
    first: Vec<u8>,
    last: Vec<u8>,
    last_rows: Vec<Row>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Added(usize),
    Unchanged,
    Lost,
}

impl Stitcher {
    pub fn new(frame: &[u8], w: usize, h: usize, skip_right: usize) -> Self {
        Stitcher { w, h, skip_right, header: 0, footer: 0, bands_known: false, out: Vec::new(), first: frame.to_vec(), last: frame.to_vec(), last_rows: rows(frame, w, h, skip_right) }
    }

    /// Current result height (with the footer that will be appended).
    pub fn height(&self) -> usize {
        if self.bands_known { self.out.len() / (self.w * 4) + self.footer } else { self.h }
    }

    pub fn push(&mut self, frame: &[u8]) -> Step {
        let cur = rows(frame, self.w, self.h, self.skip_right);
        if !self.bands_known {
            if cur == self.last_rows {
                return Step::Unchanged;
            }
            let (hd, ft) = fixed_bands(&self.last_rows, &cur);
            self.header = hd;
            self.footer = ft;
            self.bands_known = true;
            // First frame without its footer.
            let stride = self.w * 4;
            self.out.extend_from_slice(&self.first[..(self.h - self.footer) * stride]);
        }
        let (a, b) = (self.header, self.h - self.footer);
        match find_offset(&self.last_rows[a..b], &cur[a..b]) {
            Some(0) => Step::Unchanged,
            None => Step::Lost,
            Some(dy) => {
                let stride = self.w * 4;
                self.out.extend_from_slice(&frame[(b - dy) * stride..b * stride]);
                self.last = frame.to_vec();
                self.last_rows = cur;
                Step::Added(dy)
            }
        }
    }

    /// Final image: (width, height, top-down BGRA).
    pub fn finish(mut self) -> (usize, usize, Vec<u8>) {
        let stride = self.w * 4;
        if !self.bands_known {
            return (self.w, self.h, self.first);
        }
        self.out.extend_from_slice(&self.last[(self.h - self.footer) * stride..self.h * stride]);
        let h = self.out.len() / stride;
        (self.w, h, self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tall synthetic page: row y has a distinctive pattern; some rows are blank.
    fn page(w: usize, h: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let blank = y % 37 < 6; // blank gaps between "paragraphs"
                let g = if blank { 240 } else { ((x * 7 + y * 13 + (y * y) % 97) % 200) as u8 };
                v.extend_from_slice(&[g, g, g, 255]);
            }
        }
        v
    }

    /// Screen = header (fixed) + page window at `scroll` + footer (fixed).
    fn screen(page: &[u8], w: usize, scroll: usize, view: usize, header: usize, footer: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..header {
            v.extend(std::iter::repeat_n([255u8, (y * 9) as u8, 0, 255], w).flatten());
        }
        let content = view - header - footer;
        v.extend_from_slice(&page[scroll * w * 4..(scroll + content) * w * 4]);
        for y in 0..footer {
            v.extend(std::iter::repeat_n([0u8, (y * 11) as u8, 255, 255], w).flatten());
        }
        v
    }

    #[test]
    fn offset_on_shifted_rows() {
        let w = 40;
        let p = page(w, 400);
        let a = rows(&p[..200 * w * 4], w, 200, 0);
        let b = rows(&p[55 * w * 4..255 * w * 4], w, 200, 0);
        assert_eq!(find_offset(&a, &b), Some(55));
        assert_eq!(find_offset(&a, &a), Some(0));
        // Unrelated content does not match.
        let c: Vec<Row> = (0..200).map(|i| Row { hash: 1_000_000 + i, uniform: false }).collect();
        assert_eq!(find_offset(&a, &c), None);
    }

    #[test]
    fn stitch_with_header_and_footer() {
        let (w, view, hd, ft) = (48, 160, 20, 12);
        let total = 500;
        let p = page(w, total);
        let content = view - hd - ft;
        let mut st = Stitcher::new(&screen(&p, w, 0, view, hd, ft), w, view, 0);
        let mut scroll = 0;
        loop {
            let next = (scroll + 47).min(total - content);
            let step = st.push(&screen(&p, w, next, view, hd, ft));
            if next == scroll {
                assert_eq!(step, Step::Unchanged);
                break;
            }
            assert_eq!(step, Step::Added(next - scroll));
            scroll = next;
        }
        let (ow, oh, out) = st.finish();
        assert_eq!((ow, oh), (w, hd + total + ft));
        // Header once at the top, whole page in the middle, footer once at the bottom.
        let expect = {
            let mut v = screen(&p, w, 0, view, hd, ft)[..hd * w * 4].to_vec();
            v.extend_from_slice(&p);
            v.extend_from_slice(&screen(&p, w, 0, view, hd, ft)[(view - ft) * w * 4..]);
            v
        };
        assert!(out == expect);
    }

    #[test]
    fn scrollbar_column_is_ignored() {
        let w = 30;
        let p = page(w, 300);
        let mut a = p[..120 * w * 4].to_vec();
        let mut b = p[40 * w * 4..160 * w * 4].to_vec();
        // Scrollbar thumb in the last 4 columns differs between frames.
        for (buf, off) in [(&mut a, 0usize), (&mut b, 60)] {
            for y in 0..120 {
                for x in w - 4..w {
                    let v = if (off..off + 30).contains(&y) { 10 } else { 200 };
                    buf[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&[v, v, v]);
                }
            }
        }
        assert_eq!(find_offset(&rows(&a, w, 120, 4), &rows(&b, w, 120, 4)), Some(40));
        assert_eq!(find_offset(&rows(&a, w, 120, 0), &rows(&b, w, 120, 0)), None);
    }

    #[test]
    fn bands_are_capped() {
        let r = |h| Row { hash: h, uniform: false };
        // Everything identical except one row: header and footer are capped at 40 % each.
        let a: Vec<Row> = (0..10).map(r).collect();
        let mut b = a.clone();
        b[5] = r(99);
        assert_eq!(fixed_bands(&a, &b), (4, 4));
        let c: Vec<Row> = (20..30).map(r).collect();
        assert_eq!(fixed_bands(&a, &c), (0, 0));
    }
}
