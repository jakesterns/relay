//! Media Foundation HEVC decoder MFT, D3D11-backed (DXVA). Fed Annex B access
//! units, produces NV12 textures on the render device.
//!
//! We drive it synchronously: the hardware vendor decoders register only an
//! encoder MFT, so the decoder that exists on a stock Windows box is the
//! Microsoft HEVC Video Extension — a **sync** MFT that decodes on the GPU via
//! DXVA once it has the D3D device manager. There is no CPU-only fallback: if
//! no decoder can bind our D3D device, construction fails.

use anyhow::{bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;

pub struct DecodedFrame {
    pub texture: ID3D11Texture2D,
    /// Subresource index in `texture` (DXVA outputs a texture array).
    pub subresource: u32,
    pub pts_100ns: i64,
    pub width: u32,
    pub height: u32,
}

pub struct MfHevcDecoder {
    transform: IMFTransform,
    in_id: u32,
    out_id: u32,
    width: u32,
    height: u32,
    pub name: String,
    _dev_manager: IMFDXGIDeviceManager,
}

// SAFETY: driven from a single thread only.
unsafe impl Send for MfHevcDecoder {}

impl MfHevcDecoder {
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self> {
        let (transform, name) = activate_hevc_decoder()?;
        // SAFETY: standard MFT decode setup on live COM interfaces.
        unsafe {
            let attrs = transform.GetAttributes().context("decoder attributes")?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("MFCreateDXGIDeviceManager")?;
            manager.ResetDevice(device, token)?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("decoder rejected the D3D manager (no DXVA)")?;

            let (in_id, out_id) = stream_ids(&transform);

            let in_type = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)?;
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, size64(width, height))?;
            in_type.SetUINT64(&MF_MT_FRAME_RATE, size64(60, 1))?;
            in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform.SetInputType(in_id, &in_type, 0).context("decoder SetInputType HEVC")?;

            select_nv12_output(&transform, out_id)?;

            // The receive path only knows how to read a DXVA-backed sample the
            // decoder supplies itself: `frame_from_sample` casts the buffer to
            // IMFDXGIBuffer and hands the texture straight to the presenter,
            // with no system-memory copy anywhere. A decoder that wants the
            // caller to allocate cannot be doing DXVA, so say that here rather
            // than let it surface as "decoder gave no sample" on frame one.
            let info = transform.GetOutputStreamInfo(out_id)?;
            if info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                    | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
                == 0
            {
                bail!(
                    "the HEVC decoder \"{name}\" wants caller-allocated output, which means \
                     it is decoding on the CPU rather than through DXVA. Relay only presents \
                     GPU-decoded frames. Install \"HEVC Video Extensions from Device \
                     Manufacturer\" from the Microsoft Store and update the graphics driver, \
                     then receive again"
                );
            }

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            tracing::info!(decoder = %name, "HEVC DXVA decoder ready");

            Ok(Self { transform, in_id, out_id, width, height, name, _dev_manager: manager })
        }
    }

    /// Feed one Annex B access unit and drain every frame it completes.
    pub fn decode(&mut self, au: &[u8], pts_100ns: i64) -> Result<Vec<DecodedFrame>> {
        // SAFETY: build an MF sample and run the sync ProcessInput/Output cycle.
        unsafe {
            let buffer = MFCreateMemoryBuffer(au.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), ptr, au.len());
            buffer.SetCurrentLength(au.len() as u32)?;
            buffer.Unlock()?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_100ns)?;
            self.transform.ProcessInput(self.in_id, &sample, 0).context("decoder ProcessInput")?;
        }
        self.drain()
    }

    fn drain(&mut self) -> Result<Vec<DecodedFrame>> {
        let mut out_frames = Vec::new();
        loop {
            // SAFETY: sync ProcessOutput; the decoder allocates D3D samples.
            let frame = unsafe {
                let mut status = 0u32;
                let mut out = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: self.out_id,
                    pSample: std::mem::ManuallyDrop::new(None),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                }];
                let r = self.transform.ProcessOutput(0, &mut out, &mut status);
                std::mem::ManuallyDrop::drop(&mut out[0].pEvents);
                match r {
                    Ok(()) => {}
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        let _ = std::mem::ManuallyDrop::take(&mut out[0].pSample);
                        break;
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let _ = std::mem::ManuallyDrop::take(&mut out[0].pSample);
                        select_nv12_output(&self.transform, self.out_id)?;
                        continue;
                    }
                    Err(e) => return Err(e).context("decoder ProcessOutput"),
                }
                let sample = std::mem::ManuallyDrop::take(&mut out[0].pSample)
                    .context("decoder gave no sample")?;
                self.frame_from_sample(sample)?
            };
            out_frames.push(frame);
        }
        Ok(out_frames)
    }

    fn frame_from_sample(&self, sample: IMFSample) -> Result<DecodedFrame> {
        // SAFETY: pull the D3D texture backing the decoded sample.
        unsafe {
            let pts = sample.GetSampleTime().unwrap_or(0);
            let buffer = sample.GetBufferByIndex(0)?;
            let dxgi: IMFDXGIBuffer = buffer.cast()?;
            let mut texture: Option<ID3D11Texture2D> = None;
            dxgi.GetResource(
                &ID3D11Texture2D::IID,
                &mut texture as *mut _ as *mut *mut core::ffi::c_void,
            )?;
            let subresource = dxgi.GetSubresourceIndex()?;
            Ok(DecodedFrame {
                texture: texture.context("no texture in decoded sample")?,
                subresource,
                pts_100ns: pts,
                width: self.width,
                height: self.height,
            })
        }
    }
}

fn size64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn stream_ids(transform: &IMFTransform) -> (u32, u32) {
    let mut ins = [0u32];
    let mut outs = [0u32];
    // SAFETY: fixed-size query; E_NOTIMPL means the ids are 0.
    match unsafe { transform.GetStreamIDs(&mut ins, &mut outs) } {
        Ok(()) => (ins[0], outs[0]),
        Err(_) => (0, 0),
    }
}

fn select_nv12_output(transform: &IMFTransform, out_id: u32) -> Result<()> {
    // SAFETY: iterate the decoder's offered output types for NV12.
    unsafe {
        let mut i = 0;
        loop {
            let t = transform
                .GetOutputAvailableType(out_id, i)
                .context("decoder offers no NV12 output")?;
            if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                transform.SetOutputType(out_id, &t, 0).context("decoder SetOutputType NV12")?;
                return Ok(());
            }
            i += 1;
        }
    }
}

/// First HEVC decoder MFT, hardware or locally-registered (the Microsoft HEVC
/// Video Extension is a LOCALMFT). Preference order comes from
/// `SORTANDFILTER`. Software-only category decoders are still DXVA-accelerated
/// once the D3D manager is attached, which the caller does.
fn activate_hevc_decoder() -> Result<(IMFTransform, String)> {
    let in_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
    };
    // SAFETY: enumeration; array freed below.
    unsafe {
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_HARDWARE
                | MFT_ENUM_FLAG_SYNCMFT
                | MFT_ENUM_FLAG_ASYNCMFT
                | MFT_ENUM_FLAG_LOCALMFT
                | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&in_type),
            None,
            &mut activates,
            &mut count,
        )
        .context("MFTEnumEx(HEVC decoder)")?;
        if activates.is_null() || count == 0 {
            // Deliberately ASCII: this reaches a console, and on a Windows
            // console still on the OEM code page a UTF-8 em dash renders as
            // mojibake ("a-tilde-EUR-dash"). Seen for real on a Windows 10 box.
            //
            // Also deliberately does NOT name the "from Device Manufacturer"
            // package or tell anyone to search the Store. Verified 2026-09-16
            // on a real machine: that package is OEM-entitlement only and its
            // Install button is greyed out, and a Store search surfaces paid
            // and third-party apps instead. Sending a user there is worse than
            // saying nothing.
            bail!(
                "no HEVC decoder is registered on this PC, so it cannot receive a share. \
                 Windows includes one for free only on PCs whose manufacturer licensed it; \
                 GPU drivers register encoders only. Microsoft sells the decoder in the \
                 Store as \"HEVC Video Extensions\" (ProductId 9NMZLZ57R3T7). Relay cannot \
                 bundle it: Microsoft does not license that codec for redistribution by apps"
            );
        }
        let slice = std::slice::from_raw_parts(activates, count as usize);
        let first = slice[0].as_ref().context("null activate")?;
        let name = first
            .GetStringLength(&MFT_FRIENDLY_NAME_Attribute)
            .ok()
            .and_then(|len| {
                let mut buf = vec![0u16; len as usize + 1];
                first.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, None).ok()?;
                Some(String::from_utf16_lossy(&buf[..len as usize]))
            })
            .unwrap_or_else(|| "(unnamed)".into());
        let transform: IMFTransform = first.ActivateObject()?;
        for i in 0..count as usize {
            std::ptr::drop_in_place(activates.add(i));
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
        Ok((transform, name))
    }
}
