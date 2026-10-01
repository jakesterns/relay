//! Aggregates + the player's goal → the game-layer EQ curve.
//!
//! **The masking matrix.** For every *target* class T (what the goal wants
//! heard) and every *masker* class M (what the goal wants tamed), per band:
//! the share of M's material that sits within [`CLEAR_DB`] of T's typical
//! level there — loud enough that T would not stand clear of it. Weighted by
//! how much of the time M is on, that is how often T is buried in the band
//! ([`Derived::buried`]). Overlapping sounds never have to be separated:
//! each class's level distribution and its share of the time are enough.
//!
//! Then, bounded by [`Limits`]:
//! - **Lift** a band by `max_boost × target weight × T's presence there ×
//!   how often T is buried` (largest over targets).
//! - **Cut** a low band (≤ [`CUT_MAX_HZ`]) by how far a masker's level sticks
//!   out above the mix (gently: [`CUT_SLOPE`] dB per dB), never where a
//!   target lives.
//! - **Goal scale**: Immersion scales the whole curve by [`IMMERSION_SCALE`].
//! - No boost at or below [`NO_BOOST_BELOW_HZ`]; 1-2-1 smoothing; at most
//!   [`MAX_STEP_DB`] between neighbouring bands; caps
//!   [`MAX_BOOST_DB`] / −[`MAX_CUT_DB`].
//! - **Speech guard**, every goal: no band from [`SPEECH_GUARD_LO_HZ`] to
//!   [`SPEECH_GUARD_HI_HZ`] is cut by more than [`SPEECH_GUARD_DB`], so
//!   callouts and chat stay intelligible.
//! - **Hearing rule**: the curve may not make the game louder overall. The
//!   power gain weighted by the game's own spectrum must be ≤ 0 dB; if not,
//!   the boosts are scaled down until it is. Peaks are the limiter's job.

use serde::{Deserialize, Serialize};

use super::analyzer::{SoundClass, Stats, NCLASSES};
use super::{BANDS_HZ, GAME_BUDGET, NBANDS};
use crate::params::BandParams;

/// A target needs this much clearance over a masker to be clearly heard.
pub const CLEAR_DB: f32 = 6.0;
/// Bands further than this below a target's strongest band are not its bands.
pub const PRESENCE_RANGE_DB: f32 = 12.0;
/// Masker dominance below this is ignored.
pub const DOMINANCE_FLOOR_DB: f32 = 6.0;
/// dB of cut per dB of masker dominance above the floor.
pub const CUT_SLOPE: f32 = 0.3;
/// Only bands at or below this are cut.
pub const CUT_MAX_HZ: f32 = 315.0;
/// A band is left uncut if a target's weighted presence there is above this.
pub const CUE_PROTECT: f32 = 0.5;
/// Largest boost the game layer may ask for, dB.
pub const MAX_BOOST_DB: f32 = 6.0;
/// Deepest cut, dB.
pub const MAX_CUT_DB: f32 = 9.0;
/// Largest step between neighbouring 1/3-octave bands, dB.
pub const MAX_STEP_DB: f32 = 3.0;
/// Never boost at or below this frequency.
pub const NO_BOOST_BELOW_HZ: f32 = 80.0;
/// Masker events at which a masker's low cut reaches full depth.
pub const FULL_CUT_MASKERS: u64 = 30;
/// The speech band the guard protects.
pub const SPEECH_GUARD_LO_HZ: f32 = 300.0;
pub const SPEECH_GUARD_HI_HZ: f32 = 4000.0;
/// Deepest net cut allowed inside it, dB.
pub const SPEECH_GUARD_DB: f32 = 2.0;
/// Immersion is gentle: the whole curve at this fraction.
pub const IMMERSION_SCALE: f32 = 0.5;

/// What the player wants from this game. Stored per profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Goal {
    /// Hear footsteps, foliage, reloads and callouts; tame explosions, music
    /// and vehicles.
    #[default]
    Awareness,
    /// Voices clear over effects and music.
    Dialogue,
    /// A gentle balance, close to the game's own mix.
    Immersion,
}

/// Per-class weights a goal uses, in [`SoundClass::ALL`] order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassWeights {
    /// How much each class should be heard (0..1).
    pub target: [f32; NCLASSES],
    /// How much each class should be tamed (0..1).
    pub masker: [f32; NCLASSES],
    /// Whole-curve scale.
    pub scale: f32,
}

impl Goal {
    pub const ALL: [Goal; 3] = [Goal::Awareness, Goal::Dialogue, Goal::Immersion];

    /// One line for the goal prompt.
    pub fn describe(self) -> &'static str {
        match self {
            Goal::Awareness => {
                "Hear footsteps, reloads and callouts; tame explosions, music and engines."
            }
            Goal::Dialogue => "Keep voices clear over effects and music.",
            Goal::Immersion => "A gentle balance that stays close to the game's own mix.",
        }
    }

    /// Class weights, in order: footsteps, foliage, mechanical, voice,
    /// gunshot, explosion, vehicle, music, ambience.
    pub fn weights(self) -> ClassWeights {
        match self {
            Goal::Awareness => ClassWeights {
                target: [1.0, 0.8, 0.8, 0.6, 0.0, 0.0, 0.0, 0.0, 0.0],
                masker: [0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 0.8, 0.8, 0.5],
                scale: 1.0,
            },
            Goal::Dialogue => ClassWeights {
                target: [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                masker: [0.2, 0.2, 0.2, 0.0, 0.6, 0.8, 0.6, 1.0, 0.6],
                scale: 1.0,
            },
            Goal::Immersion => ClassWeights {
                target: [0.4, 0.3, 0.3, 0.4, 0.0, 0.0, 0.0, 0.0, 0.0],
                masker: [0.0, 0.0, 0.0, 0.0, 0.2, 0.4, 0.2, 0.2, 0.2],
                scale: IMMERSION_SCALE,
            },
        }
    }

    /// Classes whose events count as evidence: (targets, maskers).
    pub fn evidence_classes(self) -> (&'static [SoundClass], &'static [SoundClass]) {
        use SoundClass::*;
        const MASKERS: &[SoundClass] = &[Gunshot, Explosion, Vehicle, Music];
        match self {
            Goal::Awareness => (&[Footsteps, Foliage, Mechanical], MASKERS),
            Goal::Dialogue => (&[Voice], MASKERS),
            Goal::Immersion => (&[Footsteps, Foliage, Mechanical, Voice], MASKERS),
        }
    }

    /// (target events, masker events) in `stats` for this goal.
    pub fn evidence(self, stats: &Stats) -> (u64, u64) {
        let (t, m) = self.evidence_classes();
        (t.iter().map(|&c| stats.count(c)).sum(), m.iter().map(|&c| stats.count(c)).sum())
    }
}

/// Hard limits on the derived curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub max_boost_db: f32,
    pub max_cut_db: f32,
    /// Largest difference between neighbouring 1/3-octave bands.
    pub max_step_db: f32,
    /// No boost at or below this frequency.
    pub no_boost_below_hz: f32,
    /// Masker events needed for a low cut to reach full depth.
    pub full_cut_maskers: u64,
    /// Deepest cut inside the speech band.
    pub speech_guard_db: f32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_boost_db: MAX_BOOST_DB,
            max_cut_db: MAX_CUT_DB,
            max_step_db: MAX_STEP_DB,
            no_boost_below_hz: NO_BOOST_BELOW_HZ,
            full_cut_maskers: FULL_CUT_MASKERS,
            speech_guard_db: SPEECH_GUARD_DB,
        }
    }
}

/// A derived game layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    /// Gain per analysis band, dB.
    pub gains: [f32; NBANDS],
    /// Per band, how often the goal's targets are buried (0..1).
    pub buried: [f32; NBANDS],
    /// Background-weighted overall power gain after the hearing rule, dB (≤ 0).
    pub overall_db: f32,
}

impl Derived {
    /// The curve as ascending `(hz, db)` points, extended to 20 Hz and 16 kHz.
    pub fn curve(&self) -> Vec<(f32, f32)> {
        curve_from_gains(&self.gains)
    }

    /// Fitted to at most [`GAME_BUDGET`] cascade bands.
    pub fn bands(&self) -> Vec<BandParams> {
        crate::fit::fit_curve(&self.curve(), GAME_BUDGET).bands
    }
}

/// Band gains → curve points.
pub fn curve_from_gains(gains: &[f32; NBANDS]) -> Vec<(f32, f32)> {
    let mut c = Vec::with_capacity(NBANDS + 2);
    c.push((20.0, round(gains[0])));
    for (&hz, &g) in BANDS_HZ.iter().zip(gains.iter()) {
        c.push((hz, round(g)));
    }
    c.push((16_000.0, round(gains[NBANDS - 1])));
    c
}

fn round(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

/// Why a derivation was refused (the caller keeps the last good curve).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DeriveError {
    #[error("the aggregates are malformed")]
    Malformed,
    #[error("the derived curve is not finite")]
    NotFinite,
}

/// [`derive`], refusing malformed input or a non-finite result.
pub fn derive_checked(stats: &Stats, limits: &Limits, goal: Goal) -> Result<Derived, DeriveError> {
    if !stats.well_formed() {
        return Err(DeriveError::Malformed);
    }
    let d = derive(stats, limits, goal);
    if d.gains.iter().all(|g| g.is_finite()) && d.overall_db.is_finite() {
        Ok(d)
    } else {
        Err(DeriveError::NotFinite)
    }
}

/// True for a band inside the protected speech range.
pub fn in_speech_band(hz: f32) -> bool {
    (SPEECH_GUARD_LO_HZ..=SPEECH_GUARD_HI_HZ).contains(&hz)
}

/// Derive the curve for `goal`. All-zero gains when nothing has been learned.
pub fn derive(stats: &Stats, limits: &Limits, goal: Goal) -> Derived {
    let mut gains = [0f32; NBANDS];
    let mut buried = [0f32; NBANDS];
    if !stats.well_formed() {
        return Derived { gains, buried, overall_db: 0.0 };
    }
    let w = goal.weights();
    let total_frames: u64 = stats.class_frames.iter().sum();
    let share = |c: SoundClass| -> f32 {
        if total_frames == 0 {
            0.0
        } else {
            stats.class_frames[c.index()] as f32 / total_frames as f32
        }
    };

    // Each target's typical level and presence per band.
    let mut presence = [[0f32; NBANDS]; NCLASSES];
    let mut level = [[None::<f32>; NBANDS]; NCLASSES];
    for t in SoundClass::ALL {
        if w.target[t.index()] <= 0.0 || stats.class_frames[t.index()] == 0 {
            continue;
        }
        let h = stats.hist(t);
        let lv: [Option<f32>; NBANDS] = std::array::from_fn(|b| Stats::median(h, b));
        let top = lv.iter().flatten().cloned().fold(f32::MIN, f32::max);
        for b in 0..NBANDS {
            if let Some(l) = lv[b] {
                presence[t.index()][b] = (1.0 - (top - l) / PRESENCE_RANGE_DB).clamp(0.0, 1.0);
            }
        }
        level[t.index()] = lv;
    }

    // Lift: how often each target is buried, per band, by the maskers.
    for b in 0..NBANDS {
        let mut best = 0f32;
        for t in SoundClass::ALL {
            let (tw, p) = (w.target[t.index()], presence[t.index()][b]);
            let Some(lt) = level[t.index()][b] else { continue };
            if tw <= 0.0 || p <= 0.0 {
                continue;
            }
            let (mut num, mut den) = (0f32, 0f32);
            for m in SoundClass::ALL {
                if m == t {
                    continue;
                }
                let s = share(m);
                den += s;
                let mw = w.masker[m.index()];
                if mw > 0.0 && s > 0.0 {
                    num += mw * s * Stats::fraction_at_or_above(stats.hist(m), b, lt - CLEAR_DB);
                }
            }
            let bur = if den > 0.0 { num / den } else { 0.0 };
            buried[b] = buried[b].max(bur);
            best = best.max(tw * p * bur);
        }
        if BANDS_HZ[b] > limits.no_boost_below_hz {
            gains[b] = limits.max_boost_db * best;
        }
    }

    // Cut: masker lows that stick out of the mix.
    for b in 0..NBANDS {
        if BANDS_HZ[b] > CUT_MAX_HZ {
            continue;
        }
        let protect = SoundClass::ALL
            .iter()
            .map(|t| w.target[t.index()] * presence[t.index()][b])
            .fold(0f32, f32::max);
        if protect > CUE_PROTECT {
            continue;
        }
        let Some(mix) = Stats::median(&stats.frame_hist, b) else { continue };
        let mut cut = 0f32;
        for m in SoundClass::ALL {
            let mw = w.masker[m.index()];
            if mw <= 0.0 {
                continue;
            }
            let Some(lm) = Stats::median(stats.hist(m), b) else { continue };
            let evidence = if limits.full_cut_maskers == 0 {
                1.0
            } else {
                (stats.count(m) as f32 / limits.full_cut_maskers as f32).min(1.0)
            };
            let c = ((lm - mix - DOMINANCE_FLOOR_DB) * CUT_SLOPE).clamp(0.0, limits.max_cut_db);
            cut = cut.max(mw * c * evidence);
        }
        gains[b] -= cut;
    }

    for g in gains.iter_mut() {
        *g *= w.scale;
    }

    // Smooth (1-2-1), twice.
    for _ in 0..2 {
        let g = gains;
        for b in 0..NBANDS {
            let l = g[b.saturating_sub(1)];
            let r = g[(b + 1).min(NBANDS - 1)];
            gains[b] = 0.25 * l + 0.5 * g[b] + 0.25 * r;
        }
    }

    // Caps, the speech guard and the step limit, until they agree. Every move
    // shrinks a gain's magnitude, so this settles.
    let guard_and_cap = |gains: &mut [f32; NBANDS]| {
        for b in 0..NBANDS {
            let hi = if BANDS_HZ[b] > limits.no_boost_below_hz { limits.max_boost_db } else { 0.0 };
            let lo = if in_speech_band(BANDS_HZ[b]) {
                -limits.speech_guard_db
            } else {
                -limits.max_cut_db
            };
            gains[b] = gains[b].clamp(lo, hi);
        }
    };
    for _ in 0..4 * NBANDS {
        guard_and_cap(&mut gains);
        let mut changed = false;
        for b in 1..NBANDS {
            let d = gains[b] - gains[b - 1];
            if d.abs() > limits.max_step_db + 1e-6 {
                // Move the larger-magnitude side to within a step of the other.
                if gains[b].abs() > gains[b - 1].abs() {
                    gains[b] = gains[b - 1] + limits.max_step_db * d.signum();
                } else {
                    gains[b - 1] = gains[b] - limits.max_step_db * d.signum();
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    guard_and_cap(&mut gains);

    // Hearing rule: not louder overall, weighted by the game's own spectrum.
    let wb: [f32; NBANDS] = std::array::from_fn(|b| {
        Stats::median(&stats.frame_hist, b).map(|d| 10f32.powf(d / 10.0)).unwrap_or(0.0)
    });
    let power = |g: &[f32; NBANDS], k: f32| -> f32 {
        let (mut num, mut den) = (0f32, 0f32);
        for b in 0..NBANDS {
            let gb = if g[b] > 0.0 { g[b] * k } else { g[b] };
            num += wb[b] * 10f32.powf(gb / 10.0);
            den += wb[b];
        }
        if den > 0.0 {
            10.0 * (num / den).log10()
        } else {
            0.0
        }
    };
    if power(&gains, 1.0) > 0.0 {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..30 {
            let mid = 0.5 * (lo + hi);
            if power(&gains, mid) > 0.0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        for g in gains.iter_mut() {
            if *g > 0.0 {
                *g *= lo;
            }
        }
    }
    let overall_db = power(&gains, 1.0);
    Derived { gains, buried, overall_db }
}

/// Largest per-band difference between two gain sets, dB.
pub fn max_delta(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::INFINITY;
    }
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::analyzer::Analyzer;
    use crate::learn::synth::{Segment, Synth, FS};
    use crate::learn::HIST_BINS;

    fn idx(hz: f32) -> usize {
        BANDS_HZ.iter().position(|&f| f == hz).unwrap()
    }

    /// `secs` of synthetic gameplay with footsteps at `step` amplitude.
    fn learned(secs: f32, step: f32) -> Stats {
        let mut a = Analyzer::new(FS);
        let mut s = Synth::new(FS, 0x5eed);
        s.step = step;
        s.render(Segment::Gameplay, secs, |b| a.push(b));
        a.into_stats()
    }

    /// A scene with every layer: gameplay, a mixed scene, voice and music.
    pub(crate) fn scene() -> Stats {
        let mut a = Analyzer::new(FS);
        let mut s = Synth::new(FS, 11);
        s.step = 0.08;
        s.render(Segment::Gameplay, 60.0, |b| a.push(b));
        s.render(Segment::MixedScene, 60.0, |b| a.push(b));
        s.render(Segment::Speech, 30.0, |b| a.push(b));
        s.render(Segment::Music, 30.0, |b| a.push(b));
        a.into_stats()
    }

    fn check_limits(d: &Derived, lim: &Limits) {
        let g = d.gains;
        for (b, &x) in g.iter().enumerate() {
            assert!(x <= lim.max_boost_db + 1e-4 && x >= -lim.max_cut_db - 1e-4, "{g:?}");
            if BANDS_HZ[b] <= lim.no_boost_below_hz {
                assert!(x <= 0.0, "boost at {} Hz", BANDS_HZ[b]);
            }
            if in_speech_band(BANDS_HZ[b]) {
                assert!(
                    x >= -lim.speech_guard_db - 1e-4,
                    "speech cut at {} Hz: {g:?}",
                    BANDS_HZ[b]
                );
            }
        }
        for w in g.windows(2) {
            assert!((w[1] - w[0]).abs() <= lim.max_step_db + 1e-3, "{g:?}");
        }
        assert!(d.overall_db <= 1e-3, "never louder overall: {}", d.overall_db);
    }

    #[test]
    fn limits_are_the_documented_caps() {
        let l = Limits::default();
        assert_eq!((l.max_boost_db, l.max_cut_db), (6.0, 9.0));
        assert_eq!(l.no_boost_below_hz, 80.0);
        assert_eq!(l.max_step_db, 3.0);
        assert_eq!(l.speech_guard_db, 2.0);
        assert_eq!((SPEECH_GUARD_LO_HZ, SPEECH_GUARD_HI_HZ), (300.0, 4000.0));
        assert_eq!(CLEAR_DB, 6.0);
        assert!(CUT_MAX_HZ <= 315.0 && CUT_SLOPE < 1.0, "the low cut is gentle");
        assert!(IMMERSION_SCALE < 1.0);
        assert_eq!(Goal::default(), Goal::Awareness);
        for g in Goal::ALL {
            let w = g.weights();
            assert!(w.target.iter().chain(w.masker.iter()).all(|x| (0.0..=1.0).contains(x)));
            assert!(!g.describe().is_empty());
        }
    }

    #[test]
    fn nothing_learned_means_a_flat_curve() {
        for g in Goal::ALL {
            let d = derive(&Stats::default(), &Limits::default(), g);
            assert!(d.gains.iter().all(|&x| x == 0.0));
        }
    }

    #[test]
    fn buried_footsteps_are_lifted_and_explosion_lows_cut_within_limits() {
        let lim = Limits::default();
        let d = derive(&learned(90.0, 0.05), &lim, Goal::Awareness);
        let g = d.gains;
        let cue = g[idx(2000.0)].max(g[idx(2500.0)]).max(g[idx(3150.0)]).max(g[idx(4000.0)]);
        assert!(cue >= 0.5, "footstep band lift {cue}: {g:?} buried {:?}", d.buried);
        assert!(g[idx(63.0)] <= -2.0, "63 Hz {g:?}");
        assert!(g[idx(100.0)] < 0.0, "100 Hz {g:?}");
        check_limits(&d, &lim);
    }

    #[test]
    fn clearly_audible_cues_get_less_lift() {
        let loud = derive(&learned(60.0, 1.0), &Limits::default(), Goal::Awareness);
        let quiet = derive(&learned(60.0, 0.05), &Limits::default(), Goal::Awareness);
        let top = |d: &Derived| (16..=19).map(|b| d.gains[b]).fold(f32::MIN, f32::max);
        assert!(top(&loud) < top(&quiet), "{:?} vs {:?}", loud.gains, quiet.gains);
    }

    #[test]
    fn the_hearing_rule_scales_boosts_down() {
        let lim = Limits { full_cut_maskers: u64::MAX, ..Limits::default() };
        let d = derive(&learned(60.0, 0.05), &lim, Goal::Awareness);
        assert!(d.overall_db <= 1e-3, "{}", d.overall_db);
    }

    #[test]
    fn every_goal_stays_inside_the_limits_on_a_full_scene() {
        let s = scene();
        for g in Goal::ALL {
            check_limits(&derive(&s, &Limits::default(), g), &Limits::default());
        }
    }

    #[test]
    fn the_speech_guard_holds_even_against_a_huge_masker() {
        // Music pinned at the top of every band would ask for deep cuts; the
        // guard keeps 300 Hz - 4 kHz within 2 dB, and the step limit still holds.
        let mut s = scene();
        let mi = SoundClass::Music.index();
        let n = NBANDS * HIST_BINS;
        for b in 0..NBANDS {
            let row = &mut s.class_hist[mi * n + b * HIST_BINS..][..HIST_BINS];
            row.iter_mut().for_each(|c| *c = 0);
            row[HIST_BINS - 1] = 10_000;
        }
        s.events[mi] = 1_000;
        let lim = Limits::default();
        for g in Goal::ALL {
            let d = derive(&s, &lim, g);
            check_limits(&d, &lim);
            // The fitted filters respect it too (fitting error allowed).
            let bands = d.bands();
            // Inside the band (300 Hz itself sits on the slope from the
            // unguarded 250 Hz band).
            for hz in [400.0, 500.0, 1000.0, 2000.0, 3150.0] {
                let r = crate::fit::response_db(&bands, hz);
                assert!(r >= -(SPEECH_GUARD_DB as f64) - 0.75, "{g:?} fitted {r} dB at {hz} Hz");
            }
        }
    }

    #[test]
    fn switching_goal_moves_the_curve_the_expected_way() {
        let s = scene();
        let lim = Limits::default();
        let aware = derive(&s, &lim, Goal::Awareness);
        let dialogue = derive(&s, &lim, Goal::Dialogue);
        let immersion = derive(&s, &lim, Goal::Immersion);
        // Awareness lifts the footstep band (2 - 4 kHz) more than Dialogue...
        let steps = |d: &Derived| (16..=19).map(|b| d.gains[b]).sum::<f32>();
        assert!(steps(&aware) > steps(&dialogue), "{:?} vs {:?}", aware.gains, dialogue.gains);
        // ...Dialogue favours the voice band (500 Hz - 1.6 kHz) over the
        // footstep band more than Awareness does...
        let voice = |d: &Derived| (10..=15).map(|b| d.gains[b]).sum::<f32>();
        assert!(
            voice(&dialogue) - steps(&dialogue) > voice(&aware) - steps(&aware),
            "dialogue {:?} vs awareness {:?}",
            dialogue.gains,
            aware.gains
        );
        // ...and Immersion stays nearest neutral.
        let size = |d: &Derived| d.gains.iter().map(|g| g.abs()).fold(0f32, f32::max);
        assert!(size(&immersion) < size(&aware), "{} vs {}", size(&immersion), size(&aware));
        assert!(size(&immersion) <= MAX_BOOST_DB * IMMERSION_SCALE + 1e-3);
    }

    #[test]
    fn evidence_counts_follow_the_goal() {
        let s = scene();
        let (t_a, m_a) = Goal::Awareness.evidence(&s);
        let (t_d, m_d) = Goal::Dialogue.evidence(&s);
        assert_eq!(t_a, s.cue_events());
        assert_eq!(t_d, s.count(SoundClass::Voice));
        assert_eq!(m_a, m_d);
    }

    #[test]
    fn same_input_gives_the_same_curve() {
        let a = derive(&learned(45.0, 0.05), &Limits::default(), Goal::Awareness);
        let b = derive(&learned(45.0, 0.05), &Limits::default(), Goal::Awareness);
        assert_eq!(a, b);
        assert_eq!(a.curve(), b.curve());
        assert_eq!(a.bands(), b.bands());
    }

    #[test]
    fn malformed_aggregates_are_refused() {
        let mut s = Stats::default();
        s.frame_hist.pop();
        assert_eq!(
            derive_checked(&s, &Limits::default(), Goal::Awareness),
            Err(DeriveError::Malformed)
        );
        assert!(derive_checked(&Stats::default(), &Limits::default(), Goal::Awareness).is_ok());
    }

    #[test]
    fn fitted_bands_fit_the_budget_and_follow_the_curve() {
        let d = derive(&learned(90.0, 0.05), &Limits::default(), Goal::Awareness);
        let bands = d.bands();
        assert!(!bands.is_empty() && bands.len() <= GAME_BUDGET);
        let at = |hz: f64| crate::fit::response_db(&bands, hz) as f32;
        assert!(at(63.0) < 0.0);
        assert!(at(63.0) >= -10.0 && at(3150.0) <= 7.0);
    }
}
