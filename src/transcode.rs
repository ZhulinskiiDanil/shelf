//! Re-encoding of recorded MP4s with Media Foundation: shrinking to a size limit (M16),
//! cutting (M17) and decoding frames for GIF export and timeline thumbnails.
//! Times are in 100 ns units, as everywhere in Media Foundation.

use crate::clipboard::Image;
use std::path::Path;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::VT_I8;
use windows::core::*;

const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const FIRST_AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const ANY_STREAM: u32 = MF_SOURCE_READER_ANY_STREAM.0 as u32;
const SOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Info {
    pub duration: i64,
    pub w: u32,
    pub h: u32,
    pub fps: f64,
    pub audio: bool,
}

/// Video bitrate that makes a file of `dur_s` seconds fit `max_bytes`, leaving room
/// for the sound and ~4 % container overhead. None if even the minimum would not fit.
pub fn target_bitrate(max_bytes: u64, dur_s: f64, audio_bps: u32) -> Option<u32> {
    if dur_s <= 0.0 {
        return None;
    }
    let total = max_bytes as f64 * 8.0 * 0.96 / dur_s;
    let video = total - audio_bps as f64;
    (video >= MIN_VIDEO_BPS as f64).then(|| video.min(40_000_000.0) as u32)
}

pub const MIN_VIDEO_BPS: u32 = 150_000;

/// Output size for a bitrate: keep at least ~0.06 bit per pixel per frame by scaling
/// down (never below a quarter of the width), with even dimensions for H.264.
pub fn fit_size(w: u32, h: u32, fps: f64, bps: u32) -> (u32, u32) {
    let bpp = bps as f64 / (w as f64 * h as f64 * fps.max(1.0));
    let scale = if bpp >= 0.06 { 1.0 } else { (bpp / 0.06).sqrt().max(0.25) };
    let even = |v: f64| ((v.round() as u32) & !1).max(16);
    (even(w as f64 * scale), even(h as f64 * scale))
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub fn init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
}

/// Opens a reader decoding video to RGB32 (scaled to `size` if given) and, if the file
/// has sound and `audio` is set, audio to 16-bit PCM.
fn open(path: &Path, size: Option<(u32, u32)>, audio: bool) -> Result<(IMFSourceReader, Info)> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromURL(PCWSTR(wide(path).as_ptr()), &attrs)?;
        let native = reader.GetNativeMediaType(FIRST_VIDEO, 0)?;
        let (w, h) = unpack(native.GetUINT64(&MF_MT_FRAME_SIZE)?);
        let (num, den) = native.GetUINT64(&MF_MT_FRAME_RATE).map(unpack).unwrap_or((30, 1));
        let fps = if den == 0 { 30.0 } else { num as f64 / den as f64 };
        let (ow, oh) = size.unwrap_or((w, h));
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(ow, oh))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        reader.SetCurrentMediaType(FIRST_VIDEO, None, &t)?;
        let mut has_audio = false;
        if audio && reader.SetStreamSelection(FIRST_AUDIO, true).is_ok() {
            let a = MFCreateMediaType()?;
            a.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            a.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            a.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            has_audio = reader.SetCurrentMediaType(FIRST_AUDIO, None, &a).is_ok();
        }
        if !has_audio {
            let _ = reader.SetStreamSelection(FIRST_AUDIO, false);
        }
        let dur = reader.GetPresentationAttribute(SOURCE, &MF_PD_DURATION).map(|v| v.Anonymous.Anonymous.Anonymous.uhVal as i64).unwrap_or(0);
        Ok((reader, Info { duration: dur, w, h, fps, audio: has_audio }))
    }
}

pub fn probe(path: &Path) -> Result<Info> {
    let (_, info) = open(path, None, true)?;
    Ok(info)
}

fn seek(reader: &IMFSourceReader, t: i64) -> Result<()> {
    let mut v = PROPVARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_I8;
        inner.Anonymous.hVal = t;
        reader.SetCurrentPosition(&GUID::zeroed(), &v)
    }
}

/// Re-encodes `segments` of `input` (start, end) back to back into `output` as H.264 at
/// `bps` and `size`, keeping the sound (AAC) if present and `audio` is set.
pub fn transcode(input: &Path, output: &Path, segments: &[(i64, i64)], bps: u32, size: (u32, u32), audio: bool) -> Result<()> {
    unsafe {
        let (reader, info) = open(input, Some(size), audio)?;
        let _ = std::fs::remove_file(output);
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        let writer = MFCreateSinkWriterFromURL(PCWSTR(wide(output).as_ptr()), None, &attrs)?;
        let fps_num = (info.fps * 1000.0).round() as u32;
        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, bps)?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, pack(size.0, size.1))?;
        out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_num, 1000))?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        let vs = writer.AddStream(&out)?;
        writer.SetInputMediaType(vs, &reader.GetCurrentMediaType(FIRST_VIDEO)?, None)?;
        let as_ = if info.audio {
            let pcm = reader.GetCurrentMediaType(FIRST_AUDIO)?;
            let a = MFCreateMediaType()?;
            a.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            a.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            a.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            a.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, pcm.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?)?;
            a.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, pcm.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?)?;
            a.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, crate::record::AUDIO_BPS / 8)?;
            let s = writer.AddStream(&a)?;
            writer.SetInputMediaType(s, &pcm, None)?;
            Some(s)
        } else {
            None
        };
        // Which reader stream index is the video one (the others are audio).
        let video_index = {
            let mut idx = 0u32;
            while let Ok(t) = reader.GetCurrentMediaType(idx) {
                if t.GetGUID(&MF_MT_MAJOR_TYPE)? == MFMediaType_Video {
                    break;
                }
                idx += 1;
            }
            idx
        };
        writer.BeginWriting()?;
        let mut base = 0i64;
        for &(s, e) in segments {
            seek(&reader, s)?;
            let (mut vdone, mut adone) = (false, as_.is_none());
            while !(vdone && adone) {
                let mut idx = 0u32;
                let mut flags = 0u32;
                let mut ts = 0i64;
                let mut sample = None;
                reader.ReadSample(ANY_STREAM, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
                let is_video = idx == video_index;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    if is_video {
                        vdone = true;
                    } else {
                        adone = true;
                    }
                }
                let Some(sample) = sample else { continue };
                if ts >= e {
                    if is_video {
                        vdone = true;
                    } else {
                        adone = true;
                    }
                    continue;
                }
                if ts < s || (is_video && vdone) || (!is_video && adone) {
                    continue;
                }
                sample.SetSampleTime(ts - s + base)?;
                if is_video {
                    writer.WriteSample(vs, &sample)?;
                } else if let Some(a) = as_ {
                    writer.WriteSample(a, &sample)?;
                }
            }
            base += e - s;
        }
        writer.Finalize()
    }
}

/// Decodes video frames at least `step` apart, scaled to `size`; `f` returns false to stop.
pub fn frames(input: &Path, size: (u32, u32), step: i64, mut f: impl FnMut(i64, Image) -> bool) -> Result<()> {
    unsafe {
        let (reader, _) = open(input, Some(size), false)?;
        let cur = reader.GetCurrentMediaType(FIRST_VIDEO)?;
        let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or((size.0 * 4) as i32);
        let mut next = 0i64;
        loop {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample = None;
            reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(());
            }
            let Some(sample) = sample else { continue };
            if ts < next {
                continue;
            }
            next = ts + step;
            let buf = sample.ConvertToContiguousBuffer()?;
            let mut p = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut p, None, Some(&mut len))?;
            let row = (size.0 * 4) as usize;
            let mut bgra = vec![0u8; row * size.1 as usize];
            for y in 0..size.1 as usize {
                let src_row = if stride < 0 { size.1 as usize - 1 - y } else { y };
                let off = src_row * stride.unsigned_abs() as usize;
                if off + row <= len as usize {
                    bgra[y * row..(y + 1) * row].copy_from_slice(std::slice::from_raw_parts(p.add(off), row));
                }
            }
            let _ = buf.Unlock();
            for px in bgra.chunks_exact_mut(4) {
                px[3] = 255;
            }
            if !f(ts, Image { w: size.0, h: size.1, bgra }) {
                return Ok(());
            }
        }
    }
}

/// Shrinks `path` in place to fit `max_bytes` if it is larger. Returns the new size.
pub fn fit_to_limit(path: &Path, max_bytes: u64) -> std::result::Result<u64, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if max_bytes == 0 || size <= max_bytes {
        return Ok(size);
    }
    init();
    let info = probe(path).map_err(|e| e.to_string())?;
    let dur_s = info.duration as f64 / 1e7;
    let audio_bps = if info.audio { crate::record::AUDIO_BPS } else { 0 };
    let tmp = path.with_extension("fit.mp4");
    let mut budget = max_bytes;
    // The encoder's rate control is not exact; retry a bit lower if the first pass overshoots.
    for attempt in 0..3 {
        let bps = target_bitrate(budget, dur_s, audio_bps).ok_or("recording too long for the size limit")?;
        let dims = fit_size(info.w, info.h, info.fps, bps);
        let t0 = std::time::Instant::now();
        transcode(path, &tmp, &[(0, info.duration.max(1))], bps, dims, info.audio).map_err(|e| e.to_string())?;
        let got = std::fs::metadata(&tmp).map_err(|e| e.to_string())?.len();
        log::info!("record: fit {:.1} MB -> {:.1} MB at {} kbit/s {}x{} in {} ms (attempt {})", size as f64 / 1e6, got as f64 / 1e6, bps / 1000, dims.0, dims.1, t0.elapsed().as_millis(), attempt + 1);
        if got <= max_bytes || attempt == 2 {
            std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
            return Ok(got);
        }
        budget = (budget as f64 * max_bytes as f64 / got as f64 * 0.95) as u64;
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_for_limit() {
        // 10 MB over 60 s with sound: ~1.28 Mbit/s total, minus 128 kbit/s of audio.
        let b = target_bitrate(10_000_000, 60.0, 128_000).unwrap();
        assert_eq!(b, 1_152_000);
        assert!(target_bitrate(10_000_000, 600.0, 128_000).is_none());
        assert_eq!(target_bitrate(1_000_000_000, 1.0, 0), Some(40_000_000));
        assert!(target_bitrate(10_000_000, 0.0, 0).is_none());
    }

    #[test]
    fn size_for_bitrate() {
        // Plenty of bits: unchanged.
        assert_eq!(fit_size(1920, 1080, 30.0, 8_000_000), (1920, 1080));
        // A quarter of the needed bits: half the width and height.
        let need = (1920.0 * 1080.0 * 30.0 * 0.06) as u32;
        assert_eq!(fit_size(1920, 1080, 30.0, need / 4), (960, 540));
        // Never below a quarter, always even.
        assert_eq!(fit_size(1000, 602, 30.0, 1000), (250, 150));
    }
}
