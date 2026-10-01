//! BGRA → NV12 on the GPU via the D3D11 video processor, with optional
//! scaling. The output ring keeps a few NV12 textures alive so the encoder
//! can still be reading one while the next is written.

use anyhow::{Context, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_COLOR,
    D3D11_VIDEO_COLOR_0, D3D11_VIDEO_COLOR_RGBA, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
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
    /// The whole input frame, for fitting when no crop is set.
    in_size: (u32, u32),
    pub out_width: u32,
    pub out_height: u32,
}

impl Converter {
    /// BGRA in, NV12 out — what the encoder wants, and what the preview
    /// thumbnail reads back. A BGRA output was tried for the preview and
    /// reverted: the processor accepts the output view, reports success and
    /// writes solid black.
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
                .context("creating the video-processor output texture")?;
            ring.push(tex.unwrap());
        }

        Ok(Self {
            video_device,
            video_context,
            processor,
            enumerator,
            ring,
            next: 0,
            in_size,
            out_width: out_size.0,
            out_height: out_size.1,
        })
        .inspect(|c| {
            // Black bars, not the processor's default: the part of the frame
            // a fitted source does not cover.
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 },
                },
            };
            // SAFETY: the processor is live; the colour is read during the call.
            unsafe {
                c.video_context.VideoProcessorSetOutputBackgroundColor(&c.processor, false, &black)
            };
            c.fit(in_size);
        })
    }

    /// Place a `src`-sized picture in the middle of the output at its own
    /// aspect ratio. Without a destination rect the processor stretches the
    /// source over the whole frame: a 1280x1392 window arrived squashed into
    /// 16:9 on the second PC (r36, row 6).
    fn fit(&self, src: (u32, u32)) {
        let (x, y, w, h) = fit_rect(src, (self.out_width, self.out_height));
        let r = windows::Win32::Foundation::RECT {
            left: x as i32,
            top: y as i32,
            right: (x + w) as i32,
            bottom: (y + h) as i32,
        };
        // SAFETY: stream 0 exists for the life of the processor.
        unsafe {
            self.video_context.VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&r));
        }
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
        self.fit(rect.map_or(self.in_size, |(_, _, w, h)| (w, h)));
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

/// The largest `src`-shaped rectangle centred in `out`, as (x, y, w, h).
/// Even sizes and offsets: NV12 is 2x2-subsampled, and an odd edge smears
/// one chroma row into the bars.
pub fn fit_rect(src: (u32, u32), out: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sw, sh) = (u64::from(src.0.max(1)), u64::from(src.1.max(1)));
    let (ow, oh) = (u64::from(out.0), u64::from(out.1));
    let (w, h) = if sw * oh >= sh * ow { (ow, ow * sh / sw) } else { (oh * sw / sh, oh) };
    let (w, h) = ((w as u32) & !1, (h as u32) & !1);
    let x = ((out.0 - w) / 2) & !1;
    let y = ((out.1 - h) / 2) & !1;
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::fit_rect;

    #[test]
    fn same_aspect_fills_the_frame() {
        assert_eq!(fit_rect((1280, 720), (2560, 1440)), (0, 0, 2560, 1440));
        assert_eq!(fit_rect((2560, 1440), (2560, 1440)), (0, 0, 2560, 1440));
    }

    #[test]
    fn a_tall_window_is_pillarboxed_not_stretched() {
        let (x, y, w, h) = fit_rect((1280, 1392), (2560, 1440));
        assert_eq!((y, h), (0, 1440));
        assert_eq!(w, 1324);
        assert_eq!(x, 618);
    }

    #[test]
    fn a_wide_region_is_letterboxed() {
        assert_eq!(fit_rect((3440, 1440), (2560, 1440)), (0, 184, 2560, 1070));
    }
}
