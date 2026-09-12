//! Control side of the virtual camera, run by the receiver process.
//!
//! Session lifetime + current-user access: the camera is visible only to
//! this user's apps and is torn down by Windows when the receiver exits —
//! there is nothing to clean up after a crash. The only persistent artefact
//! of the whole feature is the COM registration (see [`crate::reg`]),
//! which `MFCreateVirtualCamera` requires to be in place already.

#![allow(unsafe_code)] // Media Foundation calls; SAFETY notes inline

use windows::core::{Interface, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFVirtualCamera, MFCreateVirtualCamera, MFVirtualCameraAccess_CurrentUser,
    MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource,
};

use super::{RELAY_VCAM_ATTR_FPS, RELAY_VCAM_ATTR_HEIGHT, RELAY_VCAM_ATTR_WIDTH};
use crate::reg::{VCAM_CLSID, VCAM_FRIENDLY_NAME};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A running "Relay Camera". Dropping it stops and removes the camera.
pub struct VirtualCamera {
    cam: IMFVirtualCamera,
}

// SAFETY: IMFVirtualCamera is a free-threaded MF object; the receiver only
// touches it from one thread anyway.
unsafe impl Send for VirtualCamera {}

impl VirtualCamera {
    /// Create and start the camera for a stream of `width`×`height` at
    /// `fps`. Requires `MFStartup` in this process and the media source
    /// CLSID registered (HKLM); fails cleanly otherwise.
    pub fn start(width: u32, height: u32, fps: u32) -> windows::core::Result<Self> {
        let name = wide(VCAM_FRIENDLY_NAME);
        let clsid = wide(VCAM_CLSID);
        // SAFETY: NUL-terminated strings outlive the call.
        let cam = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                PCWSTR(name.as_ptr()),
                PCWSTR(clsid.as_ptr()),
                None,
            )?
        };
        // Geometry travels to the media source's activation attributes.
        let attrs: IMFAttributes = cam.cast()?;
        // SAFETY: valid attribute store; UINT32 sets cannot alias.
        unsafe {
            attrs.SetUINT32(&RELAY_VCAM_ATTR_WIDTH, width)?;
            attrs.SetUINT32(&RELAY_VCAM_ATTR_HEIGHT, height)?;
            attrs.SetUINT32(&RELAY_VCAM_ATTR_FPS, fps.clamp(1, 240))?;
            cam.Start(None)?;
        }
        Ok(Self { cam })
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        // SAFETY: valid camera object; best-effort teardown (session
        // lifetime would reclaim it anyway).
        unsafe {
            let _ = self.cam.Stop();
            let _ = self.cam.Remove();
            let _ = self.cam.Shutdown();
        }
    }
}
