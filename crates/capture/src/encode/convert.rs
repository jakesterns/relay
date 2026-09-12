//! BGRA → NV12 on the GPU via the D3D11 video processor, with optional
//! scaling. The output ring keeps a few NV12 textures alive so the encoder
//! can still be reading one while the next is written.

use anyhow::{Context, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};

use crate::d3d::Gpu;

const RING: usize = 4;

pub struct Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
    pub out_width: u32,
    pub out_height: u32,
}

impl Converter {
    pub fn new(gpu: &Gpu, in_size: (u32, u32), out_size: (u32, u32)) -> Result<Self> {
        let video_device: ID3D11VideoDevice = gpu.device.cast().context("ID3D11VideoDevice")?;
        let video_context: ID3D11VideoContext = gpu.context.cast()?;

        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            OutputWidth: out_size.0,
            OutputHeight: out_size.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: plain object creation with valid descriptors.
        let (enumerator, processor) = unsafe {
            let e = video_device.CreateVideoProcessorEnumerator(&desc)?;
            let p = video_device.CreateVideoProcessor(&e, 0)?;
            (e, p)
        };

        let mut ring = Vec::with_capacity(RING);
        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: out_size.0,
            Height: out_size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        for _ in 0..RING {
            let mut tex = None;
            // SAFETY: valid descriptor, out pointer is ours.
            unsafe { gpu.device.CreateTexture2D(&tex_desc, None, Some(&mut tex)) }
                .context("creating NV12 target")?;
            ring.push(tex.unwrap());
        }

        Ok(Self {
            video_device,
            video_context,
            processor,
            enumerator,
            ring,
            next: 0,
            out_width: out_size.0,
            out_height: out_size.1,
        })
    }

    /// Crop the input to `rect` (left, top, width, height) before scaling —
    /// region capture. `None` restores full-frame conversion. Sticky until
    /// changed.
    pub fn set_source_rect(&mut self, rect: Option<(u32, u32, u32, u32)>) {
        use windows::Win32::Foundation::RECT;
        let (enable, r) = match rect {
            Some((x, y, w, h)) => (
                true,
                RECT {
                    left: x as i32,
                    top: y as i32,
                    right: (x + w) as i32,
                    bottom: (y + h) as i32,
                },
            ),
            None => (false, RECT::default()),
        };
        // SAFETY: stream 0 exists for the life of the processor.
        unsafe {
            self.video_context.VideoProcessorSetStreamSourceRect(
                &self.processor,
                0,
                enable,
                if enable { Some(&r) } else { None },
            );
        }
    }

    /// Convert (and scale) `src` into the next NV12 ring texture and return it.
    /// The returned texture stays valid until `RING - 1` further calls.
    pub fn convert(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let dst = self.ring[self.next].clone();
        self.next = (self.next + 1) % self.ring.len();

        let in_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: Default::default(),
        };
        let out_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        // SAFETY: views are created on live textures and dropped after the blt.
        unsafe {
            let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device.CreateVideoProcessorInputView(
                src,
                &self.enumerator,
                &in_view_desc,
                Some(&mut in_view),
            )?;
            let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
            self.video_device.CreateVideoProcessorOutputView(
                &dst,
                &self.enumerator,
                &out_view_desc,
                Some(&mut out_view),
            )?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(in_view),
                ..Default::default()
            };
            let result = self.video_context.VideoProcessorBlt(
                &self.processor,
                out_view.as_ref().unwrap(),
                0,
                std::slice::from_ref(&stream),
            );
            // Release the input view we wrapped in ManuallyDrop above.
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
        }
        Ok(dst)
    }
}
