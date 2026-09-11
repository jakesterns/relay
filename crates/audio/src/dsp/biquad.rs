//! RBJ-cookbook biquads. Coefficients and state in `f64`, I/O in `f32`.
//!
//! Reference: Robert Bristow-Johnson, "Cookbook formulae for audio equalizer
//! biquad filter coefficients".

use super::PrepareError;
pub use crate::params::{BandParams, FilterKind, MAX_BANDS};

/// Any recursive state smaller than this is snapped to zero after each block.
/// Far below hearing (−500 dB) and far above `f64::MIN_POSITIVE`.
const DENORMAL_FLOOR: f64 = 1e-25;

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
    ) -> Result<Coeffs, PrepareError> {
        if !freq_hz.is_finite() || freq_hz <= 0.0 || freq_hz >= sample_rate / 2.0 {
            return Err(PrepareError::InvalidParam("band frequency outside (0, Nyquist)"));
        }
        if !q.is_finite() || q <= 0.0 {
            return Err(PrepareError::InvalidParam("Q must be positive"));
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

/// One biquad section, transposed direct form II.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    c: Coeffs,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(c: Coeffs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    pub fn process_sample(&mut self, x: f64) -> f64 {
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        y
    }

    /// In-place block processing.
    pub fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = self.process_sample(*s as f64) as f32;
        }
    }

    pub fn coeffs(&self) -> &Coeffs {
        &self.c
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn flush_denormals(&mut self) {
        if self.z1.abs() < DENORMAL_FLOOR {
            self.z1 = 0.0;
        }
        if self.z2.abs() < DENORMAL_FLOOR {
            self.z2 = 0.0;
        }
    }

    pub fn state_is_denormal_free(&self) -> bool {
        !self.z1.is_subnormal() && !self.z2.is_subnormal()
    }
}

/// Up to [`MAX_BANDS`] biquads in series, with per-band runtime bypass.
pub struct EqCascade {
    sections: Vec<Biquad>,
    enabled: Vec<bool>,
}

impl EqCascade {
    pub fn design(bands: &[BandParams], sample_rate: u32) -> Result<Self, PrepareError> {
        if bands.len() > MAX_BANDS {
            return Err(PrepareError::TooManyBands(bands.len()));
        }
        let mut sections = Vec::with_capacity(bands.len());
        let mut enabled = Vec::with_capacity(bands.len());
        for b in bands {
            sections.push(Biquad::new(Coeffs::design(
                b.kind,
                sample_rate as f64,
                b.freq_hz as f64,
                b.gain_db as f64,
                b.q as f64,
            )?));
            enabled.push(b.enabled);
        }
        Ok(Self { sections, enabled })
    }

    /// Toggle one band without redesigning. Out-of-range indices are ignored.
    pub fn set_band_enabled(&mut self, index: usize, on: bool) {
        if let Some(e) = self.enabled.get_mut(index) {
            *e = on;
            if !on {
                self.sections[index].reset();
            }
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for (i, s) in self.sections.iter_mut().enumerate() {
            if self.enabled[i] {
                s.process(buf);
            }
        }
    }

    /// Sum of the enabled bands' closed-form responses, in dB.
    pub fn magnitude_db(&self, freq_hz: f64, sample_rate: f64) -> f64 {
        self.sections
            .iter()
            .zip(&self.enabled)
            .filter(|(_, e)| **e)
            .map(|(s, _)| s.coeffs().magnitude_db(freq_hz, sample_rate))
            .sum()
    }

    pub fn flush_denormals(&mut self) {
        for s in &mut self.sections {
            s.flush_denormals();
        }
    }

    pub fn state_is_denormal_free(&self) -> bool {
        self.sections.iter().all(|s| s.state_is_denormal_free())
    }
}
