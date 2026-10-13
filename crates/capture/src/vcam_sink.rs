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
//!
//! Size (r54): the camera that opened the ring may ask for a smaller frame
//! (the ring's size request: the size the app negotiated, 1080p for a 1440p
//! stream). The writer then scales on the GPU with the D3D11 video
//! processor — one blit, BT.709 limited range in and out, aspect kept with
//! black bars — and copies the smaller frame, so the app's own threads never
//! scale anything and the staging copy shrinks too. If the processor cannot
//! be made, it falls back once, for good, to native frames, which the camera
//! then area-scales on the CPU.

#![allow(unsafe_code)] // D3D11 staging copy + map; SAFETY notes inline

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{info, warn};
use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ,
    D3D11_MAP_READ, D3D11_TEX2D_VPIV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0, D3D11_VIDEO_COLOR_RGBA,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};

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
    /// The GPU downscaler for the camera's requested size, rebuilt when the
    /// sizes change; `gpu_failed` stops retrying after one failure.
    scaler: Option<GpuScaler>,
    gpu_failed: bool,
    frames_written: u64,
}

/// NV12 → NV12 resize on the D3D11 video processor (r54).
struct GpuScaler {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    out: ID3D11Texture2D,
    /// (input w, h, output w, h).
    key: (u32, u32, u32, u32),
}

/// BT.709, studio range (16-235): what the decoder hands over and the ring
/// carries. The processor defaults to BT.601, which would shift colours.
fn bt709_limited() -> D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
    // Bits: Usage 0, RGB_Range 1, YCbCr_Matrix 2 (1 = BT.709),
    // YCbCr_xvYCC 3, Nominal_Range 4-5 (1 = 16-235).
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) }
}

impl GpuScaler {
    fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        key: (u32, u32, u32, u32),
    ) -> Result<Self> {
        let (iw, ih, ow, oh) = key;
        let video_device: ID3D11VideoDevice = device.cast().context("ID3D11VideoDevice")?;
        let video_context: ID3D11VideoContext = context.cast().context("ID3D11VideoContext")?;
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            InputWidth: iw,
            InputHeight: ih,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            OutputWidth: ow,
            OutputHeight: oh,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: ow,
            Height: oh,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        // SAFETY: object creation with valid descriptors; state set on the
        // live processor.
        unsafe {
            let enumerator =
                video_device.CreateVideoProcessorEnumerator(&desc).context("enumerator")?;
            let processor =
                video_device.CreateVideoProcessor(&enumerator, 0).context("processor")?;
            let mut out = None;
            device.CreateTexture2D(&tex_desc, None, Some(&mut out)).context("output texture")?;
            let cs = bt709_limited();
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &cs);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &cs);
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 },
                },
            };
            video_context.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
            // The picture only (an H.264 1080p texture is 1088 rows), fitted
            // at its own aspect -- the same rectangle the camera's CPU
            // scaler uses, so either path gives the same bars.
            let src = RECT { left: 0, top: 0, right: iw as i32, bottom: ih as i32 };
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            let (x, y, w, h) = relay_vdevice::camera::picture::fit_rect((iw, ih), (ow, oh));
            let dst = RECT {
                left: x as i32,
                top: y as i32,
                right: (x + w) as i32,
                bottom: (y + h) as i32,
            };
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            Ok(Self {
                video_device,
                video_context,
                enumerator,
                processor,
                out: out.context("scaled texture")?,
                key,
            })
        }
    }

    /// Blit `frame` into the output texture.
    fn run(&self, frame: &crate::decode::mf::DecodedFrame) -> Result<()> {
        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: frame.subresource },
            },
        };
        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        // SAFETY: views on live textures, dropped after the blit; the input
        // view in the stream struct is released explicitly.
        unsafe {
            let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device
                .CreateVideoProcessorInputView(
                    &frame.texture,
                    &self.enumerator,
                    &in_desc,
                    Some(&mut in_view),
                )
                .context("input view")?;
            let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
            self.video_device
                .CreateVideoProcessorOutputView(
                    &self.out,
                    &self.enumerator,
                    &out_desc,
                    Some(&mut out_view),
                )
                .context("output view")?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(in_view),
                ..Default::default()
            };
            let r = self.video_context.VideoProcessorBlt(
                &self.processor,
                out_view.as_ref().context("output view")?,
                0,
                std::slice::from_ref(&stream),
            );
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            r.context("blit")?;
        }
        Ok(())
    }
}

impl Default for RingWriter {
    fn default() -> Self {
        Self::named(section_name_from_env(), None)
    }
}

impl RingWriter {
    fn named(ring_name: String, hint: Option<(u32, u32, u32)>) -> Self {
        Self {
            ring: None,
            ring_name,
            hint,
            last_attach: None,
            staging: None,
            scaler: None,
            gpu_failed: false,
            frames_written: 0,
        }
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

        // What the camera asked for: only ever smaller than the stream
        // (growing a picture on the producer helps nobody).
        let want = self.ring.as_ref().and_then(|r| r.block().requested_size()).filter(|&(w, h)| {
            w <= frame.width && h <= frame.height && (w, h) != (frame.width, frame.height)
        });
        let (src_tex, src_sub, out_w, out_h) = match want {
            Some((w, h)) if !self.gpu_failed => match self.gpu_scaled(device, context, frame, w, h)
            {
                Ok(tex) => (tex, 0, w, h),
                Err(e) => {
                    warn!(error = %e, w, h, "GPU scaling for Relay Camera unavailable; the camera scales instead");
                    self.gpu_failed = true;
                    self.scaler = None;
                    (frame.texture.clone(), frame.subresource, frame.width, frame.height)
                }
            },
            _ => (frame.texture.clone(), frame.subresource, frame.width, frame.height),
        };

        // SAFETY: live textures on one thread; staging desc mirrors the
        // source texture; map/unmap balanced.
        let ok = unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            src_tex.GetDesc(&mut desc);

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

            context.CopySubresourceRegion(staging, 0, 0, 0, 0, &src_tex, src_sub, None);

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
                out_w,
                out_h,
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
            warn!(w = out_w, h = out_h, "frame rejected by ring");
        }
        Ok(ok)
    }

    /// Scale `frame` to `w`×`h` on the GPU; the returned texture holds it
    /// until the next call.
    fn gpu_scaled(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        frame: &crate::decode::mf::DecodedFrame,
        w: u32,
        h: u32,
    ) -> Result<ID3D11Texture2D> {
        let key = (frame.width, frame.height, w, h);
        if self.scaler.as_ref().map(|s| s.key) != Some(key) {
            self.scaler = None;
            self.scaler = Some(GpuScaler::new(device, context, key)?);
            info!(
                from_w = frame.width,
                from_h = frame.height,
                w,
                h,
                "Relay Camera: scaling on the GPU to the size the camera asked for"
            );
        }
        let s = self.scaler.as_ref().expect("just set");
        s.run(frame)?;
        Ok(s.out.clone())
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
