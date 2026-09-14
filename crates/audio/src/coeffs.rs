//! RBJ-cookbook biquad coefficients and their closed-form magnitude.
//!
//! Reference: Robert Bristow-Johnson, "Cookbook formulae for audio equalizer
//! biquad filter coefficients".
//!
//! This sits outside the `dsp` feature on purpose. Designing a filter and
//! asking what it does to a frequency is a few dozen floating-point
//! operations with no FFT and no allocation, and the always-on core needs it
//! to fit headset-correction curves ([`crate::fit`]). Everything that streams
//! audio — the cascade state, the limiter, the HRTF convolver — stays behind
//! the feature, so the core still never links the FFT.

use crate::params::FilterKind;

/// A filter could not be designed from the given parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DesignError {
    #[error("invalid parameter: {0}")]
    InvalidParam(&'static str),
}

/// Normalised transfer-function coefficients (a0 = 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    /// RBJ cookbook design. `freq_hz` must be in (0, fs/2), `q > 0`.
    pub fn design(
        kind: FilterKind,
        sample_rate: f64,
        freq_hz: f64,
        gain_db: f64,
        q: f64,
    ) -> Result<Coeffs, DesignError> {
        if !freq_hz.is_finite() || freq_hz <= 0.0 || freq_hz >= sample_rate / 2.0 {
            return Err(DesignError::InvalidParam("band frequency outside (0, Nyquist)"));
        }
        if !q.is_finite() || q <= 0.0 {
            return Err(DesignError::InvalidParam("Q must be positive"));
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq_hz / sample_rate;
        let (sw, cw) = w0.sin_cos();
        let alpha = sw / (2.0 * q);
        let sqrt_a = a.sqrt();

        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterKind::Peaking => (
                1.0 + alpha * a,
                -2.0 * cw,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cw,
                1.0 - alpha / a,
            ),
            FilterKind::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cw + 2.0 * sqrt_a * alpha),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                a * ((a + 1.0) - (a - 1.0) * cw - 2.0 * sqrt_a * alpha),
                (a + 1.0) + (a - 1.0) * cw + 2.0 * sqrt_a * alpha,
                -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                (a + 1.0) + (a - 1.0) * cw - 2.0 * sqrt_a * alpha,
            ),
            FilterKind::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cw + 2.0 * sqrt_a * alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                a * ((a + 1.0) + (a - 1.0) * cw - 2.0 * sqrt_a * alpha),
                (a + 1.0) - (a - 1.0) * cw + 2.0 * sqrt_a * alpha,
                2.0 * ((a - 1.0) - (a + 1.0) * cw),
                (a + 1.0) - (a - 1.0) * cw - 2.0 * sqrt_a * alpha,
            ),
            FilterKind::LowPass => {
                let b1 = 1.0 - cw;
                (b1 / 2.0, b1, b1 / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha)
            }
            FilterKind::HighPass => {
                let b1 = 1.0 + cw;
                (b1 / 2.0, -b1, b1 / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha)
            }
        };
        Ok(Coeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 })
    }

    /// Closed-form magnitude of `H(e^{jω})` at `freq_hz`, in dB.
    pub fn magnitude_db(&self, freq_hz: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq_hz / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }
}
