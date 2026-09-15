//! The vendor's HEVC hardware encoder MFT (NVENC / QSV / AMF behind Media
//! Foundation), driven as an async MFT with D3D11 texture input.
//!
//! Tuning for the latency budget: low-latency mode on, CBR, zero B-frames,
//! 10 s GOP with keyframe-on-request. Software MFTs are never enumerated
//! (`MFT_ENUM_FLAG_HARDWARE` only) — if no hardware encoder exists on the
//! capture adapter, construction fails.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Foundation::LUID;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4,
};

fn variant(vt: VARENUM, value: VARIANT_0_0_0) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: value,
            }),
        },
    }
}

fn variant_u32(v: u32) -> VARIANT {
    variant(VT_UI4, VARIANT_0_0_0 { ulVal: v })
}

fn variant_bool(b: bool) -> VARIANT {
    variant(
        VT_BOOL,
        VARIANT_0_0_0 { boolVal: windows::Win32::Foundation::VARIANT_BOOL(if b { -1 } else { 0 }) },
    )
}

use super::EncodedFrame;
use crate::d3d::Gpu;

pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
}

pub enum EncoderEvent {
    /// The MFT wants one more input frame.
    NeedInput,
    /// One encoded access unit is ready.
    Output(EncodedFrame),
}

pub struct MfHevcEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: ICodecAPI,
    in_id: u32,
    out_id: u32,
    /// The MFT hands back its own output samples. When false we allocate them
    /// (see [`MfHevcEncoder::alloc_output_sample`]).
    provides_samples: bool,
    /// `cbSize` / `cbAlignment` from `GetOutputStreamInfo`, for that path.
    output_sample_size: u32,
    output_alignment: u32,
    frame_duration_100ns: i64,
    pub name: String,
    /// Keep the device manager alive for the life of the encoder.
    _dev_manager: IMFDXGIDeviceManager,
}

// SAFETY: the MFT is only driven from one thread; hardware MFTs are free-threaded COM objects.
unsafe impl Send for MfHevcEncoder {}

impl MfHevcEncoder {
    pub fn new(gpu: &Gpu, cfg: &EncoderConfig) -> Result<Self> {
        let (transform, name) = activate_hardware_hevc(gpu.adapter_luid, &gpu.adapter_name)?;

        // SAFETY: standard MFT setup sequence; all pointers are live COM interfaces.
        unsafe {
            let attrs = transform.GetAttributes().context("MFT attributes")?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;

            // Hand the MFT our D3D11 device so input stays on the GPU.
            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("MFCreateDXGIDeviceManager")?;
            manager.ResetDevice(&gpu.device, token)?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("MFT rejected the D3D manager (not a hardware MFT?)")?;

            let (in_id, out_id) = stream_ids(&transform)?;

            // Latency-critical codec settings, before types are set.
            let codec_api: ICodecAPI = transform.cast()?;
            let set = |guid: &windows::core::GUID, v: &VARIANT| -> Result<()> {
                codec_api.SetValue(guid, v).with_context(|| format!("codecapi {guid:?}"))
            };
            set(&CODECAPI_AVLowLatencyMode, &variant_bool(true))?;
            set(
                &CODECAPI_AVEncCommonRateControlMode,
                &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
            )?;
            set(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(cfg.bitrate_bps))?;
            if let Err(e) = set(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0)) {
                tracing::debug!(error = %e, "encoder does not expose B-picture count");
            }
            if let Err(e) = set(&CODECAPI_AVEncMPVGOPSize, &variant_u32(cfg.fps * 10)) {
                tracing::debug!(error = %e, "encoder does not expose GOP size");
            }

            // Output type first (encoders require it), then input.
            let out_type = MFCreateMediaType()?;
            out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)?;
            out_type.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate_bps)?;
            out_type.SetUINT64(&MF_MT_FRAME_SIZE, size64(cfg.width, cfg.height))?;
            out_type.SetUINT64(&MF_MT_FRAME_RATE, size64(cfg.fps, 1))?;
            out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, size64(1, 1))?;
            out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform.SetOutputType(out_id, &out_type, 0).context("SetOutputType HEVC")?;

            let in_type = MFCreateMediaType()?;
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, size64(cfg.width, cfg.height))?;
            in_type.SetUINT64(&MF_MT_FRAME_RATE, size64(cfg.fps, 1))?;
            in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, size64(1, 1))?;
            in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform.SetInputType(in_id, &in_type, 0).context("SetInputType NV12")?;

            let info = transform.GetOutputStreamInfo(out_id)?;
            // PROVIDES means the MFT insists on supplying its own samples;
            // CAN_PROVIDE means it will do either. Every hardware HEVC MFT this
            // was developed against sets one of them, but the contract allows
            // neither, in which case the caller allocates — so we do, rather
            // than failing on the first encoded frame the way this used to.
            let must_provide = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            let can_provide = info.dwFlags & (MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
            // Lets the allocator path be exercised on a machine whose MFT would
            // otherwise always take the other branch (`CAN_PROVIDE` only).
            let force_alloc = std::env::var_os("RELAY_FORCE_MFT_ALLOCATOR").is_some();
            if force_alloc && must_provide {
                tracing::warn!(
                    "RELAY_FORCE_MFT_ALLOCATOR ignored: {name} requires MFT-provided samples"
                );
            }
            let provides_samples = must_provide || (can_provide && !force_alloc);
            // A caller-allocated buffer must be at least cbSize; when the MFT
            // reports 0 (allowed only alongside PROVIDES) fall back to a frame
            // of 4:2:0 luma, far above any single HEVC access unit at our rates.
            let output_sample_size = if info.cbSize > 0 {
                info.cbSize
            } else {
                cfg.width.saturating_mul(cfg.height).max(1 << 20)
            };
            tracing::info!(
                encoder = %name,
                provides_samples,
                output_sample_size,
                "HEVC hardware encoder ready"
            );

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            let events: IMFMediaEventGenerator = transform.cast()?;
            Ok(Self {
                transform,
                events,
                codec_api,
                in_id,
                out_id,
                provides_samples,
                output_sample_size,
                output_alignment: info.cbAlignment,
                frame_duration_100ns: 10_000_000 / cfg.fps as i64,
                name,
                _dev_manager: manager,
            })
        }
    }

    /// Block until the MFT signals need-input or have-output.
    pub fn next_event(&self) -> Result<EncoderEvent> {
        loop {
            // SAFETY: blocking GetEvent on the MFT's event queue.
            let ev = unsafe { self.events.GetEvent(MF_EVENT_FLAG_NONE) }.context("GetEvent")?;
            // SAFETY: freshly received event.
            let kind = MF_EVENT_TYPE(unsafe { ev.GetType() }? as i32);
            if kind == METransformNeedInput {
                return Ok(EncoderEvent::NeedInput);
            } else if kind == METransformHaveOutput {
                return Ok(EncoderEvent::Output(self.take_output()?));
            } else {
                tracing::debug!(?kind, "ignoring MFT event");
            }
        }
    }

    /// Feed one NV12 texture. Call only after [`EncoderEvent::NeedInput`].
    pub fn submit(
        &self,
        texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        pts_100ns: i64,
    ) -> Result<()> {
        // SAFETY: wrapping a live texture in an MF sample; MF add-refs it.
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(
                &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D::IID,
                texture,
                0,
                false,
            )?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_100ns)?;
            sample.SetSampleDuration(self.frame_duration_100ns)?;
            self.transform.ProcessInput(self.in_id, &sample, 0).context("ProcessInput")
        }
    }

    /// Change the target mean bitrate at runtime (adaptive step-down/up).
    pub fn set_bitrate(&self, bps: u32) -> Result<()> {
        // SAFETY: documented runtime-settable codec property.
        unsafe {
            self.codec_api
                .SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bps))
                .context("set bitrate")
        }
    }

    /// Ask for an IDR on the next frame (new receiver joined / loss recovery).
    pub fn request_keyframe(&self) -> Result<()> {
        // SAFETY: documented codec property.
        unsafe {
            self.codec_api
                .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1))
                .context("force keyframe")
        }
    }

    fn take_output(&self) -> Result<EncodedFrame> {
        // SAFETY: ProcessOutput per the async-MFT contract. When the MFT
        // provides samples it transfers ownership to us; when it does not we
        // pass in our own and take the same reference back out.
        unsafe {
            let mut status = 0u32;
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.out_id,
                pSample: std::mem::ManuallyDrop::new(if self.provides_samples {
                    None
                } else {
                    Some(alloc_output_sample(self.output_sample_size, self.output_alignment)?)
                }),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            // Reclaim both out-params before propagating, or a failed
            // ProcessOutput leaks the sample we just allocated.
            let r = self.transform.ProcessOutput(0, &mut out, &mut status);
            let sample = std::mem::ManuallyDrop::take(&mut out[0].pSample);
            std::mem::ManuallyDrop::drop(&mut out[0].pEvents);
            r.context("ProcessOutput")?;
            let sample = sample.context("ProcessOutput returned no sample")?;

            let pts = sample.GetSampleTime().unwrap_or(0);
            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 1;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            Ok(EncodedFrame { data, pts_100ns: pts, keyframe })
        }
    }
}

/// One output sample for an MFT that does not supply its own. The bitstream
/// lands in system memory either way, so a plain aligned memory buffer is all
/// that is needed; a fresh one per call keeps us clear of an MFT that holds a
/// reference past `ProcessOutput`.
///
/// `alignment` is `MFT_OUTPUT_STREAM_INFO::cbAlignment`, a byte count;
/// `MFCreateAlignedMemoryBuffer` wants the `MF_*_BYTE_ALIGNMENT` form, which
/// is one less.
fn alloc_output_sample(size: u32, alignment: u32) -> Result<IMFSample> {
    // SAFETY: MF allocates both objects; we hand back the only references.
    unsafe {
        let buffer = MFCreateAlignedMemoryBuffer(size, alignment.saturating_sub(1))
            .context("allocating an output buffer for the encoder")?;
        buffer.SetCurrentLength(0)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
    }
}

fn size64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn stream_ids(transform: &IMFTransform) -> Result<(u32, u32)> {
    let mut ins = [0u32];
    let mut outs = [0u32];
    // SAFETY: fixed-size id queries; E_NOTIMPL means "use 0".
    match unsafe { transform.GetStreamIDs(&mut ins, &mut outs) } {
        Ok(()) => Ok((ins[0], outs[0])),
        Err(_) => Ok((0, 0)),
    }
}

/// Software HEVC encode is out of scope by design, not by omission: a CPU
/// encoder cannot hold 4K60 inside the <50 ms budget, and CLAUDE.md pins the
/// share to NVENC / Quick Sync / AMF. So the failure is permanent — which
/// makes it all the more important that it names the adapter it looked at,
/// and says whether some *other* adapter in the machine could do the job.
/// That second case is the common one: a laptop whose display hangs off the
/// iGPU while the encoder lives on the dGPU.
fn no_encoder_message(adapter: &str, all_adapters: &[String]) -> String {
    let mut msg = format!(
        "no hardware HEVC encoder on \"{adapter}\", the GPU driving the display you are \
         capturing. Relay encodes HEVC in hardware only (NVENC / Quick Sync / AMF) — there \
         is no software encode path, so this PC can receive a share but not send one."
    );
    if all_adapters.is_empty() {
        msg.push_str(
            " No other GPU in this PC has one either. If this GPU is recent, update its \
             graphics driver: Media Foundation only lists the encoder once the vendor \
             driver is installed.",
        );
    } else {
        msg.push_str(&format!(
            " Another GPU in this PC does have one ({}). Move the window or display you are \
             sharing onto that GPU's output, or set Relay to \"High performance\" under \
             Settings > System > Display > Graphics, then start the share again.",
            all_adapters.join(", ")
        ));
    }
    msg
}

/// First hardware HEVC encoder MFT on the adapter with `luid`. `adapter` is
/// that GPU's description, used only to make the failure message specific.
fn activate_hardware_hevc(luid: LUID, adapter: &str) -> Result<(IMFTransform, String)> {
    let out_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
    };
    // SAFETY: attribute store + enumeration; array freed below.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        let luid_bytes: [u8; 8] = std::mem::transmute(luid);
        attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid_bytes)?;

        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&out_type),
            &attrs,
            &mut activates,
            &mut count,
        )
        .context("MFTEnum2")?;
        if count == 0 {
            bail!(no_encoder_message(
                adapter,
                &crate::probe::hevc_hardware_encoders()
                    .map(|e| e.into_iter().map(|m| m.friendly_name).collect::<Vec<_>>())
                    .unwrap_or_default()
            ));
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

/// Latency map from submitted pts → submit QPC time; lets the bench attribute
/// each output to its input without assuming in-order completion.
#[derive(Default)]
pub struct InflightClock {
    map: HashMap<i64, i64>,
}

impl InflightClock {
    pub fn submitted(&mut self, pts: i64, now_100ns: i64) {
        self.map.insert(pts, now_100ns);
    }

    pub fn completed(&mut self, pts: i64, now_100ns: i64) -> Option<i64> {
        self.map.remove(&pts).map(|t| now_100ns - t)
    }

    pub fn in_flight(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflight_clock_matches_output_to_input() {
        let mut c = InflightClock::default();
        c.submitted(100, 1_000);
        c.submitted(200, 2_000);
        assert_eq!(c.in_flight(), 2);
        // Out-of-order completion still matches by PTS.
        assert_eq!(c.completed(200, 2_500), Some(500));
        assert_eq!(c.completed(100, 4_000), Some(3_000));
        assert_eq!(c.in_flight(), 0);
        // An unknown PTS (encoder-generated frame) yields no sample.
        assert_eq!(c.completed(999, 5_000), None);
    }

    /// The caller-allocated output path cannot be exercised end-to-end on a
    /// machine whose MFT sets `MFT_OUTPUT_STREAM_PROVIDES_SAMPLES` (both
    /// encoders on the dev PC do). What *can* be checked without an encoder is
    /// the part that would actually be wrong: that the sample we hand
    /// `ProcessOutput` is a writable, correctly sized, zero-length buffer, and
    /// that `take_output`'s read-back sequence recovers exactly what an MFT
    /// would have written into it.
    #[test]
    fn caller_allocated_samples_round_trip_through_the_read_path() {
        let _mf = crate::probe::MediaFoundation::start().expect("MFStartup");
        for alignment in [0u32, 1, 16, 512] {
            let sample = alloc_output_sample(4096, alignment).expect("allocate");
            // SAFETY: freshly created sample with exactly one buffer.
            unsafe {
                let buffer = sample.GetBufferByIndex(0).unwrap();
                assert_eq!(buffer.GetCurrentLength().unwrap(), 0, "alignment {alignment}");
                assert!(buffer.GetMaxLength().unwrap() >= 4096, "alignment {alignment}");

                // Stand in for the MFT writing an access unit.
                let mut ptr = std::ptr::null_mut();
                let mut max = 0u32;
                buffer.Lock(&mut ptr, Some(&mut max), None).unwrap();
                let written: Vec<u8> = (0..64u8).collect();
                std::ptr::copy_nonoverlapping(written.as_ptr(), ptr, written.len());
                buffer.Unlock().unwrap();
                buffer.SetCurrentLength(written.len() as u32).unwrap();
                sample.SetSampleTime(123_456).unwrap();
                sample.SetUINT32(&MFSampleExtension_CleanPoint, 1).unwrap();

                // Exactly what take_output does with it afterwards.
                assert_eq!(sample.GetSampleTime().unwrap(), 123_456);
                assert_eq!(sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap(), 1);
                let contiguous = sample.ConvertToContiguousBuffer().unwrap();
                let mut ptr = std::ptr::null_mut();
                let mut len = 0u32;
                contiguous.Lock(&mut ptr, None, Some(&mut len)).unwrap();
                let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                contiguous.Unlock().unwrap();
                assert_eq!(data, written, "alignment {alignment}");
            }
        }
    }

    #[test]
    fn the_no_encoder_message_says_which_gpu_and_what_to_do() {
        let alone = no_encoder_message("Intel(R) UHD Graphics 630", &[]);
        assert!(alone.contains("Intel(R) UHD Graphics 630"), "{alone}");
        assert!(alone.contains("No other GPU"), "{alone}");
        assert!(alone.contains("update its graphics driver"), "{alone}");

        let multi =
            no_encoder_message("Intel(R) UHD Graphics 630", &["NVIDIA HEVC Encoder MFT".into()]);
        assert!(multi.contains("NVIDIA HEVC Encoder MFT"), "{multi}");
        assert!(multi.contains("High performance"), "{multi}");
        // Never claims the machine is hopeless when another adapter can encode.
        assert!(!multi.contains("No other GPU"), "{multi}");
    }
}
