//! Pure part of the settings window: the field schema, value validation, writing
//! values back into config.toml without losing comments, and hotkey conflict checks.

use crate::config::{self, Hotkey};
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// Global hotkey (modifiers + key).
    Hotkey,
    /// Capture-mode single key (no modifiers).
    Key,
    /// Key or mouse side button held down (ink hold).
    Hold,
    Text,
    Path,
    Bool,
    Int,
    Float,
    Color,
}

pub struct Field {
    pub page: &'static str,
    pub section: &'static str, // "" = top level
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
}

const fn f(page: &'static str, section: &'static str, key: &'static str, label: &'static str, kind: Kind) -> Field {
    Field { page, section, key, label, kind }
}

pub const PAGES: [&str; 7] = ["Общие", "Хоткеи", "Захват", "Инструменты", "Рисование", "Диктовка", "Запись"];

pub const FIELDS: &[Field] = &[
    f("Общие", "", "autostart", "Запускать вместе с Windows", Kind::Bool),
    f("Общие", "", "log_level", "Уровень лога (error, warn, info, debug, trace)", Kind::Text),
    f("Общие", "history", "watch_clipboard", "Сохранять картинки из буфера обмена", Kind::Bool),
    f("Общие", "history", "max_items", "Лимит истории, снимков", Kind::Int),
    f("Общие", "history", "max_mb", "Лимит истории, МБ", Kind::Int),
    f("Хоткеи", "hotkeys", "capture", "Захват экрана", Kind::Hotkey),
    f("Хоткеи", "hotkeys", "shelf", "Полка", Kind::Hotkey),
    f("Хоткеи", "ink", "hold", "Рисовать (удерживать; или XButton1/XButton2)", Kind::Hold),
    f("Хоткеи", "ink", "sticky", "Рисование без удержания", Kind::Hotkey),
    f("Хоткеи", "ink", "clear", "Стереть рисунки", Kind::Hotkey),
    f("Хоткеи", "ink", "spotlight", "Прожектор", Kind::Hotkey),
    f("Хоткеи", "ink", "zoom", "Лупа", Kind::Hotkey),
    f("Хоткеи", "ink", "click_rings_toggle", "Подсветка кликов", Kind::Hotkey),
    f("Хоткеи", "ink", "keys_toggle", "Показ клавиш", Kind::Hotkey),
    f("Хоткеи", "board", "hotkey", "Доска", Kind::Hotkey),
    f("Хоткеи", "dictation", "hold", "Диктовка (удерживать)", Kind::Hotkey),
    f("Хоткеи", "record", "stop", "Остановить запись", Kind::Hotkey),
    f("Захват", "capture", "magnifier", "Лупа у курсора", Kind::Bool),
    f("Захват", "save", "dir", "Папка для Ctrl+S (пусто = Изображения\\Shelf)", Kind::Path),
    f("Захват", "save", "filename", "Шаблон имени (%Y %m %d %H %M %S)", Kind::Text),
    f("Инструменты", "tools", "pen", "Карандаш", Kind::Key),
    f("Инструменты", "tools", "line", "Линия", Kind::Key),
    f("Инструменты", "tools", "arrow", "Стрелка", Kind::Key),
    f("Инструменты", "tools", "rect", "Прямоугольник", Kind::Key),
    f("Инструменты", "tools", "ellipse", "Эллипс", Kind::Key),
    f("Инструменты", "tools", "marker", "Маркер", Kind::Key),
    f("Инструменты", "tools", "text", "Текст", Kind::Key),
    f("Инструменты", "tools", "blur", "Пикселизация", Kind::Key),
    f("Инструменты", "tools", "number", "Номера", Kind::Key),
    f("Рисование", "ink", "color", "Цвет чернил", Kind::Color),
    f("Рисование", "ink", "width", "Толщина", Kind::Float),
    f("Рисование", "ink", "fade_ms", "Таяние через, мс", Kind::Int),
    f("Рисование", "ink", "keep_modifier", "Не таять с модификатором (Shift, Ctrl, Alt)", Kind::Text),
    f("Рисование", "ink", "click_rings", "Подсветка кликов при запуске", Kind::Bool),
    f("Рисование", "ink", "show_keys", "Показ клавиш при запуске", Kind::Bool),
    f("Рисование", "board", "opacity", "Непрозрачность доски (0.1–1)", Kind::Float),
    f("Рисование", "board", "background", "Фон доски", Kind::Color),
    f("Диктовка", "dictation", "model", "Файл модели (пусто = models\\ggml-base.bin)", Kind::Path),
    f("Диктовка", "dictation", "language", "Язык (auto, ru, en)", Kind::Text),
    f("Диктовка", "dictation", "paste", "Сразу вставлять текст", Kind::Bool),
    f("Диктовка", "dictation", "unload_after_min", "Выгружать модель через, мин", Kind::Int),
    f("Запись", "record", "fps", "Кадров в секунду", Kind::Int),
    f("Запись", "record", "cursor", "Показывать курсор", Kind::Bool),
];

/// Current value of a field from the effective config, as text for the editor.
pub fn read(c: &config::Config, fl: &Field) -> String {
    let b = |v: bool| if v { "1" } else { "0" }.to_string();
    match (fl.section, fl.key) {
        ("", "autostart") => b(c.autostart),
        ("", "log_level") => c.log_level.clone(),
        ("history", "watch_clipboard") => b(c.history.watch_clipboard),
        ("history", "max_items") => c.history.max_items.to_string(),
        ("history", "max_mb") => c.history.max_mb.to_string(),
        ("hotkeys", "capture") => c.hotkeys.capture.clone(),
        ("hotkeys", "shelf") => c.hotkeys.shelf.clone(),
        ("ink", "hold") => c.ink.hold.clone(),
        ("ink", "sticky") => c.ink.sticky.clone(),
        ("ink", "clear") => c.ink.clear.clone(),
        ("ink", "spotlight") => c.ink.spotlight.clone(),
        ("ink", "zoom") => c.ink.zoom.clone(),
        ("ink", "click_rings_toggle") => c.ink.click_rings_toggle.clone(),
        ("ink", "keys_toggle") => c.ink.keys_toggle.clone(),
        ("ink", "color") => c.ink.color.clone(),
        ("ink", "width") => c.ink.width.to_string(),
        ("ink", "fade_ms") => c.ink.fade_ms.to_string(),
        ("ink", "keep_modifier") => c.ink.keep_modifier.clone(),
        ("ink", "click_rings") => b(c.ink.click_rings),
        ("ink", "show_keys") => b(c.ink.show_keys),
        ("board", "hotkey") => c.board.hotkey.clone(),
        ("board", "opacity") => c.board.opacity.to_string(),
        ("board", "background") => c.board.background.clone(),
        ("dictation", "hold") => c.dictation.hold.clone(),
        ("dictation", "model") => c.dictation.model.clone(),
        ("dictation", "language") => c.dictation.language.clone(),
        ("dictation", "paste") => b(c.dictation.paste),
        ("dictation", "unload_after_min") => c.dictation.unload_after_min.to_string(),
        ("record", "stop") => c.record.stop.clone(),
        ("record", "fps") => c.record.fps.to_string(),
        ("record", "cursor") => b(c.record.cursor),
        ("capture", "magnifier") => b(c.capture.magnifier),
        ("save", "dir") => c.save.dir.clone(),
        ("save", "filename") => c.save.filename.clone(),
        ("tools", k) => {
            let t = &c.tools;
            match k {
                "pen" => &t.pen,
                "line" => &t.line,
                "arrow" => &t.arrow,
                "rect" => &t.rect,
                "ellipse" => &t.ellipse,
                "marker" => &t.marker,
                "text" => &t.text,
                "blur" => &t.blur,
                _ => &t.number,
            }
            .clone()
        }
        _ => String::new(),
    }
}

/// Validates editor text for a field and converts it to a TOML value.
pub fn parse(fl: &Field, text: &str) -> Result<Item, String> {
    let t = text.trim();
    let err = |m: &str| Err(format!("{}: {m}", fl.label));
    match fl.kind {
        Kind::Bool => Ok(value(t == "1" || t.eq_ignore_ascii_case("true"))),
        Kind::Int => t.parse::<i64>().ok().filter(|v| *v >= 0).map(value).map_or_else(|| err("нужно целое число ≥ 0"), Ok),
        Kind::Float => t.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).map(value).map_or_else(|| err("нужно число"), Ok),
        Kind::Color => match config::parse_color(t) {
            Some(_) => Ok(value(format!("#{}", t.trim_start_matches('#').to_ascii_uppercase()))),
            None => err("цвет в виде #RRGGBB"),
        },
        Kind::Hotkey => match config::parse_hotkey(t) {
            Ok(_) => Ok(value(t)),
            Err(e) => err(&e),
        },
        Kind::Key => match config::parse_hotkey(t) {
            Ok(h) if h.mods == 0 => Ok(value(t)),
            Ok(_) => err("одна клавиша без модификаторов"),
            Err(e) => err(&e),
        },
        Kind::Hold => {
            let l = t.to_ascii_lowercase();
            if matches!(l.as_str(), "xbutton1" | "xbutton2" | "mouse4" | "mouse5") {
                return Ok(value(t));
            }
            match config::parse_hotkey(t) {
                Ok(h) if h.mods == 0 => Ok(value(t)),
                Ok(_) => err("одна клавиша без модификаторов"),
                Err(e) => err(&e),
            }
        }
        Kind::Text | Kind::Path => Ok(value(t)),
    }
}

/// Writes values into the TOML document, creating sections as needed and keeping comments.
pub fn apply(doc: &mut DocumentMut, updates: &[(&Field, Item)]) {
    for (fl, v) in updates {
        if fl.section.is_empty() {
            doc[fl.key] = v.clone();
        } else {
            if !doc.contains_table(fl.section) {
                doc[fl.section] = Item::Table(Table::new());
            }
            doc[fl.section][fl.key] = v.clone();
        }
    }
}

/// Human-readable conflicts among the given (label, hotkey) pairs and among capture keys.
pub fn conflicts(global: &[(&str, Option<Hotkey>)], capture_keys: &[(&str, u32)]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, (a, ha)) in global.iter().enumerate() {
        for (b, hb) in &global[i + 1..] {
            if let (Some(x), Some(y)) = (ha, hb) {
                if x == y {
                    out.push(format!("«{a}» и «{b}» — одно и то же сочетание"));
                }
            }
        }
    }
    for (i, (a, ka)) in capture_keys.iter().enumerate() {
        if *ka == 0 {
            continue;
        }
        if (0x31..=0x38).contains(ka) {
            out.push(format!("«{a}»: цифры 1–8 заняты выбором цвета"));
        }
        if matches!(*ka, 0x1B | 0x0D | 0x25..=0x28) {
            out.push(format!("«{a}»: клавиша занята в режиме захвата"));
        }
        for (b, kb) in &capture_keys[i + 1..] {
            if ka == kb {
                out.push(format!("«{a}» и «{b}» — одна клавиша"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(section: &str, key: &str) -> &'static Field {
        FIELDS.iter().find(|f| f.section == section && f.key == key).unwrap()
    }

    #[test]
    fn every_field_reads_a_value() {
        let c = config::Config::default();
        for fl in FIELDS {
            let v = read(&c, fl);
            if !matches!(fl.kind, Kind::Path) {
                assert!(!v.is_empty(), "{}.{} has no value", fl.section, fl.key);
            }
            // Defaults must pass validation.
            assert!(parse(fl, &v).is_ok(), "{}.{} = {v:?} rejected", fl.section, fl.key);
        }
        assert!(FIELDS.iter().all(|f| PAGES.contains(&f.page)));
    }

    #[test]
    fn validation() {
        assert!(parse(field("hotkeys", "shelf"), "Ctrl+Shift+").is_err());
        assert!(parse(field("tools", "pen"), "Ctrl+P").is_err());
        assert!(parse(field("ink", "hold"), "XButton2").is_ok());
        assert!(parse(field("history", "max_items"), "-3").is_err());
        assert_eq!(parse(field("board", "background"), "#1e2a3b").unwrap().as_str(), Some("#1E2A3B"));
        assert!(parse(field("board", "background"), "red").is_err());
    }

    #[test]
    fn apply_keeps_comments_and_creates_sections() {
        let mut doc: DocumentMut = "# my comment\nautostart = false\n\n[hotkeys]\n# keep me\nshelf = \"Alt+S\"\n".parse().unwrap();
        let ups = vec![
            (field("hotkeys", "shelf"), value("Ctrl+Alt+S")),
            (field("", "autostart"), value(true)),
            (field("record", "fps"), value(60i64)),
        ];
        apply(&mut doc, &ups);
        let s = doc.to_string();
        assert!(s.contains("# my comment") && s.contains("# keep me"));
        assert!(s.contains("shelf = \"Ctrl+Alt+S\""));
        assert!(s.contains("autostart = true"));
        let back = config::parse(&s).unwrap();
        assert_eq!(back.record.fps, 60);
    }

    #[test]
    fn conflict_detection() {
        let hk = |s: &str| config::parse_hotkey(s).ok();
        let c = conflicts(&[("Полка", hk("Ctrl+Alt+S")), ("Прожектор", hk("ctrl+alt+s")), ("Доска", hk("Ctrl+Alt+B"))], &[("Карандаш", b'P' as u32), ("Линия", b'P' as u32), ("Номера", b'3' as u32)]);
        assert_eq!(c.len(), 3, "{c:?}");
        let defaults = config::Config::default();
        assert!(global_conflicts(&defaults).is_empty(), "{:?}", global_conflicts(&defaults));
    }
}

/// Conflicts in a whole config (used by the settings window and at start-up).
pub fn global_conflicts(c: &config::Config) -> Vec<String> {
    let globals: Vec<(&str, Option<Hotkey>)> = FIELDS
        .iter()
        .filter(|f| f.kind == Kind::Hotkey)
        .map(|f| (f.label, config::parse_hotkey(&read(c, f)).ok()))
        .collect();
    let names = ["Карандаш", "Линия", "Стрелка", "Прямоугольник", "Эллипс", "Маркер", "Текст", "Пикселизация", "Номера"];
    let keys: Vec<(&str, u32)> = names.iter().copied().zip(c.tools.tool_keys()).collect();
    conflicts(&globals, &keys)
}
