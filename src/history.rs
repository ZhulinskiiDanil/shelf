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
        idx.push(Entry { file: name.clone(), created, pinned: false, source: source.into(), bytes: png.len() as u64, w: img.w, h: img.h });
        write_thumb(&name, img);
        let (max_items, max_bytes) = *LIMITS.lock().unwrap();
        for victim in prune_victims(idx, max_items, max_bytes) {
            let e = idx.remove(victim);
            remove_files(&e.file);
        }
        save_index(idx);
        notify();
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

// ! --- Limits --- !

static LIMITS: Mutex<(usize, u64)> = Mutex::new((usize::MAX, u64::MAX));

pub fn set_limits(max_items: usize, max_mb: u64) {
    *LIMITS.lock().unwrap() = (max_items.max(1), max_mb.saturating_mul(1024 * 1024).max(1));
}

/// Indices to delete (descending, so they can be removed in order) to bring the
/// unpinned history within limits. Oldest unpinned entries go first; pinned never.
pub fn prune_victims(idx: &[Entry], max_items: usize, max_bytes: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..idx.len()).filter(|&i| !idx[i].pinned).collect();
    order.sort_by_key(|&i| (idx[i].created, i));
    let mut count = idx.len();
    let mut bytes: u64 = idx.iter().map(|e| e.bytes).sum();
    let mut out = Vec::new();
    for i in order {
        if count <= max_items && bytes <= max_bytes {
            break;
        }
        count -= 1;
        bytes -= idx[i].bytes;
        out.push(i);
    }
    out.sort_unstable_by(|a, b| b.cmp(a));
    out
}

// ! --- Thumbnails and shelf operations --- !

pub const THUMB_W: u32 = 320;
pub const THUMB_H: u32 = 200;

fn thumb_path(file: &str) -> PathBuf {
    dir().join("thumbs").join(format!("{file}.thumb"))
}

fn write_thumb(file: &str, img: &Image) {
    let t = crate::imageio::thumbnail(img, THUMB_W, THUMB_H);
    let _ = std::fs::create_dir_all(dir().join("thumbs"));
    let mut b = Vec::with_capacity(8 + t.bgra.len());
    b.extend_from_slice(&t.w.to_le_bytes());
    b.extend_from_slice(&t.h.to_le_bytes());
    b.extend_from_slice(&t.bgra);
    let _ = std::fs::write(thumb_path(file), b);
}

/// Thumbnail for a history file; generated from the PNG if missing.
pub fn load_thumb(file: &str) -> Option<Image> {
    if let Ok(b) = std::fs::read(thumb_path(file)) {
        if b.len() >= 8 {
            let w = u32::from_le_bytes(b[0..4].try_into().unwrap());
            let h = u32::from_le_bytes(b[4..8].try_into().unwrap());
            if b.len() == 8 + (w * h * 4) as usize {
                return Some(Image { w, h, bgra: b[8..].to_vec() });
            }
        }
    }
    let img = crate::imageio::decode_png(&std::fs::read(dir().join(file)).ok()?)?;
    write_thumb(file, &img);
    load_thumb_mem(&img)
}

fn load_thumb_mem(img: &Image) -> Option<Image> {
    Some(crate::imageio::thumbnail(img, THUMB_W, THUMB_H))
}

fn remove_files(file: &str) {
    let _ = std::fs::remove_file(dir().join(file));
    let _ = std::fs::remove_file(thumb_path(file));
}

/// Newest first; drops entries whose file is gone.
pub fn list() -> Vec<Entry> {
    with_index(|idx| {
        let before = idx.len();
        idx.retain(|e| dir().join(&e.file).exists());
        if idx.len() != before {
            save_index(idx);
        }
        let mut v = idx.clone();
        v.reverse();
        v
    })
}

pub fn delete(files: &[String]) {
    with_index(|idx| {
        idx.retain(|e| {
            let gone = files.contains(&e.file);
            if gone {
                remove_files(&e.file);
            }
            !gone
        });
        save_index(idx);
    });
    notify();
}

pub fn set_pinned(files: &[String], pinned: bool) {
    with_index(|idx| {
        for e in idx.iter_mut().filter(|e| files.contains(&e.file)) {
            e.pinned = pinned;
        }
        save_index(idx);
    });
    notify();
}

pub fn path_of(file: &str) -> PathBuf {
    dir().join(file)
}

/// Optional callback (e.g. shelf window refresh) after history changes; called from any thread.
static ON_CHANGE: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

pub fn on_change(f: impl Fn() + Send + 'static) {
    *ON_CHANGE.lock().unwrap() = Some(Box::new(f));
}

fn notify() {
    if let Some(f) = ON_CHANGE.lock().unwrap().as_ref() {
        f();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(created: u64, bytes: u64, pinned: bool) -> Entry {
        Entry { file: format!("{created}.png"), created, pinned, source: String::new(), bytes, w: 1, h: 1 }
    }

    #[test]
    fn prune_by_count_skips_pinned() {
        let idx = vec![e(1, 10, true), e(2, 10, false), e(3, 10, false), e(4, 10, false)];
        assert_eq!(prune_victims(&idx, 2, u64::MAX), vec![2, 1]);
    }

    #[test]
    fn prune_by_size_oldest_first() {
        let idx = vec![e(5, 50, false), e(1, 50, false), e(9, 50, false)];
        assert_eq!(prune_victims(&idx, 100, 100), vec![1]);
    }

    #[test]
    fn prune_cannot_go_below_pinned() {
        let idx = vec![e(1, 100, true), e(2, 100, true)];
        assert!(prune_victims(&idx, 1, 10).is_empty());
    }
}

/// Moves an already encoded file (e.g. a recording) into history with a thumbnail.
pub fn add_file(src: &Path, ext: &str, thumb: &Image, source: &str) -> Option<PathBuf> {
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let stem = names::format_stem(&TEMPLATE.lock().unwrap(), &names::now());
    let path = with_index(|idx| {
        let name = names::unique_name(&stem, ext, |n| d.join(n).exists());
        let path = d.join(&name);
        if std::fs::rename(src, &path).is_err() {
            std::fs::copy(src, &path).ok()?;
            let _ = std::fs::remove_file(src);
        }
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        idx.push(Entry { file: name.clone(), created, pinned: false, source: source.into(), bytes, w: thumb.w, h: thumb.h });
        write_thumb(&name, thumb);
        save_index(idx);
        Some(path)
    });
    notify();
    path
}
