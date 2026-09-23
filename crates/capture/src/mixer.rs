//! Per-track faders (S37): gain and mute for each audio track, shared between
//! the command loop that sets them and the audio thread that applies them.
//!
//! Applied in exactly two places — just before a frame is encoded on the
//! sender, and per decoded stream before the one sum on the receiver — and
//! nowhere else. Not a mixer engine: three tracks, three faders, a multiply.
//! Gains change by a linear ramp across the frame they change in, so a fader
//! move is never a click.
//!
//! Lock-free on purpose: the audio threads read these every 10 ms and must
//! never wait on the stdin loop.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::command::{FaderLevel, FaderSet};

/// Unity, as the fixed-point the atomics hold.
const MILLI: f32 = 1000.0;
/// Loudest a fader goes: +6 dB. Enough to lift a quiet game over a loud
/// mic; anything more is a limiter's job, which is not this path's.
pub const MAX_GAIN: f32 = 2.0;

/// One track's fader. `gain` is linear, 0.0–[`MAX_GAIN`]; muted reads as 0.
#[derive(Debug)]
pub struct Fader {
    gain_milli: AtomicU32,
    mute: AtomicBool,
}

impl Default for Fader {
    fn default() -> Self {
        Fader { gain_milli: AtomicU32::new(MILLI as u32), mute: AtomicBool::new(false) }
    }
}

impl Fader {
    pub fn set(&self, level: FaderLevel) {
        let g = if level.gain.is_finite() { level.gain.clamp(0.0, MAX_GAIN) } else { 1.0 };
        self.gain_milli.store((g * MILLI).round() as u32, Ordering::Relaxed);
        self.mute.store(level.mute, Ordering::Relaxed);
    }

    /// What to multiply by right now: the gain, or 0 when muted.
    pub fn target(&self) -> f32 {
        if self.mute.load(Ordering::Relaxed) {
            0.0
        } else {
            self.gain_milli.load(Ordering::Relaxed) as f32 / MILLI
        }
    }

    pub fn is_muted(&self) -> bool {
        self.mute.load(Ordering::Relaxed)
    }
}

/// Which fader a track reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// The program track: the app, or the whole desktop mix.
    App,
    /// Everything on the PC except the app (the exclude-loopback track).
    Rest,
    Mic,
    /// The call coming back from the receiving PC (S19), heard on the
    /// sender. Only the sender ever reads this one.
    Call,
}

/// The faders both engines know about. A track that is not being sent
/// or has not arrived simply has a fader nobody reads.
#[derive(Debug, Default)]
pub struct Faders {
    pub app: Fader,
    pub rest: Fader,
    pub mic: Fader,
    pub call: Fader,
}

impl Faders {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn get(&self, track: Track) -> &Fader {
        match track {
            Track::App => &self.app,
            Track::Rest => &self.rest,
            Track::Mic => &self.mic,
            Track::Call => &self.call,
        }
    }

    /// Apply a command. Faders it does not mention are left as they are.
    pub fn apply(&self, set: &FaderSet) {
        if let Some(l) = set.app {
            self.app.set(l);
        }
        if let Some(l) = set.rest {
            self.rest.set(l);
        }
        if let Some(l) = set.mic {
            self.mic.set(l);
        }
        if let Some(l) = set.call {
            self.call.set(l);
        }
    }
}

/// Multiply interleaved stereo `frame` by a gain that ramps linearly from
/// `from` to `to` across it, and return `to` for the caller to remember.
///
/// The ramp is per stereo pair, not per sample, so both channels get the
/// same gain at the same instant. A frame where nothing changed is one
/// multiply per sample and no branch inside the loop.
pub fn apply_gain(frame: &mut [f32], from: f32, to: f32) -> f32 {
    if from == to {
        if to != 1.0 {
            for s in frame.iter_mut() {
                *s *= to;
            }
        }
        return to;
    }
    let pairs = (frame.len() / 2).max(1) as f32;
    let step = (to - from) / pairs;
    let mut g = from;
    for pair in frame.chunks_exact_mut(2) {
        g += step;
        pair[0] *= g;
        pair[1] *= g;
    }
    to
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(gain: f32, mute: bool) -> FaderLevel {
        FaderLevel { gain, mute }
    }

    #[test]
    fn a_fader_starts_at_unity_and_mute_reads_as_silence() {
        let f = Fader::default();
        assert_eq!(f.target(), 1.0);
        f.set(level(0.5, false));
        assert_eq!(f.target(), 0.5);
        f.set(level(0.5, true));
        assert_eq!(f.target(), 0.0);
        assert!(f.is_muted());
        // Unmuting restores the gain that was set, not unity.
        f.set(level(0.5, false));
        assert_eq!(f.target(), 0.5);
    }

    #[test]
    fn gain_is_clamped_and_garbage_is_unity() {
        let f = Fader::default();
        f.set(level(9.0, false));
        assert_eq!(f.target(), MAX_GAIN);
        f.set(level(-1.0, false));
        assert_eq!(f.target(), 0.0);
        f.set(level(f32::NAN, false));
        assert_eq!(f.target(), 1.0);
    }

    #[test]
    fn a_command_leaves_unmentioned_faders_alone() {
        let f = Faders::default();
        f.rest.set(level(0.25, false));
        f.apply(&FaderSet {
            app: Some(level(0.5, false)),
            rest: None,
            mic: Some(level(1.0, true)),
            call: None,
        });
        assert_eq!(f.app.target(), 0.5);
        assert_eq!(f.rest.target(), 0.25, "untouched");
        assert_eq!(f.mic.target(), 0.0);
        assert_eq!(f.call.target(), 1.0, "untouched");
        f.apply(&FaderSet { call: Some(level(0.75, false)), ..Default::default() });
        assert_eq!(f.get(Track::Call).target(), 0.75);
    }

    #[test]
    fn a_steady_gain_is_a_plain_multiply_and_unity_is_free() {
        let mut frame = vec![0.5f32; 8];
        assert_eq!(apply_gain(&mut frame, 1.0, 1.0), 1.0);
        assert!(frame.iter().all(|&s| s == 0.5));
        assert_eq!(apply_gain(&mut frame, 0.5, 0.5), 0.5);
        assert!(frame.iter().all(|&s| s == 0.25));
    }

    #[test]
    fn a_change_ramps_across_the_frame_and_lands_exactly() {
        // Four stereo pairs, 1.0 -> 0.0: no pair jumps, and the last pair is
        // fully at the target. That is what stops a mute from clicking.
        let mut frame = vec![1.0f32; 8];
        assert_eq!(apply_gain(&mut frame, 1.0, 0.0), 0.0);
        let left: Vec<f32> = frame.chunks_exact(2).map(|p| p[0]).collect();
        assert_eq!(left, [0.75, 0.5, 0.25, 0.0]);
        // Both channels of a pair get the same gain.
        assert_eq!(frame[0], frame[1]);
        assert_eq!(frame[6], frame[7]);
    }
}
