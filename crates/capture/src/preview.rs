//! A small JPEG of what is being captured, for the app window.
//!
//! The share engine renders into a native swapchain on the receiving PC, and
//! the sending PC never sees its own stream — so until now the Share screen
//! could only say "capture starts when you share". This produces a thumbnail
//! the webview can display, which answers the question that actually matters
//! before you hit Start: am I sharing the right screen?
//!
//! Two things keep it off the hot path:
//!
//! - **The GPU does the scaling.** The video processor already in the encode
//!   path is reused at thumbnail size, so the readback is ~200 KB rather than
//!   the 14 MB a full-resolution 1440p frame would cost.
//! - **It is rate-limited and opt-in.** Nothing is copied, mapped or encoded
//!   unless a preview was asked for and the interval has elapsed.
//!
//! The thumbnail comes back as NV12 and is converted to RGB here, on the CPU.
//! Asking the video processor for a BGRA output instead looked like the
//! obvious shortcut and was tried first: it creates the output view, reports
//! success on every blt, and writes solid black. NV12 is the format the same
//! processor is already proven to produce for the encoder, and converting
//! 480×270 pixels costs nothing worth measuring.
//!
//! JPEG encoding goes through WIC, which Windows already provides; an image
//! crate would add a dependency to a crate that is on the latency budget.

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

use crate::d3d::Gpu;
use crate::encode::convert::Converter;

/// Longest edge of the thumbnail. Enough to recognise a window layout at a
/// glance without making the JPEG or the IPC line big.
pub const MAX_EDGE: u32 = 480;

/// JPEG quality. Low enough to keep a frame a few tens of kilobytes; this is
/// a "which screen is that" image, not a quality reference.
const QUALITY: f32 = 0.6;

pub struct Preview {
    conv: Converter,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl Preview {
    /// Build a preview scaler for a source of `in_size`. The thumbnail keeps
    /// the source aspect ratio with its longest edge at [`MAX_EDGE`].
    pub fn new(gpu: &Gpu, in_size: (u32, u32)) -> Result<Self> {
        let (w, h) = thumb_size(in_size);
        let conv = Converter::new(gpu, in_size, (w, h)).context("preview scaler")?;

        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        // SAFETY: valid descriptor; the out pointer is ours.
        unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .context("preview staging texture")?;

        Ok(Self { conv, staging: staging.unwrap(), width: w, height: h })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Scale `src`, read it back and JPEG-encode it.
    pub fn jpeg(&mut self, gpu: &Gpu, src: &ID3D11Texture2D) -> Result<Vec<u8>> {
        let small = self.conv.convert(src)?;
        let (w, h) = (self.width as usize, self.height as usize);
        // SAFETY: both textures are live, the same size and NV12; the map is
        // released before the borrowed slices go out of scope.
        let bgr = unsafe {
            gpu.context.CopyResource(&self.staging, &small);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            gpu.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("mapping the preview staging texture")?;
            let pitch = mapped.RowPitch as usize;
            // Staging NV12: Y rows [0, h) then interleaved UV rows at the
            // same pitch, exactly as the virtual-camera sink reads it.
            let base = mapped.pData as *const u8;
            let y_plane = std::slice::from_raw_parts(base, pitch * h);
            let uv_plane = std::slice::from_raw_parts(base.add(pitch * h), pitch * h.div_ceil(2));
            let out = nv12_to_bgr(y_plane, uv_plane, pitch, w, h);
            gpu.context.Unmap(&self.staging, 0);
            out
        };
        encode_jpeg(&bgr, self.width, self.height)
    }
}

/// Bytes per pixel handed to WIC. Three, not four: JPEG has no alpha, and a
/// 32bpp buffer is where this went wrong the first time — see [`encode_jpeg`].
const BPP: usize = 3;

/// NV12 → packed BGR, BT.709 limited range (what the video processor emits).
///
/// Pure and small so it can be tested without a GPU. Chroma is sampled
/// nearest-neighbour: this is a thumbnail, and bilinear chroma would cost
/// more than the whole rest of the preview.
fn nv12_to_bgr(y_plane: &[u8], uv_plane: &[u8], pitch: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * BPP];
    for row in 0..h {
        let y_row = row * pitch;
        let uv_row = (row / 2) * pitch;
        for col in 0..w {
            let y = *y_plane.get(y_row + col).unwrap_or(&16) as f32;
            let uv = uv_row + (col & !1);
            let u = *uv_plane.get(uv).unwrap_or(&128) as f32 - 128.0;
            let v = *uv_plane.get(uv + 1).unwrap_or(&128) as f32 - 128.0;
            // BT.709, 16..235 luma / 16..240 chroma.
            let yy = (y - 16.0) * 1.164_383;
            let r = yy + 1.792_741 * v;
            let g = yy - 0.213_249 * u - 0.532_909 * v;
            let b = yy + 2.112_402 * u;
            let px = (row * w + col) * BPP;
            out[px] = b.clamp(0.0, 255.0) as u8;
            out[px + 1] = g.clamp(0.0, 255.0) as u8;
            out[px + 2] = r.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Thumbnail dimensions for a source, aspect preserved, both edges even
/// (the video processor dislikes odd sizes).
pub fn thumb_size(in_size: (u32, u32)) -> (u32, u32) {
    let (w, h) = (in_size.0.max(1), in_size.1.max(1));
    let longest = w.max(h) as f32;
    let scale = (MAX_EDGE as f32 / longest).min(1.0);
    let tw = ((w as f32 * scale).round() as u32).max(2) & !1;
    let th = ((h as f32 * scale).round() as u32).max(2) & !1;
    (tw, th)
}

/// Packed BGR bytes → JPEG, via the imaging component Windows already ships.
///
/// The pixel format must be one JPEG actually supports. `SetPixelFormat` is
/// an in/out parameter: ask for 32bppBGRA and WIC quietly negotiates down to
/// 24bppBGR, then reads a 4-byte-per-pixel buffer as 3 — which renders as
/// fine vertical stripes over a recognisable image. That is exactly what the
/// first version of this did, and it survived a test that only checked the
/// JPEG markers. Hence 24bppBGR up front, and an explicit check that WIC
/// agreed.
fn encode_jpeg(bgr: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
        IWICImagingFactory, WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

    // SAFETY: COM object creation and use on one thread; every out-parameter
    // is initialised before it is read, and the stream is read back only
    // after the encoder commits.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .context("WIC factory (is COM initialised on this thread?)")?;
        let stream = CreateStreamOnHGlobal(Default::default(), true).context("JPEG stream")?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())
            .context("JPEG encoder")?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;

        let mut frame = None;
        let mut props = None;
        encoder.CreateNewFrame(&mut frame, &mut props)?;
        let frame = frame.context("WIC gave no frame")?;

        // Quality has to be set through the property bag before Initialize.
        if let Some(props) = props {
            use windows::core::PWSTR;
            use windows::Win32::System::Com::StructuredStorage::PROPBAG2;
            use windows::Win32::System::Variant::{VariantClear, VARIANT};
            let name = windows::core::w!("ImageQuality");
            let bag = PROPBAG2 { pstrName: PWSTR(name.as_ptr() as *mut u16), ..Default::default() };
            let value = VARIANT::from(QUALITY);
            let _ = props.Write(1, &bag, &value);
            let mut value = value;
            let _ = VariantClear(&mut value);
            frame.Initialize(&props)?;
        } else {
            frame.Initialize(None)?;
        }

        frame.SetSize(width, height)?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut format)?;
        if format != GUID_WICPixelFormat24bppBGR {
            anyhow::bail!(
                "WIC negotiated a pixel format we do not write ({format:?}); \
                 the buffer stride would not match"
            );
        }
        frame.WritePixels(height, width * BPP as u32, bgr)?;
        frame.Commit()?;
        encoder.Commit()?;

        // Rewind and read the whole stream back.
        let mut stat = Default::default();
        stream.Stat(&mut stat, windows::Win32::System::Com::STATFLAG_NONAME)?;
        let len = stat.cbSize as usize;
        stream.Seek(0, windows::Win32::System::Com::STREAM_SEEK_SET, None)?;
        let mut out = vec![0u8; len];
        let mut read = 0u32;
        stream.Read(out.as_mut_ptr() as *mut _, len as u32, Some(&mut read)).ok()?;
        out.truncate(read as usize);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a JPEG back to packed BGR with WIC, so a test can check pixels
    /// rather than just that the bytes start with the right marker.
    #[cfg(windows)]
    fn decode_jpeg_bgr(jpeg: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
        use windows::Win32::Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_WICPixelFormat24bppBGR, IWICImagingFactory,
            WICDecodeMetadataCacheOnLoad,
        };
        use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

        // SAFETY: the stream is written and rewound before the decoder reads
        // it; every out-parameter is initialised before use.
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = CreateStreamOnHGlobal(Default::default(), true)?;
            let mut written = 0u32;
            stream.Write(jpeg.as_ptr() as *const _, jpeg.len() as u32, Some(&mut written)).ok()?;
            stream.Seek(0, windows::Win32::System::Com::STREAM_SEEK_SET, None)?;

            let decoder = factory.CreateDecoderFromStream(
                &stream,
                std::ptr::null(),
                WICDecodeMetadataCacheOnLoad,
            )?;
            let frame = decoder.GetFrame(0)?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h)?;

            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat24bppBGR,
                windows::Win32::Graphics::Imaging::WICBitmapDitherTypeNone,
                None,
                0.0,
                windows::Win32::Graphics::Imaging::WICBitmapPaletteTypeCustom,
            )?;
            let stride = w as usize * BPP;
            let mut out = vec![0u8; stride * h as usize];
            converter.CopyPixels(std::ptr::null(), stride as u32, &mut out)?;
            Ok((w, h, out))
        }
    }

    #[test]
    fn thumbnails_keep_aspect_and_fit_the_long_edge() {
        let (w, h) = thumb_size((2560, 1440));
        assert_eq!(w, MAX_EDGE);
        assert!((h as f32 - MAX_EDGE as f32 * 1440.0 / 2560.0).abs() < 2.0, "{h}");

        // Portrait sources scale on their height instead.
        let (w, h) = thumb_size((1080, 1920));
        assert_eq!(h, MAX_EDGE);
        assert!(w < h);
    }

    #[test]
    fn thumbnails_are_never_upscaled_or_odd() {
        // Smaller than the cap: left alone rather than blown up.
        let (w, h) = thumb_size((320, 200));
        assert_eq!((w, h), (320, 200));
        // Odd inputs round down to even, which the video processor requires.
        let (w, h) = thumb_size((321, 201));
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }

    /// Build an NV12 plane pair of a solid colour at a given stride.
    fn solid_nv12(y: u8, u: u8, v: u8, pitch: usize, w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
        let mut yp = vec![0u8; pitch * h];
        for row in 0..h {
            yp[row * pitch..row * pitch + w].fill(y);
        }
        let mut uvp = vec![0u8; pitch * h.div_ceil(2)];
        for row in 0..h.div_ceil(2) {
            for col in (0..w).step_by(2) {
                uvp[row * pitch + col] = u;
                uvp[row * pitch + col + 1] = v;
            }
        }
        (yp, uvp)
    }

    #[test]
    fn nv12_black_and_white_convert_to_black_and_white() {
        let (w, h, pitch) = (8, 4, 64);
        // Limited-range black is Y=16, white is Y=235, both with neutral chroma.
        let (y, uv) = solid_nv12(16, 128, 128, pitch, w, h);
        let black = nv12_to_bgr(&y, &uv, pitch, w, h);
        assert!(
            black.chunks(BPP).all(|p| p[0] < 4 && p[1] < 4 && p[2] < 4),
            "not black: {:?}",
            &black[..BPP]
        );

        let (y, uv) = solid_nv12(235, 128, 128, pitch, w, h);
        let white = nv12_to_bgr(&y, &uv, pitch, w, h);
        assert!(
            white.chunks(BPP).all(|p| p[0] > 250 && p[1] > 250 && p[2] > 250),
            "not white: {:?}",
            &white[..BPP]
        );
    }

    #[test]
    fn nv12_chroma_lands_on_the_right_channel() {
        let (w, h, pitch) = (4, 2, 32);
        // Mid luma with V pushed high is red; U pushed high is blue. Getting
        // these the wrong way round is the classic NV12 bug and looks fine
        // until something is actually coloured.
        let (y, uv) = solid_nv12(128, 128, 240, pitch, w, h);
        let red = nv12_to_bgr(&y, &uv, pitch, w, h);
        assert!(red[2] > red[0], "red channel should dominate blue: {:?}", &red[..4]);

        let (y, uv) = solid_nv12(128, 240, 128, pitch, w, h);
        let blue = nv12_to_bgr(&y, &uv, pitch, w, h);
        assert!(blue[0] > blue[2], "blue channel should dominate red: {:?}", &blue[..4]);
    }

    #[test]
    fn a_short_plane_is_padded_rather_than_panicking() {
        // A driver reporting a smaller pitch than we assume must not index
        // out of bounds mid-share.
        let out = nv12_to_bgr(&[16u8; 4], &[128u8; 2], 64, 8, 4);
        assert_eq!(out.len(), 8 * 4 * BPP);
    }

    #[test]
    fn a_degenerate_source_does_not_produce_a_zero_sized_thumbnail() {
        let (w, h) = thumb_size((0, 0));
        assert!(w >= 2 && h >= 2, "{w}x{h}");
    }

    #[cfg(windows)]
    #[test]
    fn wic_round_trips_the_pixels_it_was_given() {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
        // SAFETY: paired init/uninit on this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        // Solid red, so a stride or channel-order mistake is unmissable. The
        // previous version of this test made a gradient and checked only the
        // JPEG markers, which is why it passed while every real frame came
        // out covered in vertical stripes.
        let (w, h) = (32u32, 16u32);
        let mut bgr = Vec::with_capacity((w * h) as usize * BPP);
        for _ in 0..w * h {
            bgr.extend_from_slice(&[0x20, 0x30, 0xE0]); // B, G, R
        }
        let jpeg = encode_jpeg(&bgr, w, h).expect("encode");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "not a JPEG: {:02X?}", &jpeg[..4.min(jpeg.len())]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "no end-of-image marker");

        let (dw, dh, pixels) = decode_jpeg_bgr(&jpeg).expect("decode");
        assert_eq!((dw, dh), (w, h), "dimensions survived the round trip");
        // JPEG is lossy, so allow drift, but red must still dominate and the
        // image must be uniform — stripes would break both.
        for px in pixels.chunks(BPP) {
            assert!(px[2] > 0xB0, "red channel lost: {px:?}");
            assert!(px[0] < 0x60 && px[1] < 0x70, "channel order wrong: {px:?}");
        }
        // SAFETY: matching the init above.
        unsafe { CoUninitialize() };
    }
}
