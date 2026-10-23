//! Relay display: the primitive display-control operations.
//!
//! Layout:
//! - [`vcp`] — VCP opcode constants, per-model quirks (write delays, vendor
//!   codes) and the pure profile-field → opcode mapping. No OS calls.
//! - [`gamma`] — gamma-ramp maths (pure) and the GDI `SetDeviceGammaRamp`
//!   read/write path per monitor DC. Vendor-neutral: gamma, contrast and
//!   shadow lift ride this path on every GPU, AMD included.
//! - [`ddc`] — DDC/CI monitor controls over `dxva2`: physical-monitor handles
//!   from an `HMONITOR`, get/set VCP with retries and write delays, because
//!   DDC/CI is slow and flaky.
//! - [`nvapi`] — digital vibrance and hue via `nvapi64.dll`, loaded
//!   dynamically so there is no link-time dependency and non-NVIDIA machines
//!   simply report "unavailable".
//! - [`amd`] — the same two controls on AMD, via the AMD Display Library
//!   (`atiadlxx.dll`), loaded the same way. The profile → raw-unit curve is
//!   *not* NVIDIA's, because the two drivers do not expose the same control;
//!   the module docs carry the reasoning.
//! - [`mode`] (S52) — Windows display mode (resolution × refresh) and HDR on
//!   one display: pure helpers plus the live calls.
//! - [`nvcolor`] (S52) — NVIDIA output colour (format, dynamic range, bit
//!   depth) as plain values; the NvAPI calls live in [`nvapi`].
//! - [`recommend`] (S52) — suggested values per panel kind and GPU vendor,
//!   each with a one-line reason. Pure; never applies anything.
//!
//! This crate deliberately does not depend on `relay-core`: it exposes raw
//! operations and raw state; the `DisplayControl` adapter in the core is what
//! enforces capture-before-apply and owns the snapshot shape.

pub mod gamma;
pub mod learn;
pub mod mode;
pub mod nvcolor;
pub mod recommend;
pub mod vcp;

#[cfg(windows)]
pub mod amd;
#[cfg(windows)]
pub mod ddc;
#[cfg(windows)]
pub mod nvapi;

pub const CRATE: &str = "relay-display";
