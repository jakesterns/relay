//! Learned game EQ (S46).
//!
//! Relay never ships a per-game preset. Instead it listens to the game's own
//! audio (process loopback of the game's PID, the same OS path Discord and
//! OBS use), keeps only long-term *statistics* of what it hears, and derives
//! a gentle "game layer" EQ from them for the player's goal (Awareness,
//! Dialogue or Immersion): lift the bands where what the goal wants heard is
//! regularly buried, cut the low bands that what it wants tamed dominates.
//!
//! - [`analyzer`] the per-frame analysis: a 24-band 1/3-octave filterbank,
//!   a pitch tracker, onset detection, event segmentation, and nine sound
//!   classes (footsteps, foliage, mechanical, voice, gunshot, explosion,
//!   vehicle, music, ambience), with context rules for what counts.
//!   Allocation-free after construction.
//! - [`derive`] aggregates + goal → target curve via a per-band masking
//!   matrix, within hard limits (caps, smoothness, no boost below 80 Hz, a
//!   speech-band guard, never louder overall).
//! - [`state`] the confidence gate and the per-game learning record that is
//!   persisted between sessions (aggregates only — never audio).
//! - [`file`] the versioned import/export format (`docs/eq-file-format.md`).
//!
//! The game layer stacks on top of the listening device's headphone
//! correction (S41); it never replaces it.

// Per-band DSP reads several parallel arrays by band index; iterator chains
// would hide that. Tests pin the documented threshold values as asserts.
#![allow(clippy::needless_range_loop)]
#![cfg_attr(test, allow(clippy::assertions_on_constants))]

pub mod analyzer;
pub mod derive;
pub mod file;
pub mod state;
#[cfg(test)]
pub(crate) mod synth;

pub use analyzer::{Analyzer, SoundClass, Stats, NCLASSES};
pub use derive::{derive, derive_checked, Derived, Goal, Limits};
pub use file::{GameEqFile, GameEqLayer, LayerSource};
pub use state::{status, LearnRecord, LearnStatus, Outcome, Thresholds};

/// Band centres: the ISO 1/3-octave series from 50 Hz to 10 kHz.
pub const BANDS_HZ: [f32; NBANDS] = [
    50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0,
    1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10000.0,
];
/// Number of analysis bands.
pub const NBANDS: usize = 24;

/// Level histograms: 2 dB bins from -100 dBFS to 0 dBFS.
pub const HIST_BINS: usize = 50;
pub const HIST_LO_DB: f32 = -100.0;
pub const HIST_STEP_DB: f32 = 2.0;

/// How many cascade bands the game layer may use. The headset correction
/// keeps up to 8 and the user's own bands keep theirs.
pub const GAME_BUDGET: usize = 4;

/// The histogram bin a level falls in (clamped at both ends).
pub(crate) fn bin_of(db: f32) -> usize {
    let i = ((db - HIST_LO_DB) / HIST_STEP_DB).floor();
    if i.is_nan() || i < 0.0 {
        0
    } else {
        (i as usize).min(HIST_BINS - 1)
    }
}

/// Centre level of a bin.
pub(crate) fn bin_db(i: usize) -> f32 {
    HIST_LO_DB + (i as f32 + 0.5) * HIST_STEP_DB
}
