//! Capability probes: which hardware HEVC and H.264 encoder MFTs exist, which
//! decoders exist for each, and whether Windows.Graphics.Capture is available. `relay-share probe` prints this and
//! the output is recorded in docs/plans/M4-share.md → Measurements.

use anyhow::{Context, Result};

use crate::codec::VideoCodec;
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MFMediaType_Video, MFShutdown, MFStartup, MFTEnumEx,
    MFT_ENUM_HARDWARE_URL_Attribute, MFT_FRIENDLY_NAME_Attribute, MFSTARTUP_LITE,
    MFT_CATEGORY_VIDEO_DECODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ASYNCMFT,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_ENUM_FLAG_SYNCMFT, MFT_REGISTER_TYPE_INFO, MF_VERSION,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct EncoderMft {
    pub friendly_name: String,
    pub hardware_url: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeReport {
    /// GPUs with a display attached, best-guess order (DXGI adapter order).
    /// Only so the "this PC cannot share" message can name the GPU it means.
    pub adapters: Vec<String>,
    pub hevc_hardware_encoders: Vec<EncoderMft>,
    pub hevc_hardware_decoders: Vec<EncoderMft>,
    pub hevc_any_decoders: Vec<EncoderMft>,
    /// H.264 alongside (S27). Decode ships with every Windows install, so
    /// this list is what makes receiving free.
    pub h264_hardware_encoders: Vec<EncoderMft>,
    pub h264_any_decoders: Vec<EncoderMft>,
    pub wgc_supported: bool,
}

/// Guard that pairs `MFStartup` with `MFShutdown`.
pub struct MediaFoundation;

impl MediaFoundation {
    pub fn start() -> Result<Self> {
        // SAFETY: standard MF initialisation; balanced by Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.context("MFStartup")?;
        Ok(Self)
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        // SAFETY: balances the MFStartup in `start`.
        unsafe {
            let _ = MFShutdown();
        }
    }
}

/// Hardware encoder MFTs for `codec` on any adapter, best first
/// (`MFT_ENUM_FLAG_SORTANDFILTER`). Software MFTs are deliberately not
/// requested: there is no CPU encode path.
pub fn hardware_encoders(codec: VideoCodec) -> Result<Vec<EncoderMft>> {
    let out_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: crate::encode::mf::mf_subtype(codec),
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: out pointers are ours; the returned array is freed below.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&out_type),
            &mut activates,
            &mut count,
        )
    }
    .with_context(|| format!("MFTEnumEx(video encoder, {}, hardware)", codec.label()))?;

    if activates.is_null() || count == 0 {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    // SAFETY: MFTEnumEx returned `count` activation objects at `activates`.
    let slice = unsafe { std::slice::from_raw_parts(activates, count as usize) };
    for activate in slice.iter().flatten() {
        found.push(EncoderMft {
            friendly_name: get_string(activate, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| "(unnamed)".into()),
            hardware_url: get_string(activate, &MFT_ENUM_HARDWARE_URL_Attribute),
        });
    }
    // SAFETY: the interface pointers were released by dropping `slice`'s
    // contents' clones; the array itself was CoTaskMemAlloc'd by MFTEnumEx.
    unsafe {
        // Drop each element in place before freeing the array memory.
        for i in 0..count as usize {
            std::ptr::drop_in_place(activates.add(i));
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
    }
    Ok(found)
}

fn get_string(activate: &IMFActivate, key: &windows::core::GUID) -> Option<String> {
    // SAFETY: size query, then a read into a buffer of exactly that size (+ NUL).
    unsafe {
        let len = activate.GetStringLength(key).ok()?;
        let mut buf = vec![0u16; len as usize + 1];
        activate.GetString(key, &mut buf, None).ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Decoder MFTs for `codec` (any flags unless `hardware_only`), for
/// receiver decode support.
pub fn decoders(codec: VideoCodec, hardware_only: bool) -> Result<Vec<EncoderMft>> {
    let in_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: crate::encode::mf::mf_subtype(codec),
    };
    let flags = if hardware_only {
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER
    } else {
        MFT_ENUM_FLAG_HARDWARE
            | MFT_ENUM_FLAG_SYNCMFT
            | MFT_ENUM_FLAG_ASYNCMFT
            | MFT_ENUM_FLAG_LOCALMFT
            | MFT_ENUM_FLAG_SORTANDFILTER
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: out array is ours; freed below.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            flags,
            Some(&in_type),
            None,
            &mut activates,
            &mut count,
        )
        .with_context(|| format!("MFTEnumEx({} decoder)", codec.label()))?;
        if activates.is_null() || count == 0 {
            return Ok(Vec::new());
        }
        let slice = std::slice::from_raw_parts(activates, count as usize);
        let mut found = Vec::new();
        for a in slice.iter().flatten() {
            found.push(EncoderMft {
                friendly_name: get_string(a, &MFT_FRIENDLY_NAME_Attribute)
                    .unwrap_or_else(|| "(unnamed)".into()),
                hardware_url: get_string(a, &MFT_ENUM_HARDWARE_URL_Attribute),
            });
        }
        for i in 0..count as usize {
            std::ptr::drop_in_place(activates.add(i));
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
        Ok(found)
    }
}

/// Names of the DXGI adapters that drive a display. Creates no D3D device —
/// this runs in the short-lived `relay-share probe` child.
pub fn display_adapters() -> Vec<String> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let mut names = Vec::new();
    // SAFETY: plain DXGI enumeration; every interface is ref-counted.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return names;
        };
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            // Skip adapters with no output: the Microsoft Basic Render Driver
            // and headless compute cards are never what we capture from.
            if adapter.EnumOutputs(0).is_err() {
                continue;
            }
            if let Ok(desc) = adapter.GetDesc1() {
                let end = desc.Description.iter().position(|c| *c == 0).unwrap_or(0);
                names.push(String::from_utf16_lossy(&desc.Description[..end]));
            }
        }
    }
    names
}

pub fn wgc_supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

/// Codecs this PC can decode, in preference order, narrowed by
/// `RELAY_VIDEO_CODECS`. What a receiver registers for the answer. Needs MF
/// started.
pub fn decodable_codecs() -> Vec<VideoCodec> {
    crate::codec::filter_supported(&crate::codec::allowed_codecs(), |c| {
        decoders(c, false).map(|d| !d.is_empty()).unwrap_or(false)
    })
}

/// Full report; requires MF started (see [`MediaFoundation::start`]).
pub fn report() -> Result<ProbeReport> {
    Ok(ProbeReport {
        adapters: display_adapters(),
        hevc_hardware_encoders: hardware_encoders(VideoCodec::Hevc)?,
        hevc_hardware_decoders: decoders(VideoCodec::Hevc, true).unwrap_or_default(),
        hevc_any_decoders: decoders(VideoCodec::Hevc, false).unwrap_or_default(),
        h264_hardware_encoders: hardware_encoders(VideoCodec::H264).unwrap_or_default(),
        h264_any_decoders: decoders(VideoCodec::H264, false).unwrap_or_default(),
        wgc_supported: wgc_supported(),
    })
}
