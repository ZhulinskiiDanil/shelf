//! Offline text recognition with Windows.Media.Ocr (ru + en, whatever is installed).

use crate::clipboard::Image;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::core::HSTRING;

pub enum OcrError {
    /// Neither the Russian nor the English OCR language pack is installed.
    NoLanguages,
    Failed(String),
}

/// Recognises text in `img`, trying every installed engine among ru/en and keeping
/// the result that best matches its own alphabet. Blocking; call off the UI thread.
pub fn recognize(img: &Image) -> Result<String, OcrError> {
    let engines = engines();
    if engines.is_empty() {
        return Err(OcrError::NoLanguages);
    }
    let prepared = prepare(img);
    let bitmap = to_software_bitmap(&prepared).map_err(|e| OcrError::Failed(e.to_string()))?;
    let mut results = Vec::new();
    for (tag, engine) in engines {
        let r = engine.RecognizeAsync(&bitmap).and_then(|op| op.join()).map_err(|e| OcrError::Failed(e.to_string()))?;
        let lines = r.Lines().map_err(|e| OcrError::Failed(e.to_string()))?;
        let text: Vec<String> = lines.into_iter().filter_map(|l| l.Text().ok()).map(|t| t.to_string()).collect();
        results.push((tag, text.join("\n")));
    }
    Ok(pick(results))
}

/// OCR is unreliable on tiny text and refuses images above MaxImageDimension:
/// upscale small captures 2× and downscale huge ones.
fn prepare(img: &Image) -> Image {
    let max = OcrEngine::MaxImageDimension().unwrap_or(10000);
    let longest = img.w.max(img.h);
    if longest > max {
        return crate::imageio::thumbnail(img, max, max);
    }
    if img.h < 200 && longest * 2 <= max {
        let (w, h) = (img.w * 2, img.h * 2);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let row = &img.bgra[((y / 2) * img.w * 4) as usize..((y / 2 + 1) * img.w * 4) as usize];
            for px in row.chunks_exact(4) {
                out.extend_from_slice(px);
                out.extend_from_slice(px);
            }
        }
        return Image { w, h, bgra: out };
    }
    Image { w: img.w, h: img.h, bgra: img.bgra.clone() }
}

fn to_software_bitmap(img: &Image) -> windows::core::Result<SoftwareBitmap> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(&img.bgra)?;
    let buf = writer.DetachBuffer()?;
    SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, img.w as i32, img.h as i32)
}

/// Chooses between per-language results. The English engine reads Cyrillic as Latin
/// look-alikes ("колесо" → "Koneco"), and only the Russian one can output Cyrillic, so:
/// if at least half of the Russian result's letters are Cyrillic, it wins; otherwise English
/// (the Russian engine sometimes reads Latin as Cyrillic look-alikes).
pub fn pick(results: Vec<(&str, String)>) -> String {
    match pick_index(&results) {
        Some(i) => results[i].1.clone(),
        None => String::new(),
    }
}

/// Index of the result to use (see `pick`).
pub fn pick_index(results: &[(&str, String)]) -> Option<usize> {
    let cyr = |t: &str| t.chars().filter(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё')).count();
    let letters = |t: &str| t.chars().filter(|c| c.is_alphabetic()).count();
    let ru = results.iter().position(|r| r.0 == "ru");
    let en = results.iter().position(|r| r.0 == "en");
    match (ru, en) {
        (Some(r), Some(e)) => Some(if cyr(&results[r].1) * 2 >= letters(&results[r].1).max(1) { r } else { e }),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => (!results.is_empty()).then_some(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_own_script() {
        let ru = ("ru", "Привет мир".to_string());
        let en = ("en", "Npnbet mnp".to_string());
        assert_eq!(pick(vec![("ru", "L + колесо".into()), ("en", "L + Koneco".into())]), "L + колесо");
        assert_eq!(pick(vec![ru.clone(), en.clone()]), "Привет мир");
        let ru2 = ("ru", "Неllо wоrld".to_string()); // Latin misread as Cyrillic look-alikes
        let en2 = ("en", "Hello world".to_string());
        assert_eq!(pick(vec![ru2, en2]), "Hello world");
        assert_eq!(pick(vec![]), "");
    }

    #[test]
    fn small_images_are_upscaled() {
        let img = Image { w: 2, h: 1, bgra: vec![1, 2, 3, 255, 4, 5, 6, 255] };
        // MaxImageDimension needs WinRT; only run the pure path if available.
        if OcrEngine::MaxImageDimension().is_ok() {
            let p = prepare(&img);
            assert_eq!((p.w, p.h), (4, 2));
            assert_eq!(&p.bgra[..8], &[1, 2, 3, 255, 1, 2, 3, 255]);
            assert_eq!(&p.bgra[16..20], &[1, 2, 3, 255]);
        }
    }
}

fn engines() -> Vec<(&'static str, OcrEngine)> {
    ["ru", "en"]
        .into_iter()
        .filter_map(|tag| {
            let lang = Language::CreateLanguage(&HSTRING::from(tag)).ok()?;
            if !OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
                return None;
            }
            OcrEngine::TryCreateFromLanguage(&lang).ok().map(|e| (tag, e))
        })
        .collect()
}

/// Words with boxes (pixels of `img`), grouped by line. Tall images are processed in
/// overlapping horizontal tiles so small text survives (no downscaling of long captures).
pub fn recognize_words(img: &Image) -> Result<Vec<Vec<crate::redact::Word>>, OcrError> {
    const TILE: u32 = 2400;
    const OVERLAP: u32 = 120;
    let engines = engines();
    if engines.is_empty() {
        return Err(OcrError::NoLanguages);
    }
    let fail = |e: windows::core::Error| OcrError::Failed(e.to_string());
    let mut out = Vec::new();
    let mut y0 = 0u32;
    loop {
        let h = (img.h - y0).min(TILE);
        let stride = img.w as usize * 4;
        let tile = Image { w: img.w, h, bgra: img.bgra[y0 as usize * stride..(y0 + h) as usize * stride].to_vec() };
        let prepared = prepare(&tile);
        let k = prepared.w as f32 / tile.w as f32;
        let bitmap = to_software_bitmap(&prepared).map_err(fail)?;
        let mut per_engine: Vec<(&str, String, Vec<Vec<crate::redact::Word>>)> = Vec::new();
        for (tag, engine) in &engines {
            let r = engine.RecognizeAsync(&bitmap).and_then(|op| op.join()).map_err(fail)?;
            let mut lines = Vec::new();
            let mut text = String::new();
            for line in r.Lines().map_err(fail)? {
                let mut words = Vec::new();
                for w in line.Words().map_err(fail)? {
                    let b = w.BoundingRect().map_err(fail)?;
                    let t = w.Text().map_err(fail)?.to_string();
                    words.push(crate::redact::Word { text: t, rect: (b.X / k, b.Y / k + y0 as f32, (b.X + b.Width) / k, (b.Y + b.Height) / k + y0 as f32) });
                }
                text.push_str(&line.Text().map(|t| t.to_string()).unwrap_or_default());
                text.push('\n');
                lines.push(words);
            }
            per_engine.push((tag, text, lines));
        }
        let texts: Vec<(&str, String)> = per_engine.iter().map(|(t, s, _)| (*t, s.clone())).collect();
        let chosen = pick_index(&texts).map(|i| per_engine.swap_remove(i).2).unwrap_or_default();
        let last = y0 + h >= img.h;
        for line in chosen {
            let top = line.iter().map(|w| w.rect.1).fold(f32::MAX, f32::min);
            // Lines in an overlap belong to exactly one tile.
            if y0 > 0 && top < (y0 + OVERLAP / 2) as f32 {
                continue;
            }
            if !last && top >= (y0 + h - OVERLAP / 2) as f32 {
                continue;
            }
            out.push(line);
        }
        if last {
            break;
        }
        y0 += h - OVERLAP;
    }
    Ok(out)
}
