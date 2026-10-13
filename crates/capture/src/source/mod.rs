//! Frame sources. Windows.Graphics.Capture is the primary path; DXGI Desktop
//! Duplication is the fallback for systems where WGC is unavailable.
//!
//! A [`CapturedFrame`] is a GPU texture (BGRA8) plus the QPC presentation
//! timestamp. It is never copied to system memory on the send path; the
//! consumer converts it to NV12 on the GPU and closes it.

pub mod dxgi;
pub mod pattern;
pub mod switch;
pub mod wgc;

use std::time::Duration;

use anyhow::Result;
use windows::Graphics::Capture::Direct3D11CaptureFrame;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

pub struct CapturedFrame {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    /// Presentation time in QPC 100 ns ticks (same clock as
    /// [`crate::time::qpc_now_100ns`]).
    pub qpc_100ns: i64,
    /// Keeps the underlying frame alive; closed on drop so the pool buffer
    /// recycles immediately.
    wgc_frame: Option<Direct3D11CaptureFrame>,
}

// SAFETY: the COM/WinRT pointers inside are agile (D3D11 is free-threaded and
// the frame pool is created free-threaded); the frame is only ever owned by
// one thread at a time.
unsafe impl Send for CapturedFrame {}

impl Drop for CapturedFrame {
    fn drop(&mut self) {
        if let Some(f) = self.wgc_frame.take() {
            let _ = f.Close();
        }
    }
}

/// WGC when available (it is, on Win10 1903+), Desktop Duplication otherwise.
pub fn create(
    gpu: &crate::d3d::Gpu,
    hmonitor: windows::Win32::Graphics::Gdi::HMONITOR,
    cursor: bool,
) -> anyhow::Result<Box<dyn FrameSource>> {
    if std::env::var_os("RELAY_TEST_SOURCE").is_some() {
        let monitor = crate::d3d::monitor_size(hmonitor).unwrap_or((1920, 1080));
        if let Some(size) = pattern::requested(monitor) {
            return Ok(Box::new(pattern::NoiseSource::new(gpu, size)?));
        }
    }
    let force_dxgi = std::env::var("RELAY_CAPTURE").is_ok_and(|v| v == "dxgi");
    if crate::probe::wgc_supported() && !force_dxgi {
        Ok(Box::new(wgc::WgcCapture::monitor(gpu, hmonitor, cursor)?))
    } else {
        tracing::info!("WGC unavailable; falling back to DXGI Desktop Duplication");
        Ok(Box::new(dxgi::DxgiCapture::monitor(gpu, hmonitor)?))
    }
}

pub trait FrameSource: Send {
    /// Wait up to `timeout` for the next frame. `Ok(None)` on timeout.
    fn next(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>>;
    fn size(&self) -> (u32, u32);
    /// Frames the source discarded because the consumer was busy (the
    /// zero-copy way to drop from refresh rate to target fps).
    fn dropped(&self) -> u64;
}
