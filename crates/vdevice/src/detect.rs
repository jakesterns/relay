//! Read-only environment probes — safe anywhere, never write anything.
//!
//! - Windows build support for the frame-server virtual camera
//!   (`MFCreateVirtualCamera` needs Windows 11 22H2, build 22621+).
//! - OBS VirtualCam presence (the DirectShow filter CLSID) — the fallback
//!   the M5 plan names for pre-22H2 machines.
//! - VB-Cable / VoiceMeeter render endpoints — the interim virtual-mic
//!   route while the signed driver waits on the EV certificate.
//!
//! The names and the matching rules are portable; the probes themselves
//! (registry, MMDevice) live in the Windows-only `win` submodule.

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{frameserver_supported, mic_targets, obs_virtualcam, windows_build};

/// The OBS VirtualCam DirectShow filter CLSID (stable across OBS releases).
pub const OBS_VCAM_CLSID: &str = "{A3FCE0F5-3493-419F-958A-ABA1250EC20B}";

/// Minimum Windows build for `MFCreateVirtualCamera` (Windows 11 22H2).
pub const MIN_VCAM_BUILD: u32 = 22621;

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

/// Which virtual audio device a render endpoint's friendly name belongs to.
pub fn mic_kind(name: &str) -> Option<MicTargetKind> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_kind_matches_known_names() {
        assert_eq!(mic_kind("CABLE Input (VB-Audio Virtual Cable)"), Some(MicTargetKind::VbCable));
        assert_eq!(
            mic_kind("VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)"),
            Some(MicTargetKind::VoiceMeeter)
        );
        assert_eq!(mic_kind("Speakers (RØDECaster)"), None);
        assert_eq!(mic_kind("NVIDIA Broadcast"), None);
    }
}
