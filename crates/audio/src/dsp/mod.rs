//! The real-time signal chain: EQ cascade → band-split soft limiter → HRTF.
//!
//! Contract:
//! - `prepare(sample_rate, max_block)` may allocate; `process` never does.
//! - `process` on a bypassed chain is a plain `copy_from_slice`.
//! - The chain runs at the endpoint rate it was prepared for; it never
//!   resamples. HRTF requires a bundled IR at that exact rate.
//! - Denormals are flushed from every recursive state after each block so a
//!   fading tail cannot blow up CPU on x87/SSE without FTZ.

pub mod biquad;
pub mod hrtf;
pub mod limiter;

use serde::{Deserialize, Serialize};

pub use biquad::{BandParams, Coeffs, EqCascade, FilterKind, MAX_BANDS};
pub use hrtf::Hrtf;
pub use limiter::{Limiter, LimiterParams};

/// Everything `Chain::prepare` needs. Plain data so the core (and later the
/// APO's shared-memory control block) can build it from a `Profile`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChainParams {
    /// Up to [`MAX_BANDS`] EQ bands; extras are rejected at prepare.
    pub bands: Vec<BandParams>,
    pub limiter: Option<LimiterParams>,
    /// Spatialize stereo → binaural with the bundled "Relay Arena" IR set.
    pub hrtf: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("{0} EQ bands configured; the cascade holds at most {MAX_BANDS}")]
    TooManyBands(usize),
    #[error("no bundled HRTF impulse response at {0} Hz (bundled: 44100/48000/96000)")]
    HrtfUnsupportedRate(u32),
    #[error("invalid parameter: {0}")]
    InvalidParam(&'static str),
}

/// The full stereo chain. I/O is interleaved stereo `f32` (L R L R …).
pub struct Chain {
    params: ChainParams,
    bypass: bool,
    prepared: Option<Prepared>,
}

struct Prepared {
    eq: [EqCascade; 2],
    limiter: Option<[Limiter; 2]>,
    hrtf: Option<Hrtf>,
    /// De-interleave scratch, `max_block` frames per channel.
    left: Vec<f32>,
    right: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    max_block: usize,
}

impl Chain {
    pub fn new(params: ChainParams) -> Self {
        Self { params, bypass: false, prepared: None }
    }

    pub fn params(&self) -> &ChainParams {
        &self.params
    }

    /// True while the chain passes audio through untouched.
    pub fn bypassed(&self) -> bool {
        self.bypass
    }

    pub fn set_bypass(&mut self, on: bool) {
        self.bypass = on;
    }

    /// Latency introduced by the chain at the prepared rate, in frames.
    /// Limiter look-ahead + one HRTF partition.
    pub fn latency_frames(&self) -> usize {
        let p = match &self.prepared {
            Some(p) => p,
            None => return 0,
        };
        let lim = p.limiter.as_ref().map_or(0, |l| l[0].lookahead_frames());
        let hrtf = p.hrtf.as_ref().map_or(0, |h| h.latency_frames());
        lim + hrtf
    }

    /// Design all stages and allocate every buffer `process` will touch.
    /// `max_block` is in frames (samples per channel).
    pub fn prepare(&mut self, sample_rate: u32, max_block: usize) -> Result<(), PrepareError> {
        if self.params.bands.len() > MAX_BANDS {
            return Err(PrepareError::TooManyBands(self.params.bands.len()));
        }
        if sample_rate == 0 || max_block == 0 {
            return Err(PrepareError::InvalidParam("sample_rate and max_block must be non-zero"));
        }
        let eq = [
            EqCascade::design(&self.params.bands, sample_rate)?,
            EqCascade::design(&self.params.bands, sample_rate)?,
        ];
        let limiter = match &self.params.limiter {
            Some(lp) => {
                Some([Limiter::prepare(lp, sample_rate)?, Limiter::prepare(lp, sample_rate)?])
            }
            None => None,
        };
        let hrtf = if self.params.hrtf {
            Some(Hrtf::prepare_arena(sample_rate, max_block)?)
        } else {
            None
        };
        self.prepared = Some(Prepared {
            eq,
            limiter,
            hrtf,
            left: vec![0.0; max_block],
            right: vec![0.0; max_block],
            out_l: vec![0.0; max_block],
            out_r: vec![0.0; max_block],
            max_block,
        });
        Ok(())
    }

    /// Process one interleaved stereo block. `input.len() == output.len()`,
    /// even, and at most `2 * max_block`. Allocation-free; bypass is a copy.
    ///
    /// Panics if called before `prepare` (programming error, not a runtime
    /// condition — the APO host guarantees the order).
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        assert_eq!(input.len(), output.len(), "in/out length mismatch");
        if self.bypass {
            output.copy_from_slice(input);
            return;
        }
        let p = self.prepared.as_mut().expect("Chain::process before prepare");
        assert!(input.len() % 2 == 0, "interleaved stereo requires an even length");
        let frames = input.len() / 2;
        assert!(frames <= p.max_block, "block larger than prepared max_block");

        for i in 0..frames {
            p.left[i] = input[2 * i];
            p.right[i] = input[2 * i + 1];
        }
        p.eq[0].process(&mut p.left[..frames]);
        p.eq[1].process(&mut p.right[..frames]);
        if let Some(l) = p.limiter.as_mut() {
            l[0].process(&mut p.left[..frames]);
            l[1].process(&mut p.right[..frames]);
        }
        match p.hrtf.as_mut() {
            Some(h) => {
                h.process(
                    &p.left[..frames],
                    &p.right[..frames],
                    &mut p.out_l[..frames],
                    &mut p.out_r[..frames],
                );
                for i in 0..frames {
                    output[2 * i] = p.out_l[i];
                    output[2 * i + 1] = p.out_r[i];
                }
            }
            None => {
                for i in 0..frames {
                    output[2 * i] = p.left[i];
                    output[2 * i + 1] = p.right[i];
                }
            }
        }

        p.eq[0].flush_denormals();
        p.eq[1].flush_denormals();
        if let Some(l) = p.limiter.as_mut() {
            l[0].flush_denormals();
            l[1].flush_denormals();
        }
    }

    /// True when no recursive state holds a subnormal number. Used by the
    /// denormal-handling test; cheap enough to keep in release builds.
    pub fn state_is_denormal_free(&self) -> bool {
        match &self.prepared {
            None => true,
            Some(p) => {
                p.eq.iter().all(|c| c.state_is_denormal_free())
                    && p.limiter
                        .as_ref()
                        .is_none_or(|l| l.iter().all(|x| x.state_is_denormal_free()))
            }
        }
    }
}
