//! M12: "clipboard on display" — a short-lived chip next to the cursor showing what was
//! just copied anywhere in the system. Reading and classifying happen on a worker thread;
//! the chip itself is drawn on the ink layer (click-through, topmost, visible to screen sharing).

use crate::clipboard::Image;
use crate::clipkind::{self, TextKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

pub enum Content {
    /// Password managers mark their copies as private: show only that something was copied.
    Locked,
    Color { rgba: [u8; 4], text: String },
    Files { icon: Option<Image>, name: String, detail: String },
    Url { domain: String, rest: String },
    Json { title: String, keys: String },
    Picture { thumb: Image, w: u32, h: u32 },
    Text { preview: String, stats: String },
    Code { lines: Vec<String>, more: String },
    Secret { masked: String },
    /// Feedback for the on/off hotkey.
    Notice(String),
}

/// Chip background colour (file icons are pre-composited onto it).
pub const BG: u32 = 0x262626;

/// Runtime switch: the hotkey flips it, a config reload resets it.
pub static ENABLED: AtomicBool = AtomicBool::new(true);
static WORKER: OnceLock<Mutex<Sender<()>>> = OnceLock::new();

/// WM_CLIPBOARDUPDATE on the UI thread: only a nudge to the worker, nothing is read here.
pub fn on_update() {
    if !chip_on() && !crate::config::current().text_history.enabled {
        return;
    }
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = channel::<()>();
        std::thread::Builder::new()
            .name("clipchip".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    // Rapid copies: only the latest one matters, no queue.
                    while rx.try_recv().is_ok() {}
                    if let Some((content, at)) = build() {
                        crate::run_on_main(move || {
                            show(content, at);
                        });
                    }
                }
            })
            .unwrap();
        Mutex::new(tx)
    });
    let _ = tx.lock().unwrap().send(());
}

enum Raw {
    Files(Vec<String>),
    Text(String, bool),
    Picture(Image, u32, u32),
}

fn open_clipboard() -> bool {
    // The source app may still hold the clipboard: retry briefly, then quietly give up.
    for _ in 0..3 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    false
}

fn owner_process(owner: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string())
    }
}

fn chip_on() -> bool {
    ENABLED.load(Ordering::Relaxed) && crate::config::current().clipboard_chip.enabled
}

/// Reads only what is needed under the clipboard lock, then classifies outside it.
/// Also feeds the text history (M13), which shares this listener and its privacy rules.
fn build() -> Option<(Content, (i32, i32))> {
    let cfg = crate::config::current().clipboard_chip;
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let owner = unsafe { GetClipboardOwner() }.unwrap_or_default();
    let exe = owner_process(owner).unwrap_or_default();
    let show = chip_on() && !(owner == crate::main_hwnd() && !cfg.show_own) && !cfg.ignore_processes.iter().any(|p| p.trim().eq_ignore_ascii_case(&exe));
    let excl = unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
    let hist = unsafe { RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")) };
    if !open_clipboard() {
        log::debug!("clipchip: clipboard busy, skipped");
        return None;
    }
    let raw = unsafe {
        let private = IsClipboardFormatAvailable(excl).is_ok() || (IsClipboardFormatAvailable(hist).is_ok() && read_dword(hist) == Some(0));
        let raw = if private { None } else { Some(read_raw()) };
        let _ = CloseClipboard();
        raw
    };
    if let Some(Some(Raw::Text(t, truncated))) = &raw {
        crate::texthist::record(t, &exe, *truncated);
    }
    let pos = (pt.x, pt.y);
    if !show {
        return None;
    }
    match raw {
        None => Some((Content::Locked, pos)),
        Some(r) => Some((describe(r?), pos)),
    }
}

unsafe fn read_dword(fmt: u32) -> Option<u32> {
    unsafe {
        let h = GetClipboardData(fmt).ok()?;
        let p = GlobalLock(HGLOBAL(h.0)) as *const u32;
        if p.is_null() {
            return None;
        }
        let v = *p;
        let _ = GlobalUnlock(HGLOBAL(h.0));
        Some(v)
    }
}

unsafe fn read_raw() -> Option<Raw> {
    unsafe {
        if let Ok(h) = GetClipboardData(CF_HDROP.0 as u32) {
            let hdrop = HDROP(h.0);
            let n = DragQueryFileW(hdrop, u32::MAX, None).min(50);
            let files = (0..n)
                .map(|i| {
                    let mut buf = vec![0u16; 1024];
                    let len = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                    String::from_utf16_lossy(&buf[..len])
                })
                .collect();
            return Some(Raw::Files(files));
        }
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            // Only the first TEXT_LIMIT bytes are looked at.
            let units = GlobalSize(g) / 2;
            let max = units.min(clipkind::TEXT_LIMIT / 2);
            let s = std::slice::from_raw_parts(p, max);
            let end = s.iter().position(|&c| c == 0).unwrap_or(max);
            let truncated = end == max && units > max;
            let text = String::from_utf16_lossy(&s[..end]);
            let _ = GlobalUnlock(g);
            return Some(Raw::Text(text, truncated));
        }
        // CF_DIB is preferred over "PNG": Shelf delay-renders its own PNG on the UI thread.
        if let Ok(h) = GetClipboardData(CF_DIB.0 as u32) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u8;
            if p.is_null() {
                return None;
            }
            let dib = std::slice::from_raw_parts(p, GlobalSize(g));
            let thumb = crate::imageio::dib_thumbnail(dib, 240, 150);
            let _ = GlobalUnlock(g);
            let (img, w, h) = thumb?;
            return Some(Raw::Picture(img, w, h));
        }
        None
    }
}

fn human_size(b: u64) -> String {
    const U: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} {}", U[0]) } else { format!("{v:.1} {}", U[i]) }
}

fn plural(n: usize, one: &str, few: &str, many: &str) -> String {
    let w = match (n % 10, n % 100) {
        (1, x) if x != 11 => one,
        (2..=4, x) if !(12..=14).contains(&x) => few,
        _ => many,
    };
    format!("{n} {w}")
}

fn describe_paths(paths: &[String]) -> Option<Content> {
    if paths.is_empty() || !paths.iter().all(|p| std::path::Path::new(p).exists()) {
        return None;
    }
    let first = std::path::Path::new(&paths[0]);
    let icon = file_icon(&paths[0]);
    let first_name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| paths[0].clone());
    if paths.len() == 1 {
        let detail = match std::fs::metadata(first) {
            Ok(m) if m.is_dir() => "папка".to_string(),
            Ok(m) => human_size(m.len()),
            Err(_) => String::new(),
        };
        return Some(Content::Files { icon, name: first_name, detail });
    }
    let total: u64 = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).filter(|m| m.is_file()).map(|m| m.len()).sum();
    Some(Content::Files { icon, name: first_name + " …", detail: format!("{} · {}", plural(paths.len(), "файл", "файла", "файлов"), human_size(total)) })
}

fn describe(raw: Raw) -> Content {
    match raw {
        Raw::Picture(thumb, w, h) => Content::Picture { thumb, w, h },
        Raw::Files(files) => describe_paths(&files).unwrap_or_else(|| plain(&files.join("\n"), false)),
        Raw::Text(t, truncated) => text_content(&t, truncated),
    }
}

fn text_content(t: &str, truncated: bool) -> Content {
    match clipkind::classify(t, truncated) {
        TextKind::Color { rgba, text } => Content::Color { rgba, text },
        TextKind::Paths(p) => describe_paths(&p).unwrap_or_else(|| plain(t, truncated)),
        TextKind::Url { domain, rest } => Content::Url { domain, rest },
        TextKind::Json { keys, array_len } => match array_len {
            Some(n) => Content::Json { title: "JSON ✓ массив".into(), keys: plural(n, "элемент", "элемента", "элементов") },
            None => Content::Json { title: "JSON ✓ объект".into(), keys: if keys.is_empty() { "пустой".into() } else { keys.join(", ") + " …" } },
        },
        TextKind::Code { lines, total_lines } => {
            let more = if total_lines > lines.len() { format!("ещё {}", plural(total_lines - lines.len(), "строка", "строки", "строк")) } else { String::new() };
            Content::Code { lines, more }
        }
        TextKind::Secret { masked } => Content::Secret { masked },
        TextKind::Text { .. } => plain(t, truncated),
    }
}

fn plain(t: &str, truncated: bool) -> Content {
    match clipkind::classify_plain(t, truncated) {
        TextKind::Text { preview, chars, lines, truncated } => {
            let more = if truncated { "+" } else { "" };
            Content::Text { preview, stats: format!("{}{more} · {}{more}", plural(chars, "символ", "символа", "символов"), plural(lines, "строка", "строки", "строк")) }
        }
        _ => Content::Notice(String::new()),
    }
}

/// Shell icon of a file or folder, rendered onto the chip background colour.
fn file_icon(path: &str) -> Option<Image> {
    const SIZE: i32 = 32;
    let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut sfi = SHFILEINFOW::default();
        if SHGetFileInfoW(PCWSTR(w.as_ptr()), Default::default(), Some(&mut sfi), size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON) == 0 || sfi.hIcon.is_invalid() {
            return None;
        }
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: SIZE, biHeight: -SIZE, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        ReleaseDC(None, screen);
        let out = bmp.map(|bmp| {
            let old = SelectObject(dc, bmp.into());
            let px = std::slice::from_raw_parts_mut(bits as *mut u8, (SIZE * SIZE * 4) as usize);
            for p in px.chunks_exact_mut(4) {
                p.copy_from_slice(&[(BG & 0xFF) as u8, (BG >> 8 & 0xFF) as u8, (BG >> 16) as u8, 255]);
            }
            let _ = DrawIconEx(dc, 0, 0, sfi.hIcon, SIZE, SIZE, 0, None, DI_NORMAL);
            let _ = GdiFlush();
            let img = Image { w: SIZE as u32, h: SIZE as u32, bgra: px.to_vec() };
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            img
        });
        let _ = DeleteDC(dc);
        let _ = DestroyIcon(sfi.hIcon);
        out
    }
}


// ! --- Chip window --- !

use crate::annot;
use crate::geom::Rect;
use crate::ink::{Gpu, create_gpu};
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_numerics::Vector2;

const CLASS: PCWSTR = w!("Shelf.Chip");
const TIMER_HIDE: usize = 1;
const TIMER_FADE: usize = 2;
/// The swap chain is allocated once at this size; the window itself is sized to the chip,
/// so DWM only composes a small surface (a full-screen layer slowed down capture hiding).
const SURFACE: (u32, u32) = (900, 520);

struct ChipState {
    content: Content,
    at: (i32, i32),
    fade_start: Option<Instant>,
    bmp: Option<ID2D1Bitmap1>,
}

struct Win {
    hwnd: HWND,
    gpu: Option<Gpu>,
    chip: Option<ChipState>,
}

thread_local! {
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
}

fn with_win<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    WIN.with(|w| w.try_borrow_mut().ok().and_then(|mut w| w.as_mut().map(f)))
}

/// Pre-creates the chip window and its swap chain (UI thread).
pub fn init() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW { cbSize: size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassExW(&wc);
        // Click-through, never activated, above everything, visible to screen capture.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            CLASS,
            w!("Shelf clipboard chip"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        let gpu = create_gpu(hwnd, SURFACE.0, SURFACE.1).map_err(|e| log::error!("chip: GPU init failed: {e}")).ok();
        WIN.with(|w| *w.borrow_mut() = Some(Win { hwnd, gpu, chip: None }));
    }
}

/// Shows (or replaces) the chip; it hides itself after `duration_ms`.
pub fn show(content: Content, at: (i32, i32)) {
    with_win(|w| {
        w.chip = Some(ChipState { content, at, fade_start: None, bmp: None });
        let ms = crate::config::current().clipboard_chip.duration_ms.clamp(200, 10_000) as u32;
        unsafe {
            let _ = KillTimer(Some(w.hwnd), TIMER_FADE);
            let _ = KillTimer(Some(w.hwnd), TIMER_HIDE);
            SetTimer(Some(w.hwnd), TIMER_HIDE, ms, None);
        }
        w.render();
    });
}

impl Win {
    fn hide(&mut self) {
        self.chip = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_FADE);
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn render(&mut self) {
        if self.gpu.is_none() {
            self.gpu = create_gpu(self.hwnd, SURFACE.0, SURFACE.1).ok();
        }
        let (Some(g), Some(mut chip)) = (self.gpu.as_ref(), self.chip.take()) else { return };
        let alpha = match chip.fade_start {
            Some(t) => 1.0 - t.elapsed().as_millis() as f32 / crate::config::current().clipboard_chip.fade_ms.max(1) as f32,
            None => 1.0,
        };
        if alpha <= 0.0 {
            self.hide();
            return;
        }
        let s = crate::geom::scale_at(&crate::capture::monitors(), chip.at.0, chip.at.1);
        let placed = unsafe {
            g.dc.BeginDraw();
            g.dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let placed = draw_chip(&g.dc, &g.res, &mut chip, s, alpha);
            if g.dc.EndDraw(None, None).is_err() {
                self.gpu = None;
                return;
            }
            let _ = g.swap.Present(0, DXGI_PRESENT(0));
            placed
        };
        let (w, h) = (placed.w().min(SURFACE.0 as i32), placed.h().min(SURFACE.1 as i32));
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), placed.l, placed.t, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        self.chip = Some(chip);
    }

    fn expired(&mut self) {
        let fade = crate::config::current().clipboard_chip.fade_ms;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
        }
        match self.chip.as_mut() {
            Some(c) if fade > 0 => {
                c.fade_start = Some(Instant::now());
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_FADE, 16, None);
                }
            }
            _ => self.hide(),
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER if wp.0 == TIMER_HIDE => {
                with_win(|w| w.expired());
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_FADE => {
                with_win(|w| w.render());
                LRESULT(0)
            }
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DPICHANGED => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// One text line of the chip: text, size, colour, monospace.
struct ChipLine {
    text: String,
    size: f32,
    color: u32,
    mono: bool,
}

fn chip_lines(c: &crate::chip::Content) -> Vec<ChipLine> {
    use crate::chip::Content::*;
    let main = |t: &str| ChipLine { text: t.to_string(), size: 14.0, color: 0xF2F2F2, mono: false };
    let sub = |t: &str| ChipLine { text: t.to_string(), size: 12.0, color: 0xA0A0A0, mono: false };
    let mono = |t: &str| ChipLine { text: t.to_string(), size: 12.5, color: 0xE6E6E6, mono: true };
    match c {
        Locked => vec![main("🔒 скопировано")],
        Color { rgba, text } => {
            let mut v = vec![main(text)];
            v.push(sub(&if rgba[3] < 255 { format!("цвет · прозрачность {}%", (rgba[3] as u32 * 100 + 127) / 255) } else { "цвет".into() }));
            v
        }
        Files { name, detail, .. } => vec![main(&crate::clipkind::clip_chars(name, 42)), sub(detail)],
        Url { domain, rest } => {
            let mut v = vec![ChipLine { text: domain.clone(), size: 16.0, color: 0xF2F2F2, mono: false }];
            if !rest.is_empty() {
                v.push(sub(rest));
            }
            v
        }
        Json { title, keys } => vec![main(title), sub(&crate::clipkind::clip_chars(keys, 52))],
        Picture { w, h, .. } => vec![main(&format!("{w} × {h}")), sub("картинка")],
        Text { preview, stats } => vec![main(preview), sub(stats)],
        Code { lines, more } => {
            let mut v: Vec<ChipLine> = lines.iter().map(|l| mono(l)).collect();
            if !more.is_empty() {
                v.push(sub(more));
            }
            v
        }
        Secret { masked } => vec![mono(masked), sub("похоже на секрет — скрыто")],
        Notice(t) => vec![main(t)],
    }
}

/// Lays the chip out next to `c.at` (flipped/clamped to stay on that monitor), draws it at
/// the target's origin and returns where the window must go. `s` is the monitor scale.
unsafe fn draw_chip(dc: &ID2D1DeviceContext, res: &annot::Res, c: &mut ChipState, s: f32, alpha: f32) -> Rect {
    use crate::chip::Content;
    unsafe {
        let pad = 10.0 * s;
        let gap = 10.0 * s;
        let max_text_w = 320.0 * s;
        // Leading visual: swatch, icon or thumbnail.
        let lead: (f32, f32) = match &c.content {
            Content::Color { .. } => (30.0 * s, 30.0 * s),
            Content::Files { icon: Some(_), .. } => (32.0 * s, 32.0 * s),
            Content::Picture { thumb, .. } => {
                let k = (200.0 * s / thumb.w as f32).min(120.0 * s / thumb.h as f32).min(s);
                (thumb.w as f32 * k, thumb.h as f32 * k)
            }
            _ => (0.0, 0.0),
        };
        let img = match &c.content {
            Content::Files { icon: Some(i), .. } => Some(i),
            Content::Picture { thumb, .. } => Some(thumb),
            _ => None,
        };
        if let (Some(img), None) = (img, &c.bmp) {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            c.bmp = dc.CreateBitmap(D2D_SIZE_U { width: img.w, height: img.h }, Some(img.bgra.as_ptr() as _), img.w * 4, &props).ok();
        }
        // Measure text lines.
        let lines = chip_lines(&c.content);
        let mut laid = Vec::new();
        let (mut text_w, mut text_h) = (0.0f32, 0.0f32);
        for l in &lines {
            let font = if l.mono { w!("Consolas") } else { w!("Segoe UI") };
            let Ok(fmt) = res.dw.CreateTextFormat(font, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, l.size * s, w!("")) else { continue };
            if l.mono {
                let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            }
            let t: Vec<u16> = l.text.encode_utf16().collect();
            let Ok(layout) = res.dw.CreateTextLayout(&t, &fmt, max_text_w, 2.6 * l.size * s) else { continue };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut m);
            let (w, h) = (m.widthIncludingTrailingWhitespace.min(max_text_w), m.height.min(2.6 * l.size * s));
            text_w = text_w.max(w);
            laid.push((layout, text_h, l.color, h));
            text_h += h + 2.0 * s;
        }
        let inner_w = lead.0 + if lead.0 > 0.0 { gap } else { 0.0 } + text_w;
        let inner_h = lead.1.max(text_h - 2.0 * s);
        let (w, h) = ((inner_w + 2.0 * pad).ceil() as i32, (inner_h + 2.0 * pad).ceil() as i32);
        let mons = crate::capture::monitors();
        let mon = mons.iter().map(|m| m.0).find(|r| r.contains(c.at.0, c.at.1)).unwrap_or(Rect::new(c.at.0 - 800, c.at.1 - 600, c.at.0 + 800, c.at.1 + 600));
        let (x, y) = crate::geom::place_near(c.at.0, c.at.1, w, h, (18.0 * s) as i32, &mon);
        let placed = Rect::new(x, y, x + w, y + h);
        let (x, y) = (0.0f32, 0.0f32);
        let brush = |col: u32, a: f32| dc.CreateSolidColorBrush(&annot::rgba(col, a * alpha), None).ok();
        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x, top: y, right: x + w as f32, bottom: y + h as f32 }, radiusX: 8.0 * s, radiusY: 8.0 * s };
        if let (Some(bg), Some(border)) = (brush(crate::chip::BG, 0.96), brush(0x4A4A4A, 1.0)) {
            dc.FillRoundedRectangle(&rr, &bg);
            dc.DrawRoundedRectangle(&rr, &border, 1.0, None);
        }
        let lx = x + pad;
        let ly = y + pad + (inner_h - lead.1) / 2.0;
        match &c.content {
            Content::Color { rgba, .. } => {
                let sw = D2D_RECT_F { left: lx, top: ly, right: lx + lead.0, bottom: ly + lead.1 };
                // Translucent colours over a light/dark split so the alpha is visible.
                if rgba[3] < 255 {
                    if let (Some(l), Some(d)) = (brush(0xFFFFFF, 1.0), brush(0x000000, 1.0)) {
                        dc.FillRectangle(&D2D_RECT_F { right: lx + lead.0 / 2.0, ..sw }, &l);
                        dc.FillRectangle(&D2D_RECT_F { left: lx + lead.0 / 2.0, ..sw }, &d);
                    }
                }
                let col = (rgba[0] as u32) << 16 | (rgba[1] as u32) << 8 | rgba[2] as u32;
                if let (Some(b), Some(edge)) = (brush(col, rgba[3] as f32 / 255.0), brush(0xFFFFFF, 0.5)) {
                    let r = D2D1_ROUNDED_RECT { rect: sw, radiusX: 4.0 * s, radiusY: 4.0 * s };
                    dc.FillRoundedRectangle(&r, &b);
                    dc.DrawRoundedRectangle(&r, &edge, 1.0, None);
                }
            }
            _ => {
                if let Some(b) = &c.bmp {
                    let d = D2D_RECT_F { left: lx, top: ly, right: lx + lead.0, bottom: ly + lead.1 };
                    dc.DrawBitmap(b, Some(&d), alpha, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
                }
            }
        }
        let tx = lx + if lead.0 > 0.0 { lead.0 + gap } else { 0.0 };
        let ty = y + pad + (inner_h - (text_h - 2.0 * s)) / 2.0;
        for (layout, dy, col, _) in laid {
            if let Some(b) = brush(col, 1.0) {
                dc.DrawTextLayout(Vector2::new(tx, ty + dy), &layout, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT | D2D1_DRAW_TEXT_OPTIONS_CLIP);
            }
        }
        placed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_plurals() {
        assert_eq!(plural(1, "файл", "файла", "файлов"), "1 файл");
        assert_eq!(plural(3, "файл", "файла", "файлов"), "3 файла");
        assert_eq!(plural(11, "файл", "файла", "файлов"), "11 файлов");
        assert_eq!(plural(22, "файл", "файла", "файлов"), "22 файла");
        assert_eq!(human_size(512), "512 Б");
        assert_eq!(human_size(1536), "1.5 КБ");
    }
}
