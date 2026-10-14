use crate::clipboard::Image;

/// Encodes top-down BGRA as an opaque RGB PNG.
pub fn encode_png(img: &Image, compression: png::Compression) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((img.w * img.h * 3) as usize);
    for px in img.bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w, img.h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(compression);
        let mut wr = enc.write_header().expect("png header");
        wr.write_image_data(&rgb).expect("png data");
    }
    out
}

/// Decodes a PNG into top-down BGRA (opaque or with alpha flattened onto white).
pub fn decode_png(bytes: &[u8]) -> Option<Image> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    let px = &buf[..info.buffer_size()];
    let flat = |c: u8, a: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
    match info.color_type {
        png::ColorType::Rgba => px.chunks_exact(4).for_each(|p| bgra.extend_from_slice(&[flat(p[2], p[3]), flat(p[1], p[3]), flat(p[0], p[3]), 255])),
        png::ColorType::Rgb => px.chunks_exact(3).for_each(|p| bgra.extend_from_slice(&[p[2], p[1], p[0], 255])),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).for_each(|p| {
            let g = flat(p[0], p[1]);
            bgra.extend_from_slice(&[g, g, g, 255])
        }),
        png::ColorType::Grayscale => px.iter().for_each(|&g| bgra.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::Indexed => return None, // EXPAND turns this into Rgb(a)
    }
    Some(Image { w, h, bgra })
}

/// Converts a packed CF_DIB (BITMAPINFOHEADER + optional masks/palette + pixels) to top-down BGRA.
/// Supports 24 and 32 bpp, BI_RGB and BI_BITFIELDS with the standard masks.
pub fn dib_to_image(dib: &[u8]) -> Option<Image> {
    let rd_u32 = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let rd_i32 = |o: usize| rd_u32(o).map(|v| v as i32);
    let hdr_size = rd_u32(0)? as usize;
    let w = rd_i32(4)?;
    let h = rd_i32(8)?;
    let bpp = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = rd_u32(16)?;
    let clr_used = rd_u32(32)? as usize;
    if w <= 0 || h == 0 || w > 32768 || h.abs() > 32768 {
        return None;
    }
    let mut off = hdr_size;
    match (compression, bpp) {
        (0, 24) | (0, 32) => off += clr_used * 4,
        (3, 32) => {
            // Masks follow a plain BITMAPINFOHEADER; V4/V5 headers contain them.
            if hdr_size == 40 {
                off += 12;
            }
        }
        _ => return None,
    }
    let (wu, hu) = (w as usize, h.unsigned_abs() as usize);
    let bypp = bpp as usize / 8;
    let stride = (wu * bypp + 3) & !3;
    if dib.len() < off + stride * hu {
        return None;
    }
    // 32-bpp DIBs on the clipboard usually have garbage or zero alpha; treat as opaque.
    let mut bgra = Vec::with_capacity(wu * hu * 4);
    for row in 0..hu {
        let src_row = if h > 0 { hu - 1 - row } else { row };
        let line = &dib[off + src_row * stride..off + src_row * stride + wu * bypp];
        for p in line.chunks_exact(bypp) {
            bgra.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
    }
    Some(Image { w: w as u32, h: hu as u32, bgra })
}

/// Nearest-neighbour thumbnail straight from a packed CF_DIB, without converting the
/// whole image (a huge clipboard bitmap is sampled, not copied). Same formats as dib_to_image.
/// Returns the thumbnail and the original size.
pub fn dib_thumbnail(dib: &[u8], max_w: u32, max_h: u32) -> Option<(Image, u32, u32)> {
    let rd_u32 = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let hdr_size = rd_u32(0)? as usize;
    let w = rd_u32(4)? as i32;
    let h = rd_u32(8)? as i32;
    let bpp = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = rd_u32(16)?;
    let clr_used = rd_u32(32)? as usize;
    if w <= 0 || h == 0 || w > 65535 || h.abs() > 65535 {
        return None;
    }
    let mut off = hdr_size;
    match (compression, bpp) {
        (0, 24) | (0, 32) => off += clr_used * 4,
        (3, 32) if hdr_size == 40 => off += 12,
        (3, 32) => {}
        _ => return None,
    }
    let (wu, hu) = (w as usize, h.unsigned_abs() as usize);
    let bypp = bpp as usize / 8;
    let stride = (wu * bypp + 3) & !3;
    if dib.len() < off + stride * hu {
        return None;
    }
    let s = (max_w as f64 / wu as f64).min(max_h as f64 / hu as f64).min(1.0);
    let (tw, th) = (((wu as f64 * s) as usize).max(1), ((hu as f64 * s) as usize).max(1));
    let mut bgra = Vec::with_capacity(tw * th * 4);
    for ty in 0..th {
        let sy = ty * hu / th;
        let row = if h > 0 { hu - 1 - sy } else { sy };
        for tx in 0..tw {
            let i = off + row * stride + (tx * wu / tw) * bypp;
            bgra.extend_from_slice(&[dib[i], dib[i + 1], dib[i + 2], 255]);
        }
    }
    Some((Image { w: tw as u32, h: th as u32, bgra }, wu as u32, hu as u32))
}

/// Box-filter downscale so the result fits in max_w × max_h (never upscales).
pub fn thumbnail(img: &Image, max_w: u32, max_h: u32) -> Image {
    let s = (max_w as f64 / img.w as f64).min(max_h as f64 / img.h as f64).min(1.0);
    let tw = ((img.w as f64 * s).round() as u32).max(1);
    let th = ((img.h as f64 * s).round() as u32).max(1);
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    for ty in 0..th {
        let y0 = (ty as u64 * img.h as u64 / th as u64) as u32;
        let y1 = (((ty + 1) as u64 * img.h as u64 / th as u64) as u32).max(y0 + 1);
        for tx in 0..tw {
            let x0 = (tx as u64 * img.w as u64 / tw as u64) as u32;
            let x1 = (((tx + 1) as u64 * img.w as u64 / tw as u64) as u32).max(x0 + 1);
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * img.w + x) * 4) as usize;
                    for c in 0..3 {
                        sum[c] += img.bgra[i + c] as u32;
                    }
                }
            }
            let n = (x1 - x0) * (y1 - y0);
            out.extend_from_slice(&[(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255]);
        }
    }
    Image { w: tw, h: th, bgra: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dib(w: i32, h: i32, bpp: u16, comp: u32, extra: &[u8], px: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&bpp.to_le_bytes());
        v.extend_from_slice(&comp.to_le_bytes());
        v.extend_from_slice(&[0; 20]);
        v.extend_from_slice(extra);
        v.extend_from_slice(px);
        v
    }

    #[test]
    fn dib_24bpp_bottom_up_with_padding() {
        // 1x2 image, rows padded to 4 bytes; bottom-up: first stored row is the bottom one.
        let px = [1, 2, 3, 0, 4, 5, 6, 0];
        let img = dib_to_image(&dib(1, 2, 24, 0, &[], &px)).unwrap();
        assert_eq!((img.w, img.h), (1, 2));
        assert_eq!(img.bgra, vec![4, 5, 6, 255, 1, 2, 3, 255]);
    }

    #[test]
    fn dib_32bpp_bitfields_top_down() {
        let masks = [0, 0, 0xFF, 0, 0, 0xFF, 0, 0, 0xFF, 0, 0, 0];
        let px = [9, 8, 7, 0, 6, 5, 4, 0];
        let img = dib_to_image(&dib(2, -1, 32, 3, &masks, &px)).unwrap();
        assert_eq!(img.bgra, vec![9, 8, 7, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn dib_rejects_truncated_and_unsupported() {
        assert!(dib_to_image(&dib(2, 2, 32, 0, &[], &[0; 8])).is_none());
        assert!(dib_to_image(&dib(1, 1, 8, 0, &[], &[0; 4])).is_none());
        assert!(dib_to_image(&[1, 2, 3]).is_none());
    }

    #[test]
    fn png_roundtrip() {
        let img = Image { w: 2, h: 1, bgra: vec![10, 20, 30, 255, 40, 50, 60, 255] };
        let back = decode_png(&encode_png(&img, png::Compression::Fast)).unwrap();
        assert_eq!(back.bgra, img.bgra);
    }

    #[test]
    fn thumbnail_averages_and_keeps_aspect() {
        let img = Image { w: 4, h: 2, bgra: [[0, 0, 0, 255], [100, 100, 100, 255]].repeat(4).concat() };
        let t = thumbnail(&img, 2, 2);
        assert_eq!((t.w, t.h), (2, 1));
        assert_eq!(&t.bgra[..4], &[50, 50, 50, 255]);
        let same = thumbnail(&img, 100, 100);
        assert_eq!((same.w, same.h), (4, 2));
    }
}
