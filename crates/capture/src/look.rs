//! S47: the look sampler (`relay-share look`).
//!
//! Spawned by the core only while a game with "Learn this game's look" on
//! has focus, killed on blur. Once per interval it takes the latest frame of
//! the game's monitor through DXGI Desktop Duplication, scales it to
//! 480×270 on the GPU (the preview tap's video processor), reads that back,
//! analyses it with `relay_display::learn` and prints one line of numbers.
//! The frame itself is dropped straight away: nothing is written to disk and
//! no pixels leave this process.
//!
//! Why Desktop Duplication and not Windows.Graphics.Capture: duplication is
//! output-level (it never names or touches the game's window or process),
//! it coalesces updates on its own so sampling at 1 fps costs nothing in
//! between, and it draws no capture border. WGC would need a capture item
//! for the game window — the same OS broker, but aimed at the game.
//!
//! HDR: an output in PQ/BT.2020 mode does not hand out SDR code values.
//! Unless the core passed `--hdr` (the user chose HDR learning for this
//! game), the sampler says `look_hdr` and exits rather than learn from the
//! wrong space. With `--hdr` it duplicates the output as scRGB half floats,
//! box-filters it down on the GPU with `GenerateMips` (no shaders), reads
//! back one small mip and hands it to `relay_display::learn::hdr`, which
//! encodes it for the same analyser. Its lines carry `"hdr": true`.

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use relay_display::learn::{hdr, with_input_idle, Analyser, Frame, Order};
use windows::core::Interface;
use windows::Win32::Foundation::DXGI_STATUS_OCCLUDED;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11ShaderResourceView, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_RESOURCE_MISC_GENERATE_MIPS, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput5, IDXGIOutput6, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::HMONITOR;

use crate::d3d;
use crate::preview::Preview;
use crate::source::dxgi::DxgiCapture;
use crate::source::FrameSource;

/// Highest sampling rate accepted. More would not learn faster and would
/// start to cost something.
pub const MAX_FPS: u32 = 2;

/// Is this monitor's output in HDR (PQ) mode?
pub fn output_is_hdr(hmonitor: HMONITOR) -> Result<bool> {
    let output = d3d::output_for_monitor(hmonitor)?;
    let Ok(o6) = output.cast::<IDXGIOutput6>() else { return Ok(false) };
    // SAFETY: plain descriptor query on a live output.
    let desc = unsafe { o6.GetDesc1() }.context("GetDesc1")?;
    Ok(relay_display::learn::is_hdr_color_space(desc.ColorSpace.0))
}

/// The panel's peak in nits as Windows reports it (EDID HDR metadata).
fn output_peak_nits(hmonitor: HMONITOR) -> f32 {
    let peak = d3d::output_for_monitor(hmonitor)
        .ok()
        .and_then(|o| o.cast::<IDXGIOutput6>().ok())
        // SAFETY: plain descriptor query on a live output.
        .and_then(|o6| unsafe { o6.GetDesc1() }.ok())
        .map(|d| d.MaxLuminance)
        .unwrap_or(0.0);
    hdr::peak_nits(peak)
}

/// The HDR mip is the smallest level whose longest edge is still at least
/// this; the chain halves, so it lands between this and twice it.
const HDR_MIN_EDGE: u32 = crate::preview::MAX_EDGE;

/// The mip level whose longest edge is the smallest still >= `min_edge`.
pub fn hdr_mip_level(size: (u32, u32), min_edge: u32) -> u32 {
    let long = size.0.max(size.1);
    let mut level = 0;
    while (long >> (level + 1)) >= min_edge {
        level += 1;
    }
    level
}

/// Desktop duplication of an HDR output as scRGB, scaled down on the GPU.
struct HdrSource {
    duplication: IDXGIOutputDuplication,
    mips: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    staging: ID3D11Texture2D,
    level: u32,
    small: (u32, u32),
    peak: f32,
}

impl HdrSource {
    fn new(gpu: &d3d::Gpu, hmonitor: HMONITOR) -> Result<Self> {
        let output5: IDXGIOutput5 = d3d::output_for_monitor(hmonitor)?.cast()?;
        // SAFETY: duplicating on our own device, asking for the FP16 format.
        let duplication =
            unsafe { output5.DuplicateOutput1(&gpu.device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT]) }
                .context("DuplicateOutput1 (HDR)")?;
        // SAFETY: plain descriptor query.
        let desc = unsafe { duplication.GetDesc() };
        let size = (desc.ModeDesc.Width, desc.ModeDesc.Height);
        let level = hdr_mip_level(size, HDR_MIN_EDGE);
        let small = ((size.0 >> level).max(1), (size.1 >> level).max(1));
        let tex = |w: u32, h: u32, levels: u32, usage, bind: u32, cpu: u32, misc: u32| {
            let d = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: levels,
                ArraySize: 1,
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: usage,
                BindFlags: bind,
                CPUAccessFlags: cpu,
                MiscFlags: misc,
            };
            let mut t = None;
            // SAFETY: valid descriptor; the out pointer is ours.
            unsafe { gpu.device.CreateTexture2D(&d, None, Some(&mut t)) }.map(|()| t.unwrap())
        };
        let mips = tex(
            size.0,
            size.1,
            level + 1,
            D3D11_USAGE_DEFAULT,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            0,
            D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
        )
        .context("HDR mip texture")?;
        let staging =
            tex(small.0, small.1, 1, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32, 0)
                .context("HDR staging texture")?;
        let mut srv = None;
        // SAFETY: a view of our own texture with its default description.
        unsafe { gpu.device.CreateShaderResourceView(&mips, None, Some(&mut srv)) }
            .context("HDR mip view")?;
        Ok(Self {
            duplication,
            mips,
            srv: srv.unwrap(),
            staging,
            level,
            small,
            peak: output_peak_nits(hmonitor),
        })
    }

    /// The newest frame as packed RGB display codes, or `None` when nothing
    /// new was presented. An error means "rebuild".
    fn next(&mut self, gpu: &d3d::Gpu, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: the standard acquire/copy/release sequence on our device;
        // the map is released before the slice goes out of scope.
        unsafe {
            match self.duplication.AcquireNextFrame(
                timeout.as_millis() as u32,
                &mut info,
                &mut resource,
            ) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(e) if e.code() == DXGI_STATUS_OCCLUDED => return Ok(None),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    anyhow::bail!("desktop duplication access lost (mode change?)")
                }
                Err(e) => return Err(e).context("AcquireNextFrame (HDR)"),
            }
            let src: ID3D11Texture2D = resource.context("no resource")?.cast()?;
            gpu.context.CopySubresourceRegion(&self.mips, 0, 0, 0, 0, &src, 0, None);
            drop(src);
            self.duplication.ReleaseFrame()?;
            if info.LastPresentTime == 0 {
                return Ok(None); // a mouse move only
            }
            gpu.context.GenerateMips(&self.srv);
            gpu.context.CopySubresourceRegion(
                &self.staging,
                0,
                0,
                0,
                0,
                &self.mips,
                self.level,
                None,
            );
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            gpu.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("mapping the HDR staging texture")?;
            let (w, h) = (self.small.0 as usize, self.small.1 as usize);
            let pitch = mapped.RowPitch as usize;
            let data =
                std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (h - 1) + w * 8);
            let out = hdr::to_display_codes(data, w, h, pitch, self.peak);
            gpu.context.Unmap(&self.staging, 0);
            Ok(Some(out))
        }
    }
}

/// Milliseconds since the last keyboard/mouse input anywhere on the system.
/// One system-wide timestamp: no hook, no key data.
fn input_idle_ms() -> Option<u64> {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: a sized out-struct of our own.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return None;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount() };
    Some(now.wrapping_sub(info.dwTime) as u64)
}

fn lower_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    };
    // SAFETY: our own pseudo-handle.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
}

/// Run until stdin closes (the core went away or stopped us). `allow_hdr`:
/// the user chose HDR learning for this game.
pub fn run(hmonitor: HMONITOR, fps: u32, allow_hdr: bool) -> Result<()> {
    let fps = fps.clamp(1, MAX_FPS);
    lower_priority();
    // DuplicateOutput1 (the HDR path) refuses a process that is not
    // per-monitor DPI aware v2. Harmless for the SDR path: this process has
    // no windows.
    // SAFETY: a process-wide flag set once, before any window or device.
    let _ = unsafe {
        windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    };

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let paused = paused.clone();
        std::thread::Builder::new().name("look-stdin".into()).spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) if l.trim() == "stop" => break,
                    // Alt-Tab grace: the game is out of focus; sample nothing
                    // until it comes back (the core resumes or stops us).
                    Ok(l) if l.trim() == "pause" => paused.store(true, Ordering::SeqCst),
                    Ok(l) if l.trim() == "resume" => paused.store(false, Ordering::SeqCst),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            stop.store(true, Ordering::SeqCst);
        })?;
    }

    let interval = Duration::from_millis(1000 / fps as u64);
    let mut analyser = Analyser::new();
    while !stop.load(Ordering::SeqCst) {
        // (Re)build on start and after a mode change or exclusive fullscreen
        // took the output away. HDR is checked on every rebuild, since
        // switching it is a mode change. An output whose colour space cannot
        // be read counts as HDR: learning from the wrong space is worse than
        // not learning.
        let is_hdr = output_is_hdr(hmonitor).unwrap_or(true);
        if is_hdr && !allow_hdr {
            println!("{}", serde_json::json!({ "event": "look_hdr" }));
            return Ok(());
        }
        let gpu = d3d::device_for_monitor(hmonitor)?;
        if is_hdr {
            match run_hdr(&gpu, hmonitor, interval, &stop, &paused) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "HDR duplication unavailable; retrying");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
            analyser = Analyser::new();
            continue;
        }
        let mut src = match DxgiCapture::monitor(&gpu, hmonitor) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "duplication unavailable; retrying");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let mut preview = Preview::new(&gpu, src.size())?;
        let (w, h) = preview.size();
        let mut next = Instant::now();
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if paused.load(Ordering::SeqCst) {
                std::thread::sleep(interval);
                next = Instant::now();
                // Whatever is on screen now is not the game: no motion
                // reference or content region carries across the pause.
                analyser = Analyser::new();
                continue;
            }
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
            }
            next += interval;
            let frame = match src.next(Duration::from_millis(50)) {
                Ok(Some(f)) => f,
                Ok(None) => continue, // nothing new on screen: no sample
                Err(e) => {
                    tracing::info!(error = %e, "duplication lost; rebuilding");
                    break;
                }
            };
            let bgr = preview.bgr(&gpu, &frame.texture)?;
            drop(frame);
            let report = analyser.analyse(&Frame {
                width: w as usize,
                height: h as usize,
                data: &bgr,
                order: Order::Bgr,
            });
            drop(bgr);
            let report = with_input_idle(report, input_idle_ms());
            println!("{}", serde_json::json!({ "event": "look", "report": report }));
        }
        analyser = Analyser::new();
    }
    Ok(())
}

/// One HDR duplication session. `Ok(true)` = stopped; `Ok(false)` = the
/// output changed, rebuild.
fn run_hdr(
    gpu: &d3d::Gpu,
    hmonitor: HMONITOR,
    interval: Duration,
    stop: &AtomicBool,
    paused: &AtomicBool,
) -> Result<bool> {
    let mut src = HdrSource::new(gpu, hmonitor)?;
    tracing::info!(peak_nits = src.peak, mip = src.level, size = ?src.small, "sampling in HDR");
    let crush = hdr::crush_threshold(src.peak);
    let mut analyser = Analyser::with_crush(crush);
    let (w, h) = src.small;
    let mut next = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(true);
        }
        if paused.load(Ordering::SeqCst) {
            std::thread::sleep(interval);
            next = Instant::now();
            analyser = Analyser::with_crush(crush);
            continue;
        }
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        next += interval;
        let rgb = match src.next(gpu, Duration::from_millis(50)) {
            Ok(Some(f)) => f,
            Ok(None) => continue,
            Err(e) => {
                tracing::info!(error = %e, "HDR duplication lost; rebuilding");
                return Ok(false);
            }
        };
        let report = analyser.analyse(&Frame {
            width: w as usize,
            height: h as usize,
            data: &rgb,
            order: Order::Rgb,
        });
        drop(rgb);
        let report = with_input_idle(report, input_idle_ms());
        println!("{}", serde_json::json!({ "event": "look", "hdr": true, "report": report }));
    }
}

#[cfg(test)]
mod tests {
    use super::hdr_mip_level;

    #[test]
    fn mip_level_keeps_at_least_the_preview_edge() {
        assert_eq!(hdr_mip_level((3840, 2160), 480), 3); // 480×270
        assert_eq!(hdr_mip_level((2560, 1440), 480), 2); // 640×360
        assert_eq!(hdr_mip_level((1920, 1080), 480), 2); // 480×270
        assert_eq!(hdr_mip_level((5120, 1440), 480), 3); // 640×180
        assert_eq!(hdr_mip_level((400, 300), 480), 0);
    }
}
