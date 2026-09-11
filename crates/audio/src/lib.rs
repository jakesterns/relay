//! Relay audio.
//!
//! - [`dsp`] biquad EQ cascade, band-split soft limiter, partitioned-convolution
//!   HRTF. Real-time safe: no allocations after `prepare()`, bypass is a straight
//!   copy. Runs at the endpoint sample rate; never resamples.
//! - [`offline`] renders a WAV through the chain for the A/B listening test.
//! - [`sessions`] WASAPI render-session enumeration and detection of
//!   exclusive-mode streams that bypass the APO (Windows only).
//! - `apo/` (M3b) will host [`dsp`] inside the Windows audio engine as a signed
//!   endpoint APO; not part of this crate yet.
//!
//! Constraints from the brief: OS-layer only (WASAPI / APO), never a global EQ,
//! detect WASAPI-exclusive streams that bypass the APO and report them.
//!
//! Unsafe code is denied crate-wide; only [`sessions`] (raw WASAPI/COM) may
//! opt back in, with SAFETY comments on every block.

#![deny(unsafe_code)]

pub mod dsp;
pub mod offline;
#[cfg(windows)]
pub mod sessions;

pub use dsp::{BandParams, Chain, ChainParams, FilterKind, LimiterParams, PrepareError};
