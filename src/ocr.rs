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
    let engines: Vec<(&str, OcrEngine)> = ["ru", "en"]
        .into_iter()
        .filter_map(|tag| {
            let lang = Language::CreateLanguage(&HSTRING::from(tag)).ok()?;
            if !OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
                return None;
            }
            OcrEngine::TryCreateFromLanguage(&lang).ok().map(|e| (tag, e))
        })
        .collect();
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
    let cyr = |t: &str| t.chars().filter(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё')).count();
    let letters = |t: &str| t.chars().filter(|c| c.is_alphabetic()).count();
    let ru = results.iter().find(|r| r.0 == "ru");
    let en = results.iter().find(|r| r.0 == "en");
    match (ru, en) {
        (Some(r), Some(e)) => {
            if cyr(&r.1) * 2 >= letters(&r.1).max(1) { r.1.clone() } else { e.1.clone() }
        }
        (Some(only), None) | (None, Some(only)) => only.1.clone(),
        (None, None) => results.into_iter().next().map(|r| r.1).unwrap_or_default(),
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
