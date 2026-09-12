//! Read-only environment probes — safe anywhere, never write anything.
//!
//! - Windows build support for the frame-server virtual camera
//!   (`MFCreateVirtualCamera` needs Windows 11 22H2, build 22621+).
//! - OBS VirtualCam presence (the DirectShow filter CLSID) — the fallback
//!   the M5 plan names for pre-22H2 machines.
//! - VB-Cable / VoiceMeeter render endpoints — the interim virtual-mic
//!   route while the signed driver waits on the EV certificate.

#![allow(unsafe_code)] // registry reads and MMDevice enumeration

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Media::Audio::{
    eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::Foundation::PROPERTYKEY;

/// The OBS VirtualCam DirectShow filter CLSID (stable across OBS releases).
pub const OBS_VCAM_CLSID: &str = "{A3FCE0F5-3493-419F-958A-ABA1250EC20B}";

/// Minimum Windows build for `MFCreateVirtualCamera` (Windows 11 22H2).
pub const MIN_VCAM_BUILD: u32 = 22621;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a REG_SZ value under HKLM. Returns `None` on any failure — these
/// are presence probes, not error paths.
fn hklm_sz(path: &str, value: &str) -> Option<String> {
    let path_w = wide(path);
    let value_w = wide(value);
    let mut len: u32 = 0;
    // SAFETY: NUL-terminated names; first call sizes, second fills.
    unsafe {
        let err = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path_w.as_ptr()),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut len),
        );
        if err != ERROR_SUCCESS || len < 2 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let err = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path_w.as_ptr()),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        );
        if err != ERROR_SUCCESS {
            return None;
        }
        let units: Vec<u16> =
            buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
    }
}

/// The running Windows build number (from the registry; no manifest games).
pub fn windows_build() -> Option<u32> {
    hklm_sz(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber")?
        .parse()
        .ok()
}

/// True when the frame-server virtual camera API is available.
pub fn frameserver_supported() -> bool {
    windows_build().is_some_and(|b| b >= MIN_VCAM_BUILD)
}

/// The OBS VirtualCam filter DLL path, when the filter is registered.
pub fn obs_virtualcam() -> Option<String> {
    hklm_sz(&format!(r"SOFTWARE\Classes\CLSID\{OBS_VCAM_CLSID}\InprocServer32"), "")
}

/// Which third-party virtual audio device an endpoint belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicTargetKind {
    VbCable,
    VoiceMeeter,
}

/// A render endpoint the interim mic route can feed: playing into it makes
/// the paired virtual *capture* device carry our audio.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MicTarget {
    /// MMDevice endpoint id (pass to `--mic-route`).
    pub endpoint_id: String,
    /// Friendly name, e.g. "CABLE Input (VB-Audio Virtual Cable)".
    pub name: String,
    pub kind: MicTargetKind,
}

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

fn mic_kind(name: &str) -> Option<MicTargetKind> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("cable input") || lower.contains("vb-audio virtual cable") {
        Some(MicTargetKind::VbCable)
    } else if lower.starts_with("voicemeeter input")
        || lower.starts_with("voicemeeter aux input")
        || lower.starts_with("voicemeeter vaio")
    {
        Some(MicTargetKind::VoiceMeeter)
    } else {
        None
    }
}

/// Enumerate active render endpoints and keep the VB-Cable / VoiceMeeter
/// inputs. Requires COM initialised on the calling thread.
pub fn mic_targets() -> windows::core::Result<Vec<MicTarget>> {
    // SAFETY: standard MMDevice enumeration; strings are copied out and
    // freed, PROPVARIANTs cleared after reading (same as core's probe).
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let coll = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let n = coll.GetCount()?;
        let mut out = Vec::new();
        for i in 0..n {
            let dev = coll.Item(i)?;
            let Some(id) = device_id(&dev) else { continue };
            let Some(name) = friendly_name(&dev) else { continue };
            if let Some(kind) = mic_kind(&name) {
                out.push(MicTarget { endpoint_id: id, name, kind });
            }
        }
        Ok(out)
    }
}

unsafe fn device_id(dev: &IMMDevice) -> Option<String> {
    // SAFETY: `p` is the NUL-terminated id the OS allocated; freed after.
    unsafe {
        let p = dev.GetId().ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

unsafe fn friendly_name(dev: &IMMDevice) -> Option<String> {
    // SAFETY: PROPVARIANT union read guarded by the vt tag; cleared after.
    unsafe {
        let store = dev.OpenPropertyStore(STGM_READ).ok()?;
        let mut pv = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME).ok()?;
        let out = (pv.Anonymous.Anonymous.vt == VT_LPWSTR)
            .then(|| pv.Anonymous.Anonymous.Anonymous.pwszVal.to_string().ok())
            .flatten();
        let _ = PropVariantClear(&mut pv);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_kind_matches_known_names() {
        assert_eq!(
            mic_kind("CABLE Input (VB-Audio Virtual Cable)"),
            Some(MicTargetKind::VbCable)
        );
        assert_eq!(mic_kind("VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)"),
            Some(MicTargetKind::VoiceMeeter));
        assert_eq!(mic_kind("Speakers (RØDECaster)"), None);
        assert_eq!(mic_kind("NVIDIA Broadcast"), None);
    }

    #[test]
    fn windows_build_parses_on_this_machine() {
        // Read-only probe; any modern Windows has a numeric build.
        let b = windows_build().expect("build number");
        assert!(b > 10_000);
    }
}
