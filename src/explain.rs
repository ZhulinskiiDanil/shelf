//! Pure helpers for the explanation tools on the ink layer (M6).

use crate::geom::Rect;
use std::time::{Duration, Instant};

pub const LASER_LIFE: Duration = Duration::from_millis(450);
pub const RING_LIFE: Duration = Duration::from_millis(450);
pub const KEY_LIFE: Duration = Duration::from_millis(2500);
pub const KEY_MAX: usize = 4;

/// Packed modifier bits for key display (same values as config::MOD_*).
pub const KM_ALT: u32 = 0x1;
pub const KM_CTRL: u32 = 0x2;
pub const KM_SHIFT: u32 = 0x4;
pub const KM_WIN: u32 = 0x8;

/// Human-readable combo for the key overlay, or None for keys we do not show.
/// Plain characters without Ctrl/Alt/Win are hidden on purpose: the overlay shows
/// shortcuts, not what is being typed (passwords during a screen share).
pub fn format_combo(vk: u32, mods: u32) -> Option<String> {
    let name: String = match vk {
        0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5 | 0x14 => return None, // modifiers, Caps Lock
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x21 => "PgUp".into(),
        0x22 => "PgDn".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "←".into(),
        0x26 => "↑".into(),
        0x27 => "→".into(),
        0x28 => "↓".into(),
        0x2C => "PrtScrn".into(),
        0x2D => "Ins".into(),
        0x2E => "Del".into(),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).unwrap().to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        _ => return None,
    };
    let is_char = matches!(vk, 0x30..=0x39 | 0x41..=0x5A | 0xBA..=0xC0 | 0xDB..=0xDE | 0x20);
    if is_char && mods & (KM_CTRL | KM_ALT | KM_WIN) == 0 {
        return None;
    }
    let mut s = String::new();
    for (bit, n) in [(KM_CTRL, "Ctrl"), (KM_ALT, "Alt"), (KM_SHIFT, "Shift"), (KM_WIN, "Win")] {
        if mods & bit != 0 {
            s.push_str(n);
            s.push('+');
        }
    }
    s.push_str(&name);
    Some(s)
}

/// Recent combos; repeats of the last one become "Ctrl+Z ×3".
#[derive(Default)]
pub struct KeyLog {
    pub entries: Vec<(String, u32, Instant)>,
}

impl KeyLog {
    pub fn push(&mut self, combo: String, now: Instant) {
        if let Some(last) = self.entries.last_mut() {
            if last.0 == combo {
                last.1 += 1;
                last.2 = now;
                return;
            }
        }
        self.entries.push((combo, 1, now));
        if self.entries.len() > KEY_MAX {
            self.entries.remove(0);
        }
    }
    pub fn prune(&mut self, now: Instant) {
        self.entries.retain(|e| now.duration_since(e.2) < KEY_LIFE);
    }
    pub fn label(e: &(String, u32, Instant)) -> String {
        if e.1 > 1 { format!("{} ×{}", e.0, e.1) } else { e.0.clone() }
    }
}

/// Source rect (in virtual-screen pixels) for a full-monitor zoom of `level` centred on
/// the cursor, clamped so it never leaves the monitor.
pub fn zoom_src(mon: &Rect, cursor: (i32, i32), level: f32) -> (f32, f32, f32, f32) {
    let level = level.max(1.0);
    let w = mon.w() as f32 / level;
    let h = mon.h() as f32 / level;
    let l = (cursor.0 as f32 - w / 2.0).clamp(mon.l as f32, mon.r as f32 - w);
    let t = (cursor.1 as f32 - h / 2.0).clamp(mon.t as f32, mon.b as f32 - h);
    (l, t, l + w, t + h)
}

/// Drops laser trail points older than LASER_LIFE.
pub fn trim_trail(trail: &mut Vec<(f32, f32, Instant)>, now: Instant) {
    let keep_from = trail.iter().position(|p| now.duration_since(p.2) < LASER_LIFE).unwrap_or(trail.len());
    trail.drain(..keep_from);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos() {
        assert_eq!(format_combo(0x43, KM_CTRL).as_deref(), Some("Ctrl+C"));
        assert_eq!(format_combo(0x53, KM_CTRL | KM_SHIFT).as_deref(), Some("Ctrl+Shift+S"));
        assert_eq!(format_combo(0x09, KM_ALT).as_deref(), Some("Alt+Tab"));
        assert_eq!(format_combo(0x0D, 0).as_deref(), Some("Enter"));
        assert_eq!(format_combo(0x74, 0).as_deref(), Some("F5"));
        // Typing is not shown.
        assert_eq!(format_combo(0x41, 0), None);
        assert_eq!(format_combo(0x41, KM_SHIFT), None);
        assert_eq!(format_combo(0x20, 0), None);
        assert_eq!(format_combo(0x11, KM_CTRL), None);
    }

    #[test]
    fn keylog_merges_repeats_and_caps_length() {
        let t = Instant::now();
        let mut k = KeyLog::default();
        k.push("Ctrl+Z".into(), t);
        k.push("Ctrl+Z".into(), t);
        assert_eq!(KeyLog::label(&k.entries[0]), "Ctrl+Z ×2");
        for i in 0..10 {
            k.push(format!("F{i}"), t);
        }
        assert_eq!(k.entries.len(), KEY_MAX);
        k.prune(t + KEY_LIFE);
        assert!(k.entries.is_empty());
    }

    #[test]
    fn zoom_is_clamped_to_monitor() {
        let m = Rect::new(-1920, 0, 0, 1080);
        assert_eq!(zoom_src(&m, (-960, 540), 2.0), (-1440.0, 270.0, -480.0, 810.0));
        // Near the top-left corner the source rect sticks to the monitor edge.
        assert_eq!(zoom_src(&m, (-1910, 5), 4.0), (-1920.0, 0.0, -1440.0, 270.0));
    }

    #[test]
    fn trail_trim() {
        let t = Instant::now();
        let mut tr = vec![(0.0, 0.0, t), (1.0, 1.0, t + Duration::from_millis(400))];
        trim_trail(&mut tr, t + Duration::from_millis(500));
        assert_eq!(tr.len(), 1);
        trim_trail(&mut tr, t + Duration::from_secs(5));
        assert!(tr.is_empty());
    }
}
