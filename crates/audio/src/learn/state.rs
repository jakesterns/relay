//! Readiness, convergence and the per-game learning record.
//!
//! A [`LearnRecord`] is what persists per game exe between sessions: the
//! aggregates ([`Stats`]) of the last ~60 minutes of active play, the exe's
//! file version, the last few checkpoint curves, and the candidate curve
//! once there is enough evidence. Never audio.
//!
//! **Ready is evidence, not a timer.** A curve is offered only when both hold
//! ([`Thresholds`]):
//! - the rolling window holds at least [`MIN_CUES`] cue events and
//!   [`MIN_MASKERS`] masker events;
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

use super::analyzer::{Stats, FRAME_MS};
use super::derive::{derive_checked, max_delta, Goal, Limits};

/// Schema of the on-disk record. A different schema is discarded and
/// relearned (it holds statistics only, so nothing of the user's is lost).
pub const RECORD_SCHEMA: u32 = 3;
/// Target events the window must hold (for Awareness: footsteps, foliage,
/// reloads; for Dialogue: half-second chunks of game voice).
pub const MIN_CUES: u64 = 120;
/// Masker events (gunshots, explosions, and half-second chunks of vehicles
/// and music) the window must hold.
pub const MIN_MASKERS: u64 = 60;
/// Active-play seconds between checkpoints.
pub const CHECKPOINT_SECS: u64 = 60;
/// And at least this much active play, so a short burst of action cannot
/// count as knowing the game. With the counts and convergence this lands
/// typical games at 10-20 min (r51: ~25 targets/min, 30 maskers/min).
pub const MIN_ACTIVE_SECS: u64 = 10 * 60;
/// Converged: the last checkpoints agree within this in every band, dB.
pub const CONVERGE_DB: f32 = 0.5;
/// ...over this many checkpoints.
pub const CONVERGE_CHECKPOINTS: usize = 3;
/// A converged curve replaces the frozen one only past this difference, dB.
pub const UPDATE_DB: f32 = 1.5;
/// Rolling window: segments of this many active seconds...
pub const SEGMENT_SECS: u64 = 600;
/// ...this many of them (60 min).
pub const WINDOW_SEGMENTS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub min_cues: u64,
    pub min_maskers: u64,
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

    /// Whether the window holds enough target and masker events for the goal.
    pub fn enough_evidence(&self, th: &Thresholds) -> bool {
        let w = self.window();
        let (t, m) = self.goal.evidence(&w);
        t >= th.min_cues && m >= th.min_maskers && w.active_secs() >= th.min_active_secs
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
        let evidence = frac(t, th.min_cues)
            .min(frac(m, th.min_maskers))
            .min(frac(w.active_secs(), th.min_active_secs));
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

    /// r51 on PC2 (Warzone, no commentary): ~10 cues/min, voice callouts
    /// while playing, plenty of gunfire and explosions. Ready in 10-20 min.
    #[test]
    fn r51_like_play_is_ready_in_ten_to_twenty_minutes() {
        let th = Thresholds::default();
        let mut r = LearnRecord::new("game.exe", None);
        let mut a = Analyzer::new(FS);
        let mut s = Synth::new(FS, 51);
        s.step = 0.12;
        s.step_ms = 6000; // ~10 footsteps / min
        s.boom_ms = 6000;
        let mut ready_at = None;
        'outer: for minute in 0..25u32 {
            for (seg, secs) in
                [(Segment::Gameplay, 50u32), (Segment::Speech, 6), (Segment::Gunfire, 4)]
            {
                for _ in 0..secs {
                    s.render(seg, 1.0, |b| a.push(b));
                    r.absorb(&a.take_stats(), &th);
                    if r.checkpoint(&th, &Limits::default()) == Outcome::FirstCurve {
                        ready_at = Some(minute + 1);
                        break 'outer;
                    }
                }
            }
        }
        let m = ready_at.unwrap_or_else(|| {
            panic!("not ready in 25 min: {}% {:?}", r.progress(&th), r.window().events)
        });
        eprintln!("S46 r51-like time to ready: {m} min");
        assert!((10..=20).contains(&m), "{m} min");
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
        assert_eq!((c.agreeing, c.needed), (1, 3));
        assert!((c.max_delta_db - 2.0).abs() < 1e-6);
    }

    #[test]
    fn named_thresholds_have_their_documented_values() {
        assert_eq!(CONVERGE_DB, 0.5);
        assert_eq!(CONVERGE_CHECKPOINTS, 3);
        assert_eq!(CHECKPOINT_SECS, 60);
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
        // Three agreeing checkpoints 20 s apart cannot happen before 60 s.
        assert!(*at >= 60, "{at}");
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
        eprintln!("S46 time to ready (synthetic, 40 cues/min, 7.5 maskers/min): {m} min");
        assert!((5..=20).contains(&m), "{m} min");
    }
}
