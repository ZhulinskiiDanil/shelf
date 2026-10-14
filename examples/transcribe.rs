//! Dev tool: runs a whisper model on a 16-bit PCM WAV file, the same way dictation does.
//! Usage: cargo run --release --example transcribe -- model.bin speech.wav [auto|ru|en]

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let wav = std::fs::read(&a[2]).expect("wav");
    // Minimal RIFF parsing: find "fmt " and "data" chunks.
    let (mut ch, mut rate, mut data) = (1u16, 16000u32, &wav[..0]);
    let mut i = 12;
    while i + 8 <= wav.len() {
        let id = &wav[i..i + 4];
        let len = u32::from_le_bytes(wav[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &wav[i + 8..(i + 8 + len).min(wav.len())];
        if id == b"fmt " {
            ch = u16::from_le_bytes(body[2..4].try_into().unwrap());
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
        } else if id == b"data" {
            data = body;
        }
        i += 8 + len + (len & 1);
    }
    let mono: Vec<f32> = data
        .chunks_exact(2 * ch as usize)
        .map(|f| f.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0).sum::<f32>() / ch as f32)
        .collect();
    let step = rate as f64 / 16000.0;
    let pcm: Vec<f32> = (0..(mono.len() as f64 / step) as usize).map(|k| mono[((k as f64 * step) as usize).min(mono.len() - 1)]).collect();
    let t0 = std::time::Instant::now();
    let ctx = WhisperContext::new_with_params(&a[1], WhisperContextParameters::default()).expect("model");
    println!("load {:.2} s; {}", t0.elapsed().as_secs_f32(), whisper_rs::print_system_info());
    let mut st = ctx.create_state().unwrap();
    let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    p.set_language(Some(a.get(3).map(|s| s.as_str()).unwrap_or("auto")));
    p.set_n_threads(std::thread::available_parallelism().map(|n| n.get().min(8) as i32).unwrap_or(4));
    p.set_temperature_inc(0.0);
    p.set_print_progress(false);
    p.set_print_realtime(false);
    p.set_print_timestamps(false);
    let t1 = std::time::Instant::now();
    st.full(p, &pcm).unwrap();
    let text: String = st.as_iter().map(|s| s.to_string()).collect();
    println!("{:.1} s audio in {:.2} s: {}", pcm.len() as f32 / 16000.0, t1.elapsed().as_secs_f32(), text.trim());
}
