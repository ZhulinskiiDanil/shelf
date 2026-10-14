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
    pub history: History,
    pub ink: Ink,
    pub board: Board,
    pub dictation: Dictation,
    pub record: Record,
    pub clipboard_chip: ClipboardChip,
    pub text_history: TextHistory,
    pub scroll_capture: ScrollCapture,
    pub redaction: Redaction,
}

/// M15: check a capture for personal data before sending it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Redaction {
    /// Key in capture mode (and `R` on the shelf).
    pub key: String,
    /// "fill" (solid), "pixelate" or "blur".
    pub style: String,
    /// Margin around each found word, px.
    pub padding: u32,
    /// Quietly check every Ctrl+C capture afterwards and offer to hide what was found.
    pub background: bool,
    /// Replaces the clipboard image with the hidden version after a background warning.
    pub hide_hotkey: String,
    pub email: bool,
    pub phone: bool,
    pub ip: bool,
    pub card: bool,
    pub token: bool,
    pub url_secret: bool,
}

impl Default for Redaction {
    fn default() -> Self {
        Self {
            key: "Ctrl+P".into(),
            style: "fill".into(),
            padding: 3,
            background: false,
            hide_hotkey: "Ctrl+Alt+H".into(),
            email: true,
            phone: true,
            ip: true,
            card: true,
            token: true,
            url_secret: true,
        }
    }
}

impl Redaction {
    pub fn detectors(&self) -> crate::redact::Detectors {
        crate::redact::Detectors { email: self.email, phone: self.phone, ip: self.ip, card: self.card, token: self.token, url_secret: self.url_secret }
    }
}

/// M14: scrolling (long) screenshots.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScrollCapture {
    /// Key in capture mode that starts a scrolling capture of the selection.
    pub key: String,
    /// "auto" (Shelf scrolls with the wheel) or "manual" (you scroll, Enter finishes).
    pub mode: String,
    /// Wheel notches per step in auto mode.
    pub step: u32,
    /// Wait after each scroll before grabbing, ms.
    pub settle_ms: u64,
    pub max_height: u32,
    /// Scroll the window back to where it was afterwards (auto mode).
    pub restore_scroll: bool,
}

impl Default for ScrollCapture {
    fn default() -> Self {
        Self { key: "Ctrl+L".into(), mode: "auto".into(), step: 3, settle_ms: 150, max_height: 30000, restore_scroll: true }
    }
}

/// True if `spec` (e.g. "Ctrl+L") is the key `vk` pressed with exactly these modifiers.
pub fn hotkey_is(spec: &str, vk: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
    parse_hotkey(spec).is_ok_and(|h| {
        h.vk == vk && (h.mods & MOD_CONTROL != 0) == ctrl && (h.mods & MOD_SHIFT != 0) == shift && (h.mods & MOD_ALT != 0) == alt
    })
}

/// M13: history of copied text (encrypted with DPAPI) and the picker.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct TextHistory {
    pub enabled: bool,
    /// Opens the picker next to the cursor.
    pub picker: String,
    /// Pauses/resumes recording.
    pub pause: String,
    pub max_items: usize,
    /// Unpinned entries older than this are removed; 0 = keep forever.
    pub max_age_days: u64,
    /// Larger copies are not stored.
    pub max_entry_kb: usize,
    /// Executable names whose copies are never stored.
    pub ignore_processes: Vec<String>,
}

impl Default for TextHistory {
    fn default() -> Self {
        Self {
            enabled: true,
            picker: "Ctrl+Shift+V".into(),
            pause: "Ctrl+Alt+P".into(),
            max_items: 1000,
            max_age_days: 30,
            max_entry_kb: 64,
            ignore_processes: ["KeePass.exe", "KeePassXC.exe", "1Password.exe", "Bitwarden.exe"].map(String::from).to_vec(),
        }
    }
}

/// M12: a short chip near the cursor showing what was just copied.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClipboardChip {
    pub enabled: bool,
    /// Global hotkey that turns the chip on/off until the next config change.
    pub toggle: String,
    /// How long the chip stays, in milliseconds.
    pub duration_ms: u64,
    /// Fade-out length in milliseconds; 0 = disappear at once.
    pub fade_ms: u64,
    /// Also show the chip for copies made by Shelf itself.
    pub show_own: bool,
    /// Executable names (e.g. "KeePass.exe") whose copies never show a chip.
    pub ignore_processes: Vec<String>,
}

impl Default for ClipboardChip {
    fn default() -> Self {
        Self {
            enabled: true,
            toggle: "Ctrl+Alt+X".into(),
            duration_ms: 1200,
            fade_ms: 0,
            show_own: true,
            ignore_processes: ["KeePass.exe", "KeePassXC.exe", "1Password.exe", "Bitwarden.exe"].map(String::from).to_vec(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Record {
    /// Global hotkey that stops a recording started with Ctrl+R in capture mode.
    pub stop: String,
    pub fps: u32,
    /// Draw the mouse cursor into the video.
    pub cursor: bool,
}

impl Default for Record {
    fn default() -> Self {
        Self { stop: "Ctrl+Alt+R".into(), fps: 30, cursor: true }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Dictation {
    /// Hold to record; release to transcribe.
    pub hold: String,
    /// Path to a ggml whisper model; empty = %APPDATA%Shelfmodelsggml-base.bin.
    pub model: String,
    /// "auto", "ru" or "en".
    pub language: String,
    /// Paste right away (Ctrl+V) instead of only copying.
    pub paste: bool,
    /// Unload the model after this many idle minutes.
    pub unload_after_min: u64,
}

impl Default for Dictation {
    fn default() -> Self {
        Self { hold: "Ctrl+Alt+V".into(), model: String::new(), language: "auto".into(), paste: false, unload_after_min: 5 }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Board {
    pub hotkey: String,
    /// Background opacity, 0.1..1.0.
    pub opacity: f32,
    /// Background color, "#RRGGBB".
    pub background: String,
}

impl Default for Board {
    fn default() -> Self {
        Self { hotkey: "Ctrl+Alt+B".into(), opacity: 0.85, background: "#1E1E1E".into() }
    }
}

/// Parses "#RRGGBB" or "RRGGBB" into 0xRRGGBB.
pub fn parse_color(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ink {
    /// Hold to draw: a key ("CapsLock") or a mouse side button ("XButton1" / "XButton2").
    pub hold: String,
    /// Holding this while drawing keeps the stroke until cleared: "Shift", "Ctrl" or "Alt".
    pub keep_modifier: String,
    /// Toggle drawing without holding (left button draws; Esc exits).
    pub sticky: String,
    pub clear: String,
    /// Delay before a stroke starts to fade, in milliseconds.
    pub fade_ms: u64,
    pub width: f32,
    /// Default ink color, "#RRGGBB" (1-8 switch it while drawing).
    pub color: String,
    // Explanation tools. The laser is the hold key pressed together with Ctrl.
    pub spotlight: String,
    pub zoom: String,
    pub click_rings_toggle: String,
    pub keys_toggle: String,
    /// Initial state of click rings and the pressed-keys plate.
    pub click_rings: bool,
    pub show_keys: bool,
}

impl Default for Ink {
    fn default() -> Self {
        Self {
            hold: "CapsLock".into(),
            keep_modifier: "Shift".into(),
            sticky: "Ctrl+Alt+D".into(),
            clear: "Ctrl+Alt+Backspace".into(),
            fade_ms: 2500,
            width: 4.0,
            color: "#E53935".into(),
            spotlight: "Ctrl+Alt+S".into(),
            zoom: "Ctrl+Alt+Z".into(),
            click_rings_toggle: "Ctrl+Alt+C".into(),
            keys_toggle: "Ctrl+Alt+K".into(),
            click_rings: false,
            show_keys: false,
        }
    }
}

impl Ink {
    /// Virtual key of `keep_modifier`.
    pub fn keep_vk(&self) -> u16 {
        match self.keep_modifier.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => 0x11,
            "alt" => 0x12,
            _ => 0x10,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct History {
    /// Also keep images copied by other apps.
    pub watch_clipboard: bool,
    /// Limits apply to unpinned entries; pinned ones are never removed.
    pub max_items: usize,
    pub max_mb: u64,
}

impl Default for History {
    fn default() -> Self {
        Self { watch_clipboard: true, max_items: 500, max_mb: 2048 }
    }
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
        Self { log_level: "info".into(), autostart: false, hotkeys: Hotkeys::default(), capture: Capture::default(), save: Save::default(), tools: Tools::default(), history: History::default(), ink: Ink::default(), board: Board::default(), dictation: Dictation::default(), record: Record::default(), clipboard_chip: ClipboardChip::default(), text_history: TextHistory::default(), scroll_capture: ScrollCapture::default(), redaction: Redaction::default() }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self { capture: "PrintScreen".into(), shelf: "Ctrl+Shift+Space".into() }
    }
}

pub const DEFAULT_TOML: &str = r##"# Shelf config. Changes are applied on save.

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

# Markup tool keys in capture mode. Colors: 1-8, width: [ ] or mouse wheel.
[tools]
pen = "P"
line = "L"
arrow = "A"
rect = "R"
ellipse = "E"
marker = "M"
text = "T"
blur = "B"
number = "N"

[history]
# Also keep images copied by other apps
watch_clipboard = true
# Limits for unpinned captures; pinned ones are never removed
max_items = 500
max_mb = 2048

# Drawing over the screen (visible in screen sharing)
[ink]
# Hold to draw: a key ("CapsLock") or a mouse side button ("XButton1" / "XButton2")
hold = "CapsLock"
# Hold while drawing to keep the stroke until cleared: Shift, Ctrl or Alt
keep_modifier = "Shift"
# Draw without holding: left button draws, Esc exits
sticky = "Ctrl+Alt+D"
clear = "Ctrl+Alt+Backspace"
# Strokes fade this many milliseconds after release
fade_ms = 2500
width = 4.0
color = "#E53935"
# Explanation tools: hold Ctrl+<hold key> for the laser pointer,
# click while drawing for numbered markers. Esc leaves spotlight and zoom.
spotlight = "Ctrl+Alt+S"
zoom = "Ctrl+Alt+Z"
click_rings_toggle = "Ctrl+Alt+C"
keys_toggle = "Ctrl+Alt+K"
click_rings = false
show_keys = false

# Thinking board: a semi-transparent canvas with the markup tools. Esc saves it to the shelf.
[board]
hotkey = "Ctrl+Alt+B"
opacity = 0.85
background = "#1E1E1E"

# Hold-to-talk dictation with a local whisper model (nothing is downloaded automatically)
[dictation]
hold = "Ctrl+Alt+V"
# Empty = %APPDATA%\Shelf\models\ggml-base.bin
model = ""
# auto, ru or en
language = "auto"
# Paste the text right away (Ctrl+V) instead of only copying it
paste = false
unload_after_min = 5

# Area recording: select an area after PrtScrn and press Ctrl+R; stop with this hotkey
[record]
stop = "Ctrl+Alt+R"
fps = 30
cursor = true

# A short chip next to the cursor showing what was just copied (in any app)
[clipboard_chip]
enabled = true
# Turn the chip on/off
toggle = "Ctrl+Alt+X"
duration_ms = 1200
# Fade-out in ms when hiding; 0 = disappear at once
fade_ms = 0
# Also for copies made by Shelf itself
show_own = true
# Never show the chip for copies from these programs
ignore_processes = ["KeePass.exe", "KeePassXC.exe", "1Password.exe", "Bitwarden.exe"]

# History of copied text (encrypted for the current Windows user) and the picker
[text_history]
enabled = true
picker = "Ctrl+Shift+V"
pause = "Ctrl+Alt+P"
max_items = 1000
# Unpinned entries older than this are removed (0 = keep)
max_age_days = 30
# Larger copies are not stored
max_entry_kb = 64
ignore_processes = ["KeePass.exe", "KeePassXC.exe", "1Password.exe", "Bitwarden.exe"]

# Long screenshots: select an area after PrtScrn and press this key
[scroll_capture]
key = "Ctrl+L"
# auto (Shelf scrolls) or manual (you scroll, Enter finishes)
mode = "auto"
# Wheel notches per step and wait after each step (ms)
step = 3
settle_ms = 150
max_height = 30000
# Scroll the window back afterwards (auto mode)
restore_scroll = true

# Check before sending: find personal data in a capture (OCR) and hide it
[redaction]
key = "Ctrl+P"
# fill, pixelate or blur
style = "fill"
padding = 3
# Quietly check every Ctrl+C capture and offer to hide what was found
background = false
hide_hotkey = "Ctrl+Alt+H"
email = true
phone = true
ip = true
card = true
token = true
url_secret = true
"##;

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
        "backspace" | "back" => 0x08,
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
        // Every section must be spelled out, so users see all options in the generated file.
        let t: toml::Table = toml::from_str(DEFAULT_TOML).unwrap();
        for s in ["hotkeys", "capture", "save", "tools", "history", "ink", "board", "dictation", "record", "clipboard_chip", "text_history", "scroll_capture", "redaction"] {
            assert!(t.contains_key(s), "DEFAULT_TOML lacks [{s}]");
        }
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let c = parse("[hotkeys]\nshelf = \"Alt+S\"").unwrap();
        assert_eq!(c.hotkeys.shelf, "Alt+S");
        assert_eq!(c.hotkeys.capture, "PrintScreen");
        assert_eq!(c.log_level, "info");
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#1E1E1E"), Some(0x1E1E1E));
        assert_eq!(parse_color("ff8800"), Some(0xFF8800));
        assert_eq!(parse_color("#fff"), None);
        assert_eq!(parse_color("#GGGGGG"), None);
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
        assert_eq!(parse_hotkey("Ctrl+Alt+Backspace").unwrap(), Hotkey { mods: MOD_CONTROL | MOD_ALT, vk: 0x08 });
    }
}
