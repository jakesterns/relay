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

/// Upper bound on dynamic-EQ bands in one stage.
pub const MAX_DYN_BANDS: usize = 4;

/// Reference the dynamic stage's levels are expressed against, dB: a level
/// `L` means "band level − program loudness + `DYN_REF_DB`". The learned
/// game statistics use the same scale (`learn::analyzer::REL_REF_DB`), so a
/// threshold read off a learned histogram drops straight in, and turning the
/// game or Windows volume up or down never moves where a band triggers.
pub const DYN_REF_DB: f32 = -50.0;

/// What a dynamic band does when its detector crosses the threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DynMode {
    /// Downward: above `threshold_db` the band is cut by
    /// `(level − threshold) × (1 − 1/ratio)`, at most `range_db`
    /// (gunshots, explosions).
    Tame,
    /// Upward: between `floor_db` and `threshold_db` the band is lifted by
    /// `(threshold − level) × (1 − 1/ratio)`, at most `range_db`, and never
    /// by more than the level stands above the floor, so silence, hiss and
    /// the steady bed are never raised and nothing is lifted past the
    /// threshold (quiet footsteps).
    Lift,
}

/// One dynamic-EQ band: a bell at `freq_hz` / `q` whose gain follows the
/// band's own level, stereo-linked.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DynBandParams {
    pub mode: DynMode,
    pub freq_hz: f32,
    pub q: f32,
    /// Relative level (see [`DYN_REF_DB`]) where the band starts acting.
    pub threshold_db: f32,
    /// ≥ 1. 1 = no action.
    pub ratio: f32,
    /// Largest cut (Tame) or boost (Lift), positive dB.
    pub range_db: f32,
    /// Lift only: no boost at or below this relative level.
    #[serde(default = "default_floor_db")]
    pub floor_db: f32,
    #[serde(default = "default_attack_ms")]
    pub attack_ms: f32,
    #[serde(default = "default_dyn_release_ms")]
    pub release_ms: f32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_floor_db() -> f32 {
    -100.0
}
fn default_attack_ms() -> f32 {
    3.0
}
fn default_dyn_release_ms() -> f32 {
    150.0
}
fn default_true() -> bool {
    true
}

/// The dynamic stage: up to [`MAX_DYN_BANDS`] bands, after the EQ cascade
/// and before the limiter.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DynamicsParams {
    pub bands: Vec<DynBandParams>,
    /// Hold Lift bands back while any Tame band is cutting (and through its
    /// release), so a gunshot's tail is never what gets lifted.
    #[serde(default)]
    pub duck_lift: bool,
}

impl DynamicsParams {
    /// True when the stage would leave the audio untouched.
    pub fn is_inert(&self) -> bool {
        !self.bands.iter().any(|b| b.enabled && b.range_db > 0.0 && b.ratio > 1.0)
    }

    /// Why this stage cannot be prepared, if it cannot.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.bands.len() > MAX_DYN_BANDS {
            return Err("too many dynamic bands");
        }
        self.bands.iter().try_for_each(DynBandParams::check)
    }
}

/// Hard bounds a dynamic band must sit inside to be prepared (the DSP's own
/// sanity limits; the learned and imported paths cap tighter).
pub mod dyn_bounds {
    pub const MIN_HZ: f32 = 20.0;
    pub const MAX_HZ: f32 = 20_000.0;
    pub const MIN_Q: f32 = 0.1;
    pub const MAX_Q: f32 = 10.0;
    pub const MIN_LEVEL_DB: f32 = -120.0;
    pub const MAX_LEVEL_DB: f32 = 40.0;
    pub const MAX_RATIO: f32 = 20.0;
    pub const MAX_RANGE_DB: f32 = 24.0;
    pub const MIN_ATTACK_MS: f32 = 0.1;
    pub const MAX_ATTACK_MS: f32 = 200.0;
    pub const MIN_RELEASE_MS: f32 = 5.0;
    pub const MAX_RELEASE_MS: f32 = 5000.0;
}

impl DynBandParams {
    /// Why this band cannot be prepared, if it cannot.
    pub fn check(&self) -> Result<(), &'static str> {
        use dyn_bounds::*;
        let all = [
            self.freq_hz,
            self.q,
            self.threshold_db,
            self.ratio,
            self.range_db,
            self.floor_db,
            self.attack_ms,
            self.release_ms,
        ];
        if all.iter().any(|x| !x.is_finite()) {
            return Err("dynamic band has a non-finite parameter");
        }
        if !(MIN_HZ..=MAX_HZ).contains(&self.freq_hz) {
            return Err("dynamic band frequency outside 20 Hz to 20 kHz");
        }
        if !(MIN_Q..=MAX_Q).contains(&self.q) {
            return Err("dynamic band Q outside 0.1 to 10");
        }
        if !(MIN_LEVEL_DB..=MAX_LEVEL_DB).contains(&self.threshold_db)
            || !(MIN_LEVEL_DB..=MAX_LEVEL_DB).contains(&self.floor_db)
        {
            return Err("dynamic band threshold or floor out of range");
        }
        if !(1.0..=MAX_RATIO).contains(&self.ratio) {
            return Err("dynamic band ratio outside 1 to 20");
        }
        if !(0.0..=MAX_RANGE_DB).contains(&self.range_db) {
            return Err("dynamic band range outside 0 to 24 dB");
        }
        if !(MIN_ATTACK_MS..=MAX_ATTACK_MS).contains(&self.attack_ms)
            || !(MIN_RELEASE_MS..=MAX_RELEASE_MS).contains(&self.release_ms)
        {
            return Err("dynamic band attack or release out of range");
        }
        if self.mode == DynMode::Lift && self.floor_db >= self.threshold_db {
            return Err("a lift band's floor must sit below its threshold");
        }
        Ok(())
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
    /// The dynamic stage (S49): after the EQ, before the limiter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamics: Option<DynamicsParams>,
}
