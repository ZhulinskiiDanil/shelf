//! Microphone capture via WASAPI (shared mode, default capture device) and conversion
//! to what whisper expects: mono f32 at 16 kHz.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::*;

pub const WHISPER_RATE: u32 = 16000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SampleKind {
    F32,
    I16,
    I32,
}

/// Interleaved device samples → mono f32 (average of channels).
pub fn to_mono(bytes: &[u8], kind: SampleKind, channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let samples: Vec<f32> = match kind {
        SampleKind::F32 => bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect(),
        SampleKind::I16 => bytes.chunks_exact(2).map(|b| i16::from_le_bytes(b.try_into().unwrap()) as f32 / 32768.0).collect(),
        SampleKind::I32 => bytes.chunks_exact(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2147483648.0).collect(),
    };
    samples.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
}

/// Linear-interpolation resampler. Good enough for speech recognition.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let out_len = (input.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * step;
            let i0 = pos.floor() as usize;
            let frac = (pos - i0 as f64) as f32;
            let a = input[i0.min(input.len() - 1)];
            let b = input[(i0 + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

pub struct Recording {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<(Vec<f32>, u32), String>>>,
}

impl Recording {
    /// Stops and returns mono 16 kHz samples.
    pub fn finish(mut self) -> Result<Vec<f32>, String> {
        self.stop.store(true, Ordering::Relaxed);
        let (mono, rate) = self.thread.take().unwrap().join().map_err(|_| "capture thread panicked".to_string())??;
        Ok(resample(&mono, rate, WHISPER_RATE))
    }
}

pub fn start() -> Recording {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::spawn(move || {
        let mut mono = Vec::new();
        let mut rate = 0;
        capture(false, &s, |bytes, frames, f| {
            rate = f.rate;
            match bytes {
                Some(b) => mono.extend(to_mono(b, f.kind, f.channels)),
                None => mono.extend(std::iter::repeat_n(0.0, frames)),
            }
        })?;
        Ok((mono, rate))
    });
    Recording { stop, thread: Some(thread) }
}

/// Device stream format.
#[derive(Clone, Copy)]
pub struct Format {
    pub kind: SampleKind,
    pub channels: usize,
    pub rate: u32,
}

/// Captures the default microphone, or with `loopback` what the speakers play, until
/// `stop`. `sink` gets each packet (None = silence) with its frame count.
fn capture(loopback: bool, stop: &AtomicBool, mut sink: impl FnMut(Option<&[u8]>, usize, Format)) -> Result<(), String> {
    let e = |e: windows::core::Error| e.to_string();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(e)?;
        let dev = if loopback {
            en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| "no audio output".to_string())?
        } else {
            en.GetDefaultAudioEndpoint(eCapture, eConsole).map_err(|_| "no microphone".to_string())?
        };
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).map_err(e)?;
        let fmt = client.GetMixFormat().map_err(e)?;
        let wf = *fmt;
        let channels = wf.nChannels as usize;
        let rate = wf.nSamplesPerSec;
        let kind = if wf.wFormatTag == WAVE_FORMAT_IEEE_FLOAT as u16 {
            SampleKind::F32
        } else if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16 {
            let ext = &*(fmt as *const WAVEFORMATEXTENSIBLE);
            let sub = ext.SubFormat;
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                SampleKind::F32
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM && wf.wBitsPerSample == 16 {
                SampleKind::I16
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM && wf.wBitsPerSample == 32 {
                SampleKind::I32
            } else {
                return Err("unsupported audio format".into());
            }
        } else if wf.wBitsPerSample == 16 {
            SampleKind::I16
        } else {
            return Err("unsupported audio format".into());
        };
        let format = Format { kind, channels, rate };
        let flags = if loopback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
        // 1 s buffer (in 100 ns units); we drain it every 10 ms.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 10_000_000, 0, fmt, None).map_err(e)?;
        let cap: IAudioCaptureClient = client.GetService().map_err(e)?;
        client.Start().map_err(e)?;
        let block = wf.nBlockAlign as usize;
        loop {
            let done = stop.load(Ordering::Relaxed);
            while cap.GetNextPacketSize().map_err(e)? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None).map_err(e)?;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    sink(None, frames as usize, format);
                } else {
                    sink(Some(std::slice::from_raw_parts(data, frames as usize * block)), frames as usize, format);
                }
                cap.ReleaseBuffer(frames).map_err(e)?;
            }
            if done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = client.Stop();
        CoTaskMemFree(Some(fmt as _));
        Ok(())
    }
}

// ! --- Recording audio (M16) --- !

/// Rate of recorded audio; samples are interleaved stereo f32.
pub const REC_RATE: u32 = 48000;

/// Interleaved device samples → interleaved stereo f32 (mono is duplicated, extra channels dropped).
pub fn to_stereo(bytes: &[u8], kind: SampleKind, channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let samples: Vec<f32> = match kind {
        SampleKind::F32 => bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect(),
        SampleKind::I16 => bytes.chunks_exact(2).map(|b| i16::from_le_bytes(b.try_into().unwrap()) as f32 / 32768.0).collect(),
        SampleKind::I32 => bytes.chunks_exact(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2147483648.0).collect(),
    };
    samples.chunks_exact(channels).flat_map(|f| [f[0], f[(channels > 1) as usize]]).collect()
}

/// Streaming linear resampler for interleaved stereo; keeps its phase between chunks.
pub struct StereoResampler {
    step: f64,
    pos: f64,
    prev: [f32; 2],
}

impl StereoResampler {
    pub fn new(from: u32, to: u32) -> Self {
        StereoResampler { step: from as f64 / to as f64, pos: 1.0, prev: [0.0; 2] }
    }

    /// `pos` is measured in input frames where frame 0 is the previous chunk's last frame.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = input.len() / 2;
        if n == 0 {
            return;
        }
        if self.step == 1.0 {
            out.extend_from_slice(&input[..n * 2]);
            return;
        }
        let at = |i: usize, c: usize| if i == 0 { self.prev[c] } else { input[(i - 1) * 2 + c] };
        while self.pos <= n as f64 {
            let i0 = self.pos.floor() as usize;
            let frac = (self.pos - i0 as f64) as f32;
            for c in 0..2 {
                let a = at(i0, c);
                let b = if i0 < n { at(i0 + 1, c) } else { a };
                out.push(a + (b - a) * frac);
            }
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = [input[(n - 1) * 2], input[(n - 1) * 2 + 1]];
    }
}

/// A running capture feeding a queue of 48 kHz interleaved stereo f32.
pub struct Stream {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
    pub queue: Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>,
}

impl Stream {
    pub fn start(loopback: bool) -> Stream {
        let stop = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let (s, q) = (stop.clone(), queue.clone());
        let thread = std::thread::spawn(move || {
            let mut rs: Option<StereoResampler> = None;
            let mut tmp = Vec::new();
            let r = capture(loopback, &s, |bytes, frames, f| {
                let rs = rs.get_or_insert_with(|| StereoResampler::new(f.rate, REC_RATE));
                let st = match bytes {
                    Some(b) => to_stereo(b, f.kind, f.channels),
                    None => vec![0.0; frames * 2],
                };
                tmp.clear();
                rs.push(&st, &mut tmp);
                q.lock().unwrap().extend(tmp.iter());
            });
            if let Err(e) = &r {
                log::warn!("record audio ({}): {e}", if loopback { "system" } else { "mic" });
            }
            r
        });
        Stream { stop, thread: Some(thread), queue }
    }

    /// Takes up to `n` samples (shorter if the device has not delivered them yet).
    pub fn take(&self, n: usize) -> Vec<f32> {
        let mut q = self.queue.lock().unwrap();
        let n = n.min(q.len());
        q.drain(..n).collect()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Sums sources sample by sample, padding short ones with silence, and converts to i16.
pub fn mix_i16(sources: &[Vec<f32>], len: usize) -> Vec<i16> {
    (0..len)
        .map(|i| {
            let s: f32 = sources.iter().map(|v| v.get(i).copied().unwrap_or(0.0)).sum();
            (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_conversion_and_mix() {
        let mono: Vec<u8> = [16384i16, -16384].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(to_stereo(&mono, SampleKind::I16, 1), vec![0.5, 0.5, -0.5, -0.5]);
        let six: Vec<u8> = (0..6).flat_map(|i| (i as f32).to_le_bytes()).collect();
        assert_eq!(to_stereo(&six, SampleKind::F32, 6), vec![0.0, 1.0]);
        assert_eq!(mix_i16(&[vec![0.5, 0.75], vec![0.25]], 3), vec![24575, 24575, 0]);
    }

    #[test]
    fn streaming_resampler_matches_whole_and_keeps_rate() {
        // 44.1 kHz ramp in uneven chunks: output length tracks the rate, values stay on the ramp.
        let input: Vec<f32> = (0..4410).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let mut rs = StereoResampler::new(44100, 48000);
        let mut out = Vec::new();
        for c in input.chunks(2 * 137) {
            rs.push(c, &mut out);
        }
        let frames = out.len() / 2;
        assert!((frames as i64 - 4800).abs() <= 2, "{frames}");
        for (k, f) in out.chunks(2).enumerate() {
            let expect = k as f64 * 44100.0 / 48000.0;
            assert!((f[0] as f64 - expect).abs() < 1e-3, "{k}: {} vs {expect}", f[0]);
            assert_eq!(f[1], -f[0]);
        }
        let mut same = StereoResampler::new(48000, 48000);
        let mut o = Vec::new();
        same.push(&[1.0, 2.0], &mut o);
        assert_eq!(o, vec![1.0, 2.0]);
    }

    #[test]
    fn stereo_f32_downmix() {
        let frames: Vec<u8> = [0.5f32, -0.5, 1.0, 0.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        assert_eq!(to_mono(&frames, SampleKind::F32, 2), vec![0.0, 0.5]);
    }

    #[test]
    fn i16_scaling() {
        let b: Vec<u8> = [16384i16, -32768].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(to_mono(&b, SampleKind::I16, 1), vec![0.5, -1.0]);
    }

    #[test]
    fn resample_48k_to_16k() {
        let input: Vec<f32> = (0..48).map(|i| i as f32).collect();
        let out = resample(&input, 48000, 16000);
        assert_eq!(out.len(), 16);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[5], 15.0);
        // Upsampling interpolates between neighbours.
        assert_eq!(resample(&[0.0, 1.0], 1, 2), vec![0.0, 0.5, 1.0, 1.0]);
        assert!(resample(&[], 48000, 16000).is_empty());
    }
}
