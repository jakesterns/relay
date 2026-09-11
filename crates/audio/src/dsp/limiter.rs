//! Band-split soft limiter — the "explosion tamer".
//!
//! The signal is split at `below_hz` with a 4th-order Linkwitz-Riley
//! crossover (magnitude-flat on recombination). Only the low band is
//! limited; the high band is delayed by the same look-ahead and summed back.
//!
//! The gain computer is a look-ahead peak limiter: desired gain per sample
//! (soft knee in dB), a running minimum over the look-ahead window
//! (monotonic wedge, fixed capacity), one-pole attack smoothing, and an
//! exponential release. The applied gain is clamped to the delayed sample's
//! own desired gain, so the ceiling is hard even mid-attack.

use serde::{Deserialize, Serialize};

use super::biquad::{Biquad, Coeffs, FilterKind};
use super::PrepareError;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LimiterParams {
    /// Crossover: only content below this frequency is limited.
    pub below_hz: f32,
    pub threshold_db: f32,
    /// Look-ahead; clamped to at most 1 ms (the brief's bound).
    #[serde(default = "default_lookahead_ms")]
    pub lookahead_ms: f32,
    #[serde(default = "default_release_ms")]
    pub release_ms: f32,
    /// Soft-knee width in dB, centred on the threshold.
    #[serde(default = "default_knee_db")]
    pub knee_db: f32,
}

fn default_lookahead_ms() -> f32 {
    1.0
}
fn default_release_ms() -> f32 {
    80.0
}
fn default_knee_db() -> f32 {
    3.0
}

impl LimiterParams {
    pub fn new(below_hz: f32, threshold_db: f32) -> Self {
        Self {
            below_hz,
            threshold_db,
            lookahead_ms: default_lookahead_ms(),
            release_ms: default_release_ms(),
            knee_db: default_knee_db(),
        }
    }
}

/// One channel of the band-split limiter.
pub struct Limiter {
    // LR4 crossover: two cascaded Butterworth sections per path.
    lp: [Biquad; 2],
    hp: [Biquad; 2],
    threshold_db: f64,
    knee_db: f64,
    /// Look-ahead in samples (≥ 1).
    lookahead: usize,
    att: f64,
    rel: f64,
    /// Smoothed gain state.
    smooth: f64,
    // Delay rings, all `lookahead` long.
    delay_low: Vec<f32>,
    delay_high: Vec<f32>,
    delay_gain: Vec<f64>,
    pos: usize,
    // Monotonic wedge for the running minimum over the last `lookahead + 1`
    // desired gains: (sequence, gain), increasing gain from front to back.
    wedge: Vec<(u64, f64)>,
    wedge_head: usize,
    wedge_len: usize,
    seq: u64,
}

impl Limiter {
    pub fn prepare(p: &LimiterParams, sample_rate: u32) -> Result<Self, PrepareError> {
        if !p.below_hz.is_finite()
            || p.below_hz <= 0.0
            || (p.below_hz as f64) >= sample_rate as f64 / 2.0
        {
            return Err(PrepareError::InvalidParam("limiter crossover outside (0, Nyquist)"));
        }
        if !p.release_ms.is_finite() || p.release_ms <= 0.0 {
            return Err(PrepareError::InvalidParam("limiter release must be positive"));
        }
        let fs = sample_rate as f64;
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let lp_c = Coeffs::design(FilterKind::LowPass, fs, p.below_hz as f64, 0.0, q)?;
        let hp_c = Coeffs::design(FilterKind::HighPass, fs, p.below_hz as f64, 0.0, q)?;
        let lookahead_ms = p.lookahead_ms.clamp(0.0, 1.0) as f64;
        let lookahead = ((lookahead_ms * fs / 1000.0).round() as usize).max(1);
        // Attack reaches ~98% of the way inside the look-ahead window.
        let att = 1.0 - (-4.0 / lookahead as f64).exp();
        let rel = 1.0 - (-1.0 / (p.release_ms as f64 * fs / 1000.0)).exp();
        Ok(Self {
            lp: [Biquad::new(lp_c); 2],
            hp: [Biquad::new(hp_c); 2],
            threshold_db: p.threshold_db as f64,
            knee_db: (p.knee_db as f64).max(0.0),
            lookahead,
            att,
            rel,
            smooth: 1.0,
            delay_low: vec![0.0; lookahead],
            delay_high: vec![0.0; lookahead],
            delay_gain: vec![1.0; lookahead],
            pos: 0,
            wedge: vec![(0, 1.0); lookahead + 1],
            wedge_head: 0,
            wedge_len: 0,
            seq: 0,
        })
    }

    pub fn lookahead_frames(&self) -> usize {
        self.lookahead
    }

    /// The gain currently applied to the low band (1.0 = no reduction).
    pub fn current_gain(&self) -> f64 {
        self.smooth
    }

    /// Desired gain for one rectified low-band level, soft knee in dB.
    #[inline]
    fn desired_gain(&self, level: f64) -> f64 {
        if level <= 0.0 {
            return 1.0;
        }
        let level_db = 20.0 * level.log10();
        let half_knee = self.knee_db / 2.0;
        let over = level_db - self.threshold_db;
        let reduction_db = if over <= -half_knee {
            0.0
        } else if over < half_knee && self.knee_db > 0.0 {
            let t = over + half_knee;
            (t * t) / (2.0 * self.knee_db)
        } else {
            over
        };
        10f64.powf(-reduction_db / 20.0)
    }

    #[inline]
    fn wedge_push(&mut self, seq: u64, gain: f64) {
        // Drop entries that can never be the minimum again.
        while self.wedge_len > 0 {
            let back = (self.wedge_head + self.wedge_len - 1) % self.wedge.len();
            if self.wedge[back].1 >= gain {
                self.wedge_len -= 1;
            } else {
                break;
            }
        }
        let slot = (self.wedge_head + self.wedge_len) % self.wedge.len();
        self.wedge[slot] = (seq, gain);
        self.wedge_len += 1;
        // Expire entries older than the window (lookahead + 1 samples).
        let horizon = seq.saturating_sub(self.lookahead as u64);
        while self.wedge_len > 0 && self.wedge[self.wedge_head].0 < horizon {
            self.wedge_head = (self.wedge_head + 1) % self.wedge.len();
            self.wedge_len -= 1;
        }
    }

    /// In-place: split, limit the low band, delay the high band, recombine.
    pub fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let x = *s as f64;
            let low1 = self.lp[0].process_sample(x);
            let low = self.lp[1].process_sample(low1);
            let high1 = self.hp[0].process_sample(x);
            let high = self.hp[1].process_sample(high1);

            let d = self.desired_gain(low.abs());
            self.seq += 1;
            let seq = self.seq;
            self.wedge_push(seq, d);
            let wmin = self.wedge[self.wedge_head].1;
            let coeff = if wmin < self.smooth { self.att } else { self.rel };
            self.smooth += coeff * (wmin - self.smooth);

            let low_out = self.delay_low[self.pos] as f64;
            let high_out = self.delay_high[self.pos] as f64;
            let d_out = self.delay_gain[self.pos];
            self.delay_low[self.pos] = low as f32;
            self.delay_high[self.pos] = high as f32;
            self.delay_gain[self.pos] = d;
            self.pos = (self.pos + 1) % self.lookahead;

            let g = self.smooth.min(d_out);
            *s = (g * low_out + high_out) as f32;
        }
    }

    pub fn flush_denormals(&mut self) {
        for b in self.lp.iter_mut().chain(self.hp.iter_mut()) {
            b.flush_denormals();
        }
        if self.smooth < 1e-25 {
            self.smooth = 0.0;
        }
    }

    pub fn state_is_denormal_free(&self) -> bool {
        self.lp.iter().chain(self.hp.iter()).all(|b| b.state_is_denormal_free())
            && !self.smooth.is_subnormal()
    }
}
