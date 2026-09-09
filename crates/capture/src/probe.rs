//! Capability probes: which hardware HEVC encoder MFTs exist, and whether
//! Windows.Graphics.Capture is available. `relay-share probe` prints this and
//! the output is recorded in docs/plans/M4-share.md → Measurements.

use anyhow::{Context, Result};
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MFMediaType_Video, MFShutdown, MFStartup, MFTEnumEx,
    MFT_ENUM_HARDWARE_URL_Attribute, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_HEVC,
    MFSTARTUP_LITE, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MF_VERSION,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct EncoderMft {
    pub friendly_name: String,
    pub hardware_url: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeReport {
    pub hevc_hardware_encoders: Vec<EncoderMft>,
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

/// Hardware HEVC encoder MFTs, best first (`MFT_ENUM_FLAG_SORTANDFILTER`).
/// Software MFTs are deliberately not requested: there is no CPU encode path.
pub fn hevc_hardware_encoders() -> Result<Vec<EncoderMft>> {
    let out_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
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
    .context("MFTEnumEx(video encoder, HEVC, hardware)")?;

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

pub fn wgc_supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

/// Full report; requires MF started (see [`MediaFoundation::start`]).
pub fn report() -> Result<ProbeReport> {
    Ok(ProbeReport {
        hevc_hardware_encoders: hevc_hardware_encoders()?,
        wgc_supported: wgc_supported(),
    })
}
