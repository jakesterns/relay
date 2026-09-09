//! Relay virtual devices — receiver side.
//!
//! Planned layout:
//! - `camera/` prefer the Windows 11 virtual camera (MediaFrameSource) API;
//!   fall back to detecting an installed OBS VirtualCam.
//! - `mic/` signed audio-class virtual microphone driver (the one kernel
//!   component in the product). Installed only after explicit opt-in.
//!
//! Both must be listed on the "what we install / how to remove" screen.

#![forbid(unsafe_code)]

pub const CRATE: &str = "relay-vdevice";
