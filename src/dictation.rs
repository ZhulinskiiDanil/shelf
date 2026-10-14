//! Hold-to-talk dictation: microphone → local whisper model → clipboard text.
//! The model is loaded on first use and unloaded after an idle period.

use crate::audio;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

struct Model {
    ctx: WhisperContext,
    path: PathBuf,
    last_used: Instant,
}

static MODEL: Mutex<Option<Model>> = Mutex::new(None);
static RECORDING: Mutex<Option<audio::Recording>> = Mutex::new(None);

pub fn default_model_path() -> PathBuf {
    crate::config::app_dir().join("models").join("ggml-base.bin")
}

pub fn model_path() -> PathBuf {
    let p = crate::config::current().dictation.model;
    if p.trim().is_empty() { default_model_path() } else { PathBuf::from(p.trim()) }
}

pub fn model_present() -> bool {
    model_path().is_file()
}

pub const MODEL_HELP: &str = "Диктовке нужен локальный файл модели Whisper в формате ggml.\n\n\
1. Скачайте модель сами, например ggml-base.bin (≈150 МБ, ru + en) или ggml-small.bin (точнее, ≈470 МБ), \
со страницы huggingface.co/ggerganov/whisper.cpp (раздел Files).\n\
2. Положите файл сюда:\n{path}\n   (или укажите свой путь в config.toml: [dictation] model = \"…\").\n\n\
Shelf ничего не скачивает сам и работает полностью офлайн.";

/// Hold-key pressed.
pub fn start() -> bool {
    if !model_present() {
        log::warn!("dictation: model not found at {}", model_path().display());
        crate::notify("Shelf: нет модели диктовки", "Трей → «Диктовка: где взять модель»");
        return false;
    }
    let mut rec = RECORDING.lock().unwrap();
    if rec.is_none() {
        *rec = Some(audio::start());
        log::info!("dictation: recording");
    }
    true
}

/// Hold-key released: transcribe on a worker thread, then `done(text)` on the UI thread.
pub fn stop(done: impl FnOnce(Result<String, String>) + Send + 'static) {
    let Some(rec) = RECORDING.lock().unwrap().take() else { return };
    std::thread::spawn(move || {
        let res = rec.finish().and_then(|pcm| transcribe(&pcm));
        crate::run_on_main(move || done(res));
    });
}

fn transcribe(pcm: &[f32]) -> Result<String, String> {
    let secs = pcm.len() as f32 / audio::WHISPER_RATE as f32;
    if secs < 0.3 {
        return Ok(String::new());
    }
    let cfg = crate::config::current().dictation;
    let path = model_path();
    let mut guard = MODEL.lock().unwrap();
    if guard.as_ref().is_none_or(|m| m.path != path) {
        let t0 = Instant::now();
        let ctx = WhisperContext::new_with_params(&path, WhisperContextParameters::default()).map_err(|e| format!("model load: {e}"))?;
        log::info!("dictation: model loaded in {:.1} s", t0.elapsed().as_secs_f32());
        *guard = Some(Model { ctx, path, last_used: Instant::now() });
    }
    let m = guard.as_mut().unwrap();
    m.last_used = Instant::now();
    let t0 = Instant::now();
    let mut state = m.ctx.create_state().map_err(|e| e.to_string())?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let lang = cfg.language.trim().to_ascii_lowercase();
    params.set_language(Some(if lang == "ru" || lang == "en" { lang.as_str() } else { "auto" }));
    params.set_n_threads(std::thread::available_parallelism().map(|n| n.get().min(8) as i32).unwrap_or(4));
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    state.full(params, pcm).map_err(|e| e.to_string())?;
    let mut text = String::new();
    for seg in state.as_iter() {
        text.push_str(&seg.to_string());
    }
    log::info!("dictation: {secs:.1} s of audio transcribed in {:.1} s", t0.elapsed().as_secs_f32());
    drop(guard);
    schedule_unload(Duration::from_secs(cfg.unload_after_min.max(1) * 60));
    Ok(clean(&text))
}

/// Whisper output: trim, collapse whitespace, drop non-speech markers like "[музыка]".
pub fn clean(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in raw.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn schedule_unload(after: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(after + Duration::from_secs(1));
        let mut g = MODEL.lock().unwrap();
        if g.as_ref().is_some_and(|m| m.last_used.elapsed() >= after) {
            *g = None;
            log::info!("dictation: model unloaded after idle");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text() {
        assert_eq!(clean("  Привет,   мир!\n"), "Привет, мир!");
        assert_eq!(clean("[музыка] Текст (смех) дальше"), "Текст дальше");
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
    }
}
