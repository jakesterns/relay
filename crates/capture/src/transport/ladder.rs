//! The resolution / frame-rate ladder (S49): 4K60 → 1440p60 → 1080p60 →
//! 1080p30 when the link cannot carry the top rung, and back up when it can.
//!
//! At a low bitrate a smaller picture looks better than a starved large one:
//! 4K60 at 15 Mb/s is a smear of blocks, 1440p60 at 15 Mb/s is clean. The
//! bitrate controller decides how much the link carries; the ladder decides
//! what to spend it on. Pure: targets and times in, rung changes out.
//!
//! Damped harder than the bitrate, because each step costs a keyframe and an
//! encoder rebuild the viewer can see:
//!
//! * down only after the target has sat below the rung's floor for
//!   [`DOWN_HOLD`], and only when the controller has actually backed off (a
//!   user who *asked* for 4K60 at 20 Mb/s on a wired LAN is never stepped
//!   down — that is their choice, not congestion);
//! * up only after the target has had [`UP_MARGIN`] headroom over the upper
//!   rung's floor for [`UP_HOLD`];
//! * a step up that is undone within [`FLAP_WINDOW`] doubles the wait before
//!   the next try, to [`UP_BACKOFF_MAX`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::codec::VideoCodec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rung {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Rung {
    pub fn label(&self) -> String {
        format!("{}p{}", self.height, self.fps)
    }

    pub fn pixel_rate(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * u64::from(self.fps)
    }
}

/// Bits per pixel below which a rung looks starved. HEVC needs about two
/// thirds of what H.264 does for the same picture.
fn bpp(codec: VideoCodec) -> f64 {
    match codec {
        VideoCodec::Hevc => 0.045,
        VideoCodec::H264 => 0.065,
    }
}

pub const DOWN_HOLD: Duration = Duration::from_secs(3);
/// The hold when the target is under half the rung's floor.
pub const DOWN_HOLD_DEEP: Duration = Duration::from_millis(500);
pub const UP_HOLD: Duration = Duration::from_secs(15);
pub const UP_MARGIN: f64 = 1.4;
const UP_BACKOFF: Duration = Duration::from_secs(30);
pub const UP_BACKOFF_MAX: Duration = Duration::from_secs(240);
pub const FLAP_WINDOW: Duration = Duration::from_secs(60);
/// The controller has backed off when the target is under this share of
/// the ceiling.
const BACKED_OFF: f64 = 0.9;

pub struct Ladder {
    rungs: Vec<Rung>,
    floors: Vec<u32>,
    idx: usize,
    below_for: Duration,
    above_for: Duration,
    since_change: Duration,
    /// Time since the last step up, while a flap could still undo it.
    since_up: Option<Duration>,
    up_backoff: Duration,
}

impl Ladder {
    /// The rungs for a share configured at `top` (its encode size and fps):
    /// `top` itself, then each standard height below it at the same aspect,
    /// then the lowest at half rate. Never above what the user asked for.
    pub fn new(top: Rung, codec: VideoCodec) -> Self {
        let mut rungs = vec![top];
        for h in [1440u32, 1080] {
            if h < top.height {
                let w = ((u64::from(top.width) * u64::from(h) / u64::from(top.height.max(1)))
                    as u32
                    + 1)
                    & !1;
                rungs.push(Rung { width: w, height: h, fps: top.fps });
            }
        }
        if top.fps >= 50 {
            let last = *rungs.last().expect("non-empty");
            rungs.push(Rung { fps: top.fps / 2, ..last });
        }
        let floors = rungs.iter().map(|r| (r.pixel_rate() as f64 * bpp(codec)) as u32).collect();
        Self {
            rungs,
            floors,
            idx: 0,
            below_for: Duration::ZERO,
            above_for: Duration::ZERO,
            since_change: Duration::MAX / 4,
            since_up: None,
            up_backoff: UP_BACKOFF,
        }
    }

    pub fn rungs(&self) -> &[Rung] {
        &self.rungs
    }

    pub fn current(&self) -> Rung {
        self.rungs[self.idx]
    }

    pub fn index(&self) -> usize {
        self.idx
    }

    /// Bitrate below which `rung` looks starved.
    pub fn floor_of(&self, i: usize) -> u32 {
        self.floors[i]
    }

    /// Advance by `dt` with the controller at `target` of `ceiling`. Returns
    /// the new rung when it changes.
    pub fn on_tick(&mut self, dt: Duration, target: u32, ceiling: u32) -> Option<Rung> {
        self.since_change = self.since_change.saturating_add(dt);
        if let Some(t) = self.since_up.as_mut() {
            *t += dt;
            if *t >= FLAP_WINDOW * 2 {
                // Held the upper rung for two minutes: trust the link again.
                self.since_up = None;
                self.up_backoff = UP_BACKOFF;
            }
        }
        let backed_off = f64::from(target) < f64::from(ceiling) * BACKED_OFF;
        let starved = backed_off && target < self.floors[self.idx];
        self.below_for = if starved { self.below_for + dt } else { Duration::ZERO };
        let roomy =
            self.idx > 0 && f64::from(target) >= f64::from(self.floors[self.idx - 1]) * UP_MARGIN;
        self.above_for = if roomy { self.above_for + dt } else { Duration::ZERO };

        // Starved to under half the rung's floor, the encoder cannot honour the
        // target at this size at all (NVENC at 4K on the S49 model would not go
        // under ~28 Mb/s with 2.5 asked), and waiting the full hold is a freeze:
        // step after half a second instead.
        let deep = target < self.floors[self.idx] / 2;
        let hold = if deep { DOWN_HOLD_DEEP } else { DOWN_HOLD };
        if self.below_for >= hold && self.idx + 1 < self.rungs.len() {
            self.idx += 1;
            if self.since_up.is_some_and(|t| t < FLAP_WINDOW) {
                self.up_backoff = (self.up_backoff * 2).min(UP_BACKOFF_MAX);
            }
            self.since_up = None;
            return Some(self.changed());
        }
        if self.above_for >= UP_HOLD && self.since_change >= self.up_backoff {
            self.idx -= 1;
            self.since_up = Some(Duration::ZERO);
            return Some(self.changed());
        }
        None
    }

    fn changed(&mut self) -> Rung {
        self.below_for = Duration::ZERO;
        self.above_for = Duration::ZERO;
        self.since_change = Duration::ZERO;
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UHD: Rung = Rung { width: 3840, height: 2160, fps: 60 };
    const TICK: Duration = Duration::from_millis(250);

    fn run(l: &mut Ladder, secs: f64, target: u32, ceiling: u32) -> Vec<Rung> {
        let n = (secs / TICK.as_secs_f64()).round() as usize;
        (0..n).filter_map(|_| l.on_tick(TICK, target, ceiling)).collect()
    }

    #[test]
    fn rungs_follow_the_brief_and_never_exceed_the_request() {
        let l = Ladder::new(UHD, VideoCodec::Hevc);
        let labels: Vec<String> = l.rungs().iter().map(Rung::label).collect();
        assert_eq!(labels, ["2160p60", "1440p60", "1080p60", "1080p30"]);
        assert_eq!(l.rungs()[1], Rung { width: 2560, height: 1440, fps: 60 });
        let l = Ladder::new(Rung { width: 2560, height: 1440, fps: 60 }, VideoCodec::H264);
        assert_eq!(l.rungs().len(), 3);
        let l = Ladder::new(Rung { width: 1920, height: 1080, fps: 30 }, VideoCodec::H264);
        assert_eq!(l.rungs().len(), 1, "1080p30 has nowhere to go");
        // An ultrawide keeps its shape, with even dimensions.
        let l = Ladder::new(Rung { width: 3440, height: 1440, fps: 60 }, VideoCodec::Hevc);
        assert_eq!(l.rungs()[1], Rung { width: 2580, height: 1080, fps: 60 });
        // Floors fall with the pixel rate.
        let f: Vec<u32> = (0..4).map(|i| Ladder::new(UHD, VideoCodec::Hevc).floor_of(i)).collect();
        assert!(f.windows(2).all(|w| w[0] > w[1]), "{f:?}");
        assert_eq!(f[0], 22_394_880);
    }

    #[test]
    fn a_starved_top_rung_steps_down_after_the_hold() {
        let mut l = Ladder::new(UHD, VideoCodec::Hevc);
        assert!(run(&mut l, 2.5, 18_000_000, 60_000_000).is_empty(), "not yet");
        let steps = run(&mut l, 1.0, 18_000_000, 60_000_000);
        assert_eq!(steps, [Rung { width: 2560, height: 1440, fps: 60 }]);
        // 18 Mb/s is plenty for 1440p60 (floor ~10): it stays.
        assert!(run(&mut l, 60.0, 18_000_000, 60_000_000).is_empty());
    }

    #[test]
    fn a_very_weak_link_walks_down_one_rung_at_a_time_to_1080p30() {
        let mut l = Ladder::new(UHD, VideoCodec::Hevc);
        // Far under every floor: half a second per rung, not three.
        let steps = run(&mut l, 2.0, 2_500_000, 60_000_000);
        let labels: Vec<String> = steps.iter().map(Rung::label).collect();
        assert_eq!(labels, ["1440p60", "1080p60", "1080p30"]);
    }

    #[test]
    fn a_user_who_asked_for_a_low_rate_is_not_stepped_down() {
        // 4K60 at 20 Mb/s by choice, wired, never backed off.
        let mut l = Ladder::new(UHD, VideoCodec::Hevc);
        assert!(run(&mut l, 120.0, 20_000_000, 20_000_000).is_empty());
    }

    #[test]
    fn it_steps_back_up_when_the_link_recovers_and_not_before() {
        let mut l = Ladder::new(UHD, VideoCodec::Hevc);
        run(&mut l, 4.0, 15_000_000, 60_000_000);
        assert_eq!(l.index(), 1);
        // Enough for 4K60 but not with margin: stays.
        assert!(run(&mut l, 60.0, 28_000_000, 60_000_000).is_empty());
        // Plenty: back up after the hold.
        let steps = run(&mut l, 14.0, 50_000_000, 60_000_000);
        assert!(steps.is_empty());
        let steps = run(&mut l, 2.0, 50_000_000, 60_000_000);
        assert_eq!(steps, [UHD]);
    }

    #[test]
    fn flapping_doubles_the_wait_before_the_next_step_up() {
        let mut l = Ladder::new(UHD, VideoCodec::Hevc);
        let mut ups = Vec::new();
        let mut t = 0.0;
        // A link that holds 50 Mb/s for 20 s, then collapses for 5 s: every
        // step up would be undone.
        while t < 600.0 {
            let target = if (t % 25.0) < 20.0 { 50_000_000 } else { 10_000_000 };
            let before = l.index();
            if l.on_tick(TICK, target, 60_000_000).is_some() && l.index() < before {
                ups.push(t);
            }
            t += TICK.as_secs_f64();
        }
        let gaps: Vec<f64> = ups.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(ups.len() <= 6, "{} step-ups in 10 minutes: {ups:?}", ups.len());
        assert!(gaps.last().copied().unwrap_or(999.0) >= 100.0, "backed off: {gaps:?}");
    }
}
