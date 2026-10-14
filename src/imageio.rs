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
