//! Area recording to MP4 (H.264) with Media Foundation. Frames are grabbed with GDI on a
//! dedicated thread at a fixed rate; the cursor is drawn into the frame.

use crate::clipboard::Image;
use crate::geom::Rect;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

pub struct Session {
    stop: std::sync::Arc<AtomicBool>,
    thread: std::thread::JoinHandle<std::result::Result<(PathBuf, Image, u32), String>>,
    started: Instant,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// H.264 needs even dimensions; shrink by one pixel where necessary.
pub fn even_rect(r: &Rect) -> Rect {
    Rect::new(r.l, r.t, r.l + (r.w() & !1), r.t + (r.h() & !1))
}

/// Bits per second for a frame size and rate (≈0.12 bit per pixel per frame, clamped).
pub fn bitrate(w: i32, h: i32, fps: u32) -> u32 {
    ((w as f64 * h as f64 * fps as f64 * 0.12) as u32).clamp(1_000_000, 40_000_000)
}

pub fn is_recording() -> bool {
    SESSION.lock().unwrap().is_some()
}

pub fn start(rect: Rect, fps: u32, cursor: bool) -> Option<Rect> {
    let mut s = SESSION.lock().unwrap();
    if s.is_some() {
        return None;
    }
    let rect = even_rect(&rect);
    if rect.w() < 16 || rect.h() < 16 {
        log::warn!("record: area too small");
        return None;
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let fps = fps.clamp(5, 60);
    let path = std::env::temp_dir().join(format!("shelf-rec-{}.mp4", std::process::id()));
    let thread = std::thread::spawn(move || run(rect, path, fps, cursor, &flag));
    *s = Some(Session { stop, thread, started: Instant::now() });
    log::info!("record: started {}x{} @ {fps} fps", rect.w(), rect.h());
    Some(rect)
}

/// Stops recording; returns the temporary MP4 path and its first frame.
pub fn stop() -> Option<std::result::Result<(PathBuf, Image), String>> {
    let s = SESSION.lock().unwrap().take()?;
    s.stop.store(true, Ordering::Relaxed);
    let secs = s.started.elapsed().as_secs_f32();
    Some(match s.thread.join() {
        Ok(Ok((p, first, frames))) => {
            log::info!("record: {frames} frames in {secs:.1} s");
            Ok((p, first))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("recording thread panicked".into()),
    })
}

fn guid_attr(t: &IMFMediaType, key: &GUID, v: &GUID) -> Result<()> {
    unsafe { t.SetGUID(key, v) }
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn run(rect: Rect, path: PathBuf, fps: u32, cursor: bool, stop: &AtomicBool) -> std::result::Result<(PathBuf, Image, u32), String> {
    let e = |e: Error| e.to_string();
    let (w, h) = (rect.w(), rect.h());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e)?;
        let _ = std::fs::remove_file(&path);
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2).map_err(e)?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(e)?;
        let wpath: Vec<u16> = path.as_os_str().encode_wide_z();
        let writer = MFCreateSinkWriterFromURL(PCWSTR(wpath.as_ptr()), None, &attrs).map_err(e)?;

        let out = MFCreateMediaType().map_err(e)?;
        guid_attr(&out, &MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e)?;
        guid_attr(&out, &MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(e)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate(w, h, fps)).map_err(e)?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32)).map_err(e)?;
        out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1)).map_err(e)?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).map_err(e)?;
        let stream = writer.AddStream(&out).map_err(e)?;

        let inp = MFCreateMediaType().map_err(e)?;
        guid_attr(&inp, &MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e)?;
        guid_attr(&inp, &MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(e)?;
        inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e)?;
        inp.SetUINT64(&MF_MT_FRAME_SIZE, pack(w as u32, h as u32)).map_err(e)?;
        inp.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1)).map_err(e)?;
        inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).map_err(e)?;
        // Positive stride = top-down rows, matching our DIB section.
        inp.SetUINT32(&MF_MT_DEFAULT_STRIDE, (w * 4) as u32).map_err(e)?;
        writer.SetInputMediaType(stream, &inp, None).map_err(e)?;
        writer.BeginWriting().map_err(e)?;

        // GDI frame buffer.
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0).map_err(e)?;
        let old = SelectObject(dc, bmp.into());
        let len = (w * h * 4) as usize;
        let period = Duration::from_secs_f64(1.0 / fps as f64);
        let dur100ns = 10_000_000 / fps as i64;
        let start = Instant::now();
        let mut first: Option<Image> = None;
        let mut n: u32 = 0;
        let mut result = Ok(());
        while !stop.load(Ordering::Relaxed) {
            let due = start + period * n;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            let _ = BitBlt(dc, 0, 0, w, h, Some(screen), rect.l, rect.t, SRCCOPY);
            if cursor {
                draw_cursor(dc, &rect);
            }
            let _ = GdiFlush();
            let px = std::slice::from_raw_parts(bits as *const u8, len);
            if first.is_none() {
                first = Some(Image { w: w as u32, h: h as u32, bgra: px.to_vec() });
            }
            // Timestamp by wall clock so dropped frames do not speed the video up.
            let t = (start.elapsed().as_nanos() / 100) as i64;
            if let Err(err) = write_frame(&writer, stream, px, t, dur100ns) {
                result = Err(err.to_string());
                break;
            }
            n += 1;
            // If we fell behind (slow encoder), skip ahead instead of bursting.
            let behind = start.elapsed().as_secs_f64() / period.as_secs_f64();
            if behind as u32 > n + 1 {
                n = behind as u32;
            }
        }
        SelectObject(dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        writer.Finalize().map_err(e)?;
        let _ = MFShutdown();
        result?;
        Ok((path, first.ok_or("no frames")?, n))
    }
}

unsafe fn write_frame(writer: &IMFSinkWriter, stream: u32, px: &[u8], t: i64, dur: i64) -> Result<()> {
    unsafe {
        let buf = MFCreateMemoryBuffer(px.len() as u32)?;
        let mut p = std::ptr::null_mut();
        buf.Lock(&mut p, None, None)?;
        std::ptr::copy_nonoverlapping(px.as_ptr(), p, px.len());
        buf.Unlock()?;
        buf.SetCurrentLength(px.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buf)?;
        sample.SetSampleTime(t)?;
        sample.SetSampleDuration(dur)?;
        writer.WriteSample(stream, &sample)
    }
}

unsafe fn draw_cursor(dc: HDC, rect: &Rect) {
    unsafe {
        let mut ci = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
        if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 {
            return;
        }
        let mut ii = ICONINFO::default();
        if GetIconInfo(HICON(ci.hCursor.0), &mut ii).is_err() {
            return;
        }
        let x = ci.ptScreenPos.x - ii.xHotspot as i32 - rect.l;
        let y = ci.ptScreenPos.y - ii.yHotspot as i32 - rect.t;
        let _ = DrawIconEx(dc, x, y, HICON(ci.hCursor.0), 0, 0, 0, None, DI_NORMAL);
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
    }
}

trait WideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}

impl WideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn even_dimensions() {
        assert_eq!(even_rect(&Rect::new(-101, 5, 0, 106)), Rect::new(-101, 5, -1, 105));
        assert_eq!(even_rect(&Rect::new(0, 0, 640, 480)), Rect::new(0, 0, 640, 480));
    }

    #[test]
    fn bitrate_bounds() {
        assert_eq!(bitrate(1920, 1080, 30), 7_464_960);
        assert_eq!(bitrate(100, 100, 30), 1_000_000);
        assert_eq!(bitrate(7680, 4320, 60), 40_000_000);
    }
}
