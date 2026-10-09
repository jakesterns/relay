//! Readiness, convergence and the freeze rule: the learner's state machine.
//!
//! - **Learning** until there is enough evidence (enough gameplay frames,
//!   spread over enough different scenes) *and* the derived look has held
//!   still over several checkpoints.
//! - **Converged** after that. Sampling continues over a rolling window, so
//!   a game update or a new area can move the result.
//! - The look actually in use (`applied`) is frozen: it only follows a new
//!   converged result that differs meaningfully, so the screen never creeps.
//! - A changed game build (exe fingerprint) starts learning over; the
//!   applied look stays in use until the new one converges and differs.

use serde::{Deserialize, Serialize};

use super::analyse::{FrameClass, FrameReport};
use super::derive::{derive_look, realize, Aggregate, LookTargets, PanelCaps, PanelKind};

/// Largest per-axis difference, in applied units, between the newest
/// checkpoint and the others kept. Shown in status so a tester can see which
/// axis keeps the learner from settling.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct CheckpointDelta {
    pub gamma: f32,
    pub shadow_lift: i32,
    pub vibrance: i32,
}

impl CheckpointDelta {
    pub fn between(a: &LookTargets, b: &LookTargets) -> Self {
        let (x, y) = (realize(a, &REFERENCE_PANEL), realize(b, &REFERENCE_PANEL));
        Self {
            gamma: ((x.gamma - y.gamma).abs() * 1000.0).round() / 1000.0,
            shadow_lift: (x.shadow_lift - y.shadow_lift).abs(),
            vibrance: (x.vibrance - y.vibrance).abs(),
        }
    }

    pub fn agrees(&self) -> bool {
        self.gamma <= AGREE_GAMMA + 1e-6
            && self.shadow_lift <= AGREE_SHADOW_LIFT
            && self.vibrance <= AGREE_VIBRANCE
    }

    fn max(self, o: Self) -> Self {
        Self {
            gamma: self.gamma.max(o.gamma),
            shadow_lift: self.shadow_lift.max(o.shadow_lift),
            vibrance: self.vibrance.max(o.vibrance),
        }
    }
}

/// The sampler's rate, live and from a video file (S48: 2 fps, was 1). Every
/// frame budget below is in samples at this rate.
pub const SAMPLE_FPS: u32 = 2;
/// Gameplay frames needed before a result can count (5 min at 2 fps).
pub const MIN_GAMEPLAY_FRAMES: u64 = 600;
/// Frames a scene (APL bucket) needs to count as visited.
pub const MIN_FRAMES_PER_SCENE: f64 = 30.0;
/// Distinct scenes needed: one dark level only would teach the wrong thing.
pub const MIN_SCENES: usize = 3;
/// A game whose frames sit overwhelmingly in one scene bucket (a night-only
/// raid) is legitimately one-scene: it may converge without `MIN_SCENES`
/// once this share of its weight is in one bucket ...
pub const DOMINANT_SCENE_SHARE: f64 = 0.9;
/// ... and it has this many gameplay frames (7.5 min at 2 fps: more than the
/// varied case, since one scene is less evidence per frame) ...
pub const MIN_FRAMES_ONE_SCENE: u64 = 900;
/// ... or only this many (4 min) when its statistics are already tight: the
/// standard error of every look axis is under [`CONFIDENT_SE`] (S48).
pub const MIN_FRAMES_ONE_SCENE_CONFIDENT: u64 = 480;
/// A look axis is known when its standard error is under a quarter of the
/// rounding step (`LOOK_STEP` = 0.05): more frames could not move the
/// rounded value.
pub const CONFIDENT_SE: f32 = 0.0125;
/// Samples this far apart count as independent for the standard error (2 s
/// at 2 fps): neighbouring frames of one scene say nearly the same thing.
pub const DECORRELATION_FRAMES: f64 = 4.0;
/// Gameplay frames between checkpoints (30 s at 2 fps).
pub const CHECKPOINT_FRAMES: u32 = 60;
/// Floor on the gameplay share the ETA divides by, so a menu-heavy start does
/// not show hours.
pub const MIN_GAMEPLAY_SHARE: f64 = 0.25;
/// Consecutive checkpoints that must agree for convergence (S48: 2, was 3).
pub const CONVERGE_CHECKPOINTS: usize = 2;
/// Kept for the look-space distance used by the freeze rule's tests; the
/// convergence test itself is on realized adjustments (below).
pub const CONVERGE_TOLERANCE: f32 = 0.05;
/// Checkpoints agree when what would actually be applied differs by no more
/// than this per axis, on the reference panel ([`REFERENCE_PANEL`], the one
/// with the widest ranges, so agreement there holds on every panel):
/// gamma multiplier ...
pub const AGREE_GAMMA: f32 = 0.02;
/// ... ramp shadow lift (profile units, 0..=100) ...
pub const AGREE_SHADOW_LIFT: i32 = 3;
/// ... and vibrance points.
pub const AGREE_VIBRANCE: i32 = 2;
/// Panel the agreement is judged on: an LCD without a verified black
/// equalizer uses every learned control at its full range.
pub const REFERENCE_PANEL: PanelCaps =
    PanelCaps { kind: PanelKind::Ips, black_equalizer_max: None };
/// Rolling window after convergence (~1 h of gameplay at 2 fps).
pub const ROLLING_WINDOW_FRAMES: f64 = 7200.0;
/// The applied look only moves when a new converged result is at least this
/// far from it on some axis.
pub const MEANINGFUL_CHANGE: f32 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Learning,
    Converged,
}

/// Counts of frames that were seen but not learned from, for the UI's
/// "why so slow" line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Excluded {
    pub static_frames: u64,
    pub loading: u64,
    pub cutscene: u64,
    pub outlier: u64,
    #[serde(default)]
    pub idle: u64,
    /// First sample of a session (no motion reference yet).
    #[serde(default)]
    pub warmup: u64,
}

impl Excluded {
    pub fn total(&self) -> u64 {
        self.static_frames + self.loading + self.cutscene + self.outlier + self.idle + self.warmup
    }

    pub fn add(&mut self, o: &Excluded) {
        self.static_frames += o.static_frames;
        self.loading += o.loading;
        self.cutscene += o.cutscene;
        self.outlier += o.outlier;
        self.idle += o.idle;
        self.warmup += o.warmup;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Learner {
    /// Game build fingerprint the evidence belongs to.
    #[serde(default)]
    pub build: String,
    pub agg: Aggregate,
    #[serde(default)]
    pub excluded: Excluded,
    /// Most recent checkpoints, newest last, at most `CONVERGE_CHECKPOINTS`.
    #[serde(default)]
    pub checkpoints: Vec<LookTargets>,
    #[serde(default)]
    pub since_checkpoint: u32,
    #[serde(default)]
    pub converged: Option<LookTargets>,
    /// The look in use while the user has chosen Apply; frozen (see module).
    #[serde(default)]
    pub applied: Option<LookTargets>,
    /// The user pressed Apply: follow convergence (under the freeze rule).
    #[serde(default)]
    pub use_learned: bool,
}

/// How far along the evidence is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Readiness {
    pub frames: u64,
    pub frames_needed: u64,
    pub scenes: usize,
    pub scenes_needed: usize,
    pub stable_checkpoints: usize,
    pub checkpoints_needed: usize,
    /// Checkpoints taken so far (kept: at most `CONVERGE_CHECKPOINTS`).
    #[serde(default)]
    pub checkpoints: usize,
    /// Weighted gameplay frames per APL bucket (dark → bright), so a tester
    /// can see which scenes are missing.
    #[serde(default)]
    pub scene_frames: Vec<u64>,
    /// Largest disagreement among the kept checkpoints, per applied axis.
    #[serde(default)]
    pub delta: Option<CheckpointDelta>,
    /// The newest checkpoint's look, before convergence too, so a tester can
    /// see what is being derived.
    #[serde(default)]
    pub candidate: Option<LookTargets>,
    /// 0..=1 overall, for the progress bar.
    pub progress: f32,
    /// Seconds still needed at [`SAMPLE_FPS`] (S48), in wall-clock time at
    /// the share of sampled frames that has been gameplay so far. `Some(0)`
    /// once settled; `None` while a needed scene has not been seen at all,
    /// which no amount of the same scene can supply. A scene seen but short
    /// of [`MIN_FRAMES_PER_SCENE`] is estimated at its rate so far (r58: the
    /// ETA was blank for all of a varied Warzone run).
    #[serde(default)]
    pub eta_secs: Option<u32>,
    /// The one-scene budget was cut short because the statistics are tight.
    #[serde(default)]
    pub confident: bool,
}

impl Learner {
    pub fn new(build: impl Into<String>) -> Self {
        Self { build: build.into(), ..Self::default() }
    }

    pub fn phase(&self) -> Phase {
        if self.converged.is_some() {
            Phase::Converged
        } else {
            Phase::Learning
        }
    }

    pub fn scenes(&self) -> usize {
        self.agg.apl_buckets.iter().filter(|w| **w >= MIN_FRAMES_PER_SCENE).count()
    }

    /// The content is overwhelmingly one scene (see [`DOMINANT_SCENE_SHARE`]).
    pub fn single_scene(&self) -> bool {
        let total: f64 = self.agg.apl_buckets.iter().sum();
        let top = self.agg.apl_buckets.iter().cloned().fold(0.0, f64::max);
        total > 0.0 && top / total >= DOMINANT_SCENE_SHARE
    }

    fn scenes_needed(&self) -> usize {
        if self.single_scene() {
            1
        } else {
            MIN_SCENES
        }
    }

    /// Every look axis is known within [`CONFIDENT_SE`]. Unknown (an older
    /// record without the squared sums, or too little weight) is not
    /// confident.
    pub fn confident(&self) -> bool {
        let Some(se) = self.agg.standard_errors(DECORRELATION_FRAMES) else { return false };
        se.iter().all(|s| *s <= CONFIDENT_SE)
    }

    fn frames_needed(&self) -> u64 {
        if self.single_scene() {
            if self.confident() {
                MIN_FRAMES_ONE_SCENE_CONFIDENT
            } else {
                MIN_FRAMES_ONE_SCENE
            }
        } else {
            MIN_GAMEPLAY_FRAMES
        }
    }

    /// Seconds of gameplay left, see [`Readiness::eta_secs`].
    pub fn eta_secs(&self) -> Option<u32> {
        if self.converged.is_some() {
            return Some(0);
        }
        let scene_frames = self.scene_frames_left()?;
        let frames_left = self.frames_needed().saturating_sub(self.agg.frames);
        let stable = if self.has_evidence() { self.stable_checkpoints() } else { 0 };
        // The checkpoint under way, plus one per agreeing checkpoint still
        // missing after it.
        let checks = CONVERGE_CHECKPOINTS.saturating_sub(stable).max(1) as u64;
        let settle = checks * CHECKPOINT_FRAMES as u64
            - (self.since_checkpoint as u64).min(CHECKPOINT_FRAMES as u64);
        let frames = frames_left.max(settle).max(scene_frames);
        // Gameplay frames come at the rate gameplay has been sampled: static,
        // loading and idle frames are not learned from (r58: Tarkov's ETA
        // said 42 s at 79 % with 400 static frames excluded).
        let seen = self.agg.frames + self.excluded.total();
        let share = if seen == 0 {
            1.0
        } else {
            (self.agg.frames as f64 / seen as f64).clamp(MIN_GAMEPLAY_SHARE, 1.0)
        };
        Some((frames as f64 / share / SAMPLE_FPS as f64).ceil() as u32)
    }

    /// Gameplay frames until enough scenes have [`MIN_FRAMES_PER_SCENE`],
    /// each still-short scene filling at its share of the gameplay so far.
    /// `None` when fewer scenes than needed have been seen at all.
    fn scene_frames_left(&self) -> Option<u64> {
        let missing = self.scenes_needed().saturating_sub(self.scenes());
        if missing == 0 {
            return Some(0);
        }
        let total: f64 = self.agg.apl_buckets.iter().sum();
        let mut left: Vec<f64> = self
            .agg
            .apl_buckets
            .iter()
            .filter(|w| **w > 0.0 && **w < MIN_FRAMES_PER_SCENE)
            .map(|w| (MIN_FRAMES_PER_SCENE - w) * total / w)
            .collect();
        if left.len() < missing {
            return None;
        }
        left.sort_by(f64::total_cmp);
        Some(left[missing - 1].ceil() as u64)
    }

    /// S48: fold `other` (a learner fed from a video file of the same game)
    /// into this one. The aggregates add, weighted by their frames, inside
    /// the rolling window; exclusions add. With no evidence of its own this
    /// learner takes the other's checkpoints, so a file that settled is
    /// settled here too. Otherwise one checkpoint of the merged aggregate is
    /// taken now and must agree with the ones before it, under the same
    /// rules: live play refines a file-learned start, and a file never
    /// overrides live evidence without agreeing with it. What is applied
    /// follows the freeze rule as always.
    pub fn merge(&mut self, other: &Learner) {
        if other.agg.frames == 0 {
            self.excluded.add(&other.excluded);
            return;
        }
        if self.agg.frames == 0 {
            self.checkpoints = other.checkpoints.clone();
            self.since_checkpoint = other.since_checkpoint;
        }
        self.agg.merge(&other.agg, ROLLING_WINDOW_FRAMES);
        self.excluded.add(&other.excluded);
        self.checkpoint();
    }

    /// Varied content needs `MIN_SCENES`; content that really is one scene
    /// needs more frames instead.
    fn has_evidence(&self) -> bool {
        self.agg.frames >= self.frames_needed() && self.scenes() >= self.scenes_needed()
    }

    fn stable_checkpoints(&self) -> usize {
        let Some(last) = self.checkpoints.last() else { return 0 };
        self.checkpoints
            .iter()
            .rev()
            .take_while(|c| CheckpointDelta::between(c, last).agrees())
            .count()
    }

    /// Max per-axis delta between the newest checkpoint and the others kept.
    pub fn checkpoint_delta(&self) -> Option<CheckpointDelta> {
        let last = self.checkpoints.last()?;
        Some(
            self.checkpoints
                .iter()
                .map(|c| CheckpointDelta::between(c, last))
                .fold(CheckpointDelta::default(), CheckpointDelta::max),
        )
    }

    pub fn readiness(&self) -> Readiness {
        let frames = self.agg.frames;
        let scenes = self.scenes();
        // Reported whether or not there is evidence yet: "0 stable" while
        // checkpoints agree read as stuck on PC2. Convergence still needs both.
        let stable = self.stable_checkpoints();
        let ev = 0.5 * (frames as f32 / self.frames_needed() as f32).min(1.0)
            + 0.3 * (scenes as f32 / self.scenes_needed() as f32).min(1.0);
        let conv = if self.converged.is_some() {
            0.2
        } else {
            let s = if self.has_evidence() { stable } else { 0 };
            0.2 * (s as f32 / CONVERGE_CHECKPOINTS as f32).min(1.0)
        };
        Readiness {
            frames,
            frames_needed: self.frames_needed(),
            scenes,
            scenes_needed: self.scenes_needed(),
            candidate: self.checkpoints.last().copied(),
            stable_checkpoints: stable,
            checkpoints_needed: CONVERGE_CHECKPOINTS,
            checkpoints: self.checkpoints.len(),
            scene_frames: self.agg.apl_buckets.iter().map(|w| w.round() as u64).collect(),
            delta: self.checkpoint_delta(),
            progress: (ev + conv).min(1.0),
            eta_secs: self.eta_secs(),
            confident: self.single_scene() && self.confident(),
        }
    }

    /// A different game build: the evidence is for another game now.
    /// Returns true when it reset.
    pub fn check_build(&mut self, build: &str) -> bool {
        if self.build == build || build.is_empty() {
            return false;
        }
        // Evidence gathered while the build was unknown (the image path was
        // not readable then) belongs to this build: adopt it, don't relearn.
        // This is what turned a 15 s Alt-Tab into "learning afresh" on PC2.
        if self.build.is_empty() {
            self.build = build.to_string();
            return false;
        }
        self.build = build.to_string();
        self.relearn();
        true
    }

    /// Drop the evidence; keep what is applied until a new result replaces it.
    pub fn relearn(&mut self) {
        self.agg = Aggregate::default();
        self.excluded = Excluded::default();
        self.checkpoints.clear();
        self.since_checkpoint = 0;
        self.converged = None;
    }

    /// Back to nothing: no evidence, nothing applied.
    pub fn reset(&mut self) {
        self.relearn();
        self.applied = None;
        self.use_learned = false;
    }

    /// The user pressed Apply.
    pub fn apply(&mut self) -> bool {
        match self.converged {
            Some(c) => {
                self.use_learned = true;
                self.applied = Some(c);
                true
            }
            None => false,
        }
    }

    /// Feed one analysed frame. Returns true when a checkpoint was taken
    /// (the caller persists then, not on every frame).
    pub fn observe(&mut self, r: &FrameReport) -> bool {
        match r.class {
            FrameClass::Gameplay => {}
            FrameClass::Static => {
                self.excluded.static_frames += 1;
                return false;
            }
            FrameClass::Loading => {
                self.excluded.loading += 1;
                return false;
            }
            FrameClass::Cutscene => {
                self.excluded.cutscene += 1;
                return false;
            }
            FrameClass::Outlier => {
                self.excluded.outlier += 1;
                return false;
            }
            FrameClass::Idle => {
                self.excluded.idle += 1;
                return false;
            }
            FrameClass::Warmup => {
                self.excluded.warmup += 1;
                return false;
            }
        }
        // Clean frames only: a NaN from a broken frame must not poison sums.
        let s = &r.stats;
        if ![s.mean_luma, s.crush_frac, s.clip_frac, s.sat_mean, s.sat_p90, s.crushed_detail]
            .iter()
            .all(|v| v.is_finite())
        {
            self.excluded.outlier += 1;
            return false;
        }
        self.agg.add(s, ROLLING_WINDOW_FRAMES);
        self.since_checkpoint += 1;
        if self.since_checkpoint < CHECKPOINT_FRAMES {
            return false;
        }
        self.since_checkpoint = 0;
        self.checkpoint();
        true
    }

    /// The checkpoint look is derived from the whole (rolling) aggregate,
    /// never from the last interval alone: minute-to-minute swings in real
    /// gameplay must not decide convergence.
    fn checkpoint(&mut self) {
        let look = derive_look(&self.agg.summary());
        self.checkpoints.push(look);
        if self.checkpoints.len() > CONVERGE_CHECKPOINTS {
            self.checkpoints.remove(0);
        }
        if self.has_evidence() && self.stable_checkpoints() >= CONVERGE_CHECKPOINTS {
            self.converged = Some(look);
            if self.use_learned {
                match self.applied {
                    Some(a) if a.distance(&look) < MEANINGFUL_CHANGE => {}
                    _ => self.applied = Some(look),
                }
            }
        }
    }
}
