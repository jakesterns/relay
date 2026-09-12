//! Relay display: the primitive display-control operations.
//!
//! Layout:
//! - [`vcp`] — VCP opcode constants, per-model quirks (write delays, vendor
//!   codes) and the pure profile-field → opcode mapping. No OS calls.
//! - [`gamma`] — gamma-ramp maths (pure) and the GDI `SetDeviceGammaRamp`
//!   read/write path per monitor DC. Vendor-neutral; the only GPU path on
//!   non-NVIDIA hardware until ADLX lands.
//! - [`ddc`] — DDC/CI monitor controls over `dxva2`: physical-monitor handles
//!   from an `HMONITOR`, get/set VCP with retries and write delays, because
//!   DDC/CI is slow and flaky.
//! - [`nvapi`] — digital vibrance and hue via `nvapi64.dll`, loaded
//!   dynamically so there is no link-time dependency and non-NVIDIA machines
//!   simply report "unavailable".
//!
//! This crate deliberately does not depend on `relay-core`: it exposes raw
//! operations and raw state; the `DisplayControl` adapter in the core is what
//! enforces capture-before-apply and owns the snapshot shape.

pub mod gamma;
pub mod vcp;

#[cfg(windows)]
pub mod ddc;
#[cfg(windows)]
pub mod nvapi;

pub const CRATE: &str = "relay-display";
