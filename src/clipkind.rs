//! Pure classification of copied text for the clipboard chip (M12): colors, paths,
//! URLs, JSON, code, plain text, and secret detection/masking.

/// Text longer than this is not analysed as a whole (only previewed and counted).
pub const TEXT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum TextKind {
    Color { rgba: [u8; 4], text: String },
    /// Candidate path(s); existence is checked by the caller (I/O).
    Paths(Vec<String>),
    Url { domain: String, rest: String },
    Json { keys: Vec<String>, array_len: Option<usize> },
    Code { lines: Vec<String>, total_lines: usize },
    Text { preview: String, chars: usize, lines: usize, truncated: bool },
    Secret { masked: String },
}

/// Classifies `text`. `truncated` means only the first TEXT_LIMIT bytes were read.
pub fn classify(text: &str, truncated: bool) -> TextKind {
    let t = text.trim();
    if t.is_empty() {
        return plain(text, truncated);
    }
    let single_line = !t.contains('\n');
    if single_line {
        if let Some(rgba) = parse_color(t) {
            return TextKind::Color { rgba, text: t.to_string() };
        }
        if let Some((domain, rest)) = parse_url(t) {
            return TextKind::Url { domain, rest };
        }
    }
    let paths: Vec<String> = t.lines().map(str::trim).filter(|l| !l.is_empty()).map(unquote).collect();
    if !paths.is_empty() && paths.len() <= 50 && paths.iter().all(|p| looks_like_path(p)) {
        return TextKind::Paths(paths);
    }
    if !truncated && (t.starts_with('{') || t.starts_with('[')) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            return match v {
                serde_json::Value::Object(m) => TextKind::Json { keys: m.keys().take(4).cloned().collect(), array_len: None },
                serde_json::Value::Array(a) => TextKind::Json { keys: Vec::new(), array_len: Some(a.len()) },
                _ => plain(text, truncated),
            };
        }
    }
    if single_line && is_secret(t) {
        return TextKind::Secret { masked: mask(t) };
    }
    if looks_like_code(t) {
        let lines: Vec<&str> = t.lines().collect();
        return TextKind::Code { lines: lines.iter().take(3).map(|l| clip_chars(&l.replace('\t', "    "), 70)).collect(), total_lines: lines.len() };
    }
    plain(text, truncated)
}

/// Plain-text description regardless of what the text looks like.
pub fn classify_plain(text: &str, truncated: bool) -> TextKind {
    plain(text, truncated)
}

fn plain(text: &str, truncated: bool) -> TextKind {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    TextKind::Text { preview: clip_chars(&flat, 60), chars: text.chars().count(), lines: text.lines().count().max(1), truncated }
}

pub fn clip_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).or_else(|| s.strip_prefix('\'').and_then(|x| x.strip_suffix('\''))).unwrap_or(s);
    s.to_string()
}

/// Windows absolute paths: `C:\...`, `C:/...`, UNC `\\server\share`.
pub fn looks_like_path(s: &str) -> bool {
    let b = s.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    let unc = s.starts_with("\\\\") && s.len() > 2;
    (drive || unc) && !s.contains(['<', '>', '|', '"', '*', '?', '\n'])
}

// ! --- Colors --- !

pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_end_matches(';');
    if let Some(h) = s.strip_prefix('#') {
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let d = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
        let x = |v: u8| v * 17;
        return match h.len() {
            3 => Some([x(d(0, 1)?), x(d(1, 1)?), x(d(2, 1)?), 255]),
            4 => Some([x(d(0, 1)?), x(d(1, 1)?), x(d(2, 1)?), x(d(3, 1)?)]),
            6 => Some([d(0, 2)?, d(2, 2)?, d(4, 2)?, 255]),
            8 => Some([d(0, 2)?, d(2, 2)?, d(4, 2)?, d(6, 2)?]),
            _ => None,
        };
    }
    let lower = s.to_ascii_lowercase();
    let (name, args) = lower.split_once('(')?;
    let args = args.strip_suffix(')')?;
    let parts: Vec<&str> = args.split([',', ' ', '/']).map(str::trim).filter(|p| !p.is_empty()).collect();
    let alpha = |p: Option<&&str>| -> Option<u8> {
        match p {
            None => Some(255),
            Some(a) => {
                let v = if let Some(pc) = a.strip_suffix('%') { pc.parse::<f32>().ok()? / 100.0 } else { a.parse::<f32>().ok()? };
                (0.0..=1.0).contains(&v).then(|| (v * 255.0).round() as u8)
            }
        }
    };
    match name.trim() {
        "rgb" | "rgba" if parts.len() == 3 || parts.len() == 4 => {
            let ch = |p: &str| -> Option<u8> {
                let v = if let Some(pc) = p.strip_suffix('%') { pc.parse::<f32>().ok()? * 2.55 } else { p.parse::<f32>().ok()? };
                (0.0..=255.0).contains(&v).then(|| v.round() as u8)
            };
            Some([ch(parts[0])?, ch(parts[1])?, ch(parts[2])?, alpha(parts.get(3))?])
        }
        "hsl" | "hsla" if parts.len() == 3 || parts.len() == 4 => {
            let h = parts[0].trim_end_matches("deg").parse::<f32>().ok()?;
            let sat = parts[1].strip_suffix('%')?.parse::<f32>().ok()? / 100.0;
            let l = parts[2].strip_suffix('%')?.parse::<f32>().ok()? / 100.0;
            if !(0.0..=1.0).contains(&sat) || !(0.0..=1.0).contains(&l) {
                return None;
            }
            let [r, g, b] = hsl_to_rgb(h, sat, l);
            Some([r, g, b, alpha(parts.get(3))?])
        }
        _ => None,
    }
}

pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [((r + m) * 255.0).round() as u8, ((g + m) * 255.0).round() as u8, ((b + m) * 255.0).round() as u8]
}

// ! --- URLs --- !

pub fn parse_url(s: &str) -> Option<(String, String)> {
    if s.contains(char::is_whitespace) {
        return None;
    }
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).or_else(|| s.strip_prefix("ftp://")).or_else(|| s.starts_with("www.").then_some(s))?;
    let (host, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.rsplit('@').next().unwrap_or(host);
    let domain = host.strip_prefix("www.").unwrap_or(host);
    if domain.is_empty() || !domain.contains(['.', ':']) && domain != "localhost" {
        return None;
    }
    let path = if path == "/" { "" } else { path };
    Some((domain.to_string(), clip_chars(path, 48)))
}

// ! --- Code --- !

fn looks_like_code(t: &str) -> bool {
    let lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return false;
    }
    let indented = lines.iter().filter(|l| l.starts_with("    ") || l.starts_with('\t')).count();
    let syntax = lines
        .iter()
        .filter(|l| {
            let l = l.trim();
            l.ends_with(['{', '}', ';', ')', ':']) && !l.ends_with(". :")
                || ["fn ", "def ", "let ", "const ", "function ", "class ", "import ", "return ", "#include", "pub ", "if (", "for ("].iter().any(|k| l.starts_with(k))
                || l.contains("=>")
                || l.contains(" = ") && l.ends_with(';')
        })
        .count();
    syntax * 2 >= lines.len() || indented >= 1 && syntax >= 1
}

// ! --- Secrets --- !

const TOKEN_PREFIXES: &[&str] = &["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", "sk-", "sk_live_", "sk_test_", "rk_live_", "xox", "AKIA", "ASIA", "AIza", "npm_", "pypi-", "hf_"];

pub fn shannon_entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts.values().map(|&c| {
        let p = c as f64 / n;
        -p * p.log2()
    }).sum()
}

fn is_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts.iter().all(|p| p.len() >= 4 && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '='))
}

/// A single token that looks like a credential.
pub fn is_secret(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    if is_jwt(s) {
        return true;
    }
    if s.len() >= 12 && TOKEN_PREFIXES.iter().any(|p| s.starts_with(p)) {
        return true;
    }
    // Generic: long, mixed letters and digits, high entropy, not a path/URL/identifier-like word.
    let letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    s.len() >= 24 && letters >= 4 && digits >= 2 && !s.contains(['/', '\\', '{', '}', '"', ':', ',']) && shannon_entropy(s) >= 3.8
}

/// First 4 characters visible, the rest as dots (capped so length is not leaked exactly).
pub fn mask(s: &str) -> String {
    let head: String = s.chars().take(4).collect();
    let rest = s.chars().count().saturating_sub(4).min(16);
    head + &"•".repeat(rest.max(4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some([255, 255, 255, 255]));
        assert_eq!(parse_color("#1E88E5"), Some([0x1E, 0x88, 0xE5, 255]));
        assert_eq!(parse_color("#1E88E580"), Some([0x1E, 0x88, 0xE5, 0x80]));
        assert_eq!(parse_color("rgb(255, 0, 10)"), Some([255, 0, 10, 255]));
        assert_eq!(parse_color("rgba(0,0,0,0.5)"), Some([0, 0, 0, 128]));
        assert_eq!(parse_color("rgb(0 128 255 / 50%)"), Some([0, 128, 255, 128]));
        assert_eq!(parse_color("hsl(0, 100%, 50%)"), Some([255, 0, 0, 255]));
        assert_eq!(parse_color("hsl(120deg 100% 25%)"), Some([0, 128, 0, 255]));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#ggg"), None);
        assert_eq!(parse_color("rgb(300,0,0)"), None);
        assert!(matches!(classify("  #ff8800;\n", false), TextKind::Color { .. }));
    }

    #[test]
    fn paths() {
        assert_eq!(classify("C:\\Users\\me\\a.png", false), TextKind::Paths(vec!["C:\\Users\\me\\a.png".into()]));
        assert_eq!(classify("\"D:/Dev/shelf\"", false), TextKind::Paths(vec!["D:/Dev/shelf".into()]));
        assert_eq!(classify("\\\\server\\share\\x.txt\nC:\\b.txt", false), TextKind::Paths(vec!["\\\\server\\share\\x.txt".into(), "C:\\b.txt".into()]));
        assert!(matches!(classify("see C:\\a.txt for details", false), TextKind::Text { .. }));
        assert!(!looks_like_path("C:"));
        assert!(!looks_like_path("/usr/bin"));
    }

    #[test]
    fn urls() {
        assert_eq!(classify("https://github.com/ZhulinskiiDanil/shelf/releases", false), TextKind::Url { domain: "github.com".into(), rest: "/ZhulinskiiDanil/shelf/releases".into() });
        assert_eq!(parse_url("http://www.example.org/"), Some(("example.org".into(), "".into())));
        assert_eq!(parse_url("www.rust-lang.org/learn?x=1").unwrap().0, "rust-lang.org");
        assert_eq!(parse_url("http://localhost:8080/api").unwrap().0, "localhost:8080");
        let long = format!("https://a.io/{}", "x".repeat(100));
        assert_eq!(parse_url(&long).unwrap().1.chars().count(), 48);
        assert_eq!(parse_url("https://exa mple.com"), None);
        assert_eq!(parse_url("example.com"), None);
    }

    #[test]
    fn json() {
        assert_eq!(classify(r#"{"name": "shelf", "version": 1, "a": [], "b": {}, "c": null}"#, false), TextKind::Json { keys: vec!["name".into(), "version".into(), "a".into(), "b".into()], array_len: None });
        assert_eq!(classify("[1, 2, 3]", false), TextKind::Json { keys: vec![], array_len: Some(3) });
        assert!(matches!(classify(r#"{"broken": }"#, false), TextKind::Text { .. }));
        // Truncated text cannot be validated as JSON.
        assert!(matches!(classify(r#"{"a": 1}"#, true), TextKind::Text { truncated: true, .. }));
    }

    #[test]
    fn code_and_text() {
        let code = "fn main() {\n    println!(\"hi\");\n}\n";
        assert!(matches!(classify(code, false), TextKind::Code { total_lines: 3, .. }));
        let prose = "Привет! Это обычный текст.\nВторая строка без кода и скобок";
        match classify(prose, false) {
            TextKind::Text { lines, chars, .. } => {
                assert_eq!(lines, 2);
                assert_eq!(chars, prose.chars().count());
            }
            k => panic!("{k:?}"),
        }
        match classify(&"слово ".repeat(40), false) {
            TextKind::Text { preview, .. } => assert_eq!(preview.chars().count(), 60),
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn secrets() {
        assert!(is_secret("ghp_1234567890abcdefghijABCDEFGHIJ123456"));
        assert!(is_secret("sk-proj-abcdefghijklmnop"));
        assert!(is_secret("xoxb-123456789012-abcdefghij"));
        assert!(is_secret("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert!(is_secret("Zq8vLp2Xr7Tn4Kw9Bm3Yc6Hd1Fg5Js0Q"));
        assert!(!is_secret("hello"));
        assert!(!is_secret("ProcessWindowStyleExtended"));
        assert!(!is_secret("2026-10-14T03:42:00.000000Z"));
        assert!(!is_secret("this is a sentence with sk- in it"));
        assert!(!is_secret(r#"{"name":"shelf","version":"1.0.0","n":12345}"#));
        assert!(matches!(classify(r#"{"name":"shelf","version":"1.0.0","offline":true}"#, false), TextKind::Json { .. }));
        assert_eq!(mask("ghp_ABCDEFGH"), "ghp_••••••••");
        assert_eq!(mask("abcdefghijklmnopqrstuvwxyz0123456789").chars().count(), 20);
        assert!(matches!(classify("ghp_1234567890abcdefghijABCDEFGHIJ123456", false), TextKind::Secret { .. }));
    }
}
