//! Parameter types for the signal chain — plain serde data, compiled even
//! without the `dsp` feature so the always-on core can describe a chain
//! (and ship it to the `relay-preview` child or, later, the APO's shared
//! memory) without linking the FFT machinery.

use serde::{Deserialize, Serialize};

/// Upper bound on EQ bands in one cascade.
pub const MAX_BANDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterKind {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

/// One EQ band as stored in a profile.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BandParams {
    pub kind: FilterKind,
    pub freq_hz: f32,
    /// Ignored for LowPass / HighPass.
    pub gain_db: f32,
    pub q: f32,
    /// A disabled band stays in the cascade as an identity stage.
    pub enabled: bool,
}

impl BandParams {
    pub fn peaking(freq_hz: f32, gain_db: f32, q: f32) -> Self {
        Self { kind: FilterKind::Peaking, freq_hz, gain_db, q, enabled: true }
    }
}

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

/// Everything `Chain::prepare` needs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChainParams {
    /// Up to [`MAX_BANDS`] EQ bands; extras are rejected at prepare.
    pub bands: Vec<BandParams>,
    pub limiter: Option<LimiterParams>,
    /// Spatialize stereo → binaural with the bundled "Relay Arena" IR set.
    pub hrtf: bool,
}
