//! Relay virtual devices — receiver side.
//!
//! - `frames` — the NV12 frame ring: a named shared-memory section the
//!   receiver writes and the camera media source reads (seqlock per slot).
//! - `camera` — control side (`MFCreateVirtualCamera`, receiver process) and,
//!   behind the `com` feature, the media source COM DLL the Frame Server
//!   hosts.
//! - `reg` — pure registration planner: exactly which HKLM keys the camera
//!   DLL needs, and the `installed.json` record that makes them removable.
//! - `livereg` — the thin gated I/O layer that replays a plan
//!   (`RELAY_VDEVICE_ALLOW_LIVE_WRITE=1` + elevation only).
//! - `detect` — read-only environment probes: Windows build support, OBS
//!   VirtualCam presence, VB-Cable / VoiceMeeter render endpoints for the
//!   interim mic route.
//! - `installed` — the `installed.json` model: consent decision + every
//!   registered component, so opt-out can remove exactly what opt-in added.
//!
//! The signed virtual mic driver is deferred until the EV certificate exists
//! (tracked in the M3b/M5 plans); the interim mic route only *renders* to an
//! already-installed third-party device and installs nothing.
//!
//! Everything here must be listed on the "what we install / how to remove"
//! consent screen before any of it touches the machine.

// Shared memory, registry I/O and COM require unsafe; each module carries
// SAFETY notes. Pure modules stay safe.
#![deny(unsafe_code)]

pub mod installed;
pub mod reg;

#[cfg(windows)]
pub mod detect;
#[cfg(windows)]
pub mod frames;
#[cfg(windows)]
pub mod livereg;

#[cfg(windows)]
pub mod camera;

pub const CRATE: &str = "relay-vdevice";
