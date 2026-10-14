//! File name generation from the `filename` template in config.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

pub fn now() -> DateTime {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    DateTime { year: t.wYear, month: t.wMonth, day: t.wDay, hour: t.wHour, minute: t.wMinute, second: t.wSecond }
}

/// Expands %Y %m %d %H %M %S and %% in `template`, then strips characters that are
/// invalid in Windows file names. Never returns an empty stem.
pub fn format_stem(template: &str, t: &DateTime) -> String {
    let mut out = String::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{:04}", t.year)),
            Some('m') => out.push_str(&format!("{:02}", t.month)),
            Some('d') => out.push_str(&format!("{:02}", t.day)),
            Some('H') => out.push_str(&format!("{:02}", t.hour)),
            Some('M') => out.push_str(&format!("{:02}", t.minute)),
            Some('S') => out.push_str(&format!("{:02}", t.second)),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    let cleaned: String = out
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).trim_start().to_string();
    if cleaned.is_empty() { "shelf".into() } else { cleaned }
}

/// Picks `stem.ext`, or `stem_2.ext`, `stem_3.ext`… — the first for which `exists` is false.
pub fn unique_name(stem: &str, ext: &str, exists: impl Fn(&str) -> bool) -> String {
    let first = format!("{stem}.{ext}");
    if !exists(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{stem}_{n}.{ext}"))
        .find(|n| !exists(n))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: DateTime = DateTime { year: 2026, month: 3, day: 7, hour: 9, minute: 5, second: 1 };

    #[test]
    fn default_template() {
        assert_eq!(format_stem("shelf_%Y-%m-%d_%H-%M-%S", &T), "shelf_2026-03-07_09-05-01");
    }

    #[test]
    fn unknown_and_escaped_percent() {
        assert_eq!(format_stem("a%%b%qc%", &T), "a%b%qc%");
    }

    #[test]
    fn invalid_chars_are_replaced() {
        assert_eq!(format_stem("%H:%M/x?", &T), "09_05_x_");
        assert_eq!(format_stem("name. ", &T), "name");
        assert_eq!(format_stem("", &T), "shelf");
        assert_eq!(format_stem("...", &T), "shelf");
    }

    #[test]
    fn uniqueness_suffix() {
        let taken = ["a.png", "a_2.png"];
        assert_eq!(unique_name("a", "png", |n| taken.contains(&n)), "a_3.png");
        assert_eq!(unique_name("b", "png", |n| taken.contains(&n)), "b.png");
    }
}
