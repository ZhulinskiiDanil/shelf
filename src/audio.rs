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
    let thread = std::thread::spawn(move || capture(&s));
    Recording { stop, thread: Some(thread) }
}

fn capture(stop: &AtomicBool) -> Result<(Vec<f32>, u32), String> {
    let e = |e: windows::core::Error| e.to_string();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(e)?;
        let dev = en.GetDefaultAudioEndpoint(eCapture, eConsole).map_err(|_| "no microphone".to_string())?;
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
                return Err("unsupported microphone format".into());
            }
        } else if wf.wBitsPerSample == 16 {
            SampleKind::I16
        } else {
            return Err("unsupported microphone format".into());
        };
        // 1 s buffer (in 100 ns units); we drain it every 10 ms.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 10_000_000, 0, fmt, None).map_err(e)?;
        let cap: IAudioCaptureClient = client.GetService().map_err(e)?;
        client.Start().map_err(e)?;
        let block = wf.nBlockAlign as usize;
        let mut mono = Vec::with_capacity(rate as usize * 30);
        loop {
            let done = stop.load(Ordering::Relaxed);
            while cap.GetNextPacketSize().map_err(e)? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None).map_err(e)?;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    mono.extend(std::iter::repeat_n(0.0, frames as usize));
                } else {
                    let bytes = std::slice::from_raw_parts(data, frames as usize * block);
                    mono.extend(to_mono(bytes, kind, channels));
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
        Ok((mono, rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
