//! M14: scrolling capture. A worker thread grabs the selected area frame by frame
//! (auto: Shelf scrolls with the wheel; manual: the user scrolls) and stitches the frames;
//! a frame + height counter window, excluded from capture, shows progress.

use crate::clipboard::Image;
use crate::geom::Rect;
use crate::ink::{Gpu, create_gpu};
use crate::stitch::{Step, Stitcher};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

/// Set while a session runs: the keyboard hook turns Enter/Esc into STOP.
pub static ACTIVE: AtomicBool = AtomicBool::new(false);
pub static STOP: AtomicBool = AtomicBool::new(false);
static HEIGHT: AtomicU32 = AtomicU32::new(0);
static MANUAL: AtomicBool = AtomicBool::new(false);

const CLASS: PCWSTR = w!("Shelf.ScrollFrame");
const PAD: i32 = 4;
const LABEL_H: i32 = 30;

struct FrameWin {
    hwnd: HWND,
    gpu: Option<Gpu>,
    rect: Rect,
    scale: f32,
}

thread_local! {
    static WIN: RefCell<Option<FrameWin>> = const { RefCell::new(None) };
}

/// Starts a session for `rect` (virtual-screen pixels). UI thread.
pub fn start(rect: Rect) {
    if ACTIVE.swap(true, Ordering::Relaxed) {
        return;
    }
    STOP.store(false, Ordering::Relaxed);
    HEIGHT.store(rect.h() as u32, Ordering::Relaxed);
    let cfg = crate::config::current().scroll_capture;
    MANUAL.store(cfg.mode.eq_ignore_ascii_case("manual"), Ordering::Relaxed);
    show_frame(rect);
    std::thread::spawn(move || {
        let result = run(rect, &cfg);
        ACTIVE.store(false, Ordering::Relaxed);
        crate::run_on_main(move || {
            hide_frame();
            match result {
                Some(img) => {
                    log::info!("scroll capture: {}x{}", img.w, img.h);
                    crate::overlay::show_image(Arc::new(img));
                }
                None => log::warn!("scroll capture: nothing captured"),
            }
        });
    });
}

fn grab(rect: &Rect, dc: HDC, bits: *const u8) -> Vec<u8> {
    unsafe {
        let screen = GetDC(None);
        let _ = BitBlt(dc, 0, 0, rect.w(), rect.h(), Some(screen), rect.l, rect.t, SRCCOPY);
        ReleaseDC(None, screen);
        let _ = GdiFlush();
        std::slice::from_raw_parts(bits, (rect.w() * rect.h() * 4) as usize).to_vec()
    }
}

fn wheel(notches: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { mouseData: (notches * WHEEL_DELTA as i32) as u32, dwFlags: MOUSEEVENTF_WHEEL, ..Default::default() } },
    };
    unsafe {
        SendInput(&[input], size_of::<INPUT>() as i32);
    }
}

fn update_label() {
    // The frame window lives on the UI thread; poke it to redraw the counter.
    crate::run_on_main(|| {
        WIN.with(|w| {
            if let Some(w) = w.borrow_mut().as_mut() {
                w.render();
            }
        })
    });
}

fn run(rect: Rect, cfg: &crate::config::ScrollCapture) -> Option<Image> {
    let (w, h) = (rect.w(), rect.h());
    if w < 16 || h < 16 {
        return None;
    }
    let s = crate::geom::scale_at(&crate::capture::monitors(), rect.l, rect.t);
    let skip_right = ((20.0 * s) as usize).min(w as usize / 4);
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        ReleaseDC(None, screen);
        let old = SelectObject(dc, bmp.into());

        let mut saved_cursor = POINT::default();
        let _ = GetCursorPos(&mut saved_cursor);
        let center = (rect.l + w / 2, rect.t + h / 2);
        let first = grab(&rect, dc, bits as *const u8);
        let mut st = Stitcher::new(&first, w as usize, h as usize, skip_right);
        let step = cfg.step.clamp(1, 20) as i32;
        let settle = Duration::from_millis(cfg.settle_ms.clamp(30, 2000));
        let max_h = cfg.max_height.clamp(h as u32, 100_000) as usize;
        let mut unchanged = 0;
        let mut scrolled: i32 = 0;
        let mut progressed = false;
        loop {
            if STOP.load(Ordering::Relaxed) || st.height() >= max_h {
                break;
            }
            let manual = MANUAL.load(Ordering::Relaxed);
            if manual {
                std::thread::sleep(Duration::from_millis(80));
            } else {
                let _ = SetCursorPos(center.0, center.1);
                wheel(-step);
                scrolled += step;
                std::thread::sleep(settle);
            }
            let frame = grab(&rect, dc, bits as *const u8);
            match st.push(&frame) {
                Step::Added(_) => {
                    unchanged = 0;
                    progressed = true;
                    HEIGHT.store(st.height() as u32, Ordering::Relaxed);
                    update_label();
                }
                Step::Unchanged if manual => {}
                Step::Unchanged => {
                    unchanged += 1;
                    if unchanged >= 2 {
                        if progressed {
                            break; // end of the page
                        }
                        // The window ignores synthetic wheel input: let the user scroll.
                        log::info!("scroll capture: no response to the wheel, switching to manual");
                        MANUAL.store(true, Ordering::Relaxed);
                        update_label();
                    }
                }
                Step::Lost => {
                    log::warn!("scroll capture: frames do not overlap (scrolled too far), stopping");
                    break;
                }
            }
        }
        if cfg.restore_scroll && scrolled > 0 {
                let _ = SetCursorPos(center.0, center.1);
                // Wheel back in chunks so apps with acceleration do not overshoot wildly.
                let mut left = scrolled;
                while left > 0 {
                    let n = left.min(10);
                    wheel(n);
                    left -= n;
                    std::thread::sleep(Duration::from_millis(15));
                }
        }
        let _ = SetCursorPos(saved_cursor.x, saved_cursor.y);
        SelectObject(dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
        let (ow, oh, mut px) = st.finish();
        let oh = oh.min(max_h);
        px.truncate(ow * oh * 4);
        Some(Image { w: ow as u32, h: oh as u32, bgra: px })
    }
}

// ! --- Frame + counter window (excluded from capture) --- !

fn show_frame(rect: Rect) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW { cbSize: size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassExW(&wc);
        let s = crate::geom::scale_at(&crate::capture::monitors(), rect.l, rect.t);
        let pad = (PAD as f32 * s) as i32;
        let label = (LABEL_H as f32 * s) as i32;
        let (x, y, w, h) = (rect.l - pad, rect.t - pad - label, rect.w() + 2 * pad, rect.h() + 2 * pad + label);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            CLASS,
            w!("Shelf scroll capture"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        // Never in the frames we grab (nor in anyone else's capture).
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        let gpu = create_gpu(hwnd, w as u32, h as u32).ok();
        let mut fw = FrameWin { hwnd, gpu, rect, scale: s };
        fw.render();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        WIN.with(|c| *c.borrow_mut() = Some(fw));
    }
}

fn hide_frame() {
    WIN.with(|c| {
        if let Some(w) = c.borrow_mut().take() {
            unsafe {
                let _ = DestroyWindow(w.hwnd);
            }
        }
    });
}

impl FrameWin {
    fn render(&mut self) {
        let Some(g) = self.gpu.as_ref() else { return };
        let s = self.scale;
        let pad = PAD as f32 * s;
        let label = LABEL_H as f32 * s;
        let (w, h) = (self.rect.w() as f32 + 2.0 * pad, self.rect.h() as f32 + 2.0 * pad + label);
        let manual = MANUAL.load(Ordering::Relaxed);
        let text = if manual {
            format!("Прокручивайте сами · {} px · Enter — готово", HEIGHT.load(Ordering::Relaxed))
        } else {
            format!("Прокрутка… {} px · Enter/Esc — стоп", HEIGHT.load(Ordering::Relaxed))
        };
        unsafe {
            let dc = &g.dc;
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let red = dc.CreateSolidColorBrush(&crate::annot::rgba(if manual { 0xFB8C00 } else { 0xE53935 }, 1.0), None).ok();
            let bg = dc.CreateSolidColorBrush(&crate::annot::rgba(0x202020, 0.9), None).ok();
            let fg = dc.CreateSolidColorBrush(&crate::annot::rgba(0xFFFFFF, 1.0), None).ok();
            if let Some(r) = &red {
                dc.DrawRectangle(&D2D_RECT_F { left: pad / 2.0, top: label + pad / 2.0, right: w - pad / 2.0, bottom: h - pad / 2.0 }, r, 2.0 * s, None);
            }
            if let (Some(bg), Some(fg), Some(fmt)) = (bg, fg, g.res.text_format(12.0 * s)) {
                let lw = (7.2 * s * text.chars().count() as f32 + 20.0 * s).min(w);
                dc.FillRectangle(&D2D_RECT_F { left: 0.0, top: 0.0, right: lw, bottom: label - 4.0 * s }, &bg);
                let t: Vec<u16> = text.encode_utf16().collect();
                let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                dc.DrawText(&t, &fmt, &D2D_RECT_F { left: 10.0 * s, top: 5.0 * s, right: lw, bottom: label }, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
            if dc.EndDraw(None, None).is_ok() {
                let _ = g.swap.Present(0, DXGI_PRESENT(0));
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
