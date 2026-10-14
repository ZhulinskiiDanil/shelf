//! Dev tool: grabs the primary output via DXGI Desktop Duplication (the path OBS and
//! Discord use) into a PNG, to verify that the ink layer is visible to screen sharing.
//! Usage: cargo run --release --example dda_grab -- out.png [delay_ms]

use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::*;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or("dda.png".into());
    let delay = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(delay));
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut dev = None;
        let mut ctx = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, Some(&mut ctx))?;
        let (dev, ctx) = (dev.unwrap(), ctx.unwrap());
        let dxgi: IDXGIDevice = dev.cast()?;
        let output: IDXGIOutput1 = dxgi.GetAdapter()?.EnumOutputs(0)?.cast()?;
        let dup = output.DuplicateOutput(&dev)?;
        // The first frames after DuplicateOutput can be empty; wait for one with content.
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res = None;
        for _ in 0..20 {
            res = None;
            if dup.AcquireNextFrame(500, &mut info, &mut res).is_ok() && info.LastPresentTime != 0 {
                break;
            }
            if res.is_some() {
                let _ = dup.ReleaseFrame();
            }
        }
        let tex: ID3D11Texture2D = res.unwrap().cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        dev.CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.unwrap();
        ctx.CopyResource(&staging, &tex);
        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
        let (w, h) = (desc.Width as usize, desc.Height as usize);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            let row = std::slice::from_raw_parts((map.pData as *const u8).add(y * map.RowPitch as usize), w * 4);
            for p in row.chunks_exact(4) {
                rgb.extend_from_slice(&[p[2], p[1], p[0]]);
            }
        }
        ctx.Unmap(&staging, 0);
        let _ = desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM;
        let file = std::fs::File::create(&out).unwrap();
        let mut enc = png::Encoder::new(file, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
        println!("{out}: {w}x{h}");
    }
    Ok(())
}
