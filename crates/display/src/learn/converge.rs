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

/// Gameplay frames needed before a result can count (10 min at 1 fps).
pub const MIN_GAMEPLAY_FRAMES: u64 = 600;
/// Frames a scene (APL bucket) needs to count as visited.
pub const MIN_FRAMES_PER_SCENE: f64 = 30.0;
/// Distinct scenes needed: one dark level only would teach the wrong thing.
pub const MIN_SCENES: usize = 3;
/// Gameplay frames between checkpoints (2 min at 1 fps).
pub const CHECKPOINT_FRAMES: u32 = 120;
/// Consecutive checkpoints that must agree for convergence.
pub const CONVERGE_CHECKPOINTS: usize = 3;
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
/// Rolling window after convergence (~1 h of gameplay at 1 fps).
pub const ROLLING_WINDOW_FRAMES: f64 = 3600.0;
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
    /// 0..=1 overall, for the progress bar.
    pub progress: f32,
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

    fn has_evidence(&self) -> bool {
        self.agg.frames >= MIN_GAMEPLAY_FRAMES && self.scenes() >= MIN_SCENES
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
        let ev = 0.5 * (frames as f32 / MIN_GAMEPLAY_FRAMES as f32).min(1.0)
            + 0.3 * (scenes as f32 / MIN_SCENES as f32).min(1.0);
        let conv = if self.converged.is_some() {
            0.2
        } else {
            let s = if self.has_evidence() { stable } else { 0 };
            0.2 * (s as f32 / CONVERGE_CHECKPOINTS as f32).min(1.0)
        };
        Readiness {
            frames,
            frames_needed: MIN_GAMEPLAY_FRAMES,
            scenes,
            scenes_needed: MIN_SCENES,
            stable_checkpoints: stable,
            checkpoints_needed: CONVERGE_CHECKPOINTS,
            checkpoints: self.checkpoints.len(),
            scene_frames: self.agg.apl_buckets.iter().map(|w| w.round() as u64).collect(),
            delta: self.checkpoint_delta(),
            progress: (ev + conv).min(1.0),
        }
    }

    /// A different game build: the evidence is for another game now.
    /// Returns true when it reset.
    pub fn check_build(&mut self, build: &str) -> bool {
        if self.build == build {
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
