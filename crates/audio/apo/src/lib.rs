//! The Relay endpoint APO and its registration engine.
//!
//! Two halves, deliberately separable:
//! - [`regfile`] + [`fxstore`]: a pure model of an endpoint's FX property
//!   store (parsed from `reg export` output or read live), the install /
//!   uninstall planner, and the byte-for-byte restore proof. No COM, no
//!   Windows dependency — unit-tested against exported fixtures *before*
//!   anything touches a live registry (brief risk #2).
//! - [`com`]: the APO itself — `IAudioProcessingObject{,RT,Configuration}`
//!   hosting `relay_audio::dsp::Chain`, parameters read lock-free from the
//!   shared-memory block in `relay_audio::shm`.
//!
//! Constraints from the brief: exactly one render endpoint is ever modified,
//! the complete prior property store is written to disk before any change,
//! and uninstall restores it byte-for-byte.

pub mod fxstore;
pub mod ids;
pub mod regfile;

#[cfg(all(windows, feature = "com"))]
pub mod com;
#[cfg(windows)]
pub mod livereg;
