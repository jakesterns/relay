//! Relay audio.
//!
//! Planned layout (nothing here is wired yet):
//! - `dsp/` biquad EQ cascade, partitioned-convolution HRTF, soft limiter.
//!   Real-time safe: no allocations after `prepare()`, bypass is a straight
//!   copy. Runs at the endpoint sample rate; never resamples.
//! - `apo/` the signed endpoint APO (`cdylib`) hosting `dsp` inside the
//!   Windows audio engine for one render endpoint only.
//! - `control` parameter block shared between the core and the APO, and the
//!   `AudioControl` adapter for `relay_core::apply`.
//!
//! Constraints from the brief: OS-layer only (WASAPI / APO), never a global EQ,
//! detect WASAPI-exclusive streams that bypass the APO and report them.

#![forbid(unsafe_code)]

/// Placeholder so the crate has a public surface to grow from.
pub const CRATE: &str = "relay-audio";
