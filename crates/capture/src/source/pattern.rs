//! A synthetic capture source for tests (S49): `RELAY_TEST_SOURCE=noise`.
//!
//! A loopback share on the development PC used to capture the owner's own
//! desktop, which is mostly still — 3-5 Mb/s instead of the 25-60 the matrix
//! runs at — and depends on what is on screen. This source needs nothing on
//! screen and keeps the encoder busy the way a game does: the whole picture
//! moves every frame (a scrolling field of block noise, which motion
//! compensation can follow), and a band of brand-new detail sweeps down it
//! (which it cannot), so the encoder has to work for every frame and its
//! output follows the bitrate it is given. A first cut that showed unrelated
//! noise every frame was a scene cut sixty times a second: no encoder can
//! hold a target on that, and it measured the encoder, not the link.
//!
//! `noise` uses the monitor's size; `noise:WIDTHxHEIGHT` sets one. `still`
//! (same size forms) delivers the first frame and then nothing, the way WGC
//! behaves on a window that does not change (r60: a ladder step on a still
//! window left the receiver with nothing for 3 s).

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use super::{CapturedFrame, FrameSource};

/// Textures the fresh band is drawn from, in turn.
const DETAIL: usize = 4;
/// The fresh band is this fraction of the picture's height.
const BANDS: u32 = 16;
/// Pixels the field scrolls per frame, and how far before it wraps.
const SPEED: u32 = 6;
const SCROLL_SPAN: u32 = 600;
/// Noise in square blocks: hard for the encoder, but not white noise.
const BLOCK: u32 = 8;
const FPS: u32 = 60;
const OUT_RING: usize = 3;

pub struct NoiseSource {
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    field: ID3D11Texture2D,
    detail: Vec<ID3D11Texture2D>,
    out: Vec<ID3D11Texture2D>,
    size: (u32, u32),
    frame: u64,
    due: Instant,
    still: bool,
}

/// `Some(size)` when `RELAY_TEST_SOURCE` asks for the noise source.
pub fn requested(monitor: (u32, u32)) -> Option<(u32, u32)> {
    let v = std::env::var("RELAY_TEST_SOURCE").ok()?;
    let rest = v.strip_prefix("noise").or_else(|| v.strip_prefix("still"))?;
    let Some(dims) = rest.strip_prefix(':') else { return Some(monitor) };
    let (w, h) = dims.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn texture(
    gpu: &crate::d3d::Gpu,
    (w, h): (u32, u32),
    seed: Option<&mut u64>,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut pixels = Vec::new();
    if let Some(seed) = seed {
        pixels = vec![0u32; (w * h) as usize];
        for by in 0..h.div_ceil(BLOCK) {
            for bx in 0..w.div_ceil(BLOCK) {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                let c = (*seed as u32) | 0xFF00_0000;
                for y in (by * BLOCK)..((by + 1) * BLOCK).min(h) {
                    let row = (y * w) as usize;
                    for x in (bx * BLOCK)..((bx + 1) * BLOCK).min(w) {
                        pixels[row + x as usize] = c;
                    }
                }
            }
        }
    }
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const _,
        SysMemPitch: w * 4,
        SysMemSlicePitch: 0,
    };
    let mut tex = None;
    // SAFETY: a valid descriptor; initial data, when given, outlives the call.
    unsafe {
        gpu.device.CreateTexture2D(
            &desc,
            (!pixels.is_empty()).then_some(&init as *const _),
            Some(&mut tex),
        )
    }
    .context("creating a test-pattern texture")?;
    tex.context("no texture")
}

impl NoiseSource {
    pub fn new(gpu: &crate::d3d::Gpu, size: (u32, u32)) -> Result<Self> {
        let (w, h) = (size.0 & !1, size.1 & !1);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let field = texture(gpu, (w + SCROLL_SPAN, h), Some(&mut seed))?;
        let detail = (0..DETAIL)
            .map(|_| texture(gpu, (w, h), Some(&mut seed)))
            .collect::<Result<Vec<_>>>()?;
        let out = (0..OUT_RING).map(|_| texture(gpu, (w, h), None)).collect::<Result<Vec<_>>>()?;
        tracing::warn!(w, h, "TEST: capturing a synthetic moving pattern, not the screen");
        Ok(Self {
            context: gpu.context.clone(),
            field,
            detail,
            out,
            size: (w, h),
            frame: 0,
            due: Instant::now(),
            still: std::env::var("RELAY_TEST_SOURCE").is_ok_and(|v| v.starts_with("still")),
        })
    }
}

impl FrameSource for NoiseSource {
    fn next(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        if self.still && self.frame > 0 {
            std::thread::sleep(timeout);
            return Ok(None);
        }
        let now = Instant::now();
        if self.due > now {
            let wait = self.due - now;
            if wait > timeout {
                std::thread::sleep(timeout);
                return Ok(None);
            }
            std::thread::sleep(wait);
        }
        self.due = self.due.max(now - Duration::from_millis(50)) + Duration::from_secs(1) / FPS;
        let (w, h) = self.size;
        let i = self.frame;
        self.frame += 1;
        let out = self.out[(i % OUT_RING as u64) as usize].clone();
        let left = (i as u32).wrapping_mul(SPEED) % SCROLL_SPAN;
        let band_h = (h / BANDS).max(2) & !1;
        let top = ((i as u32 % BANDS) * band_h).min(h - band_h);
        let field = D3D11_BOX { left, top: 0, front: 0, right: left + w, bottom: h, back: 1 };
        let band = D3D11_BOX { left: 0, top, front: 0, right: w, bottom: top + band_h, back: 1 };
        let detail = &self.detail[(i as usize / BANDS as usize) % DETAIL];
        // SAFETY: copies between live textures of this device, inside bounds.
        unsafe {
            self.context.CopySubresourceRegion(&out, 0, 0, 0, 0, &self.field, 0, Some(&field));
            self.context.CopySubresourceRegion(&out, 0, 0, top, 0, detail, 0, Some(&band));
        }
        Ok(Some(CapturedFrame {
            texture: out,
            width: w,
            height: h,
            qpc_100ns: crate::time::qpc_now_100ns(),
            wgc_frame: None,
        }))
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn dropped(&self) -> u64 {
        0
    }
}
