//! M13: text clipboard history. Entries come from the clipboard-chip worker; the file is
//! encrypted with DPAPI (current user) and written off the UI thread.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub text: String,
    /// Unix seconds of the last copy (or use from the picker).
    pub time: u64,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub max_items: usize,
    pub max_age_days: u64,
    pub max_entry_bytes: usize,
}

// ! --- Pure model --- !

/// Adds `text` (or moves an existing identical entry to the top). Newest first.
pub fn upsert(list: &mut Vec<Entry>, text: &str, source: &str, now: u64) {
    if let Some(i) = list.iter().position(|e| e.text == text) {
        let mut e = list.remove(i);
        e.time = now;
        if !source.is_empty() {
            e.source = source.to_string();
        }
        list.insert(0, e);
    } else {
        list.insert(0, Entry { text: text.to_string(), time: now, source: source.to_string(), pinned: false });
    }
}

/// Drops unpinned entries beyond the count limit or older than the age limit.
pub fn prune(list: &mut Vec<Entry>, lim: &Limits, now: u64) {
    let max_age = lim.max_age_days.saturating_mul(86_400);
    let mut kept_unpinned = 0;
    list.retain(|e| {
        if e.pinned {
            return true;
        }
        let fresh = max_age == 0 || now.saturating_sub(e.time) <= max_age;
        let keep = fresh && kept_unpinned < lim.max_items;
        if keep {
            kept_unpinned += 1;
        }
        keep
    });
}

/// Case-insensitive search: every whitespace-separated word of `query` must occur.
pub fn matches(e: &Entry, query: &str) -> bool {
    let hay = e.text.to_lowercase();
    query.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
}

/// Order for lists: pinned first, then most recent.
pub fn sorted_view<'a>(list: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let mut v: Vec<&Entry> = list.iter().filter(|e| matches(e, query)).collect();
    v.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.time.cmp(&a.time)));
    v
}

// ! --- Templates --- !

/// Field names of `{name}` placeholders, in order of first appearance. `{{` and `}}` are
/// literal braces; braces with nothing or newlines inside are not fields.
pub fn template_fields(t: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in tokens(t) {
        if let Tok::Field(f) = tok {
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// Replaces fields with values (missing values become empty).
pub fn fill_template(t: &str, values: &[(String, String)]) -> String {
    tokens(t)
        .into_iter()
        .map(|tok| match tok {
            Tok::Text(s) => s,
            Tok::Field(f) => values.iter().find(|(k, _)| *k == f).map(|(_, v)| v.clone()).unwrap_or_default(),
        })
        .collect()
}

enum Tok {
    Text(String),
    Field(String),
}

fn tokens(t: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut text = String::new();
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '{' || c == '}') && chars.get(i + 1) == Some(&c) {
            text.push(c);
            i += 2;
            continue;
        }
        if c == '{' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == '}' || x == '{' || x == '\n') {
                let name: String = chars[i + 1..i + 1 + end].iter().collect();
                if chars[i + 1 + end] == '}' && !name.trim().is_empty() && name.chars().count() <= 40 {
                    out.push(Tok::Text(std::mem::take(&mut text)));
                    out.push(Tok::Field(name.trim().to_string()));
                    i += end + 2;
                    continue;
                }
            }
        }
        text.push(c);
        i += 1;
    }
    out.push(Tok::Text(text));
    out
}

// ! --- Store --- !

static LIST: Mutex<Option<Vec<Entry>>> = Mutex::new(None);
pub static PAUSED: AtomicBool = AtomicBool::new(false);
static SAVE_PENDING: AtomicBool = AtomicBool::new(false);

fn path() -> std::path::PathBuf {
    crate::config::app_dir().join("texthistory.bin")
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn limits() -> Limits {
    let c = crate::config::current().text_history;
    Limits { max_items: c.max_items.max(1), max_age_days: c.max_age_days, max_entry_bytes: c.max_entry_kb.saturating_mul(1024).max(256) }
}

fn with_list<R>(f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    let mut g = LIST.lock().unwrap();
    let list = g.get_or_insert_with(load);
    f(list)
}

fn load() -> Vec<Entry> {
    let Ok(enc) = std::fs::read(path()) else { return Vec::new() };
    match dpapi(&enc, false).and_then(|plain| serde_json::from_slice(&plain).ok()) {
        Some(v) => v,
        None => {
            log::warn!("text history: cannot decrypt (other user or corrupted), starting empty");
            Vec::new()
        }
    }
}

/// Coalesced write on the history worker thread.
fn schedule_save() {
    if SAVE_PENDING.swap(true, Ordering::Relaxed) {
        return;
    }
    crate::history::run_async(|| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        SAVE_PENDING.store(false, Ordering::Relaxed);
        let json = with_list(|l| serde_json::to_vec(l).unwrap_or_default());
        if let Some(enc) = dpapi(&json, true) {
            let p = path();
            let tmp = p.with_extension("bin.tmp");
            if std::fs::write(&tmp, enc).and_then(|_| std::fs::rename(&tmp, &p)).is_err() {
                log::error!("text history: write failed");
            }
        }
    });
}

/// Called by the clipboard worker for every non-private text copy.
pub fn record(text: &str, source: &str, truncated: bool) {
    let cfg = crate::config::current().text_history;
    if !cfg.enabled || PAUSED.load(Ordering::Relaxed) || text.trim().is_empty() {
        return;
    }
    if cfg.ignore_processes.iter().any(|p| p.trim().eq_ignore_ascii_case(source)) {
        return;
    }
    let lim = limits();
    if truncated || text.len() > lim.max_entry_bytes {
        log::debug!("text history: entry too large, skipped");
        return;
    }
    if matches!(crate::clipkind::classify(text, false), crate::clipkind::TextKind::Secret { .. }) {
        return;
    }
    let t = now();
    with_list(|l| {
        upsert(l, text, source, t);
        prune(l, &lim, t);
    });
    schedule_save();
}

pub fn view(query: &str) -> Vec<Entry> {
    with_list(|l| sorted_view(l, query).into_iter().cloned().collect())
}

pub fn touch(text: &str) {
    with_list(|l| upsert(l, text, "", now()));
    schedule_save();
}

pub fn set_pinned(text: &str, pinned: bool) {
    with_list(|l| {
        if let Some(e) = l.iter_mut().find(|e| e.text == text) {
            e.pinned = pinned;
        }
    });
    schedule_save();
}

pub fn delete(text: &str) {
    with_list(|l| l.retain(|e| e.text != text));
    schedule_save();
}

/// DPAPI (CurrentUser scope) encrypt / decrypt.
fn dpapi(data: &[u8], encrypt: bool) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::*;
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        let ok = if encrypt {
            CryptProtectData(&input, windows::core::w!("Shelf text history"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        ok.ok()?;
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(text: &str, time: u64, pinned: bool) -> Entry {
        Entry { text: text.into(), time, source: String::new(), pinned }
    }

    #[test]
    fn duplicates_move_to_top() {
        let mut l = Vec::new();
        upsert(&mut l, "a", "x.exe", 1);
        upsert(&mut l, "b", "y.exe", 2);
        upsert(&mut l, "a", "z.exe", 3);
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].text.as_str(), l[0].time, l[0].source.as_str()), ("a", 3, "z.exe"));
    }

    #[test]
    fn prune_keeps_pinned() {
        let lim = Limits { max_items: 2, max_age_days: 1, max_entry_bytes: 100 };
        let day = 86_400;
        let mut l = vec![e("new", 10 * day, false), e("pin-old", 0, true), e("mid", 10 * day - 10, false), e("third", 10 * day - 20, false), e("old", 0, false)];
        prune(&mut l, &lim, 10 * day);
        let names: Vec<&str> = l.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(names, vec!["new", "pin-old", "mid"]);
    }

    #[test]
    fn search_and_order() {
        let l = vec![e("Hello World", 5, false), e("pinned hello", 1, true), e("other", 9, false)];
        let v: Vec<&str> = sorted_view(&l, "HELLO").iter().map(|e| e.text.as_str()).collect();
        assert_eq!(v, vec!["pinned hello", "Hello World"]);
        assert_eq!(sorted_view(&l, "world hello").len(), 1);
        assert_eq!(sorted_view(&l, "").len(), 3);
    }

    #[test]
    fn templates() {
        let t = "Привет, {имя}! Заказ {order} готов. {имя}, ждём. {{не поле}} и {} и {\nx}";
        assert_eq!(template_fields(t), vec!["имя".to_string(), "order".to_string()]);
        let out = fill_template(t, &[("имя".into(), "Аня".into()), ("order".into(), "#42".into())]);
        assert_eq!(out, "Привет, Аня! Заказ #42 готов. Аня, ждём. {не поле} и {} и {\nx}");
        assert!(template_fields("no fields here").is_empty());
        assert_eq!(fill_template("{a}{b}", &[("a".into(), "1".into())]), "1");
    }
}
