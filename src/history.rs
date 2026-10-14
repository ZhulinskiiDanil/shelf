//! Screenshot history: PNG files in %APPDATA%\Shelf\history plus index.json.

use crate::clipboard::Image;
use crate::names;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub file: String,
    /// Unix seconds.
    pub created: u64,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
}

static INDEX: Mutex<Option<Vec<Entry>>> = Mutex::new(None);
static WORKER: OnceLock<Mutex<Sender<Box<dyn FnOnce() + Send>>>> = OnceLock::new();
static TEMPLATE: Mutex<String> = Mutex::new(String::new());

pub fn dir() -> PathBuf {
    crate::config::app_dir().join("history")
}

fn index_path() -> PathBuf {
    dir().join("index.json")
}

pub fn set_template(t: &str) {
    *TEMPLATE.lock().unwrap() = t.to_string();
}

fn with_index<R>(f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    let mut g = INDEX.lock().unwrap();
    let idx = g.get_or_insert_with(|| {
        std::fs::read(index_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    });
    f(idx)
}

fn save_index(idx: &[Entry]) {
    let tmp = dir().join("index.json.tmp");
    if let Ok(b) = serde_json::to_vec_pretty(idx) {
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, index_path());
        }
    }
}

/// Runs `job` on the single history worker thread (keeps disk work off the UI thread, in order).
pub fn run_async(job: impl FnOnce() + Send + 'static) {
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = channel::<Box<dyn FnOnce() + Send>>();
        std::thread::Builder::new()
            .name("history".into())
            .spawn(move || {
                for job in rx {
                    job();
                }
            })
            .unwrap();
        Mutex::new(tx)
    });
    let _ = tx.lock().unwrap().send(Box::new(job));
}

/// Encodes and stores the image; returns the full path of the new file.
pub fn add(img: &Image, source: &str) -> Option<PathBuf> {
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let png = crate::imageio::encode_png(img, png::Compression::Fast);
    let stem = names::format_stem(&TEMPLATE.lock().unwrap(), &names::now());
    let path = with_index(|idx| {
        let name = names::unique_name(&stem, "png", |n| d.join(n).exists());
        let path = d.join(&name);
        std::fs::write(&path, &png).ok()?;
        let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        idx.push(Entry { file: name, created, pinned: false, source: source.into(), bytes: png.len() as u64, w: img.w, h: img.h });
        save_index(idx);
        Some(path)
    });
    if let Some(p) = &path {
        log::debug!("history: saved {}", p.display());
    }
    path
}

pub fn add_async(img: Arc<Image>, source: &'static str) {
    run_async(move || {
        add(&img, source);
    });
}

pub fn copy_to(src: &Path, dir: &Path) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    let name = src.file_name()?.to_string_lossy().into_owned();
    let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, "png"));
    let dest = dir.join(names::unique_name(stem, ext, |n| dir.join(n).exists()));
    std::fs::copy(src, &dest).ok()?;
    Some(dest)
}
