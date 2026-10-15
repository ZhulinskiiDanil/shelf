//! Dev tool: records an area with sound, shrinks the result to a size limit and makes a
//! GIF, the same way Shelf does after Ctrl+R, without driving the UI.
//! Usage: cargo run --release --example rec_check -- out_dir [seconds] [limit_mb] [audio]

#![allow(dead_code)]
#[path = "../src/audio.rs"]
mod audio;
#[path = "../src/clipboard.rs"]
mod clipboard;
#[path = "../src/geom.rs"]
mod geom;
#[path = "../src/gifout.rs"]
mod gifout;
#[path = "../src/imageio.rs"]
mod imageio;
#[path = "../src/record.rs"]
mod record;
#[path = "../src/transcode.rs"]
mod transcode;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[1]);
    let secs: f64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let limit: f64 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let audio = record::AudioSources::parse(a.get(4).map(|s| s.as_str()).unwrap_or("system"));
    record::start(geom::Rect::new(0, 0, 1280, 720), 30, true, audio).expect("start");
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    let (tmp, _) = record::stop().unwrap().expect("record");
    let raw = dir.join("raw.mp4");
    std::fs::copy(&tmp, &raw).unwrap();
    transcode::init();
    println!("raw: {} bytes, {:?}", std::fs::metadata(&raw).unwrap().len(), transcode::probe(&raw).unwrap());
    let t = std::time::Instant::now();
    let got = transcode::fit_to_limit(&tmp, (limit * 1e6) as u64).unwrap();
    let fit = dir.join("fit.mp4");
    std::fs::rename(&tmp, &fit).unwrap();
    println!("fit: {got} bytes in {} ms, {:?}", t.elapsed().as_millis(), transcode::probe(&fit).unwrap());
    let cut = dir.join("cut.mp4");
    let info = transcode::probe(&raw).unwrap();
    transcode::transcode(&raw, &cut, &[(5_000_000, 15_000_000), (info.duration - 10_000_000, info.duration)], 2_000_000, (info.w, info.h), true).unwrap();
    println!("cut: {:?}", transcode::probe(&cut).unwrap());
    let t = std::time::Instant::now();
    let (gif, first) = gifout::from_video(&raw, 15, 720, (limit * 1e6) as u64).unwrap();
    std::fs::write(dir.join("out.gif"), &gif).unwrap();
    std::fs::write(dir.join("first.png"), imageio::encode_png(&first, png::Compression::Fast)).unwrap();
    println!("gif: {} bytes {}x{} in {} ms", gif.len(), first.w, first.h, t.elapsed().as_millis());
}
