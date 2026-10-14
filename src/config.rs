use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub log_level: String,
    pub autostart: bool,
    pub hotkeys: Hotkeys,
    pub capture: Capture,
    pub save: Save,
    pub tools: Tools,
}

/// Single keys that pick markup tools in capture mode (no modifiers).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tools {
    pub pen: String,
    pub line: String,
    pub arrow: String,
    pub rect: String,
    pub ellipse: String,
    pub marker: String,
    pub text: String,
    pub blur: String,
    pub number: String,
}

impl Default for Tools {
    fn default() -> Self {
        let s = |k: &str| k.to_string();
        Self { pen: s("P"), line: s("L"), arrow: s("A"), rect: s("R"), ellipse: s("E"), marker: s("M"), text: s("T"), blur: s("B"), number: s("N") }
    }
}

impl Tools {
    /// Virtual-key codes in `annot::TOOLS` order; 0 for unparsable entries.
    pub fn tool_keys(&self) -> [u32; 9] {
        [&self.pen, &self.line, &self.arrow, &self.rect, &self.ellipse, &self.marker, &self.text, &self.blur, &self.number]
            .map(|k| parse_hotkey(k).ok().filter(|h| h.mods == 0).map(|h| h.vk).unwrap_or(0))
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Save {
    /// Folder for Ctrl+S; empty means Pictures\Shelf.
    pub dir: String,
    /// File name template: %Y %m %d %H %M %S.
    pub filename: String,
}

impl Default for Save {
    fn default() -> Self {
        Self { dir: String::new(), filename: "shelf_%Y-%m-%d_%H-%M-%S".into() }
    }
}

impl Config {
    pub fn save_dir(&self) -> PathBuf {
        if !self.save.dir.trim().is_empty() {
            return PathBuf::from(self.save.dir.trim());
        }
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(app_dir);
        home.join("Pictures").join("Shelf")
    }
}

static CURRENT: std::sync::RwLock<Option<Config>> = std::sync::RwLock::new(None);

pub fn set_current(c: &Config) {
    *CURRENT.write().unwrap() = Some(c.clone());
}

pub fn current() -> Config {
    CURRENT.read().unwrap().clone().unwrap_or_default()
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Capture {
    pub magnifier: bool,
}

impl Default for Capture {
    fn default() -> Self {
        Self { magnifier: true }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hotkeys {
    pub capture: String,
    pub shelf: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { log_level: "info".into(), autostart: false, hotkeys: Hotkeys::default(), capture: Capture::default(), save: Save::default(), tools: Tools::default() }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self { capture: "PrintScreen".into(), shelf: "Ctrl+Shift+Space".into() }
    }
}

pub const DEFAULT_TOML: &str = r#"# Shelf config. Changes are applied on save.

# trace | debug | info | warn | error
log_level = "info"

# Start Shelf with Windows
autostart = false

[hotkeys]
capture = "PrintScreen"
shelf = "Ctrl+Shift+Space"

[capture]
# Pixel magnifier next to the cursor
magnifier = true

[save]
# Folder for Ctrl+S (empty = Pictures\Shelf)
dir = ""
# File name template: %Y %m %d %H %M %S
filename = "shelf_%Y-%m-%d_%H-%M-%S"
"#;

pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Shelf")
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.toml")
}

pub fn parse(text: &str) -> Result<Config, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// Loads config, creating the default file on first run. Falls back to defaults on parse error.
pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).unwrap_or_else(|e| {
            log::error!("config parse error, using defaults: {e}");
            Config::default()
        }),
        Err(_) => {
            let _ = std::fs::create_dir_all(app_dir());
            let _ = std::fs::write(&path, DEFAULT_TOML);
            Config::default()
        }
    }
}

// ! --- Hotkey parsing --- !

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

/// Parses strings like "Ctrl+Shift+Space", "PrintScreen", "Alt+F1", "Win+S".
pub fn parse_hotkey(s: &str) -> Result<Hotkey, String> {
    let mut mods = 0;
    let mut vk = None;
    for part in s.split('+').map(str::trim) {
        if part.is_empty() {
            return Err(format!("empty key in '{s}'"));
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "alt" => mods |= MOD_ALT,
            "win" | "super" => mods |= MOD_WIN,
            key => {
                if vk.is_some() {
                    return Err(format!("more than one key in '{s}'"));
                }
                vk = Some(key_vk(key).ok_or_else(|| format!("unknown key '{part}'"))?);
            }
        }
    }
    vk.map(|vk| Hotkey { mods, vk }).ok_or_else(|| format!("no key in '{s}'"))
}

fn key_vk(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_alphanumeric()) {
        return Some(b[0].to_ascii_uppercase() as u32);
    }
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    Some(match key {
        "printscreen" | "prtscrn" | "prtsc" | "snapshot" => 0x2C,
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "tab" => 0x09,
        "capslock" => 0x14,
        "insert" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        "`" | "backtick" => 0xC0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_matches_defaults() {
        assert_eq!(parse(DEFAULT_TOML).unwrap(), Config::default());
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let c = parse("[hotkeys]\nshelf = \"Alt+S\"").unwrap();
        assert_eq!(c.hotkeys.shelf, "Alt+S");
        assert_eq!(c.hotkeys.capture, "PrintScreen");
        assert_eq!(c.log_level, "info");
    }

    #[test]
    fn bad_config_is_error() {
        assert!(parse("autostart = \"yes\"").is_err());
    }

    #[test]
    fn hotkeys() {
        assert_eq!(parse_hotkey("PrintScreen").unwrap(), Hotkey { mods: 0, vk: 0x2C });
        assert_eq!(
            parse_hotkey("ctrl + shift + Space").unwrap(),
            Hotkey { mods: MOD_CONTROL | MOD_SHIFT, vk: 0x20 }
        );
        assert_eq!(parse_hotkey("Alt+F12").unwrap(), Hotkey { mods: MOD_ALT, vk: 0x7B });
        assert_eq!(parse_hotkey("Win+s").unwrap(), Hotkey { mods: MOD_WIN, vk: b'S' as u32 });
        assert!(parse_hotkey("Ctrl+").is_err());
        assert!(parse_hotkey("Ctrl+Shift").is_err());
        assert!(parse_hotkey("A+B").is_err());
        assert!(parse_hotkey("F25").is_err());
    }
}
