//! From aggregates to targets, in two steps:
//!
//! 1. [`derive_look`]: the game's own look → panel-neutral [`LookTargets`]
//!    (how much shadow recovery, saturation help and highlight caution this
//!    game wants). This is what an exported file carries.
//! 2. [`realize`]: [`LookTargets`] × one monitor's [`PanelCaps`] → concrete
//!    [`Adjustments`] in the units of controls Relay already drives — the
//!    gamma ramp, vendor vibrance, and at most one verified DDC/CI code.
//!
//! Every limit is a named constant with a test. Nothing here is extreme by
//! construction: each output is a clamp of a small range around neutral.

use serde::{Deserialize, Serialize};

use super::analyse::{FrameStats, HUE_BINS};

/// APL bucket edges; a "scene" is which bucket a frame's mean luma falls in.
/// Five buckets: night / dim / mid / bright / very bright.
pub const APL_BUCKET_EDGES: [f32; 4] = [0.10, 0.25, 0.45, 0.65];
pub const APL_BUCKETS: usize = APL_BUCKET_EDGES.len() + 1;

/// Crushed fraction a game may have before any shadow recovery is asked for.
/// Plenty of games are legitimately dark; 4 % of the frame at true black is
/// normal (sky at night, outlines, vignettes).
pub const CRUSH_OK: f32 = 0.04;
/// Crushed fraction above `CRUSH_OK` that maps to full shadow recovery.
pub const CRUSH_SPAN: f32 = 0.20;
/// Mean gradient among crushed pixels below which those pixels are flat:
/// real black, not hidden detail, so lifting them would only grey the image.
/// About one 8-bit code value per pixel step.
pub const CRUSH_DETAIL_MIN: f32 = 0.004;
/// Mean saturation at or above which the game needs no saturation help.
pub const SAT_TARGET: f32 = 0.35;
/// Saturation shortfall below `SAT_TARGET` that maps to full help.
pub const SAT_SPAN: f32 = 0.25;
/// If the top decile is already this saturated, any boost would clip
/// colours: saturation help is withheld.
pub const SAT_P90_CEILING: f32 = 0.85;
/// Clipped fraction a game may have before highlight caution starts.
pub const CLIP_OK: f32 = 0.02;
/// Clipped fraction above `CLIP_OK` that maps to full caution.
pub const CLIP_SPAN: f32 = 0.10;
/// Look targets are rounded to this step so tiny drift never reaches the
/// screen as a visible change.
pub const LOOK_STEP: f32 = 0.05;

/// Panel-neutral targets, each 0..=1. 0 = leave the game alone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct LookTargets {
    /// How much crushed shadow detail to bring back.
    pub shadow: f32,
    /// How much saturation help a washed-out game wants.
    pub saturation: f32,
    /// How much the game already clips highlights (reins in brightening).
    pub highlight: f32,
}

impl LookTargets {
    pub fn is_neutral(&self) -> bool {
        self.shadow == 0.0 && self.saturation == 0.0
    }

    /// Largest per-axis difference. The convergence and freeze tests use it.
    pub fn distance(&self, o: &LookTargets) -> f32 {
        (self.shadow - o.shadow)
            .abs()
            .max((self.saturation - o.saturation).abs())
            .max((self.highlight - o.highlight).abs())
    }

    /// Valid means finite and inside 0..=1 on every axis.
    pub fn is_valid(&self) -> bool {
        [self.shadow, self.saturation, self.highlight]
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
    }
}

fn step(v: f32) -> f32 {
    ((v.clamp(0.0, 1.0) / LOOK_STEP).round() * LOOK_STEP * 100.0).round() / 100.0
}

/// Running aggregate over gameplay frames. Sums are weighted so the window
/// can roll (see [`Aggregate::add`]); nothing per-frame is kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Aggregate {
    /// Effective weight (≈ frame count inside the rolling window).
    pub weight: f64,
    /// Gameplay frames ever counted since the last reset (not rolled).
    pub frames: u64,
    pub mean_luma: f64,
    pub crush_frac: f64,
    pub clip_frac: f64,
    /// Crushed-detail weighted by crushed fraction (so dark frames dominate).
    pub crushed_detail_w: f64,
    pub crush_w: f64,
    pub sat_mean: f64,
    pub sat_p90: f64,
    pub hue_hist: [f64; HUE_BINS],
    /// Weighted frames per APL bucket.
    pub apl_buckets: [f64; APL_BUCKETS],
    /// S48: squared sums of the three look inputs, for their standard
    /// errors. Absent (0) in records written before S48.
    #[serde(default)]
    pub crush_sq: f64,
    #[serde(default)]
    pub sat_sq: f64,
    #[serde(default)]
    pub clip_sq: f64,
}

pub fn apl_bucket(mean_luma: f32) -> usize {
    APL_BUCKET_EDGES.iter().take_while(|e| mean_luma >= **e).count()
}

impl Aggregate {
    /// Add one gameplay frame. Once the weight reaches `window`, every sum
    /// decays by `1 - 1/window` first, so the aggregate becomes an
    /// exponential moving average over roughly the last `window` frames —
    /// which is what lets a game update or a new area move it.
    pub fn add(&mut self, s: &FrameStats, window: f64) {
        if window > 0.0 && self.weight >= window {
            let k = 1.0 - 1.0 / window;
            self.weight *= k;
            self.mean_luma *= k;
            self.crush_frac *= k;
            self.clip_frac *= k;
            self.crushed_detail_w *= k;
            self.crush_w *= k;
            self.sat_mean *= k;
            self.sat_p90 *= k;
            self.hue_hist.iter_mut().for_each(|v| *v *= k);
            self.apl_buckets.iter_mut().for_each(|v| *v *= k);
            self.crush_sq *= k;
            self.sat_sq *= k;
            self.clip_sq *= k;
        }
        self.weight += 1.0;
        self.frames += 1;
        self.mean_luma += s.mean_luma as f64;
        self.crush_frac += s.crush_frac as f64;
        self.clip_frac += s.clip_frac as f64;
        self.crush_sq += (s.crush_frac as f64).powi(2);
        self.sat_sq += (s.sat_mean as f64).powi(2);
        self.clip_sq += (s.clip_frac as f64).powi(2);
        self.crushed_detail_w += (s.crushed_detail * s.crush_frac) as f64;
        self.crush_w += s.crush_frac as f64;
        self.sat_mean += s.sat_mean as f64;
        self.sat_p90 += s.sat_p90 as f64;
        for (a, v) in self.hue_hist.iter_mut().zip(s.hue_hist) {
            *a += v as f64;
        }
        self.apl_buckets[apl_bucket(s.mean_luma)] += 1.0;
    }

    /// S48: add another aggregate (a video file's, say) into this one. Sums
    /// add, so each side counts by its weight; if the total passes `window`
    /// both are scaled down together to it, as the rolling decay would.
    pub fn merge(&mut self, o: &Aggregate, window: f64) {
        self.weight += o.weight;
        self.frames += o.frames;
        self.mean_luma += o.mean_luma;
        self.crush_frac += o.crush_frac;
        self.clip_frac += o.clip_frac;
        self.crushed_detail_w += o.crushed_detail_w;
        self.crush_w += o.crush_w;
        self.sat_mean += o.sat_mean;
        self.sat_p90 += o.sat_p90;
        for (a, b) in self.hue_hist.iter_mut().zip(o.hue_hist) {
            *a += b;
        }
        for (a, b) in self.apl_buckets.iter_mut().zip(o.apl_buckets) {
            *a += b;
        }
        self.crush_sq += o.crush_sq;
        self.sat_sq += o.sat_sq;
        self.clip_sq += o.clip_sq;
        if window > 0.0 && self.weight > window {
            let k = window / self.weight;
            self.weight = window;
            for v in [
                &mut self.mean_luma,
                &mut self.crush_frac,
                &mut self.clip_frac,
                &mut self.crushed_detail_w,
                &mut self.crush_w,
                &mut self.sat_mean,
                &mut self.sat_p90,
                &mut self.crush_sq,
                &mut self.sat_sq,
                &mut self.clip_sq,
            ] {
                *v *= k;
            }
            self.hue_hist.iter_mut().for_each(|v| *v *= k);
            self.apl_buckets.iter_mut().for_each(|v| *v *= k);
        }
    }

    /// Standard errors of the three look axes (shadow, saturation,
    /// highlight, in look units) with samples `decorrelation` frames apart
    /// counted as one. `None` without squared sums (a pre-S48 record) or
    /// with under two independent samples.
    pub fn standard_errors(&self, decorrelation: f64) -> Option<[f32; 3]> {
        let n = self.weight / decorrelation.max(1.0);
        if n < 2.0 || (self.crush_sq <= 0.0 && self.crush_frac > 0.0) {
            return None;
        }
        let se = |sum: f64, sq: f64, span: f32| -> Option<f32> {
            let mean = sum / self.weight;
            let var = sq / self.weight - mean * mean;
            if var < -1e-9 {
                return None;
            }
            Some((var.max(0.0).sqrt() / n.sqrt()) as f32 / span)
        };
        Some([
            se(self.crush_frac, self.crush_sq, CRUSH_SPAN)?,
            se(self.sat_mean, self.sat_sq, SAT_SPAN)?,
            se(self.clip_frac, self.clip_sq, CLIP_SPAN)?,
        ])
    }

    fn mean(&self, v: f64) -> f32 {
        if self.weight <= 0.0 {
            0.0
        } else {
            (v / self.weight) as f32
        }
    }

    pub fn summary(&self) -> AggregateSummary {
        AggregateSummary {
            mean_luma: self.mean(self.mean_luma),
            crush_frac: self.mean(self.crush_frac),
            clip_frac: self.mean(self.clip_frac),
            crushed_detail: if self.crush_w > 0.0 {
                (self.crushed_detail_w / self.crush_w) as f32
            } else {
                0.0
            },
            sat_mean: self.mean(self.sat_mean),
            sat_p90: self.mean(self.sat_p90),
        }
    }
}

/// Means of the aggregate, for derivation and for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct AggregateSummary {
    pub mean_luma: f32,
    pub crush_frac: f32,
    pub clip_frac: f32,
    pub crushed_detail: f32,
    pub sat_mean: f32,
    pub sat_p90: f32,
}

/// The game's look, panel-neutral.
pub fn derive_look(a: &AggregateSummary) -> LookTargets {
    let shadow = if a.crushed_detail < CRUSH_DETAIL_MIN {
        0.0
    } else {
        (a.crush_frac - CRUSH_OK) / CRUSH_SPAN
    };
    let saturation =
        if a.sat_p90 >= SAT_P90_CEILING { 0.0 } else { (SAT_TARGET - a.sat_mean) / SAT_SPAN };
    let highlight = (a.clip_frac - CLIP_OK) / CLIP_SPAN;
    LookTargets { shadow: step(shadow), saturation: step(saturation), highlight: step(highlight) }
}

// ---------------------------------------------------------------------------
// Panel-aware realisation
// ---------------------------------------------------------------------------

/// Panel technology, from the user's hardware library (EDID cannot say).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PanelKind {
    Oled,
    Ips,
    Va,
    Tn,
    #[default]
    Unknown,
}

impl PanelKind {
    /// Parse the library's free-text panel field ("Nano IPS", "QD-OLED").
    pub fn from_label(s: &str) -> Self {
        let s = s.to_ascii_lowercase();
        if s.contains("oled") {
            PanelKind::Oled
        } else if s.contains("ips") {
            PanelKind::Ips
        } else if s.contains("va") {
            PanelKind::Va
        } else if s.contains("tn") {
            PanelKind::Tn
        } else {
            PanelKind::Unknown
        }
    }

    /// Guess from the model name or EDID id when the user has not said.
    /// "OLED" in the name wins; otherwise a short table of models whose
    /// panel type is published. `None` = no idea (stays Unknown).
    pub fn guess_from_model(name: &str, id: &str) -> Option<Self> {
        let hay = format!("{} {}", name, id).to_ascii_uppercase();
        if hay.contains("OLED") {
            return Some(PanelKind::Oled);
        }
        KNOWN_PANELS.iter().find(|(needle, _)| hay.contains(needle)).map(|(_, k)| *k)
    }

    fn is_lcd(self) -> bool {
        matches!(self, PanelKind::Ips | PanelKind::Va | PanelKind::Tn)
    }
}

/// Models with a published panel type, matched as a substring of the EDID
/// display name or the monitor id (upper-case). Short on purpose: a guess is
/// shown as a guess and the user can correct it.
pub const KNOWN_PANELS: &[(&str, PanelKind)] = &[
    ("GSM5C7C", PanelKind::Oled), // LG 32GS95UE (WOLED)
    ("32GS95UE", PanelKind::Oled),
    ("AW3423", PanelKind::Oled),
    ("AW2725DF", PanelKind::Oled),
    ("AW3225QF", PanelKind::Oled),
    ("PG27AQDM", PanelKind::Oled),
    ("AW2518H", PanelKind::Tn),
    ("XL2546", PanelKind::Tn),
    ("XL2566K", PanelKind::Tn),
    ("XL2411", PanelKind::Tn),
    ("AW2521H", PanelKind::Ips),
    ("AW2723DF", PanelKind::Ips),
    ("27GP850", PanelKind::Ips),
    ("27GN950", PanelKind::Ips),
    ("VG27AQ", PanelKind::Ips),
    ("C27G7", PanelKind::Va),
    ("C32G7", PanelKind::Va),
    ("ODYSSEY G7", PanelKind::Va),
];

/// What one monitor can do, as the core resolved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanelCaps {
    pub kind: PanelKind,
    /// Highest black-equalizer level the learner may use, only when the
    /// model's vendor code is a `VerifiedCode` *and* advertised. `None`
    /// otherwise (today: every model, as no row is verified yet).
    pub black_equalizer_max: Option<u16>,
}

/// Gamma multiplier for full shadow recovery on OLED. Gamma keeps code 0 at
/// 0, so true black stays true black; a raised black floor is exactly what
/// OLED owners bought the panel to avoid.
pub const OLED_MAX_GAMMA_BOOST: f32 = 0.15;
/// Same on LCD and unknown panels, where some of the recovery may come from
/// the black equalizer or a small shadow lift instead.
pub const LCD_MAX_GAMMA_BOOST: f32 = 0.10;
/// Ramp shadow lift (0..=100 profile units) for full recovery on IPS/VA/TN
/// without a verified black equalizer. Never used on OLED or unknown panels:
/// it raises the black floor.
pub const LCD_MAX_SHADOW_LIFT: i32 = 20;
/// Fraction of a verified black equalizer's range the learner may use.
pub const BLACK_EQ_MAX_FRACTION: f32 = 0.5;
/// Vibrance points above neutral (50) for full saturation help on LCD.
pub const LCD_MAX_VIBRANCE_BOOST: i32 = 12;
/// On OLED: OLED panels are usually wide-gamut, so the same boost reads
/// stronger.
pub const OLED_MAX_VIBRANCE_BOOST: i32 = 8;
/// How much full highlight caution scales the gamma boost down. A game that
/// already clips should not be brightened as much (on OLED, brighter mids
/// also push ABL harder).
pub const HIGHLIGHT_DAMPING: f32 = 0.5;
/// The learner never writes more than this many DDC/CI codes. Each one adds
/// a monitor write (60 ms settle on LG) to the restore path, and restore
/// must finish within its 200 ms budget. Brightness and contrast over DDC are
/// therefore never learned; see docs/plans/S47-learned-game-display.md.
pub const LEARNED_DDC_WRITES_MAX: usize = 1;

/// Concrete adjustments in profile units. Neutral = gamma 1, lift 0,
/// vibrance 50, no black equalizer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Adjustments {
    pub gamma: f32,
    pub shadow_lift: i32,
    pub vibrance: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub black_equalizer: Option<u16>,
    /// Plain-English notes for the UI ("OLED: black level left alone").
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self { gamma: 1.0, shadow_lift: 0, vibrance: 50, black_equalizer: None, notes: Vec::new() }
    }
}

impl Adjustments {
    pub fn ddc_writes(&self) -> usize {
        usize::from(self.black_equalizer.is_some())
    }
}

/// Look × panel → adjustments.
pub fn realize(look: &LookTargets, caps: &PanelCaps) -> Adjustments {
    let mut a = Adjustments::default();
    let look = LookTargets {
        shadow: look.shadow.clamp(0.0, 1.0),
        saturation: look.saturation.clamp(0.0, 1.0),
        highlight: look.highlight.clamp(0.0, 1.0),
    };
    let damp = 1.0 - look.highlight * HIGHLIGHT_DAMPING;
    let round2 = |v: f32| (v * 100.0).round() / 100.0;

    match caps.kind {
        PanelKind::Oled => {
            a.gamma = round2(1.0 + look.shadow * OLED_MAX_GAMMA_BOOST * damp);
            a.vibrance = 50 + (look.saturation * OLED_MAX_VIBRANCE_BOOST as f32).round() as i32;
            if look.shadow > 0.0 {
                a.notes.push("OLED: shadows opened with gamma; black level left alone".into());
            }
            if look.highlight > 0.0 {
                a.notes.push("OLED: brightening held back to spare the panel's ABL".into());
            }
        }
        kind => {
            if kind.is_lcd() {
                match caps.black_equalizer_max {
                    Some(max) if look.shadow > 0.0 => {
                        let lvl = (look.shadow * max as f32 * BLACK_EQ_MAX_FRACTION).round() as u16;
                        a.black_equalizer = (lvl > 0).then_some(lvl);
                    }
                    _ => {
                        a.shadow_lift = (look.shadow * LCD_MAX_SHADOW_LIFT as f32).round() as i32;
                    }
                }
            } else if look.shadow > 0.0 {
                a.notes.push("Panel type unknown: shadows opened with gamma only".into());
            }
            a.gamma = round2(1.0 + look.shadow * LCD_MAX_GAMMA_BOOST * damp);
            a.vibrance = 50 + (look.saturation * LCD_MAX_VIBRANCE_BOOST as f32).round() as i32;
        }
    }
    debug_assert!(a.ddc_writes() <= LEARNED_DDC_WRITES_MAX);
    a
}
