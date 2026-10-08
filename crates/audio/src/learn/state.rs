//! Readiness, convergence and the per-game learning record.
//!
//! A [`LearnRecord`] is what persists per game exe between sessions: the
//! aggregates ([`Stats`]) of the last ~60 minutes of active play, the exe's
//! file version, the last few checkpoint curves, and the candidate curve
//! once there is enough evidence. Never audio.
//!
//! **Ready is evidence, not a timer.** A curve is offered only when both hold
//! ([`Thresholds`]):
//! - the rolling window holds enough target and masker events: enough that
//!   every band's median level is known within [`MEDIAN_SE_DB`] (S48), never
//!   fewer than [`MIN_CUES_FLOOR`] / [`MIN_MASKERS_FLOOR`] and never more
//!   than [`MIN_CUES`] / [`MIN_MASKERS`], over [`MIN_ACTIVE_SECS`] of play;
//! - the derived curve has **converged**: the last [`CONVERGE_CHECKPOINTS`]
//!   checkpoints (one per [`CHECKPOINT_SECS`] of active play) agree within
//!   [`CONVERGE_DB`] in every band.
//!
//! **After that the curve is frozen.** Learning carries on in a rolling
//! window ([`WINDOW_SEGMENTS`] × [`SEGMENT_SECS`] ≈ 60 min of active play) so
//! a game update that remixes its audio is noticed, but the candidate only
//! moves when a newly converged curve differs from it by more than
//! [`UPDATE_DB`] in some band. Small drift never nudges what the user hears.
//!
//! A new exe version resets the evidence and sets `needs_relearn`; the
//! previously applied curve keeps applying (it lives in the profile, not
//! here) until the new one is ready.
//!
//! Any analysis error keeps the last good candidate (and is recorded in
//! `last_error`); nothing is ever replaced by a bad curve.

use serde::{Deserialize, Serialize};

use super::analyzer::{SoundClass, Stats, FRAME_MS, STATIONARY_EVENT_FRAMES};
use super::derive::{derive_checked, max_delta, Goal, Limits};
use super::{HIST_STEP_DB, NBANDS};

/// Schema of the on-disk record. A different schema is discarded and
/// relearned (it holds statistics only, so nothing of the user's is lost).
pub const RECORD_SCHEMA: u32 = 3;
/// Target events the window must hold at most (for Awareness: footsteps,
/// foliage, reloads, callouts; for Dialogue: half-second chunks of game
/// voice). S48: this is the ceiling; a game whose levels are consistent needs
/// fewer (see [`MEDIAN_SE_DB`]), never fewer than [`MIN_CUES_FLOOR`].
pub const MIN_CUES: u64 = 120;
/// Masker events (gunshots, explosions, and half-second chunks of vehicles
/// and music) the window must hold at most; floor [`MIN_MASKERS_FLOOR`].
pub const MIN_MASKERS: u64 = 60;
/// S48: however tight the statistics, at least this many target events.
pub const MIN_CUES_FLOOR: u64 = 40;
/// S48: ...and at least this many masker events.
pub const MIN_MASKERS_FLOOR: u64 = 20;
/// S48: the evidence is enough when the median level of every band a group
/// (targets, maskers) occupies is known within this standard error, dB. The
/// standard error of a median is `sqrt(pi/2) * sd / sqrt(n)` with `n` the
/// group's events, and the spread comes from each band's interquartile range,
/// so a game with steady footsteps is ready on fewer of them than one whose
/// footsteps range from a whisper to a stomp.
pub const MEDIAN_SE_DB: f32 = 2.0;
/// Bands that count for the standard-error bound: those whose median is
/// within this of the group's loudest band (the rest is the floor the event
/// sat on).
pub const SE_BAND_RANGE_DB: f32 = 12.0;
/// A class counts for the bound only when it is at least this share of its
/// group's events; a rare class cannot hold the whole group back.
pub const CLASS_SHARE_MIN: f64 = 0.1;
/// Frames between independent level samples of a stationary class (voice,
/// music, vehicles, ambience): 100 ms, about one syllable or note apart.
pub const STATIONARY_SAMPLE_FRAMES: u64 = 10;
/// Standard error of a median per unit `sd / sqrt(n)`, for normal data.
pub const MEDIAN_SE_FACTOR: f32 = 1.2533;
/// Interquartile range per standard deviation, for normal data.
pub const IQR_PER_SD: f32 = 1.349;
/// Active-play seconds between checkpoints (S48: 30, was 60).
pub const CHECKPOINT_SECS: u64 = 30;
/// And at least this much active play, so a short burst of action cannot
/// count as knowing the game (S48: 5 min, was 10).
pub const MIN_ACTIVE_SECS: u64 = 5 * 60;
/// Converged: the last checkpoints agree within this in every band, dB.
pub const CONVERGE_DB: f32 = 0.5;
/// ...over this many checkpoints (S48: 2, was 3).
pub const CONVERGE_CHECKPOINTS: usize = 2;
/// The ETA is only estimated once this much active play shows the game's
/// event rate; before that it is unknown.
pub const ETA_MIN_ACTIVE_SECS: u64 = 60;
/// A converged curve replaces the frozen one only past this difference, dB.
pub const UPDATE_DB: f32 = 1.5;
/// Rolling window: segments of this many active seconds...
pub const SEGMENT_SECS: u64 = 600;
/// ...this many of them (60 min).
pub const WINDOW_SEGMENTS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Ceiling of the target-event requirement.
    pub min_cues: u64,
    /// Ceiling of the masker-event requirement.
    pub min_maskers: u64,
    pub min_cues_floor: u64,
    pub min_maskers_floor: u64,
    pub median_se_db: f32,
    pub min_active_secs: u64,
    pub checkpoint_secs: u64,
    pub converge_db: f32,
    pub converge_checkpoints: usize,
    pub update_db: f32,
    pub segment_secs: u64,
    pub window_segments: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            min_cues: MIN_CUES,
            min_maskers: MIN_MASKERS,
            min_cues_floor: MIN_CUES_FLOOR,
            min_maskers_floor: MIN_MASKERS_FLOOR,
            median_se_db: MEDIAN_SE_DB,
            min_active_secs: MIN_ACTIVE_SECS,
            checkpoint_secs: CHECKPOINT_SECS,
            converge_db: CONVERGE_DB,
            converge_checkpoints: CONVERGE_CHECKPOINTS,
            update_db: UPDATE_DB,
            segment_secs: SEGMENT_SECS,
            window_segments: WINDOW_SEGMENTS,
        }
    }
}

/// Per game profile, what the UI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearnStatus {
    /// Learning is switched off and no game layer is applied.
    Off,
    /// Gathering evidence.
    Learning,
    /// A curve is ready and differs from what is applied: offer Apply.
    Ready,
    /// The applied game layer is current.
    Applied,
    /// The game was updated: the old layer keeps applying while Relay
    /// relearns in the background.
    NeedsRelearn,
}

/// What one checkpoint did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Not due yet.
    NotDue,
    /// A checkpoint was taken; the candidate did not change.
    Checkpoint,
    /// The first converged curve: offer it.
    FirstCurve,
    /// A converged curve that differs from the frozen one by more than
    /// [`UPDATE_DB`]: offer the update.
    UpdateOffered,
    /// The derivation failed; the last good curve stays.
    Error,
}

impl Outcome {
    /// The candidate changed.
    pub fn offers(self) -> bool {
        matches!(self, Outcome::FirstCurve | Outcome::UpdateOffered)
    }
}

/// The convergence gate's state, for status.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Convergence {
    pub agreeing: usize,
    pub needed: usize,
    pub max_delta_db: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnRecord {
    pub schema: u32,
    /// Lower-case exe file name.
    pub exe: String,
    /// File version from the exe's version resource, when it has one.
    #[serde(default)]
    pub exe_version: Option<String>,
    /// The goal the checkpoints and candidate were derived for.
    #[serde(default)]
    pub goal: Goal,
    /// Rolling window, oldest first; the last one is being filled.
    #[serde(default)]
    pub segments: Vec<Stats>,
    /// Active frames since the evidence was last reset.
    #[serde(default)]
    pub total_active_frames: u64,
    /// `total_active_frames` at the last checkpoint.
    #[serde(default)]
    pub last_checkpoint_frames: u64,
    /// Recent checkpoint gain vectors, oldest first.
    #[serde(default)]
    pub checkpoints: Vec<Vec<f32>>,
    /// The offered (frozen) curve.
    #[serde(default)]
    pub candidate: Option<Vec<(f32, f32)>>,
    /// Its band gains, for the update comparison.
    #[serde(default)]
    pub candidate_gains: Option<Vec<f32>>,
    #[serde(default)]
    pub needs_relearn: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl LearnRecord {
    pub fn new(exe: &str, version: Option<&str>) -> Self {
        Self {
            schema: RECORD_SCHEMA,
            exe: exe.to_ascii_lowercase(),
            exe_version: version.map(str::to_owned),
            goal: Goal::default(),
            segments: Vec::new(),
            total_active_frames: 0,
            last_checkpoint_frames: 0,
            checkpoints: Vec::new(),
            candidate: None,
            candidate_gains: None,
            needs_relearn: false,
            last_error: None,
        }
    }

    /// Parse a stored record; anything unreadable, of another schema, or for
    /// another exe is `None` (the caller starts fresh).
    pub fn from_json(text: &str, exe: &str) -> Option<Self> {
        let mut r: Self = serde_json::from_str(text).ok()?;
        if !(r.schema == RECORD_SCHEMA && r.exe.eq_ignore_ascii_case(exe)) {
            return None;
        }
        // A record on disk can be edited by hand: its curve passes the same
        // guard as everything else before anyone can apply it.
        if let Some(c) = r.candidate.as_mut() {
            let g = super::derive::guard_curve(c);
            if g != *c {
                r.candidate_gains = Some(g[1..=super::NBANDS].iter().map(|p| p.1).collect());
                *c = g;
            }
        }
        Some(r)
    }

    /// Start a learning session for `version`. A changed version resets the
    /// evidence and flags a relearn. Returns true when it did.
    pub fn begin_session(&mut self, version: Option<&str>) -> bool {
        if self.segments.iter().any(|s| !s.well_formed()) {
            self.reset_evidence();
        }
        match (self.exe_version.as_deref(), version) {
            (Some(old), Some(new)) if old != new => {
                self.reset_evidence();
                self.candidate = None;
                self.candidate_gains = None;
                self.needs_relearn = true;
                self.exe_version = Some(new.to_owned());
                true
            }
            (None, Some(new)) => {
                self.exe_version = Some(new.to_owned());
                false
            }
            _ => false,
        }
    }

    /// Throw the evidence away (keeps the candidate).
    pub fn reset_evidence(&mut self) {
        self.segments.clear();
        self.total_active_frames = 0;
        self.last_checkpoint_frames = 0;
        self.checkpoints.clear();
        self.last_error = None;
    }

    /// The user's Relearn: evidence and candidate both go; the applied layer
    /// (in the profile) stays until a new curve is taken.
    pub fn relearn(&mut self) {
        self.reset_evidence();
        self.candidate = None;
        self.candidate_gains = None;
        self.needs_relearn = false;
    }

    fn frames(secs: u64) -> u64 {
        secs * 1000 / FRAME_MS as u64
    }

    /// Add freshly gathered aggregates into the rolling window.
    pub fn absorb(&mut self, fresh: &Stats, th: &Thresholds) {
        if !fresh.well_formed() {
            return;
        }
        if self.segments.is_empty() {
            self.segments.push(Stats::default());
        }
        let last = self.segments.last_mut().expect("one segment");
        last.merge(fresh);
        self.total_active_frames += fresh.active_frames;
        if last.active_frames >= Self::frames(th.segment_secs.max(1)) {
            self.segments.push(Stats::default());
        }
        while self.segments.len() > th.window_segments.max(1) {
            self.segments.remove(0);
        }
    }

    /// The rolling window as one aggregate.
    pub fn window(&self) -> Stats {
        let mut w = Stats::default();
        for s in &self.segments {
            w.merge(s);
        }
        w
    }

    /// Target and masker events the window needs for the goal, scaled to how
    /// consistent the game's levels are (S48): between the floor and the
    /// ceiling of [`Thresholds`], enough that each band's median is known
    /// within `median_se_db`.
    pub fn required(&self, th: &Thresholds) -> (u64, u64) {
        required_for(&self.window(), self.goal, th)
    }

    /// Whether the window holds enough target and masker events for the goal.
    pub fn enough_evidence(&self, th: &Thresholds) -> bool {
        let w = self.window();
        let (t, m) = self.goal.evidence(&w);
        let (rt, rm) = required_for(&w, self.goal, th);
        t >= rt && m >= rm && w.active_secs() >= th.min_active_secs
    }

    /// Seconds of active play still needed before a curve is ready, from the
    /// event rate seen so far: the slowest of the active-time minimum, the
    /// target and masker counts, and the agreeing checkpoints. `Some(0)` once
    /// ready; `None` while the rate is unknown (under a minute of play, or a
    /// group with no events at all yet).
    pub fn eta_secs(&self, th: &Thresholds) -> Option<u64> {
        if self.candidate.is_some() && !self.needs_relearn {
            return Some(0);
        }
        let w = self.window();
        let active = w.active_secs();
        if active < ETA_MIN_ACTIVE_SECS {
            return None;
        }
        let (t, m) = self.goal.evidence(&w);
        let (rt, rm) = required_for(&w, self.goal, th);
        let left = |have: u64, want: u64| -> Option<u64> {
            if have >= want {
                Some(0)
            } else if have == 0 {
                None
            } else {
                // Events arrive at have/active per second.
                Some(((want - have) as f64 * active as f64 / have as f64).ceil() as u64)
            }
        };
        let evidence =
            left(t, rt)?.max(left(m, rm)?).max(th.min_active_secs.saturating_sub(active));
        let need = th.converge_checkpoints.max(1);
        let agreeing = self.agreeing(th).min(need);
        let cp = th.checkpoint_secs.max(1);
        let since = ((self.total_active_frames.saturating_sub(self.last_checkpoint_frames))
            * FRAME_MS as u64
            / 1000)
            .min(cp);
        // Checkpoints still to agree, a checkpoint interval apart; the next
        // one is already under way.
        let settle = (need.saturating_sub(agreeing).max(1)) as u64 * cp - since;
        Some(evidence.max(settle))
    }

    /// Switch goal. The aggregates are goal-independent, so a curve that was
    /// ready is re-derived from them at once — no relearn. Returns true when
    /// the candidate changed.
    pub fn set_goal(&mut self, goal: Goal, limits: &Limits) -> bool {
        if goal == self.goal {
            return false;
        }
        self.goal = goal;
        // Old checkpoints describe another goal's curve.
        self.checkpoints.clear();
        if self.candidate.is_none() || self.needs_relearn {
            return false;
        }
        match derive_checked(&self.window(), limits, goal) {
            Ok(d) => {
                self.candidate = Some(d.curve());
                self.candidate_gains = Some(d.gains.to_vec());
                true
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                false
            }
        }
    }

    /// How many of the newest checkpoints agree with each other within
    /// `converge_db` (1 when there is one checkpoint, 0 with none).
    fn agreeing(&self, th: &Thresholds) -> usize {
        let n = self.checkpoints.len();
        let mut k = 0;
        'grow: for start in (0..n).rev() {
            for i in start + 1..n {
                if max_delta(&self.checkpoints[start], &self.checkpoints[i]) >= th.converge_db {
                    break 'grow;
                }
            }
            k += 1;
        }
        k
    }

    /// What the convergence gate is waiting on: how many of the newest
    /// checkpoints agree, out of how many are needed, and the largest
    /// per-band difference among the last `converge_checkpoints` of them.
    pub fn convergence(&self, th: &Thresholds) -> Convergence {
        let need = th.converge_checkpoints.max(1);
        let n = self.checkpoints.len();
        let tail = &self.checkpoints[n.saturating_sub(need)..];
        let mut worst = 0f32;
        for i in 0..tail.len() {
            for j in i + 1..tail.len() {
                worst = worst.max(max_delta(&tail[i], &tail[j]));
            }
        }
        Convergence { agreeing: self.agreeing(th).min(need), needed: need, max_delta_db: worst }
    }

    /// The last `converge_checkpoints` checkpoints agree.
    pub fn converged(&self, th: &Thresholds) -> bool {
        self.agreeing(th) >= th.converge_checkpoints.max(1)
    }

    /// Take a checkpoint if one is due.
    pub fn checkpoint(&mut self, th: &Thresholds, limits: &Limits) -> Outcome {
        let due = self.total_active_frames
            >= self.last_checkpoint_frames + Self::frames(th.checkpoint_secs.max(1));
        if !due {
            return Outcome::NotDue;
        }
        self.last_checkpoint_frames = self.total_active_frames;
        let window = if self.segments.iter().all(Stats::well_formed) {
            Ok(self.window())
        } else {
            Err(super::derive::DeriveError::Malformed)
        };
        let goal = self.goal;
        let d = match window.and_then(|w| derive_checked(&w, limits, goal)) {
            Ok(d) => d,
            Err(e) => {
                self.last_error = Some(e.to_string());
                return Outcome::Error;
            }
        };
        self.last_error = None;
        let gains = d.gains.to_vec();
        self.checkpoints.push(gains.clone());
        let keep = th.converge_checkpoints.max(1);
        while self.checkpoints.len() > keep {
            self.checkpoints.remove(0);
        }
        if !(self.enough_evidence(th) && self.converged(th)) {
            return Outcome::Checkpoint;
        }
        let outcome = match &self.candidate_gains {
            None => Outcome::FirstCurve,
            Some(c) if max_delta(c, &gains) > th.update_db => Outcome::UpdateOffered,
            Some(_) => return Outcome::Checkpoint,
        };
        self.candidate = Some(d.curve());
        self.candidate_gains = Some(gains);
        self.needs_relearn = false;
        outcome
    }

    /// 0–100. Evidence counts are 90 % of it, convergence the last 10 %;
    /// 100 only once a curve is ready.
    pub fn progress(&self, th: &Thresholds) -> u8 {
        if self.candidate.is_some() && !self.needs_relearn {
            return 100;
        }
        let frac = |have: u64, want: u64| -> f32 {
            if want == 0 {
                1.0
            } else {
                (have as f32 / want as f32).min(1.0)
            }
        };
        let w = self.window();
        let (t, m) = self.goal.evidence(&w);
        let (rt, rm) = required_for(&w, self.goal, th);
        let evidence = frac(t, rt).min(frac(m, rm)).min(frac(w.active_secs(), th.min_active_secs));
        let need = th.converge_checkpoints.max(1);
        let stable = if need <= 1 {
            1.0
        } else {
            (self.agreeing(th).saturating_sub(1) as f32 / (need - 1) as f32).min(1.0)
        };
        let p = evidence * 90.0 + if evidence >= 1.0 { stable * 10.0 } else { 0.0 };
        (p.floor() as u8).min(99)
    }

    /// Minutes of active play in the window.
    pub fn active_minutes(&self) -> f32 {
        self.segments.iter().map(|s| s.active_frames).sum::<u64>() as f32 * FRAME_MS as f32
            / 60_000.0
    }
}

/// Events of a group (the goal's targets, or its maskers) needed so that, for
/// every class that makes up at least [`CLASS_SHARE_MIN`] of the group, the
/// median level of every band the class lives in (within
/// [`SE_BAND_RANGE_DB`] of its loudest band) is known within `se_db`. Per
/// class and band the spread is the interquartile range, the standard error
/// of the median `MEDIAN_SE_FACTOR * sd / sqrt(n)`, so the class needs
/// `n = (MEDIAN_SE_FACTOR * sd / se_db)^2` events, and the group `n / share`
/// at the mix heard so far. Classes are never pooled: two classes at
/// different levels are not "spread". `None` with no events.
pub fn events_for_confidence(w: &Stats, classes: &[SoundClass], se_db: f32) -> Option<u64> {
    if !w.well_formed() || !(se_db.is_finite() && se_db > 0.0) {
        return None;
    }
    let total: u64 = classes.iter().map(|&c| w.count(c)).sum();
    if total == 0 {
        return None;
    }
    // A single 2 dB bin is a uniform spread of 2/sqrt(12) dB, not zero.
    let sd_floor = HIST_STEP_DB / 12f32.sqrt();
    let mut need = 0f64;
    for &c in classes {
        let events = w.count(c);
        let share = events as f64 / total as f64;
        if events == 0 || share < CLASS_SHARE_MIN {
            continue;
        }
        let h = w.hist(c);
        let medians: Vec<Option<f32>> = (0..NBANDS).map(|b| Stats::median(h, b)).collect();
        let Some(top) = medians.iter().flatten().cloned().reduce(f32::max) else { continue };
        let mut n_class = 0f32;
        for (b, med) in medians.iter().enumerate() {
            let Some(med) = med else { continue };
            if *med < top - SE_BAND_RANGE_DB {
                continue;
            }
            let (Some(q1), Some(q3)) =
                (Stats::percentile(h, b, 0.25), Stats::percentile(h, b, 0.75))
            else {
                continue;
            };
            let sd = ((q3 - q1) / IQR_PER_SD).max(sd_floor);
            n_class = n_class.max((MEDIAN_SE_FACTOR * sd / se_db).powi(2));
        }
        // A stationary class keeps one histogram entry per frame and counts
        // one event per half-second chunk; its independent samples are
        // STATIONARY_SAMPLE_FRAMES apart, several per chunk.
        if !c.is_transient() {
            n_class *= STATIONARY_SAMPLE_FRAMES as f32 / STATIONARY_EVENT_FRAMES as f32;
        }
        need = need.max(n_class as f64 / share);
    }
    Some(need.ceil() as u64)
}

/// [`LearnRecord::required`] for an aggregate and goal.
pub fn required_for(w: &Stats, goal: Goal, th: &Thresholds) -> (u64, u64) {
    let (tc, mc) = goal.evidence_classes();
    let scale = |classes: &[SoundClass], floor: u64, ceil: u64| -> u64 {
        match events_for_confidence(w, classes, th.median_se_db) {
            Some(n) => n.clamp(floor.min(ceil), ceil),
            None => ceil,
        }
    };
    (scale(tc, th.min_cues_floor, th.min_cues), scale(mc, th.min_maskers_floor, th.min_maskers))
}

/// The status a profile shows, from its record (if any), whether learning is
/// on, and the game layer it currently applies (if any).
pub fn status(
    record: Option<&LearnRecord>,
    enabled: bool,
    applied: Option<&[(f32, f32)]>,
) -> LearnStatus {
    let candidate = record.and_then(|r| r.candidate.as_deref());
    if record.is_some_and(|r| r.needs_relearn) && candidate.is_none() {
        return if applied.is_some() || enabled {
            LearnStatus::NeedsRelearn
        } else {
            LearnStatus::Off
        };
    }
    match (candidate, applied) {
        (Some(c), Some(a)) if same_curve(c, a) => LearnStatus::Applied,
        (Some(_), _) if enabled || applied.is_some() => LearnStatus::Ready,
        (_, Some(_)) => LearnStatus::Applied,
        _ if enabled => LearnStatus::Learning,
        _ => LearnStatus::Off,
    }
}

/// Curves equal within 0.05 dB at every point.
pub fn same_curve(a: &[(f32, f32)], b: &[(f32, f32)]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| (x.0 - y.0).abs() < 0.5 && (x.1 - y.1).abs() < 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::analyzer::Analyzer;
    use crate::learn::synth::{Segment, Synth, FS};

    /// Thresholds a few synthetic minutes can satisfy.
    fn quick() -> Thresholds {
        Thresholds {
            min_cues: 60,
            min_maskers: 10,
            min_active_secs: 30,
            checkpoint_secs: 20,
            segment_secs: 60,
            window_segments: 3,
            ..Thresholds::default()
        }
    }

    /// Drive a record the way the helper does: 1 s of audio, absorb, checkpoint.
    struct Rig {
        a: Analyzer,
        s: Synth,
    }
    impl Rig {
        fn new(seed: u64) -> Self {
            let mut s = Synth::new(FS, seed);
            s.step = 0.12;
            Self { a: Analyzer::new(FS), s }
        }
        /// Returns every non-trivial outcome with the second it happened at.
        fn play(
            &mut self,
            rec: &mut LearnRecord,
            secs: u32,
            th: &Thresholds,
        ) -> Vec<(u32, Outcome)> {
            let mut out = Vec::new();
            for t in 0..secs {
                let a = &mut self.a;
                self.s.render(Segment::Gameplay, 1.0, |b| a.push(b));
                rec.absorb(&self.a.take_stats(), th);
                match rec.checkpoint(th, &Limits::default()) {
                    Outcome::NotDue | Outcome::Checkpoint => {}
                    o => out.push((t, o)),
                }
            }
            out
        }
    }

    /// The S46 thresholds as shipped before S48 (60 s checkpoints, three
    /// agreeing, 10 min minimum, a raw 120 / 60 event count), for the
    /// "half the time" comparison.
    fn s46_thresholds() -> Thresholds {
        Thresholds {
            min_cues_floor: MIN_CUES,
            min_maskers_floor: MIN_MASKERS,
            min_active_secs: 10 * 60,
            checkpoint_secs: 60,
            converge_checkpoints: 3,
            ..Thresholds::default()
        }
    }

    /// Replay a live session minute by minute (`script` is one minute of
    /// segments) until the first curve. Returns (seconds to ready, the ETA
    /// the record showed at each whole minute, the record).
    fn replay(
        th: &Thresholds,
        seed: u64,
        tune: impl Fn(&mut Synth),
        script: &[(Segment, u32)],
        max_min: u32,
    ) -> (Option<u32>, Vec<Option<u64>>, LearnRecord) {
        let mut r = LearnRecord::new("game.exe", None);
        let mut a = Analyzer::new(FS);
        let mut s = Synth::new(FS, seed);
        s.step = 0.12;
        tune(&mut s);
        let mut etas = Vec::new();
        let mut t = 0u32;
        for _ in 0..max_min {
            etas.push(r.eta_secs(th));
            for &(seg, secs) in script {
                for _ in 0..secs {
                    s.render(seg, 1.0, |b| a.push(b));
                    r.absorb(&a.take_stats(), th);
                    t += 1;
                    if r.checkpoint(th, &Limits::default()) == Outcome::FirstCurve {
                        return (Some(t), etas, r);
                    }
                }
            }
        }
        (None, etas, r)
    }

    /// r51 on PC2 (Warzone, no commentary): ~10 cues/min, voice callouts
    /// while playing, plenty of gunfire and explosions.
    const R51: &[(Segment, u32)] =
        &[(Segment::Gameplay, 50), (Segment::Speech, 6), (Segment::Gunfire, 4)];
    fn r51_tune(s: &mut Synth) {
        s.step_ms = 6000; // ~10 footsteps / min
        s.boom_ms = 6000;
    }

    /// S48: the r51-like replay was ready in 10 min with the S46 thresholds;
    /// the S48 ones get there in roughly half the time.
    #[test]
    fn r51_like_play_is_ready_in_about_half_the_time() {
        let (new, etas, r) = replay(&Thresholds::default(), 51, r51_tune, R51, 25);
        let (old, _, _) = replay(&s46_thresholds(), 51, r51_tune, R51, 25);
        let (new, old) = (new.expect("S48 ready"), old.expect("S46 ready"));
        eprintln!(
            "S48 r51-like time to ready: {:.1} min (S46 thresholds: {:.1} min); needed {:?}",
            new as f32 / 60.0,
            old as f32 / 60.0,
            r.required(&Thresholds::default())
        );
        assert!((240..=420).contains(&new), "{new} s");
        assert!(new as f32 <= 0.65 * old as f32, "{new} s vs {old} s");
        // The ETA it showed along the way was honest: at 2 min in it named a
        // time within a minute and a half of the real one.
        let at2 = etas[2].expect("an ETA after 2 min");
        let real = new.saturating_sub(120) as u64;
        assert!(at2.abs_diff(real) <= 90, "ETA {at2} s, real {real} s");
    }

    /// r53 run B: a streamer talking over the game for half of every minute,
    /// at the event rates r51/r53 logged for real play (~25 targets and ~30
    /// maskers a minute). Commentary is left out (S46 r53); the replay still
    /// reaches ready in about half the S46 time.
    #[test]
    fn r53_like_commentary_replay_is_ready_in_about_half_the_time() {
        let script: &[(Segment, u32)] =
            &[(Segment::Commentary, 30), (Segment::Gameplay, 24), (Segment::Gunfire, 6)];
        let tune = |s: &mut Synth| {
            s.step_ms = 2000;
            s.boom_ms = 4000;
        };
        let (new, _, r) = replay(&Thresholds::default(), 53, tune, script, 30);
        let (old, _, _) = replay(&s46_thresholds(), 53, tune, script, 30);
        let (new, old) = (new.expect("S48 ready"), old.expect("S46 ready"));
        eprintln!(
            "S48 r53-like (commentary) time to ready: {:.1} min (S46: {:.1} min)",
            new as f32 / 60.0,
            old as f32 / 60.0
        );
        assert!(r.window().overlay_voice_frames > 0, "commentary was left out");
        assert!(new as f32 <= 0.65 * old as f32, "{new} s vs {old} s");
        assert!(new >= MIN_ACTIVE_SECS as u32, "never before the minimum");
    }

    /// Hostile and varied input does not get through faster: a game that
    /// never settles never offers, whatever the speed-up.
    #[test]
    fn an_unsettled_mix_still_never_offers() {
        let th = Thresholds { converge_db: 0.0, ..Thresholds::default() };
        let (ready, _, r) = replay(&th, 7, r51_tune, R51, 9);
        assert!(ready.is_none());
        assert!(r.enough_evidence(&th), "evidence was there; convergence holds it back");
    }

    /// The per-band standard-error rule: a group whose levels sit in one
    /// place needs only the floor; one spread over 40 dB needs the ceiling.
    #[test]
    fn evidence_scales_with_the_spread_of_levels() {
        use crate::learn::{bin_of, HIST_BINS};
        let th = Thresholds::default();
        let n = NBANDS * HIST_BINS;
        let mut tight = Stats { active_frames: 60_000, ..Stats::default() };
        let mut wide = tight.clone();
        for st in [&mut tight, &mut wide] {
            st.events[SoundClass::Footsteps.index()] = 30;
            st.events[SoundClass::Explosion.index()] = 30;
        }
        for b in 0..NBANDS {
            for (ci, base) in [(SoundClass::Footsteps.index(), -45.0), (5, -50.0)] {
                tight.class_hist[ci * n + b * HIST_BINS + bin_of(base)] = 30;
                for k in 0..30 {
                    // 30 events from base-40 to base+40 dB.
                    let db = base - 40.0 + k as f32 * 80.0 / 29.0;
                    wide.class_hist[ci * n + b * HIST_BINS + bin_of(db)] += 1;
                }
            }
        }
        let tc = Goal::Awareness.evidence_classes().0;
        assert!(events_for_confidence(&tight, tc, MEDIAN_SE_DB).unwrap() < MIN_CUES_FLOOR);
        assert!(events_for_confidence(&wide, tc, MEDIAN_SE_DB).unwrap() > MIN_CUES);
        assert_eq!(required_for(&tight, Goal::Awareness, &th), (MIN_CUES_FLOOR, MIN_MASKERS_FLOOR));
        assert_eq!(required_for(&wide, Goal::Awareness, &th), (MIN_CUES, MIN_MASKERS));
        // No events at all: the ceiling, never the floor.
        assert_eq!(required_for(&Stats::default(), Goal::Dialogue, &th), (MIN_CUES, MIN_MASKERS));
        // A malformed aggregate is never "confident".
        let mut bad = tight.clone();
        bad.class_hist.truncate(10);
        assert_eq!(events_for_confidence(&bad, tc, MEDIAN_SE_DB), None);
    }

    #[test]
    fn eta_is_unknown_at_first_then_counts_down_to_zero() {
        let th = Thresholds::default();
        let (ready, etas, r) = replay(&th, 51, r51_tune, R51, 25);
        assert!(ready.is_some());
        assert_eq!(etas[0], None, "no rate after 0 s");
        let known: Vec<u64> = etas.iter().skip(2).flatten().cloned().collect();
        assert!(known.len() >= 2, "{etas:?}");
        assert!(known.windows(2).all(|w| w[1] <= w[0] + 30), "counts down: {etas:?}");
        assert_eq!(r.eta_secs(&th), Some(0), "ready");
    }

    #[test]
    fn eta_waits_for_the_minimum_and_for_settling() {
        let th = Thresholds::default();
        let mut r = LearnRecord::new("game.exe", None);
        // Two minutes of play with plenty of events: the time minimum rules.
        let mut st = Stats { active_frames: 12_000, ..Stats::default() };
        st.events[SoundClass::Footsteps.index()] = 500;
        st.events[SoundClass::Explosion.index()] = 500;
        r.absorb(&st, &th);
        r.last_checkpoint_frames = r.total_active_frames;
        assert_eq!(r.eta_secs(&th), Some(MIN_ACTIVE_SECS - 120));
        // Past the minimum with the evidence in: two agreeing checkpoints to go.
        let mut more = Stats { active_frames: 30_000, ..Stats::default() };
        more.events[0] = 1;
        r.absorb(&more, &th);
        r.last_checkpoint_frames = r.total_active_frames;
        assert_eq!(r.eta_secs(&th), Some(2 * CHECKPOINT_SECS));
        // A group with no events at all yet: unknown, not zero.
        let mut quiet = LearnRecord::new("game.exe", None);
        let mut st = Stats { active_frames: 12_000, ..Stats::default() };
        st.events[SoundClass::Footsteps.index()] = 50;
        quiet.absorb(&st, &th);
        assert_eq!(quiet.eta_secs(&th), None);
    }

    /// S48 file learning merges into the same record: evidence from a video
    /// and from live play add up (weighted by how much of each there is), and
    /// live play continues from a file-learned start.
    #[test]
    fn file_and_live_evidence_add_up_in_one_record() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", None);
        let mut rig = Rig::new(4);
        rig.play(&mut r, 40, &th); // "from a file"
        let file = r.window();
        let mut live = Rig::new(5);
        live.play(&mut r, 30, &th);
        let both = r.window();
        assert_eq!(both.active_frames, r.total_active_frames);
        assert!(both.active_frames > file.active_frames);
        for c in SoundClass::ALL {
            assert!(both.count(c) >= file.count(c), "{c:?}");
        }
        // And it keeps going to a curve under the same rules.
        let got = live.play(&mut r, 240, &th);
        assert!(got.iter().any(|(_, o)| *o == Outcome::FirstCurve) || r.candidate.is_some());
    }

    /// r53 run B: a streamer talking over the game kept the curve moving.
    /// With the overlay voice left out, the same game with and without
    /// commentary converges to curves within 1 dB in 300 Hz - 4 kHz.
    #[test]
    fn commentary_on_top_converges_to_the_same_curve() {
        let th = quick();
        let learn = |seg: Segment| {
            let mut r = LearnRecord::new("game.exe", None);
            let mut a = Analyzer::new(FS);
            let mut s = Synth::new(FS, 53);
            s.step = 0.12;
            let mut ready = false;
            for _ in 0..360 {
                s.render(seg, 1.0, |b| a.push(b));
                r.absorb(&a.take_stats(), &th);
                ready |= r.checkpoint(&th, &Limits::default()).offers();
            }
            (r, ready)
        };
        let (plain, ok_plain) = learn(Segment::Gameplay);
        let (talk, ok_talk) = learn(Segment::Commentary);
        assert!(ok_plain && ok_talk, "both converge: {:?}", talk.convergence(&th));
        assert!(
            talk.window().overlay_voice_frames > 3_000,
            "{}",
            talk.window().overlay_voice_frames
        );
        let curve = |r: &LearnRecord| {
            crate::learn::derive::derive(&r.window(), &Limits::default(), r.goal).gains
        };
        let (a, b) = (curve(&plain), curve(&talk));
        for k in 0..crate::learn::NBANDS {
            let hz = crate::learn::BANDS_HZ[k];
            if (300.0..=4000.0).contains(&hz) {
                assert!((a[k] - b[k]).abs() <= 1.0, "{hz} Hz: {} vs {}\n{a:?}\n{b:?}", a[k], b[k]);
            }
        }
    }

    #[test]
    fn convergence_detail_says_what_the_gate_waits_on() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", None);
        assert_eq!(r.convergence(&th).agreeing, 0);
        r.checkpoints = vec![vec![0.0; 3], vec![0.2; 3], vec![2.0; 3]];
        let c = r.convergence(&th);
        assert_eq!((c.agreeing, c.needed), (1, 2));
        assert!((c.max_delta_db - 1.8).abs() < 1e-6);
    }

    #[test]
    fn named_thresholds_have_their_documented_values() {
        assert_eq!(CONVERGE_DB, 0.5);
        assert_eq!(CONVERGE_CHECKPOINTS, 2);
        assert_eq!(CHECKPOINT_SECS, 30);
        assert_eq!(MIN_ACTIVE_SECS, 300);
        assert_eq!((MIN_CUES, MIN_MASKERS), (120, 60));
        assert_eq!((MIN_CUES_FLOOR, MIN_MASKERS_FLOOR), (40, 20));
        assert_eq!(MEDIAN_SE_DB, 2.0);
        assert_eq!((SE_BAND_RANGE_DB, CLASS_SHARE_MIN, STATIONARY_SAMPLE_FRAMES), (12.0, 0.1, 10));
        assert_eq!(ETA_MIN_ACTIVE_SECS, 60);
        assert!(MIN_CUES_FLOOR < MIN_CUES && MIN_MASKERS_FLOOR < MIN_MASKERS);
        assert!(CONVERGE_CHECKPOINTS >= 2, "one checkpoint cannot agree with anything");
        assert_eq!(UPDATE_DB, 1.5);
        assert_eq!(SEGMENT_SECS * WINDOW_SEGMENTS as u64, 3600, "a 60-minute window");
        assert!(MIN_CUES > MIN_MASKERS && MIN_MASKERS > 0);
        assert!(UPDATE_DB > CONVERGE_DB, "an update must be more than convergence noise");
    }

    #[test]
    fn not_ready_before_the_evidence() {
        let th = quick();
        let mut r = LearnRecord::new("Game.exe", Some("1.0.0.0"));
        assert_eq!(r.exe, "game.exe");
        let got = Rig::new(1).play(&mut r, 15, &th);
        assert!(got.is_empty(), "{got:?}");
        assert!(r.candidate.is_none());
        let p = r.progress(&th);
        assert!(p > 0 && p < 90, "{p}");
        assert_eq!(status(Some(&r), true, None), LearnStatus::Learning);
        assert_eq!(status(Some(&r), false, None), LearnStatus::Off);
    }

    #[test]
    fn ready_needs_counts_and_convergence_then_applied() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", Some("1.0.0.0"));
        let got = Rig::new(1).play(&mut r, 240, &th);
        let first = got.iter().find(|(_, o)| *o == Outcome::FirstCurve);
        let (at, _) = first.unwrap_or_else(|| panic!("progress {} {got:?}", r.progress(&th)));
        // Two agreeing checkpoints 20 s apart cannot happen before 40 s.
        assert!(*at >= 40, "{at}");
        assert_eq!(r.progress(&th), 100);
        assert_eq!(status(Some(&r), true, None), LearnStatus::Ready);
        let c = r.candidate.clone().unwrap();
        assert_eq!(status(Some(&r), true, Some(&c)), LearnStatus::Applied);
    }

    #[test]
    fn counts_alone_are_not_enough_without_convergence() {
        // An impossible agreement bar: never converges, never offers.
        let th = Thresholds { converge_db: 0.0, ..quick() };
        let mut r = LearnRecord::new("game.exe", None);
        let got = Rig::new(1).play(&mut r, 120, &th);
        assert!(got.is_empty(), "{got:?}");
        assert!(r.enough_evidence(&th));
        let p = r.progress(&th);
        assert!((90..100).contains(&p), "{p}");
    }

    #[test]
    fn the_curve_freezes_and_small_drift_is_not_offered() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", None);
        let mut rig = Rig::new(1);
        rig.play(&mut r, 240, &th);
        let frozen = r.candidate.clone().expect("ready");
        // Keep playing the same game: the window rolls, the curve wobbles a
        // little, nothing new is offered.
        let got = rig.play(&mut r, 300, &th);
        assert!(!got.iter().any(|(_, o)| o.offers()), "{got:?}");
        assert_eq!(r.candidate.as_ref(), Some(&frozen));
    }

    #[test]
    fn an_audio_update_is_offered_once_the_window_rolls() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", None);
        let mut rig = Rig::new(1);
        rig.play(&mut r, 240, &th);
        let before = r.candidate.clone().expect("ready");
        // The game's mix changes: quieter footsteps, explosions far more often.
        rig.s.step = 0.05;
        rig.s.boom_ms = 1500;
        let got = rig.play(&mut r, 400, &th);
        assert!(got.iter().any(|(_, o)| *o == Outcome::UpdateOffered), "{got:?}");
        let after = r.candidate.clone().unwrap();
        assert!(!same_curve(&before, &after));
        // Applied is still the old curve until the user (or auto-apply) takes it.
        assert_eq!(status(Some(&r), true, Some(&before)), LearnStatus::Ready);
    }

    #[test]
    fn a_new_exe_version_flags_relearn_and_keeps_the_old_layer() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", Some("1.0.0.0"));
        Rig::new(1).play(&mut r, 240, &th);
        let old = r.candidate.clone().unwrap();

        // Same version: nothing happens.
        assert!(!r.begin_session(Some("1.0.0.0")));
        // Update.
        assert!(r.begin_session(Some("1.1.0.0")));
        assert!(r.needs_relearn && r.candidate.is_none());
        assert_eq!(r.window().active_frames, 0);
        assert_eq!(r.exe_version.as_deref(), Some("1.1.0.0"));
        // The old layer is still applied; status says relearn.
        assert_eq!(status(Some(&r), true, Some(&old)), LearnStatus::NeedsRelearn);
        assert!(r.progress(&th) < 100);

        // Relearning in the background finishes and offers a curve again.
        let got = Rig::new(2).play(&mut r, 240, &th);
        assert!(got.iter().any(|(_, o)| *o == Outcome::FirstCurve), "{got:?}");
        assert!(!r.needs_relearn);
    }

    #[test]
    fn changing_goal_re_derives_at_once_without_relearning() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", None);
        Rig::new(1).play(&mut r, 240, &th);
        let aware = r.candidate.clone().expect("ready");
        let frames = r.total_active_frames;
        assert!(r.set_goal(Goal::Immersion, &Limits::default()));
        let calm = r.candidate.clone().unwrap();
        // Same aggregates, nothing relearned, a gentler curve, still ready.
        assert_eq!(r.total_active_frames, frames);
        assert_eq!(r.progress(&th), 100);
        let size = |c: &[(f32, f32)]| c.iter().map(|p| p.1.abs()).fold(0f32, f32::max);
        assert!(size(&calm) < size(&aware), "{calm:?} vs {aware:?}");
        // Back again is exactly the Awareness curve of the saved aggregates.
        assert!(r.set_goal(Goal::Awareness, &Limits::default()));
        let want = derive_checked(&r.window(), &Limits::default(), Goal::Awareness).unwrap();
        assert_eq!(r.candidate.as_ref(), Some(&want.curve()));
        assert!(!r.set_goal(Goal::Awareness, &Limits::default()), "same goal: no change");
    }

    #[test]
    fn an_unversioned_record_adopts_the_first_version_seen() {
        let mut r = LearnRecord::new("game.exe", None);
        assert!(!r.begin_session(Some("2.0")));
        assert_eq!(r.exe_version.as_deref(), Some("2.0"));
        // No version (unreadable resource) never resets.
        assert!(!r.begin_session(None));
    }

    #[test]
    fn imported_layer_without_learning_is_applied() {
        assert_eq!(status(None, false, Some(&[(20.0, 0.0), (1000.0, 1.0)])), LearnStatus::Applied);
        assert_eq!(status(None, true, None), LearnStatus::Learning);
        assert_eq!(status(None, false, None), LearnStatus::Off);
    }

    #[test]
    fn an_analysis_error_keeps_the_last_good_curve() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", None);
        Rig::new(1).play(&mut r, 240, &th);
        let good = r.candidate.clone().unwrap();
        // Corrupt the window behind the record's back.
        r.segments.last_mut().unwrap().frame_hist.truncate(3);
        r.total_active_frames += 100_000;
        assert_eq!(r.checkpoint(&th, &Limits::default()), Outcome::Error);
        assert_eq!(r.candidate, Some(good));
        assert!(r.last_error.is_some());
        // And the next session start throws the bad aggregate away.
        r.begin_session(None);
        assert!(r.segments.is_empty());
    }

    #[test]
    fn same_input_same_curve() {
        let th = quick();
        let mut a = LearnRecord::new("game.exe", None);
        let mut b = LearnRecord::new("game.exe", None);
        let ga = Rig::new(9).play(&mut a, 200, &th);
        let gb = Rig::new(9).play(&mut b, 200, &th);
        assert_eq!(ga, gb);
        assert_eq!(a, b);
        assert!(a.candidate.is_some());
    }

    #[test]
    fn the_rolling_window_keeps_only_recent_segments() {
        let th = quick();
        let mut r = LearnRecord::new("game.exe", None);
        Rig::new(1).play(&mut r, 400, &th);
        assert!(r.segments.len() <= th.window_segments);
        assert!(
            r.active_minutes() <= (th.segment_secs * th.window_segments as u64) as f32 / 60.0 + 0.1
        );
        assert!(r.total_active_frames >= 390 * 100);
    }

    #[test]
    fn a_wild_candidate_on_disk_is_guarded_on_load() {
        let mut r = LearnRecord::new("game.exe", None);
        r.candidate = Some(vec![(20.0, 40.0), (1000.0, -40.0), (16000.0, 40.0)]);
        let back = LearnRecord::from_json(&serde_json::to_string(&r).unwrap(), "game.exe").unwrap();
        let c = back.candidate.unwrap();
        assert!(crate::learn::derive::is_guarded(&c));
        assert!(c.iter().all(|p| (-9.0..=6.0).contains(&p.1)), "{c:?}");
    }

    #[test]
    fn record_round_trips_and_a_foreign_one_is_refused() {
        let mut r = LearnRecord::new("game.exe", Some("1"));
        let mut st = Stats::default();
        st.events[0] = 3;
        r.absorb(&st, &quick());
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(LearnRecord::from_json(&json, "GAME.exe"), Some(r.clone()));
        assert_eq!(LearnRecord::from_json(&json, "other.exe"), None);
        assert_eq!(
            LearnRecord::from_json(&json.replace("\"schema\":3", "\"schema\":1"), "game.exe"),
            None
        );
        assert_eq!(LearnRecord::from_json("garbage", "game.exe"), None);
        // Statistics only: no sample data can be in it.
        assert!(!json.contains("samples") && !json.contains("pcm"));
    }

    /// The doc's "typical time to ready", measured: synthetic play at a
    /// realistic event rate (a footstep every 1.5 s, an explosion every 8 s)
    /// with the shipped thresholds. Prints the minutes; asserts a sane range.
    #[test]
    fn typical_time_to_ready_with_shipped_thresholds() {
        let th = Thresholds::default();
        let mut r = LearnRecord::new("game.exe", None);
        let mut rig = Rig::new(3);
        rig.s.step_ms = 1500;
        rig.s.boom_ms = 8000;
        let mut ready_at = None;
        for minute in 0..30u32 {
            let got = rig.play(&mut r, 60, &th);
            if got.iter().any(|(_, o)| *o == Outcome::FirstCurve) {
                ready_at = Some(minute + 1);
                break;
            }
        }
        let m = ready_at.expect("ready within 30 simulated minutes");
        eprintln!("S48 time to ready (synthetic, 40 cues/min, 7.5 maskers/min): {m} min");
        assert!((5..=20).contains(&m), "{m} min");
    }
}
