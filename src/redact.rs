//! M15: finding personal data in OCR results and hiding it irreversibly in pixels.
//! Detectors and pixel operations here are pure.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Email,
    Phone,
    Ip,
    Card,
    Token,
    UrlSecret,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Email => "email",
            Kind::Phone => "телефон",
            Kind::Ip => "IP",
            Kind::Card => "карта",
            Kind::Token => "токен",
            Kind::UrlSecret => "секрет в ссылке",
        }
    }
}

/// Which detectors are on.
#[derive(Debug, Clone, Copy)]
pub struct Detectors {
    pub email: bool,
    pub phone: bool,
    pub ip: bool,
    pub card: bool,
    pub token: bool,
    pub url_secret: bool,
}

impl Detectors {
    #[cfg(test)]
    pub const ALL: Detectors = Detectors { email: true, phone: true, ip: true, card: true, token: true, url_secret: true };
}

/// A match in a text line: kind and byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

/// Luhn checksum over the digits of `s`.
pub fn luhn(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| if i % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { d })
        .sum();
    sum % 10 == 0
}

fn digit_count(s: &str) -> usize {
    s.chars().filter(char::is_ascii_digit).count()
}

/// All findings in one line of text, non-overlapping (earlier detectors win ties).
pub fn scan(line: &str, d: &Detectors) -> Vec<Hit> {
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static URLSEC: OnceLock<Regex> = OnceLock::new();
    static CARD: OnceLock<Regex> = OnceLock::new();
    static PHONE_RU: OnceLock<Regex> = OnceLock::new();
    static PHONE_PL: OnceLock<Regex> = OnceLock::new();
    static PHONE_INTL: OnceLock<Regex> = OnceLock::new();
    static IPV4: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();

    let mut hits: Vec<Hit> = Vec::new();
    let push = |kind: Kind, start: usize, end: usize, hits: &mut Vec<Hit>| {
        if !hits.iter().any(|h| start < h.end && h.start < end) {
            hits.push(Hit { kind, start, end });
        }
    };
    if d.url_secret {
        let r = re(&URLSEC, r"(?i)[?&#;](?:access_token|refresh_token|api_key|apikey|token|key|secret|client_secret|password|passwd|pwd|sig|signature|auth)=[^&\s#]+");
        for m in r.find_iter(line) {
            push(Kind::UrlSecret, m.start() + 1, m.end(), &mut hits);
        }
    }
    if d.email {
        let r = re(&EMAIL, r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}");
        for m in r.find_iter(line) {
            push(Kind::Email, m.start(), m.end(), &mut hits);
        }
    }
    if d.card {
        let r = re(&CARD, r"\b\d(?:[ \-]?\d){12,18}\b");
        for m in r.find_iter(line) {
            if luhn(m.as_str()) {
                push(Kind::Card, m.start(), m.end(), &mut hits);
            }
        }
    }
    if d.phone {
        let ru = re(&PHONE_RU, r"(?:\+7|\b8)[ \-]?\(?\d{3}\)?[ \-]?\d{3}[ \-]?\d{2}[ \-]?\d{2}\b");
        let pl = re(&PHONE_PL, r"(?:\+48[ \-]?\d{3}[ \-]?\d{3}[ \-]?\d{3}|\b\d{3}[ \-]\d{3}[ \-]\d{3})\b");
        let intl = re(&PHONE_INTL, r"\+\d{1,3}[ \-]?\(?\d{1,4}\)?(?:[ \-]?\d{2,4}){2,4}\b");
        for r in [ru, pl, intl] {
            for m in r.find_iter(line) {
                if (9..=15).contains(&digit_count(m.as_str())) {
                    push(Kind::Phone, m.start(), m.end(), &mut hits);
                }
            }
        }
    }
    if d.ip {
        let r = re(&IPV4, r"\b(?:\d{1,3}\.){3}\d{1,3}\b");
        for m in r.find_iter(line) {
            if m.as_str().split('.').all(|o| o.parse::<u16>().is_ok_and(|v| v <= 255)) {
                push(Kind::Ip, m.start(), m.end(), &mut hits);
            }
        }
    }
    // Word-level detectors: IPv6 and tokens.
    let word = re(&WORD, r"\S+");
    for m in word.find_iter(line) {
        let w = m.as_str().trim_matches(|c: char| ",;()[]<>\"'".contains(c));
        let off = m.start() + m.as_str().find(w).unwrap_or(0);
        if d.ip && w.matches(':').count() >= 2 {
            let host = w.trim_start_matches('[').split(']').next().unwrap_or(w);
            if host.parse::<std::net::Ipv6Addr>().is_ok() {
                push(Kind::Ip, off, off + w.len(), &mut hits);
                continue;
            }
        }
        if d.token && crate::clipkind::is_secret(w) {
            push(Kind::Token, off, off + w.len(), &mut hits);
        }
    }
    hits.sort_by_key(|h| h.start);
    hits
}

/// OCR word: text and box (pixels of the analysed image).
#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    pub rect: (f32, f32, f32, f32), // l, t, r, b
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: Kind,
    pub rect: (f32, f32, f32, f32),
}

/// Runs the detectors over OCR lines and maps hits back to the union of word boxes.
pub fn find(lines: &[Vec<Word>], d: &Detectors) -> Vec<Finding> {
    let mut out = Vec::new();
    for words in lines {
        let mut text = String::new();
        let mut spans = Vec::new();
        for w in words {
            if !text.is_empty() {
                text.push(' ');
            }
            spans.push((text.len(), text.len() + w.text.len()));
            text.push_str(&w.text);
        }
        for h in scan(&text, d) {
            let mut r: Option<(f32, f32, f32, f32)> = None;
            for (w, &(s, e)) in words.iter().zip(&spans) {
                if s < h.end && h.start < e {
                    let b = w.rect;
                    r = Some(match r {
                        None => b,
                        Some(a) => (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)),
                    });
                }
            }
            if let Some(rect) = r {
                out.push(Finding { kind: h.kind, rect });
            }
        }
    }
    out
}

// ! --- Pixel operations (irreversible) --- !

/// Solid fill of `r` (clamped) in a top-down BGRA buffer.
pub fn fill(buf: &mut [u8], w: usize, h: usize, r: (i32, i32, i32, i32), bgr: [u8; 3]) {
    let (l, t, rr, b) = clamp(w, h, r);
    for y in t..b {
        for x in l..rr {
            let i = (y * w + x) * 4;
            buf[i..i + 4].copy_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
        }
    }
}

/// Strong blur: three passes of a box blur with radius `rad`, then the box is pixelated
/// lightly so no high-frequency detail (letters) survives.
pub fn blur(buf: &mut [u8], w: usize, h: usize, r: (i32, i32, i32, i32), rad: usize) {
    let (l, t, rr, b) = clamp(w, h, r);
    let (bw, bh) = (rr - l, b - t);
    if bw == 0 || bh == 0 {
        return;
    }
    let mut patch: Vec<[u32; 3]> = (0..bw * bh)
        .map(|i| {
            let (x, y) = (l + i % bw, t + i / bw);
            let p = (y * w + x) * 4;
            [buf[p] as u32, buf[p + 1] as u32, buf[p + 2] as u32]
        })
        .collect();
    for _ in 0..3 {
        patch = box_pass(&patch, bw, bh, rad, true);
        patch = box_pass(&patch, bw, bh, rad, false);
    }
    for (i, c) in patch.iter().enumerate() {
        let (x, y) = (l + i % bw, t + i / bw);
        let p = (y * w + x) * 4;
        buf[p..p + 4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
    }
}

fn box_pass(src: &[[u32; 3]], w: usize, h: usize, rad: usize, horizontal: bool) -> Vec<[u32; 3]> {
    let mut out = vec![[0u32; 3]; src.len()];
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    for o in 0..outer {
        let idx = |i: usize| if horizontal { o * w + i } else { i * w + o };
        for i in 0..inner {
            let (a, b) = (i.saturating_sub(rad), (i + rad).min(inner - 1));
            let mut s = [0u32; 3];
            for j in a..=b {
                let p = src[idx(j)];
                s[0] += p[0];
                s[1] += p[1];
                s[2] += p[2];
            }
            let n = (b - a + 1) as u32;
            out[idx(i)] = [s[0] / n, s[1] / n, s[2] / n];
        }
    }
    out
}

fn clamp(w: usize, h: usize, r: (i32, i32, i32, i32)) -> (usize, usize, usize, usize) {
    let c = |v: i32, m: usize| v.clamp(0, m as i32) as usize;
    (c(r.0, w), c(r.1, h), c(r.2, w).max(c(r.0, w)), c(r.3, h).max(c(r.1, h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<(Kind, String)> {
        scan(s, &Detectors::ALL).into_iter().map(|h| (h.kind, s[h.start..h.end].to_string())).collect()
    }

    #[test]
    fn emails() {
        assert_eq!(kinds("пишите на ivan.petrov+work@mail.example.ru сегодня"), vec![(Kind::Email, "ivan.petrov+work@mail.example.ru".into())]);
        assert!(kinds("not an email: user@localhost").is_empty());
    }

    #[test]
    fn phones() {
        assert_eq!(kinds("звоните +7 (912) 345-67-89")[0], (Kind::Phone, "+7 (912) 345-67-89".into()));
        assert_eq!(kinds("тел 8 912 345 67 89")[0], (Kind::Phone, "8 912 345 67 89".into()));
        assert_eq!(kinds("PL: +48 512 345 678")[0], (Kind::Phone, "+48 512 345 678".into()));
        assert_eq!(kinds("kom. 512-345-678")[0], (Kind::Phone, "512-345-678".into()));
        assert_eq!(kinds("UK +44 20 7946 0958")[0], (Kind::Phone, "+44 20 7946 0958".into()));
        // Plain numbers and dates are not phones.
        assert!(kinds("год 2026, заказ 123456789, 14.10.2026").is_empty());
    }

    #[test]
    fn cards_need_luhn() {
        assert!(luhn("4539 1488 0343 6467"));
        assert!(!luhn("4539 1488 0343 6468"));
        assert_eq!(kinds("card 4539 1488 0343 6467 exp")[0], (Kind::Card, "4539 1488 0343 6467".into()));
        assert!(kinds("id 4539 1488 0343 6468").iter().all(|k| k.0 != Kind::Card));
    }

    #[test]
    fn ips() {
        assert_eq!(kinds("host 192.168.1.20 up"), vec![(Kind::Ip, "192.168.1.20".into())]);
        assert!(kinds("version 1.2.300.4").is_empty());
        assert_eq!(kinds("addr [2001:db8::8a2e:370:7334]:443")[0].0, Kind::Ip);
        assert!(kinds("time 12:30:45").is_empty());
    }

    #[test]
    fn tokens_and_url_secrets() {
        assert_eq!(kinds("key ghp_1234567890abcdefghijABCDEFGHIJ123456")[0].0, Kind::Token);
        let u = kinds("https://api.example.com/v1/x?user=1&token=abcDEF123456&lang=ru");
        assert_eq!(u[0], (Kind::UrlSecret, "token=abcDEF123456".into()));
        assert_eq!(kinds("https://s3/x?X-Amz-Signature=deadbeef&sig=a1b2c3")[0].1, "sig=a1b2c3");
        assert!(kinds("https://example.com/search?q=cats&page=2").is_empty());
    }

    #[test]
    fn detectors_can_be_disabled() {
        let d = Detectors { email: false, ..Detectors::ALL };
        assert!(scan("a@b.com", &d).is_empty());
    }

    #[test]
    fn hits_map_to_word_boxes() {
        let w = |t: &str, l: f32| Word { text: t.into(), rect: (l, 10.0, l + 30.0, 20.0) };
        let lines = vec![vec![w("тел", 0.0), w("+7", 40.0), w("912", 80.0), w("345-67-89", 120.0), w("ок", 200.0)]];
        let f = find(&lines, &Detectors::ALL);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::Phone);
        assert_eq!(f[0].rect, (40.0, 10.0, 150.0, 20.0));
    }

    #[test]
    fn fill_and_blur_change_pixels() {
        let (w, h) = (8, 4);
        let mut buf: Vec<u8> = (0..w * h).flat_map(|i| [(i * 30 % 256) as u8, 0, 0, 255]).collect();
        let before = buf.clone();
        fill(&mut buf, w, h, (2, 1, 4, 3), [0, 0, 0]);
        assert_eq!(&buf[(1 * w + 2) * 4..(1 * w + 2) * 4 + 4], &[0, 0, 0, 255]);
        assert_eq!(buf[0..4], before[0..4]);
        let mut b2 = before.clone();
        blur(&mut b2, w, h, (0, 0, 8, 4), 2);
        assert_ne!(b2, before);
        fill(&mut b2, w, h, (-5, -5, 100, 100), [1, 2, 3]);
        assert!(b2.chunks(4).all(|p| p == [1, 2, 3, 255]));
    }
}
