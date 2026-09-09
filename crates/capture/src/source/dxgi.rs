//! DXGI Desktop Duplication fallback for systems without WGC.
//!
//! `AcquireNextFrame` returns a texture that is only valid until
//! `ReleaseFrame`, so the frame is copied GPU→GPU into a small ring and
//! released immediately. Still no CPU copies; duplication coalesces frames on
//! its own when the consumer is slow.

use std::time::Duration;

use anyhow::{Context, Result};
use windows::core::Interface;
use windows::Win32::Foundation::DXGI_STATUS_OCCLUDED;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::HMONITOR;

use super::{CapturedFrame, FrameSource};
use crate::d3d::Gpu;
use crate::time;

const RING: usize = 3;

pub struct DxgiCapture {
    duplication: IDXGIOutputDuplication,
    context: ID3D11DeviceContext,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
    size: (u32, u32),
}

impl DxgiCapture {
    pub fn monitor(gpu: &Gpu, hmonitor: HMONITOR) -> Result<Self> {
        let output = crate::d3d::output_for_monitor(hmonitor)?;
        let output1: IDXGIOutput1 = output.cast()?;
        // SAFETY: duplicating on our own device.
        let duplication =
            unsafe { output1.DuplicateOutput(&gpu.device) }.context("DuplicateOutput")?;
        // SAFETY: plain descriptor query.
        let desc = unsafe { duplication.GetDesc() };
        let size = (desc.ModeDesc.Width, desc.ModeDesc.Height);

        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.ModeDesc.Format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut ring = Vec::with_capacity(RING);
        for _ in 0..RING {
            let mut tex = None;
            // SAFETY: valid descriptor.
            unsafe { gpu.device.CreateTexture2D(&tex_desc, None, Some(&mut tex)) }?;
            ring.push(tex.unwrap());
        }
        Ok(Self { duplication, context: gpu.context.clone(), ring, next: 0, size })
    }
}

impl FrameSource for DxgiCapture {
    fn next(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: standard acquire/copy/release sequence on our device.
        unsafe {
            match self.duplication.AcquireNextFrame(
                timeout.as_millis() as u32,
                &mut info,
                &mut resource,
            ) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    anyhow::bail!("desktop duplication access lost (mode change?)")
                }
                Err(e) if e.code() == DXGI_STATUS_OCCLUDED => return Ok(None),
                Err(e) => return Err(e).context("AcquireNextFrame"),
            }
            let src: ID3D11Texture2D = resource.context("no resource")?.cast()?;
            let dst = self.ring[self.next].clone();
            self.next = (self.next + 1) % self.ring.len();
            self.context.CopyResource(&dst, &src);
            self.duplication.ReleaseFrame()?;

            // Mouse-move-only updates have LastPresentTime = 0; skip them.
            if info.LastPresentTime == 0 {
                return Ok(None);
            }
            Ok(Some(CapturedFrame {
                texture: dst,
                width: self.size.0,
                height: self.size.1,
                qpc_100ns: time::qpc_raw_to_100ns(info.LastPresentTime),
                wgc_frame: None,
            }))
        }
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn dropped(&self) -> u64 {
        0 // duplication coalesces internally; there is no per-frame drop count
    }
}
