//! The Relay virtual camera.
//!
//! Two paths, picked by [crate::detect::camera_path]: the Windows 11 22H2+
//! frame-server camera below, and on older Windows the DirectShow source
//! filter in [dshow] (S43), which the app loads itself. Both read the same
//! NV12 ring. The frame-server path has two halves:
//! - [`control`] — runs in the receiver process: `MFCreateVirtualCamera`
//!   with session lifetime + current-user access, so the camera exists only
//!   while a share is being received and disappears with the process.
//! - [`source`] (feature `com`) — the media source the Windows Camera Frame
//!   Server hosts: reads NV12 frames from the [`crate::frames`] ring and
//!   serves them to whichever app opened "Relay Camera".
//!
//! Dimension contract: the receiver knows the stream size (SPS probe) before
//! it creates the camera, and passes width / height / fps as custom
//! attributes on the `IMFVirtualCamera`, which the frame server hands to the
//! media source's activation attributes. The media source falls back to
//! 1080p60 if the attributes are missing.

use windows::core::GUID;

pub mod control;
pub mod diag;
#[cfg(feature = "com")]
pub mod dshow;
pub mod picture;
#[cfg(feature = "com")]
pub mod source;

/// GUID twin of [`crate::reg::VCAM_CLSID`].
pub const CLSID_RELAY_VCAM: GUID = GUID::from_u128(0x9B7E62D4_2A31_4C8E_8F5A_D0C4B6E91A27);

/// GUID twin of [crate::reg::DSHOW_CLSID] — the DirectShow filter (the
/// Windows 10 path; see [dshow]).
pub const CLSID_RELAY_DSHOW: GUID = GUID::from_u128(0x5E0B7C1F_8A34_4D62_9B1E_C47A2F90D835);

/// Custom activation attributes (UINT32) carrying the stream geometry.
pub const RELAY_VCAM_ATTR_WIDTH: GUID = GUID::from_u128(0x3E1D5B0A_92C7_4F64_A1B8_6D2E90C5F713);
pub const RELAY_VCAM_ATTR_HEIGHT: GUID = GUID::from_u128(0x51F2A8D6_0B3E_4C97_8E5D_24A7C1B96F08);
pub const RELAY_VCAM_ATTR_FPS: GUID = GUID::from_u128(0x6C4B9E21_D785_4A30_B6F1_08E3D2A75C94);

/// Default geometry when no attributes reach the media source.
pub const DEFAULT_WIDTH: u32 = 1920;
pub const DEFAULT_HEIGHT: u32 = 1080;
pub const DEFAULT_FPS: u32 = 60;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clsid_guid_matches_registry_string() {
        assert_eq!(
            format!("{{{:?}}}", CLSID_RELAY_VCAM).to_uppercase(),
            crate::reg::VCAM_CLSID.to_uppercase()
        );
        assert_eq!(
            format!("{{{:?}}}", CLSID_RELAY_DSHOW).to_uppercase(),
            crate::reg::DSHOW_CLSID.to_uppercase()
        );
    }
}
