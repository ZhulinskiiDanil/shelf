//! Video → animated GIF (M16): one median-cut palette for the whole clip, and pixels
//! that did not change since the previous frame are written as transparent, which
//! keeps screen recordings small.

use crate::clipboard::Image;
use std::path::Path;

/// Median cut (mean-split variant): splits the colour box with the widest channel range
/// until there are `n` boxes; each box's average is a palette entry.
pub fn median_cut(colors: &[[u8; 3]], n: usize) -> Vec<[u8; 3]> {
    if colors.is_empty() || n == 0 {
        return Vec::new();
    }
    let mut boxes: Vec<Vec<[u8; 3]>> = vec![colors.to_vec()];
    while boxes.len() < n {
        // Box with the widest range that can still be split.
        let range = |b: &Vec<[u8; 3]>| -> (u8, usize) {
            (0..3)
                .map(|c| {
                    let (lo, hi) = b.iter().fold((255u8, 0u8), |(lo, hi), p| (lo.min(p[c]), hi.max(p[c])));
                    (hi - lo, c)
                })
                .max()
                .unwrap()
        };
        let Some((i, (_, ch))) = boxes.iter().enumerate().map(|(i, b)| (i, range(b))).filter(|(_, (r, _))| *r > 0).max_by_key(|(i, (r, _))| *r as u64 * boxes[*i].len() as u64) else {
            break;
        };
        // Split at the mean rather than the middle element: clusters of flat UI colours
        // stay whole. Both halves are non-empty because the range is non-zero.
        let b = boxes.swap_remove(i);
        let mean = b.iter().map(|p| p[ch] as u64).sum::<u64>() as f64 / b.len() as f64;
        let (lo, hi): (Vec<[u8; 3]>, Vec<[u8; 3]>) = b.into_iter().partition(|p| (p[ch] as f64) < mean);
        boxes.push(lo);
        boxes.push(hi);
    }
    boxes
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| {
            let mut s = [0u64; 3];
            for p in b {
                for c in 0..3 {
                    s[c] += p[c] as u64;
                }
            }
            let n = b.len() as u64;
            [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8]
        })
        .collect()
}

pub fn nearest(palette: &[[u8; 3]], c: [u8; 3]) -> u8 {
    let d = |p: &[u8; 3]| (0..3).map(|i| (p[i] as i32 - c[i] as i32).pow(2)).sum::<i32>();
    palette.iter().enumerate().min_by_key(|(_, p)| d(p)).map(|(i, _)| i as u8).unwrap_or(0)
}

/// Maps colours through a 5-bit-per-channel cache of nearest palette entries.
pub struct Mapper {
    palette: Vec<[u8; 3]>,
    cache: Vec<u16>,
}

impl Mapper {
    pub fn new(palette: Vec<[u8; 3]>) -> Self {
        Mapper { palette, cache: vec![u16::MAX; 1 << 15] }
    }

    pub fn map(&mut self, c: [u8; 3]) -> u8 {
        let k = ((c[0] as usize >> 3) << 10) | ((c[1] as usize >> 3) << 5) | (c[2] as usize >> 3);
        if self.cache[k] == u16::MAX {
            let center = [(c[0] & 0xF8) | 4, (c[1] & 0xF8) | 4, (c[2] & 0xF8) | 4];
            self.cache[k] = nearest(&self.palette, center) as u16;
        }
        self.cache[k] as u8
    }
}

const TRANSPARENT: u8 = 255;

/// Encodes BGRA frames (all the same size) with per-frame delays in centiseconds.
pub fn encode(frames: &[(Image, u16)]) -> Result<Vec<u8>, String> {
    let Some((first, _)) = frames.first() else { return Err("no frames".into()) };
    let (w, h) = (first.w as u16, first.h as u16);
    // Palette from a sample of pixels across the clip.
    let mut sample = Vec::new();
    let every = (frames.len() / 12).max(1);
    for (img, _) in frames.iter().step_by(every) {
        for px in img.bgra.chunks_exact(4).step_by(7) {
            sample.push([px[2], px[1], px[0]]);
        }
    }
    let palette = median_cut(&sample, 255);
    let mut flat: Vec<u8> = palette.iter().flatten().copied().collect();
    flat.resize(256 * 3, 0);
    let mut mapper = Mapper::new(palette);
    let mut out = Vec::new();
    {
        let mut enc = gif::Encoder::new(&mut out, w, h, &flat).map_err(|e| e.to_string())?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(|e| e.to_string())?;
        let mut prev: Vec<u8> = Vec::new();
        for (i, (img, delay)) in frames.iter().enumerate() {
            let idx: Vec<u8> = img.bgra.chunks_exact(4).map(|p| mapper.map([p[2], p[1], p[0]])).collect();
            let buf: Vec<u8> = if i == 0 { idx.clone() } else { idx.iter().zip(&prev).map(|(&a, &b)| if a == b { TRANSPARENT } else { a }).collect() };
            let mut f = gif::Frame { width: w, height: h, delay: *delay, buffer: buf.into(), dispose: gif::DisposalMethod::Keep, ..Default::default() };
            if i > 0 {
                f.transparent = Some(TRANSPARENT);
            }
            enc.write_frame(&f).map_err(|e| e.to_string())?;
            prev = idx;
        }
    }
    Ok(out)
}

/// GIF from an MP4: `fps` frames per second, width at most `max_w`; returns the bytes
/// and the first frame. Shrinks until the result fits `max_bytes` (0 = no limit).
pub fn from_video(input: &Path, fps: u32, max_w: u32, max_bytes: u64) -> Result<(Vec<u8>, Image), String> {
    crate::transcode::init();
    let info = crate::transcode::probe(input).map_err(|e| e.to_string())?;
    let mut width = max_w.min(info.w);
    loop {
        let height = ((info.h as u64 * width as u64 / info.w as u64) as u32).max(2) & !1;
        let size = (width & !1, height);
        let step = 10_000_000 / fps.clamp(1, 50) as i64;
        let mut frames: Vec<(Image, i64)> = Vec::new();
        crate::transcode::frames(input, size, step, |t, img| {
            frames.push((img, t));
            true
        })
        .map_err(|e| e.to_string())?;
        // Delay of each frame = time to the next one, in centiseconds.
        let delays: Vec<u16> = (0..frames.len())
            .map(|i| {
                let next = frames.get(i + 1).map(|f| f.1).unwrap_or(frames[i].1 + step);
                // From rounded absolute times, so centisecond rounding does not accumulate.
                ((next + 50_000) / 100_000 - (frames[i].1 + 50_000) / 100_000).max(2) as u16
            })
            .collect();
        let first = frames.first().map(|f| f.0.clone()).ok_or("no frames")?;
        let list: Vec<(Image, u16)> = frames.into_iter().zip(delays).map(|((img, _), d)| (img, d)).collect();
        let t0 = std::time::Instant::now();
        let bytes = encode(&list)?;
        log::info!("gif: {} frames {}x{} -> {:.1} MB in {} ms", list.len(), size.0, size.1, bytes.len() as f64 / 1e6, t0.elapsed().as_millis());
        if max_bytes == 0 || bytes.len() as u64 <= max_bytes || width <= 160 {
            return Ok((bytes, first));
        }
        // Size grows roughly with the area.
        let ratio = (max_bytes as f64 / bytes.len() as f64).sqrt() * 0.95;
        width = ((width as f64 * ratio) as u32).max(160);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_cut_separates_clusters() {
        let mut colors = Vec::new();
        for i in 0..100u8 {
            colors.push([250, i % 3, 0]); // reds
            colors.push([0, 0, 250 - i % 3]); // blues
            colors.push([128, 128, 128]); // grey
        }
        let p = median_cut(&colors, 3);
        assert_eq!(p.len(), 3);
        for want in [[250u8, 0, 0], [0, 0, 250], [128, 128, 128]] {
            let best = p[nearest(&p, want) as usize];
            let d: i32 = (0..3).map(|c| (best[c] as i32 - want[c] as i32).abs()).sum();
            assert!(d <= 4, "{want:?} -> {best:?} in {p:?}");
        }
        // Fewer distinct colours than requested: no empty boxes.
        assert_eq!(median_cut(&[[1, 2, 3]; 10], 16), vec![[1, 2, 3]]);
        assert!(median_cut(&[], 16).is_empty());
    }

    #[test]
    fn mapper_uses_nearest_entry() {
        let mut m = Mapper::new(vec![[0, 0, 0], [255, 255, 255], [255, 0, 0]]);
        assert_eq!(m.map([10, 10, 10]), 0);
        assert_eq!(m.map([240, 250, 245]), 1);
        assert_eq!(m.map([200, 30, 20]), 2);
    }

    #[test]
    fn unchanged_pixels_become_transparent() {
        let img = |c: u8| Image { w: 2, h: 1, bgra: vec![c, c, c, 255, 0, 0, 0, 255] };
        let bytes = encode(&[(img(255), 10), (img(255), 10), (img(0), 10)]).unwrap();
        let mut d = gif::DecodeOptions::new();
        d.set_color_output(gif::ColorOutput::Indexed);
        let mut r = d.read_info(bytes.as_slice()).unwrap();
        let f1 = r.read_next_frame().unwrap().unwrap().buffer.to_vec();
        assert_ne!(f1[0], TRANSPARENT);
        let f2 = r.read_next_frame().unwrap().unwrap();
        assert_eq!(f2.transparent, Some(TRANSPARENT));
        assert_eq!(f2.buffer.to_vec(), vec![TRANSPARENT, TRANSPARENT]);
        let f3 = r.read_next_frame().unwrap().unwrap().buffer.to_vec();
        assert_ne!(f3[0], TRANSPARENT);
        assert_eq!(f3[1], TRANSPARENT);
    }
}
