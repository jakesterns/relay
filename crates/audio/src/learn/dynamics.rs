//! Learned aggregates + goal → the dynamic stage (S49).
//!
//! The static game layer ([`super::derive`]) cannot turn a gunshot down
//! without also turning down the footsteps that share its bands. The dynamic
//! stage ([`crate::params::DynamicsParams`]) can: it acts only while a band is
//! loud (Tame) or quiet-but-present (Lift). Everything it needs is read off
//! the same per-class level histograms the static layer uses, on the same
//! relative scale, so nothing here is a per-game preset:
//!
//! - **Gun tame** (Awareness), two bands: the shot's body
//!   ([`GUN_BODY_HZ`]) and its crack ([`GUN_CRACK_HZ`]), each centred where
//!   the loud gunshots stand furthest above the loudest footsteps (so taming
//!   there costs the cues least). It starts acting [`TAME_UNDER_QUIET_DB`]
//!   under the shots' quieter quartile and never below the loudest footsteps
//!   (+[`CUE_MARGIN_DB`]), so a step can never trigger it. Skipped when guns
//!   and steps overlap in level there.
//! - **Explosion tame** (every goal; light for Immersion): the same, for
//!   explosions in [`BOOM_HZ`].
//! - **Footstep lift** (Awareness): centred where steps stand furthest above
//!   the bed (1 – 5 kHz). The threshold sits [`LIFT_HEADROOM_DB`] over the
//!   typical step, so a typical step gets the full lift and a loud one less;
//!   nothing at or below the bed (+[`FLOOR_MARGIN_DB`]) is ever raised.
//!   Skipped when the steps barely clear the bed.
//!
//! Hard caps ([`MAX_TAME_DB`], [`MAX_LIFT_DB`]), a lift threshold never above
//! the program loudness (so nothing is lifted past the game's own level:
//! never louder overall), and the same evidence the static layer needs: the
//! caller derives this only when it offers a curve.
//!
//! Detector note: the stage's band level is that band's share of the
//! analysis bands' levels, power-summed through the band's response
//! ([`band_level`]); a broadband sound reads the same in both.

use serde::{Deserialize, Serialize};

use super::analyzer::{SoundClass, Stats};
use super::derive::Goal;
use super::{BANDS_HZ, NBANDS};
use crate::params::{DynBandParams, DynMode, DynamicsParams, DYN_REF_DB, MAX_DYN_BANDS};

/// Deepest cut any dynamic band may make, dB (learned, tuned or imported).
/// 24 since 2026-10-24 (owner: "if they want their gunshots to be near
/// silent, they should have the option"); the DSP accepts up to 24.
pub const MAX_TAME_DB: f32 = 24.0;
/// Largest lift, dB.
/// 12 since 2026-10-24 (owner: footsteps still not loud enough next to his
/// own gun at +8); the DSP accepts up to 24.
pub const MAX_LIFT_DB: f32 = 12.0;
/// Steepest ratio a learned, tuned or imported band may use.
pub const MAX_SAFE_RATIO: f32 = 8.0;
/// A tame threshold never sits this far under the program loudness (so a
/// band can never be held down all the time).
pub const MIN_TAME_THRESHOLD_DB: f32 = DYN_REF_DB - 20.0;
/// A lift threshold never sits more than this over the program loudness.
/// In a quiet stretch the steps *are* the program, so a threshold at the
/// loudness (the first cut of S49) never lifted them; 10 dB over lets a
/// step at the loudness come up by the full range while anything clearly
/// louder than the program is left alone (and the limiter follows a lift).
pub const MAX_LIFT_THRESHOLD_DB: f32 = DYN_REF_DB + 10.0;
/// Loud gunshots (not distant ones) a gun tame needs.
pub const MIN_GUN_EVENTS: u64 = 8;
pub const MIN_BOOM_EVENTS: u64 = 5;
pub const MIN_STEP_EVENTS: u64 = 15;
/// A tame band starts this far under the bursts' quieter quartile, so a
/// typical burst is well over it and comes down by most of the range.
pub const TAME_UNDER_QUIET_DB: f32 = 6.0;
/// A tame band starts this far above the loudest footsteps at least.
pub const CUE_MARGIN_DB: f32 = 3.0;
/// A lift band never acts within this of the bed (its detector reads the
/// bed's mean; the bed's own wobble must stay under the floor).
pub const FLOOR_MARGIN_DB: f32 = 4.0;
/// The lift threshold sits this far over the typical (median) step.
pub const LIFT_HEADROOM_DB: f32 = 10.0;
/// Steps must stand at least this far over the floor to be lifted.
pub const MIN_STEP_OVER_FLOOR_DB: f32 = 1.0;
/// The lift needs at least this much room between floor and threshold.
pub const MIN_LIFT_WINDOW_DB: f32 = 4.0;
/// User strength range (1 = as learned).
pub const MAX_STRENGTH: f32 = 2.0;
/// Owner 2026-10-23: "gunshots are still very loud when I shoot them". When
/// learning finds no gun band (MW4's shots did not stand clear of its
/// footsteps), the player's Gunshots strength adds this one instead: a wide
/// band over the crack of a shot that acts only on bursts at least as loud
/// as the whole mix — mostly the player's own gun, which is the loudest
/// thing in a shooter. Footsteps sit far under it.
pub const GUNFIRE_HZ: f32 = 3000.0;
/// About two and a half octaves (≈ 1.2 – 7 kHz): the crack, not the body.
pub const GUNFIRE_Q: f32 = 0.5;
/// Acts on the band only above the program loudness ([`DYN_REF_DB`]).
pub const GUNFIRE_THRESHOLD_DB: f32 = DYN_REF_DB;
pub const GUNFIRE_RATIO: f32 = 6.0;
/// Largest cut at 100 %; 200 % reaches [`MAX_TAME_DB`].
pub const GUNFIRE_RANGE_DB: f32 = 6.0;
/// A tame band at or above this is a gun band (body or crack); under it,
/// an explosion band.
pub const GUN_MIN_HZ: f32 = GUN_BODY_HZ.0;

/// The gunfire band at strength `s` (1 = [`GUNFIRE_RANGE_DB`]).
pub fn gunfire_band(s: f32) -> DynBandParams {
    DynBandParams {
        mode: DynMode::Tame,
        freq_hz: GUNFIRE_HZ,
        q: GUNFIRE_Q,
        threshold_db: GUNFIRE_THRESHOLD_DB,
        ratio: GUNFIRE_RATIO,
        range_db: (GUNFIRE_RANGE_DB * s).min(MAX_TAME_DB),
        floor_db: -100.0,
        attack_ms: 1.0,
        release_ms: 120.0,
        enabled: true,
    }
}

/// The three parts a player sets a level for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynPart {
    Gun,
    Boom,
    Steps,
}

/// A part's level range, dB: −24 is near silent, 0 is as the game mixes it,
/// +8 is louder.
pub const PART_MIN_DB: f32 = -MAX_TAME_DB;
pub const PART_MAX_DB: f32 = MAX_LIFT_DB;

/// Which part a learned or imported band belongs to.
pub fn part_of(b: &DynBandParams) -> DynPart {
    match b.mode {
        DynMode::Lift => DynPart::Steps,
        DynMode::Tame if b.freq_hz >= GUN_MIN_HZ => DynPart::Gun,
        DynMode::Tame => DynPart::Boom,
    }
}

/// Where a part's default band sits when nothing was learned for it.
fn default_at(part: DynPart) -> (f32, f32) {
    match part {
        DynPart::Gun => (GUNFIRE_HZ, GUNFIRE_Q),
        DynPart::Boom => (63.0, 0.7),
        DynPart::Steps => (3000.0, 0.7),
    }
}

/// The bands that set `part` to `level` dB.
///
/// - **Down** (gunshots, explosions): the part's learned tame bands with
///   their range set to the cut, or a default band acting on bursts at least
///   as loud as the whole mix. Gunshots also get a body band (500 Hz) so a
///   deep cut takes the thump down with the crack. Footsteps down: a band at
///   the footstep range acting on anything above the quiet bed.
/// - **Up**: a lift band acting between the game's average loudness and
///   10 dB over it for gunshots and explosions (the loud parts only; the bed
///   and quiet sounds are never raised), or the learned footstep lift.
///
/// Each cut stays within [`MAX_TAME_DB`], each boost within [`MAX_LIFT_DB`].
pub fn part_bands(part: DynPart, level: f32, learned: &[DynBandParams]) -> Vec<DynBandParams> {
    let level = if level.is_finite() { level.clamp(PART_MIN_DB, PART_MAX_DB) } else { 0.0 };
    if level.abs() < 0.05 {
        return Vec::new();
    }
    let mine: Vec<DynBandParams> = learned.iter().filter(|b| part_of(b) == part).copied().collect();
    let (f0, q) = mine.first().map(|b| (b.freq_hz, b.q)).unwrap_or(default_at(part));
    let band = |mode, freq_hz, q, threshold_db, ratio, range_db, floor_db| DynBandParams {
        mode,
        freq_hz,
        q,
        threshold_db,
        ratio,
        range_db,
        floor_db,
        attack_ms: if mode == DynMode::Tame { 1.0 } else { 3.0 },
        release_ms: if mode == DynMode::Tame { 120.0 } else { 80.0 },
        enabled: true,
    };
    let cut = (-level).min(MAX_TAME_DB);
    let boost = level.min(MAX_LIFT_DB);
    match (part, level < 0.0) {
        (DynPart::Gun | DynPart::Boom, true) => {
            let tames: Vec<_> = mine.iter().filter(|b| b.mode == DynMode::Tame).collect();
            if !tames.is_empty() {
                return tames.iter().map(|b| DynBandParams { range_db: cut, ..**b }).collect();
            }
            let mut v = vec![band(DynMode::Tame, f0, q, DYN_REF_DB, GUNFIRE_RATIO, cut, -100.0)];
            if part == DynPart::Gun {
                v.push(band(DynMode::Tame, 500.0, 0.7, DYN_REF_DB, GUNFIRE_RATIO, cut, -100.0));
            }
            v
        }
        (DynPart::Gun | DynPart::Boom, false) => {
            vec![band(DynMode::Lift, f0, q, MAX_LIFT_THRESHOLD_DB, 2.5, boost, DYN_REF_DB)]
        }
        (DynPart::Steps, false) => {
            let lifts: Vec<_> = mine.iter().filter(|b| b.mode == DynMode::Lift).collect();
            if !lifts.is_empty() {
                return lifts.iter().map(|b| DynBandParams { range_db: boost, ..**b }).collect();
            }
            vec![band(DynMode::Lift, f0, q, DYN_REF_DB - 5.0, 2.5, boost, DYN_REF_DB - 25.0)]
        }
        (DynPart::Steps, true) => {
            vec![band(DynMode::Tame, f0, q, MIN_TAME_THRESHOLD_DB, 4.0, cut, -100.0)]
        }
    }
}

/// Is this a gun band (as opposed to an explosion or footstep band)?
pub fn is_gun_band(b: &DynBandParams) -> bool {
    b.mode == DynMode::Tame && b.freq_hz >= GUN_MIN_HZ
}

/// Where each tame band may sit, Hz.
pub const GUN_BODY_HZ: (f32, f32) = (200.0, 1000.0);
pub const GUN_CRACK_HZ: (f32, f32) = (1600.0, 8000.0);
pub const BOOM_HZ: (f32, f32) = (40.0, 160.0);

/// The analysis filterbank's share of a 1/3-octave tile: tile width over the
/// 2nd-order band-pass's noise bandwidth (Q 4.32).
fn tile() -> f32 {
    let q_t = 2f32.powf(1.0 / 6.0) / (2f32.powf(1.0 / 3.0) - 1.0);
    let width = 2f32.powf(1.0 / 6.0) - 2f32.powf(-1.0 / 6.0);
    width / (std::f32::consts::FRAC_PI_2 / q_t)
}

/// The level a band-pass at `f0`/`q` reads for per-analysis-band levels
/// `levels` (dB, `None` = nothing there).
pub fn band_level(levels: &[Option<f32>; NBANDS], f0: f32, q: f32) -> Option<f32> {
    let t = tile();
    let mut p = 0f64;
    let mut any = false;
    for (b, l) in levels.iter().enumerate() {
        let Some(l) = l else { continue };
        let u = BANDS_HZ[b] / f0 - f0 / BANDS_HZ[b];
        let h2 = 1.0 / (1.0 + q * q * u * u);
        p += (h2 * t) as f64 * 10f64.powf(*l as f64 / 10.0);
        any = true;
    }
    (any && p > 0.0).then(|| (10.0 * p.log10()) as f32)
}

fn pct(stats: &Stats, c: SoundClass, p: f32) -> [Option<f32>; NBANDS] {
    std::array::from_fn(|b| Stats::percentile(stats.hist(c), b, p))
}

fn bed(stats: &Stats, p: f32) -> [Option<f32>; NBANDS] {
    std::array::from_fn(|b| Stats::percentile(&stats.frame_hist, b, p))
}

/// Power sum, per band, of two level sets (a cue's added energy on the bed).
fn on_bed(a: &[Option<f32>; NBANDS], bed: &[Option<f32>; NBANDS]) -> [Option<f32>; NBANDS] {
    std::array::from_fn(|b| match (a[b], bed[b]) {
        (Some(x), Some(y)) => Some(10.0 * (10f32.powf(x / 10.0) + 10f32.powf(y / 10.0)).log10()),
        (x, y) => x.or(y),
    })
}

/// The band in `lo..=hi` Hz where `a` stands furthest above `b`.
fn stand_out(
    a: &[Option<f32>; NBANDS],
    b: &[Option<f32>; NBANDS],
    lo: f32,
    hi: f32,
) -> Option<f32> {
    (0..NBANDS)
        .filter(|&i| (lo..=hi).contains(&BANDS_HZ[i]))
        .filter_map(|i| Some((i, a[i]? - b[i].unwrap_or(-100.0))))
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .map(|(i, _)| BANDS_HZ[i])
}

/// What a goal asks of the stage.
struct Plan {
    gun: Option<(f32, f32)>,
    boom: Option<(f32, f32)>,
    lift: Option<(f32, f32)>,
}

impl Plan {
    /// `(range dB, ratio)` per role.
    fn of(goal: Goal) -> Self {
        match goal {
            Goal::Awareness => {
                Plan { gun: Some((9.0, 6.0)), boom: Some((10.0, 6.0)), lift: Some((6.0, 2.5)) }
            }
            Goal::Dialogue => Plan { gun: None, boom: Some((8.0, 4.0)), lift: None },
            Goal::Immersion => Plan { gun: None, boom: Some((4.0, 2.0)), lift: None },
        }
    }
}

/// A tame band for class `c`, centred in `lo..=hi`.
fn tame_band(
    stats: &Stats,
    c: SoundClass,
    (lo, hi): (f32, f32),
    q: f32,
    (range, ratio): (f32, f32),
) -> Option<DynBandParams> {
    let mid = pct(stats, c, 0.5);
    // The loudest cues: footsteps' added energy on the bed, else the loud
    // end of the mix. The band goes where the bursts stand furthest above
    // them, so taming there costs the cues least.
    let cue_levels = if stats.count(SoundClass::Footsteps) > 0 {
        on_bed(&pct(stats, SoundClass::Footsteps, 0.9), &bed(stats, 0.5))
    } else {
        bed(stats, 0.9)
    };
    let f0 = stand_out(&mid, &cue_levels, lo, hi)?;
    let loud = band_level(&mid, f0, q)?;
    let quiet = band_level(&pct(stats, c, 0.25), f0, q)?;
    let cues = band_level(&cue_levels, f0, q).unwrap_or(f32::MIN);
    let threshold =
        (quiet - TAME_UNDER_QUIET_DB).max(cues + CUE_MARGIN_DB).max(MIN_TAME_THRESHOLD_DB);
    if threshold > loud - 2.0 {
        // The bursts are not louder than the cues there: taming would hit both.
        return None;
    }
    Some(DynBandParams {
        mode: DynMode::Tame,
        freq_hz: f0,
        q,
        threshold_db: round(threshold),
        ratio,
        range_db: range.min(MAX_TAME_DB),
        floor_db: -100.0,
        attack_ms: 1.0,
        release_ms: 120.0,
        enabled: true,
    })
}

fn lift_band(stats: &Stats, (range, ratio): (f32, f32)) -> Option<DynBandParams> {
    // About an octave and a half wide: a step's click spreads over 2-5 kHz,
    // and a narrower bell lifted only the slice at its centre.
    let q = 0.7;
    let added = pct(stats, SoundClass::Footsteps, 0.5);
    let low_bed = bed(stats, 0.25);
    let f0 = stand_out(&added, &low_bed, 1000.0, 5000.0)?;
    let mid_bed = bed(stats, 0.5);
    let typical = band_level(&on_bed(&added, &mid_bed), f0, q)?;
    let floor = band_level(&mid_bed, f0, q)? + FLOOR_MARGIN_DB;
    let threshold = (typical + LIFT_HEADROOM_DB).min(MAX_LIFT_THRESHOLD_DB);
    if typical < floor + MIN_STEP_OVER_FLOOR_DB || threshold - floor < MIN_LIFT_WINDOW_DB {
        return None;
    }
    Some(DynBandParams {
        mode: DynMode::Lift,
        freq_hz: f0,
        q,
        threshold_db: round(threshold),
        ratio,
        range_db: range.min(MAX_LIFT_DB),
        floor_db: round(floor),
        attack_ms: 3.0,
        release_ms: 80.0,
        enabled: true,
    })
}

fn round(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

/// The dynamic stage for `goal`, or `None` when the evidence asks for none.
pub fn derive_dynamics(stats: &Stats, goal: Goal) -> Option<DynamicsParams> {
    if !stats.well_formed() {
        return None;
    }
    let plan = Plan::of(goal);
    let mut bands = Vec::new();
    let loud_guns = stats.count(SoundClass::Gunshot).saturating_sub(stats.distant_gunshots);
    if let Some(r) = plan.gun.filter(|_| loud_guns >= MIN_GUN_EVENTS) {
        // A shot has a body and a crack; each gets its own band so neither
        // has to be so wide it holds the whole mix down.
        bands.extend(tame_band(stats, SoundClass::Gunshot, GUN_BODY_HZ, 0.6, r));
        bands.extend(tame_band(stats, SoundClass::Gunshot, GUN_CRACK_HZ, 0.5, r));
    }
    if let Some(r) = plan.boom.filter(|_| stats.count(SoundClass::Explosion) >= MIN_BOOM_EVENTS) {
        bands.extend(tame_band(stats, SoundClass::Explosion, BOOM_HZ, 0.7, r));
    }
    if let Some(r) = plan.lift.filter(|_| stats.count(SoundClass::Footsteps) >= MIN_STEP_EVENTS) {
        bands.extend(lift_band(stats, r));
    }
    bands.truncate(MAX_DYN_BANDS);
    let has_tame = bands.iter().any(|b| b.mode == DynMode::Tame);
    let p = DynamicsParams { bands, duck_lift: has_tame };
    (!p.is_inert()).then_some(p)
}

/// The player's adjustments, kept apart from what was learned or imported so
/// a relearn never loses them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicsTuning {
    /// The stage on at all.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Loud-burst reduction strength: 0 = off, 1 = as learned, up to
    /// [`MAX_STRENGTH`] (still capped at [`MAX_TAME_DB`]).
    #[serde(default = "one")]
    pub tame: f32,
    /// Footstep lift strength, as `tame` (capped at [`MAX_LIFT_DB`]).
    #[serde(default = "one")]
    pub lift: f32,
    /// Gunshot strength, 0 – [`MAX_STRENGTH`]. `None` (every layer before
    /// 2026-10-24) = gun bands follow `tame` and no gunfire band is added.
    /// Set, it scales the learned gun bands, or — when none were learned —
    /// adds [`gunfire_band`] at that strength. `tame` then covers explosions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gun: Option<f32>,
    /// Owner 2026-10-24: a level per part, in dB, from [`PART_MIN_DB`]
    /// (near silent) through 0 (as the game mixes it) to [`PART_MAX_DB`]
    /// (louder). A level that is set replaces that part's strength above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gun_db: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boom_db: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps_db: Option<f32>,
}

fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}

impl Default for DynamicsTuning {
    fn default() -> Self {
        Self {
            enabled: true,
            tame: 1.0,
            lift: 1.0,
            gun: None,
            gun_db: None,
            boom_db: None,
            steps_db: None,
        }
    }
}

impl DynamicsTuning {
    /// In range (a hand-edited profile or file can say anything).
    pub fn clamped(self) -> Self {
        let s = |x: f32| if x.is_finite() { x.clamp(0.0, MAX_STRENGTH) } else { 1.0 };
        let db = |x: f32| if x.is_finite() { x.clamp(PART_MIN_DB, PART_MAX_DB) } else { 0.0 };
        Self {
            enabled: self.enabled,
            tame: s(self.tame),
            lift: s(self.lift),
            gun: self.gun.map(s),
            gun_db: self.gun_db.map(db),
            boom_db: self.boom_db.map(db),
            steps_db: self.steps_db.map(db),
        }
    }

    /// The level set for `part`, if one is.
    pub fn level(&self, part: DynPart) -> Option<f32> {
        match part {
            DynPart::Gun => self.gun_db,
            DynPart::Boom => self.boom_db,
            DynPart::Steps => self.steps_db,
        }
    }
}

/// Bring any stage inside the safety caps: at most [`MAX_DYN_BANDS`] bands
/// that pass their own checks, ranges within [`MAX_TAME_DB`] /
/// [`MAX_LIFT_DB`], ratios within [`MAX_SAFE_RATIO`], tame thresholds not
/// under [`MIN_TAME_THRESHOLD_DB`], lift thresholds not over
/// [`MAX_LIFT_THRESHOLD_DB`]. Bands that cannot be made safe are dropped.
pub fn guard_dynamics(p: &DynamicsParams) -> DynamicsParams {
    let bands = p
        .bands
        .iter()
        .filter_map(|b| {
            let mut b = *b;
            match b.mode {
                DynMode::Tame => {
                    b.range_db = b.range_db.min(MAX_TAME_DB);
                    b.threshold_db = b.threshold_db.max(MIN_TAME_THRESHOLD_DB);
                }
                DynMode::Lift => {
                    b.range_db = b.range_db.min(MAX_LIFT_DB);
                    b.threshold_db = b.threshold_db.min(MAX_LIFT_THRESHOLD_DB);
                }
            }
            b.ratio = b.ratio.min(MAX_SAFE_RATIO);
            b.check().ok().map(|()| b)
        })
        .take(MAX_DYN_BANDS)
        .collect();
    DynamicsParams { bands, duck_lift: p.duck_lift }
}

/// True when `p` is already inside the caps.
pub fn is_guarded(p: &DynamicsParams) -> bool {
    guard_dynamics(p) == *p
}

/// What the stage runs with, part by part: `base` (learned or imported)
/// scaled by the player's strengths, or — for a part with a level set —
/// replaced by [`part_bands`]. Guarded; at most [`MAX_DYN_BANDS`] bands,
/// gunshots first. Empty when off.
pub fn effective_parts(
    base: &DynamicsParams,
    tuning: &DynamicsTuning,
) -> Vec<(DynPart, DynBandParams)> {
    let t = tuning.clamped();
    if !t.enabled {
        return Vec::new();
    }
    let mut out = Vec::new();
    for part in [DynPart::Gun, DynPart::Boom, DynPart::Steps] {
        if let Some(level) = t.level(part) {
            out.extend(part_bands(part, level, &base.bands).into_iter().map(|b| (part, b)));
            continue;
        }
        let s = match part {
            DynPart::Gun => t.gun.unwrap_or(t.tame),
            DynPart::Boom => t.tame,
            DynPart::Steps => t.lift,
        };
        let mine: Vec<DynBandParams> =
            base.bands.iter().filter(|b| part_of(b) == part).copied().collect();
        if part == DynPart::Gun && mine.is_empty() {
            if let Some(g) = t.gun.filter(|g| *g > 0.0) {
                out.push((part, gunfire_band(g)));
            }
            continue;
        }
        out.extend(
            mine.into_iter().map(|b| (part, DynBandParams { range_db: b.range_db * s, ..b })),
        );
    }
    let guarded = guard_dynamics(&DynamicsParams {
        bands: out.iter().map(|(_, b)| *b).collect(),
        duck_lift: false,
    });
    // Keep each surviving band's part (guarding may drop or trim, never reorder).
    let mut parts = Vec::new();
    let mut it = out.into_iter();
    for g in guarded.bands {
        for (part, b) in it.by_ref() {
            if b.freq_hz == g.freq_hz && b.mode == g.mode && b.q == g.q {
                parts.push((part, g));
                break;
            }
        }
    }
    parts.retain(|(_, b)| b.range_db > 0.0);
    parts
}

/// The level a part is at now, dB: the player's level when set, else what
/// the learned band and its strength amount to (a cut reads negative, a
/// lift positive), else 0 — as the game mixes it. What the slider shows.
pub fn part_level(base: &DynamicsParams, tuning: &DynamicsTuning, part: DynPart) -> f32 {
    let t = tuning.clamped();
    if let Some(l) = t.level(part) {
        return l;
    }
    let most = |mode: DynMode| {
        base.bands
            .iter()
            .filter(|b| part_of(b) == part && b.mode == mode)
            .map(|b| b.range_db)
            .fold(0.0f32, f32::max)
    };
    let v = match part {
        DynPart::Gun if most(DynMode::Tame) > 0.0 => -most(DynMode::Tame) * t.gun.unwrap_or(t.tame),
        DynPart::Gun => -t.gun.unwrap_or(0.0) * GUNFIRE_RANGE_DB,
        DynPart::Boom => -most(DynMode::Tame) * t.tame,
        DynPart::Steps => most(DynMode::Lift) * t.lift,
    };
    (v.clamp(PART_MIN_DB, PART_MAX_DB) * 10.0).round() / 10.0
}

/// What the stage runs with: [`effective_parts`] as one stage. `None` when
/// off or inert. Lift bands are held back while a cut acts (`duck_lift`),
/// as learned, so a gun's tail is never lifted.
pub fn effective(base: &DynamicsParams, tuning: &DynamicsTuning) -> Option<DynamicsParams> {
    let bands: Vec<DynBandParams> =
        effective_parts(base, tuning).into_iter().map(|(_, b)| b).collect();
    let has_tame = bands.iter().any(|b| b.mode == DynMode::Tame);
    let has_lift = bands.iter().any(|b| b.mode == DynMode::Lift);
    // Lifts wait for cuts (so a gun's tail is never lifted) — except a
    // Footsteps level the player set: the DSP's duck listens to *every* cut,
    // and on the main PC a learned 50 Hz explosion band reacting to MW4's
    // constant bass held the footstep lift off most of the time (2026-10-24).
    let steps_set = tuning.clamped().steps_db.is_some_and(|l| l > 0.0);
    let duck = (base.duck_lift || (has_tame && has_lift)) && !steps_set;
    let p = DynamicsParams { bands, duck_lift: duck };
    let p = guard_dynamics(&p);
    (!p.is_inert()).then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::analyzer::Analyzer;
    use crate::learn::synth::{Segment, Synth, FS};

    /// A shooter: gunfire, explosions, quiet footsteps, quiet stretches.
    pub(super) fn session(seed: u64, mut feed: impl FnMut(Segment, &[f32])) {
        let mut s = Synth::new(FS, seed);
        s.step = 0.05;
        for _ in 0..2 {
            for (seg, secs) in [
                (Segment::Footsteps, 30.0),
                (Segment::Gunfire, 30.0),
                (Segment::Gameplay, 30.0),
                (Segment::Explosions, 20.0),
                (Segment::Ambience, 10.0),
            ] {
                s.render(seg, secs, |b| feed(seg, b));
            }
        }
    }

    fn learned() -> Stats {
        let mut a = Analyzer::new(FS);
        session(0xD1A, |_, b| a.push(b));
        a.into_stats()
    }

    #[test]
    fn gunshots_strength_adds_a_gunfire_band_only_when_none_was_learned() {
        // MW4 on the main PC: one learned band, explosions at 63 Hz.
        let boom = DynamicsParams {
            bands: vec![DynBandParams {
                mode: DynMode::Tame,
                freq_hz: 63.0,
                q: 0.7,
                threshold_db: -54.0,
                ratio: 6.0,
                range_db: 10.0,
                floor_db: -100.0,
                attack_ms: 1.0,
                release_ms: 120.0,
                enabled: true,
            }],
            duck_lift: true,
        };
        // Before the setting exists: exactly as before.
        let old = effective(&boom, &DynamicsTuning::default()).unwrap();
        assert_eq!(old.bands.len(), 1);
        // Gunshots at 100 %: the gunfire band joins, explosions unchanged.
        let t = DynamicsTuning { gun: Some(1.0), ..Default::default() };
        let p = effective(&boom, &t).unwrap();
        assert_eq!(p.bands.len(), 2);
        let g = p.bands.iter().find(|b| is_gun_band(b)).unwrap();
        assert_eq!(
            (g.freq_hz, g.range_db, g.threshold_db),
            (GUNFIRE_HZ, GUNFIRE_RANGE_DB, DYN_REF_DB)
        );
        let boom_of =
            |p: &DynamicsParams| p.bands.iter().find(|b| b.freq_hz == 63.0).unwrap().range_db;
        assert_eq!(boom_of(&p), 10.0);
        // 200 % doubles it; 0 % adds nothing.
        let max =
            effective(&boom, &DynamicsTuning { gun: Some(2.0), ..Default::default() }).unwrap();
        assert_eq!(
            max.bands.iter().find(|b| is_gun_band(b)).unwrap().range_db,
            2.0 * GUNFIRE_RANGE_DB
        );
        let off =
            effective(&boom, &DynamicsTuning { gun: Some(0.0), ..Default::default() }).unwrap();
        assert_eq!(off.bands.len(), 1);
        // Explosions follow `tame` only.
        let quiet_boom =
            effective(&boom, &DynamicsTuning { tame: 0.5, gun: Some(1.0), ..Default::default() })
                .unwrap();
        assert_eq!(boom_of(&quiet_boom), 5.0);
        // From nothing at all, the gunfire band alone.
        let none = DynamicsParams { bands: vec![], duck_lift: false };
        assert_eq!(effective(&none, &t).unwrap().bands.len(), 1);
        assert!(is_guarded(&p));
    }

    fn mw4() -> DynamicsParams {
        DynamicsParams {
            bands: vec![DynBandParams {
                mode: DynMode::Tame,
                freq_hz: 63.0,
                q: 0.7,
                threshold_db: -54.0,
                ratio: 6.0,
                range_db: 10.0,
                floor_db: -100.0,
                attack_ms: 1.0,
                release_ms: 120.0,
                enabled: true,
            }],
            duck_lift: true,
        }
    }

    fn lvl(gun: Option<f32>, boom: Option<f32>, steps: Option<f32>) -> DynamicsTuning {
        DynamicsTuning { gun_db: gun, boom_db: boom, steps_db: steps, ..Default::default() }
    }

    #[test]
    fn gunshots_can_go_near_silent_or_louder() {
        // −24 dB: the crack and the body, both cut by 24 dB on loud bursts.
        let p = effective_parts(&mw4(), &lvl(Some(-24.0), None, None));
        let gun: Vec<_> = p.iter().filter(|(k, _)| *k == DynPart::Gun).map(|(_, b)| *b).collect();
        assert_eq!(gun.len(), 2);
        assert!(gun.iter().all(|b| b.mode == DynMode::Tame && b.range_db == MAX_TAME_DB));
        assert!(
            gun.iter().any(|b| b.freq_hz == GUNFIRE_HZ) && gun.iter().any(|b| b.freq_hz == 500.0)
        );
        // Explosions untouched (as learned).
        assert!(p.iter().any(|(k, b)| *k == DynPart::Boom && b.range_db == 10.0));
        // The top of the range: a lift on the loud part of the gun range only.
        let up = effective_parts(&mw4(), &lvl(Some(PART_MAX_DB), None, None));
        let g = up.iter().find(|(k, _)| *k == DynPart::Gun).unwrap().1;
        assert_eq!((g.mode, g.range_db, g.floor_db), (DynMode::Lift, MAX_LIFT_DB, DYN_REF_DB));
        assert!(g.threshold_db <= MAX_LIFT_THRESHOLD_DB);
        // 0 dB: as the game mixes it — no gun band at all.
        assert!(!effective_parts(&mw4(), &lvl(Some(0.0), None, None))
            .iter()
            .any(|(k, _)| *k == DynPart::Gun));
        // Beyond the range is clamped, never applied as given.
        let wild = effective_parts(&mw4(), &lvl(Some(-99.0), Some(99.0), None));
        assert!(wild.iter().all(|(_, b)| b.range_db <= MAX_TAME_DB));
        assert!(wild
            .iter()
            .filter(|(_, b)| b.mode == DynMode::Lift)
            .all(|(_, b)| b.range_db <= MAX_LIFT_DB));
    }

    #[test]
    fn explosions_and_footsteps_take_a_level_too() {
        // Explosions at −20 reuse the learned 63 Hz band at a 20 dB cut.
        let p = effective_parts(&mw4(), &lvl(None, Some(-20.0), None));
        let b = p.iter().find(|(k, _)| *k == DynPart::Boom).unwrap().1;
        assert_eq!((b.freq_hz, b.threshold_db, b.range_db), (63.0, -54.0, 20.0));
        // Footsteps up with nothing learned: a default lift over the bed.
        let s = effective_parts(&mw4(), &lvl(None, None, Some(6.0)));
        let st = s.iter().find(|(k, _)| *k == DynPart::Steps).unwrap().1;
        assert_eq!((st.mode, st.range_db), (DynMode::Lift, 6.0));
        assert!(st.floor_db < st.threshold_db);
        // Footsteps down: a cut over the footstep range.
        let d = effective_parts(&mw4(), &lvl(None, None, Some(-10.0)));
        let sd = d.iter().find(|(k, _)| *k == DynPart::Steps).unwrap().1;
        assert_eq!((sd.mode, sd.range_db), (DynMode::Tame, 10.0));
        // All three at once fit the band budget and the caps.
        let all = effective(&mw4(), &lvl(Some(-24.0), Some(-24.0), Some(8.0))).unwrap();
        assert!(all.bands.len() <= MAX_DYN_BANDS);
        assert!(is_guarded(&all));
        assert!(!all.duck_lift, "footsteps set by the player are not held back by cuts");
        let learned = effective(&mw4(), &lvl(Some(-24.0), None, None)).unwrap();
        assert!(learned.duck_lift, "otherwise lifts still wait for cuts, as learned");
    }

    #[test]
    fn levels_round_trip_and_old_files_read() {
        let t = lvl(Some(-12.5), None, Some(3.0));
        let back: DynamicsTuning =
            serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.level(DynPart::Boom), None);
    }

    #[test]
    fn a_learned_gun_band_follows_gunshots_and_gets_no_twin() {
        let s = learned();
        let p = derive_dynamics(&s, Goal::Awareness).unwrap();
        let guns = p.bands.iter().filter(|b| is_gun_band(b)).count();
        if guns == 0 {
            return; // this synthetic session learned none; covered above
        }
        let t = DynamicsTuning { gun: Some(2.0), tame: 0.0, ..Default::default() };
        let e = effective(&p, &t).unwrap();
        assert_eq!(e.bands.iter().filter(|b| is_gun_band(b)).count(), guns, "no extra band");
        assert!(e.bands.iter().filter(|b| is_gun_band(b)).all(|b| b.range_db > 0.0));
    }

    #[test]
    fn old_tunings_still_read() {
        let t: DynamicsTuning =
            serde_json::from_str(r#"{"enabled":true,"tame":1.5,"lift":0.5}"#).unwrap();
        assert_eq!(t.gun, None);
        assert!(!serde_json::to_string(&t).unwrap().contains("gun"));
    }

    #[test]
    fn nothing_learned_means_no_stage() {
        for g in Goal::ALL {
            assert_eq!(derive_dynamics(&Stats::default(), g), None);
        }
    }

    #[test]
    fn goals_pick_their_roles_within_the_caps() {
        let st = learned();
        let aware = derive_dynamics(&st, Goal::Awareness).expect("awareness stage");
        let tames: Vec<_> = aware.bands.iter().filter(|b| b.mode == DynMode::Tame).collect();
        let lifts: Vec<_> = aware.bands.iter().filter(|b| b.mode == DynMode::Lift).collect();
        assert!(!tames.is_empty() && lifts.len() == 1, "{aware:?}");
        assert!(aware.duck_lift);
        let l = lifts[0];
        assert!((1000.0..=5000.0).contains(&l.freq_hz), "{l:?}");
        assert!(l.floor_db < l.threshold_db && l.threshold_db <= MAX_LIFT_THRESHOLD_DB);
        assert!(is_guarded(&aware), "{aware:?}");
        assert!(aware.check().is_ok());

        let dialogue = derive_dynamics(&st, Goal::Dialogue).expect("dialogue stage");
        assert!(dialogue.bands.iter().all(|b| b.mode == DynMode::Tame && b.freq_hz <= 250.0));
        if let Some(imm) = derive_dynamics(&st, Goal::Immersion) {
            assert!(imm.bands.iter().all(|b| b.mode == DynMode::Tame && b.range_db <= 4.0));
        }
    }

    #[test]
    fn tuning_scales_within_the_caps_and_can_switch_off() {
        let st = learned();
        let base = derive_dynamics(&st, Goal::Awareness).unwrap();
        let off = DynamicsTuning { enabled: false, ..Default::default() };
        assert_eq!(effective(&base, &off), None);
        let as_learned = effective(&base, &DynamicsTuning::default()).unwrap();
        assert_eq!(as_learned, base);
        let max = DynamicsTuning {
            enabled: true,
            tame: 99.0,
            lift: f32::NAN,
            gun: None,
            gun_db: None,
            boom_db: None,
            steps_db: None,
        };
        let m = effective(&base, &max).unwrap();
        for b in &m.bands {
            match b.mode {
                DynMode::Tame => assert!(b.range_db <= MAX_TAME_DB),
                DynMode::Lift => assert!(b.range_db <= MAX_LIFT_DB),
            }
        }
        let none = DynamicsTuning {
            enabled: true,
            tame: 0.0,
            lift: 0.0,
            gun: None,
            gun_db: None,
            boom_db: None,
            steps_db: None,
        };
        assert_eq!(effective(&base, &none), None, "zero strength is inert");
    }

    #[test]
    fn guard_clamps_hostile_stages() {
        let wild = DynamicsParams {
            bands: vec![
                DynBandParams {
                    mode: DynMode::Lift,
                    freq_hz: 3000.0,
                    q: 1.0,
                    threshold_db: 0.0,
                    ratio: 20.0,
                    range_db: 24.0,
                    floor_db: -90.0,
                    attack_ms: 5.0,
                    release_ms: 100.0,
                    enabled: true,
                },
                DynBandParams {
                    mode: DynMode::Tame,
                    freq_hz: 3.0, // unusable: dropped
                    q: 1.0,
                    threshold_db: -100.0,
                    ratio: 2.0,
                    range_db: 6.0,
                    floor_db: -100.0,
                    attack_ms: 5.0,
                    release_ms: 100.0,
                    enabled: true,
                },
            ],
            duck_lift: false,
        };
        let g = guard_dynamics(&wild);
        assert_eq!(g.bands.len(), 1);
        let b = g.bands[0];
        assert_eq!(
            (b.range_db, b.ratio, b.threshold_db),
            (MAX_LIFT_DB, MAX_SAFE_RATIO, MAX_LIFT_THRESHOLD_DB)
        );
        assert!(is_guarded(&g));
    }

    /// The point of it all: run the derived stage over fresh audio of the same
    /// game and measure what it does to each kind of sound.
    #[cfg(feature = "dsp")]
    #[test]
    fn derived_stage_turns_guns_down_and_quiet_steps_up_but_not_the_bed() {
        use crate::dsp::{Chain, ChainParams};
        let dyns = derive_dynamics(&learned(), Goal::Awareness).unwrap();
        let mut chain =
            Chain::new(ChainParams { dynamics: Some(dyns.clone()), ..Default::default() });
        chain.prepare(FS, 2400).unwrap();

        // The footstep region (2-5 kHz, where the synthetic steps live), to
        // look at the steps rather than the broadband bed under them.
        let lift_hz = dyns.bands.iter().find(|b| b.mode == DynMode::Lift).unwrap().freq_hz;
        let bp = crate::coeffs::Coeffs::design(
            crate::params::FilterKind::Peaking,
            FS as f64,
            lift_hz as f64,
            0.0,
            1.0,
        )
        .unwrap();
        let w0 = 2.0 * std::f64::consts::PI * lift_hz as f64 / FS as f64;
        let alpha = w0.sin() / 2.0;
        let k = 1.0 + alpha;
        let bpc = crate::coeffs::Coeffs { b0: alpha / k, b1: 0.0, b2: -alpha / k, ..bp };
        let (mut band_dry, mut band_wet) =
            (crate::dsp::biquad::Biquad::new(bpc), crate::dsp::biquad::Biquad::new(bpc));

        // Per segment, per 10 ms frame: (dry, wet) broadband and band power.
        #[derive(Default)]
        struct Seg {
            dry: Vec<f32>,
            wet: Vec<f32>,
            band_dry: Vec<f32>,
            band_wet: Vec<f32>,
        }
        let mut segs: Vec<(Segment, Seg)> = Vec::new();
        let frame = FS as usize / 100;
        let p = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32;
        session(0xF00D, |seg, b| {
            let inter: Vec<f32> = b.iter().flat_map(|&x| [x, x]).collect();
            let mut out = vec![0.0; inter.len()];
            chain.process(&inter, &mut out);
            let wet: Vec<f32> = out.iter().step_by(2).copied().collect();
            let mut bd = b.to_vec();
            band_dry.process(&mut bd);
            let mut bw = wet.clone();
            band_wet.process(&mut bw);
            if segs.last().map(|s| s.0) != Some(seg) {
                segs.push((seg, Seg::default()));
            }
            let s = &mut segs.last_mut().unwrap().1;
            for i in (0..b.len()).step_by(frame) {
                let r = i..(i + frame).min(b.len());
                s.dry.push(p(&b[r.clone()]));
                s.wet.push(p(&wet[r.clone()]));
                s.band_dry.push(p(&bd[r.clone()]));
                s.band_wet.push(p(&bw[r]));
            }
        });
        // Second pass only: the loudness has settled.
        let pick = |s: Segment| &segs.iter().rfind(|f| f.0 == s).unwrap().1;
        let db = |p: f32| 10.0 * p.max(1e-20).log10();
        let peak = |v: &[f32]| v.iter().copied().fold(0.0f32, f32::max);
        // Sum of `wet` over `dry` on the frames whose dry power ranks in
        // `from..to` (fractions, loudest first), dB.
        let gain = |dry: &[f32], wet: &[f32], from: f32, to: f32| {
            let mut idx: Vec<usize> = (0..dry.len()).collect();
            idx.sort_by(|&a, &b| dry[b].total_cmp(&dry[a]));
            let n = idx.len() as f32;
            let sel = &idx[(n * from) as usize..(n * to) as usize];
            db(sel.iter().map(|&i| wet[i]).sum()) - db(sel.iter().map(|&i| dry[i]).sum())
        };

        // Gunshots: the loudest 10 % of frames come down, the peak never rises.
        let gun = pick(Segment::Gunfire);
        let gun_drop = -gain(&gun.dry, &gun.wet, 0.0, 0.1);
        assert!(gun_drop >= 3.0, "gunshots {gun_drop:.1} dB down; {dyns:?}");
        assert!(db(peak(&gun.wet)) <= db(peak(&gun.dry)) + 0.1, "never louder at the peak");

        // Footsteps: in their band, the step frames (loudest 30 %) go up, the
        // bed between them (quietest half) does not.
        let steps = pick(Segment::Footsteps);
        let rise = gain(&steps.band_dry, &steps.band_wet, 0.0, 0.3);
        assert!(rise >= 0.5, "footsteps up {rise:.2} dB; {dyns:?}");
        let between = gain(&steps.band_dry, &steps.band_wet, 0.5, 1.0);
        assert!(between <= 0.5, "the bed between steps up {between:.2} dB");

        // The bed alone and the whole ambience stretch: not raised.
        let amb = pick(Segment::Ambience);
        for (d, w) in [(&amb.dry, &amb.wet), (&amb.band_dry, &amb.band_wet)] {
            let up = gain(d, w, 0.0, 1.0);
            assert!(up <= 0.5, "the bed is not raised: {up:.2} dB");
        }
    }
}

#[cfg(all(test, feature = "dsp"))]
mod footstep_lift {
    use super::*;
    use crate::learn::analyzer::Analyzer;
    use crate::learn::synth::{Segment, Synth, FS};

    /// Steps in the 2-5 kHz region, `step` amplitude, over the standard bed.
    fn walk(step: f32, secs: f32, seed: u64, mut feed: impl FnMut(&[f32])) {
        let mut s = Synth::new(FS, seed);
        s.step = step;
        s.render(Segment::Footsteps, secs, |b| feed(b));
    }

    /// A shooter whose footsteps sit clearly above the bed (a quiet map).
    fn learned(step: f32) -> Stats {
        let mut a = Analyzer::new(FS);
        let mut s = Synth::new(FS, 0xBEEF);
        s.step = step;
        for _ in 0..2 {
            for (seg, secs) in [
                (Segment::Footsteps, 30.0),
                (Segment::Gunfire, 30.0),
                (Segment::Gameplay, 30.0),
                (Segment::Explosions, 20.0),
                (Segment::Ambience, 10.0),
            ] {
                s.render(seg, secs, |b| a.push(b));
            }
        }
        a.into_stats()
    }

    /// In-band (2-5 kHz) power gain of the step frames (loudest 30 % of
    /// 10 ms frames), dB, after `warm` seconds of settling.
    fn step_rise(dyns: &DynamicsParams, step: f32) -> f64 {
        use crate::dsp::{Chain, ChainParams};
        let mut chain =
            Chain::new(ChainParams { dynamics: Some(dyns.clone()), ..Default::default() });
        chain.prepare(FS, 4800).unwrap();
        let band = |x: &[f32]| {
            let c = crate::coeffs::Coeffs::design(
                crate::params::FilterKind::HighPass,
                FS as f64,
                2000.0,
                0.0,
                0.707,
            )
            .unwrap();
            let mut hp = crate::dsp::biquad::Biquad::new(c);
            let mut v: Vec<f32> = x.to_vec();
            hp.process(&mut v);
            v
        };
        let (mut dry, mut wet) = (Vec::new(), Vec::new());
        let mut t = 0usize;
        walk(step, 20.0, 0x5EED, |b| {
            let inter: Vec<f32> = b.iter().flat_map(|&x| [x, x]).collect();
            let mut out = vec![0.0; inter.len()];
            chain.process(&inter, &mut out);
            if t >= FS as usize * 5 {
                dry.extend_from_slice(b);
                wet.extend(out.iter().step_by(2));
            }
            t += b.len();
        });
        let (d, w) = (band(&dry), band(&wet));
        let frame = FS as usize / 100;
        let p = |x: &[f32]| x.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        let mut fr: Vec<(f64, f64)> =
            d.chunks(frame).zip(w.chunks(frame)).map(|(a, b)| (p(a), p(b))).collect();
        fr.sort_by(|a, b| b.0.total_cmp(&a.0));
        let top = &fr[..fr.len() * 3 / 10];
        10.0 * (top.iter().map(|f| f.1).sum::<f64>() / top.iter().map(|f| f.0).sum::<f64>()).log10()
    }

    #[test]
    fn quiet_footsteps_clearly_above_the_bed_are_lifted_audibly() {
        let st = learned(0.15);
        let dyns = derive_dynamics(&st, Goal::Awareness).expect("awareness stage");
        let typical = step_rise(&dyns, 0.15);
        let distant = step_rise(&dyns, 0.08);
        eprintln!("typical {typical:.2} dB, distant {distant:.2} dB; {dyns:?}");
        assert!(distant >= 2.5, "distant steps up only {distant:.2} dB");
        assert!(distant <= MAX_LIFT_DB as f64 + 0.5);
        assert!(typical >= 1.5 && typical <= distant + 0.5, "louder steps get less lift");
        // The bed alone (no steps): its loudest stretches are not lifted.
        let bed = step_rise(&dyns, 0.0);
        assert!(bed <= 1.0, "the bed alone rose {bed:.2} dB");
    }
}
