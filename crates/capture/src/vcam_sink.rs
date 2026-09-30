//! Feed decoded receiver frames to the Relay virtual camera.
//!
//! The sink owns the `IMFVirtualCamera` (session lifetime: created when the
//! receive render loop comes up, gone when the process exits) and pushes
//! every decoded NV12 frame into the shared frame ring the camera media
//! source reads. GPU→CPU is one `CopySubresourceRegion` into a cached
//! staging texture plus a mapped row copy — the same frame the swapchain
//! presents, one extra copy, no extra decode.
//!
//! Everything here is best-effort: a missing registration, an unsupported
//! Windows build or a ring that is not up yet must never kill the receive
//! window. Failures surface once as an NDJSON event and the sink goes
//! dormant.
//!
//! [`RingWriter`] is the camera-free half (staging copy → ring), split out
//! so the pixel path is testable without the frame server.

#![allow(unsafe_code)] // D3D11 staging copy + map; SAFETY notes inline

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{info, warn};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};

use relay_vdevice::camera::control::VirtualCamera;
use relay_vdevice::detect::CameraPath;
use relay_vdevice::frames::{dshow_section_name_from_env, section_name_from_env, SharedFrames};

/// How often to retry attaching the frame ring while the camera spins up.
const ATTACH_RETRY: Duration = Duration::from_millis(500);

/// Staging-copies decoded NV12 textures into the shared frame ring.
pub struct RingWriter {
    ring: Option<SharedFrames>,
    /// Section to attach to: `Global\` for the frame-server camera,
    /// `Local\` for the DirectShow one (S43).
    ring_name: String,
    /// Geometry announced in the ring header on attach, so the DirectShow
    /// filter can offer the stream's size before its first frame.
    hint: Option<(u32, u32, u32)>,
    last_attach: Option<Instant>,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    frames_written: u64,
}

impl Default for RingWriter {
    fn default() -> Self {
        Self::named(section_name_from_env(), None)
    }
}

impl RingWriter {
    fn named(ring_name: String, hint: Option<(u32, u32, u32)>) -> Self {
        Self { ring: None, ring_name, hint, last_attach: None, staging: None, frames_written: 0 }
    }

    /// Use an already-mapped section (tests use a `Local\` one they created).
    pub fn with_ring(ring: SharedFrames) -> Self {
        Self { ring: Some(ring), ..Self::named(String::new(), None) }
    }

    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    /// Copy one decoded frame into the ring. `Ok(false)` = skipped (ring not
    /// reachable yet, or the frame was rejected); errors are D3D failures.
    pub fn push(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        frame: &crate::decode::mf::DecodedFrame,
    ) -> Result<bool> {
        // Attach the ring lazily: the media source creates the section when
        // the frame server activates it, which races our first frames.
        if self.ring.is_none() {
            if self.last_attach.is_some_and(|t| t.elapsed() < ATTACH_RETRY) {
                return Ok(false);
            }
            self.last_attach = Some(Instant::now());
            match SharedFrames::create(&self.ring_name) {
                Ok(s) => {
                    if let Some((w, h, fps)) = self.hint {
                        s.block().set_geometry_hint(w, h, fps);
                    }
                    self.ring = Some(s)
                }
                Err(_) => return Ok(false), // camera not consumed yet
            }
        }

        // SAFETY: live textures on one thread; staging desc mirrors the
        // decoder texture; map/unmap balanced.
        let ok = unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            frame.texture.GetDesc(&mut desc);

            if self.staging.as_ref().map(|(_, w, h)| (*w, *h)) != Some((desc.Width, desc.Height)) {
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: desc.Format,
                    SampleDesc: desc.SampleDesc,
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut tex = None;
                device.CreateTexture2D(&staging_desc, None, Some(&mut tex))?;
                self.staging = Some((tex.context("staging texture")?, desc.Width, desc.Height));
            }
            let (staging, _, tex_h) = self.staging.as_ref().expect("just set");

            context.CopySubresourceRegion(
                staging,
                0,
                0,
                0,
                0,
                &frame.texture,
                frame.subresource,
                None,
            );

            let mut mapped = Default::default();
            context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let pitch = mapped.RowPitch as usize;
            // Staging NV12 layout: Y rows [0, tex_h), UV rows follow at the
            // same pitch. Visible frame is a top-left prefix of the aligned
            // texture.
            let y = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * *tex_h as usize);
            let uv = std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(pitch * *tex_h as usize),
                pitch * (*tex_h as usize / 2),
            );
            let ok = self.ring.as_ref().expect("attached above").block().write_frame(
                frame.width,
                frame.height,
                frame.pts_100ns,
                y,
                pitch,
                uv,
                pitch,
            );
            context.Unmap(staging, 0);
            ok
        };
        if ok {
            self.frames_written += 1;
        } else {
            warn!(w = frame.width, h = frame.height, "frame rejected by ring");
        }
        Ok(ok)
    }
}

pub struct VcamSink {
    /// `None` on the DirectShow path: the app that opens Relay Camera
    /// loads the filter itself; this side only writes the ring.
    _camera: Option<VirtualCamera>,
    writer: RingWriter,
}

impl VcamSink {
    /// Create and start "Relay Camera" for a `width`×`height` stream.
    /// Requires the media source registered (HKLM) and Windows 11 22H2+;
    /// the caller reports the error and continues without a camera.
    pub fn start(width: u32, height: u32, fps: u32) -> Result<Self> {
        match relay_vdevice::detect::camera_path() {
            CameraPath::FrameServer => {
                let camera = VirtualCamera::start(width, height, fps)
                    .context("MFCreateVirtualCamera (is the camera registered?)")?;
                info!(width, height, fps, "virtual camera started (frame server)");
                Ok(Self { _camera: Some(camera), writer: RingWriter::default() })
            }
            CameraPath::DirectShow => Ok(Self::start_dshow(width, height, fps)),
        }
    }

    /// The Windows 10 path (S43): no camera object to create — the per-user
    /// DirectShow filter is loaded by whichever app opens "Relay Camera" and
    /// reads the `Local\` ring this writes. The size is announced in the
    /// ring header so the app's format list matches the stream.
    pub fn start_dshow(width: u32, height: u32, fps: u32) -> Self {
        info!(width, height, fps, "Relay Camera ring up (DirectShow filter path)");
        Self {
            _camera: None,
            writer: RingWriter::named(dshow_section_name_from_env(), Some((width, height, fps))),
        }
    }

    pub fn frames_written(&self) -> u64 {
        self.writer.frames_written()
    }

    /// Push one decoded frame. Errors are D3D failures (the caller drops the
    /// sink); a ring that is not up yet is simply skipped.
    pub fn push(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        frame: &crate::decode::mf::DecodedFrame,
    ) -> Result<()> {
        self.writer.push(device, context, frame).map(|_| ())
    }
}
