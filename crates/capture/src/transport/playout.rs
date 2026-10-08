//! Receiver playout buffer (S49): hold each frame just long enough that the
//! link's jitter does not reach the screen.
//!
//! Before S49 a frame was decoded and shown the moment its last packet
//! arrived. On a wired LAN that is ideal — arrival jitter is a millisecond —
//! and it stays that way: the buffer only engages when the jitter it measures
//! is worth hiding ([`ENGAGE_MS`]). On Wi-Fi, frames arrive bunched (a 30 ms
//! hold releases two frames together), which on screen is judder: one frame
//! shown twice as long, the next for no time at all. Holding every frame by
//! the jitter's 90th percentile turns that back into an even cadence, for a
//! bounded cost in latency ([`MAX_DELAY_MS`]).
//!
//! What it does not try to hide: a 300 ms stall. A buffer deep enough for
//! that would add 300 ms to every frame, and the brief's latency target is
//! 50 ms. A stall shows as a pause, then the stream resumes at once.
//!
//! Pure: times in, release times out. The same struct drives the real
//! render loop and the headless receiver's smoothness measurement.

use std::collections::VecDeque;

/// Never hold a frame longer than this beyond its fastest transit.
pub const MAX_DELAY_MS: f64 = 50.0;
/// Below this much measured jitter the buffer stays off (target 0): wired.
pub const ENGAGE_MS: f64 = 4.0;
/// Frames the jitter percentile is taken over: two seconds at 60 fps.
const RECENT: usize = 120;
/// How long the fastest transit is remembered: the base the delay is
/// measured from. Long enough to span a capacity dip, short enough to
/// follow the clock offset as it is re-measured.
const BASE_WINDOW_MS: f64 = 10_000.0;
/// The target falls this much per frame at most: shrinking the buffer
/// shows frames early, so it is done gradually rather than in one skip.
const SHRINK_PER_FRAME_MS: f64 = 0.25;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PlayoutStats {
    pub frames: u64,
    /// Frames that arrived after the time the buffer would have shown them.
    pub late: u64,
    pub target_ms: f64,
}

pub struct Playout {
    /// (arrival, transit) for the sliding minimum, increasing transit.
    base: VecDeque<(f64, f64)>,
    recent: VecDeque<f64>,
    target_ms: f64,
    last_release: f64,
    pub stats: PlayoutStats,
}

impl Playout {
    pub fn new() -> Self {
        Self {
            base: VecDeque::new(),
            recent: VecDeque::with_capacity(RECENT + 1),
            target_ms: 0.0,
            last_release: f64::MIN,
            stats: PlayoutStats::default(),
        }
    }

    pub fn target_ms(&self) -> f64 {
        self.target_ms
    }

    /// A frame captured at `capture_ms` (sender clock, mapped to ours) whose
    /// last packet arrived at `arrival_ms`. Returns when to show it, never
    /// earlier than `arrival_ms` and never before the previous frame.
    pub fn on_frame(&mut self, arrival_ms: f64, capture_ms: f64) -> f64 {
        self.stats.frames += 1;
        let transit = arrival_ms - capture_ms;
        while self.base.back().is_some_and(|&(_, t)| t >= transit) {
            self.base.pop_back();
        }
        self.base.push_back((arrival_ms, transit));
        while self.base.front().is_some_and(|&(at, _)| arrival_ms - at > BASE_WINDOW_MS) {
            self.base.pop_front();
        }
        let base = self.base.front().map_or(transit, |&(_, t)| t);
        let excess = transit - base;

        self.recent.push_back(excess);
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        let mut sorted: Vec<f64> = self.recent.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let p90 = sorted[(sorted.len() * 9 / 10).min(sorted.len() - 1)];
        let want = if p90 > ENGAGE_MS { p90.min(MAX_DELAY_MS) } else { 0.0 };
        self.target_ms = if want >= self.target_ms {
            want
        } else {
            (self.target_ms - SHRINK_PER_FRAME_MS).max(want)
        };
        self.stats.target_ms = self.target_ms;

        let due = capture_ms + base + self.target_ms;
        if arrival_ms > due + 0.5 && self.target_ms > 0.0 {
            self.stats.late += 1;
        }
        let release = due.max(arrival_ms).max(self.last_release);
        self.last_release = release;
        release
    }
}

impl Default for Playout {
    fn default() -> Self {
        Self::new()
    }
}

/// Presentation cadence, for judging smoothness: the gaps between frames as
/// shown. A stall is a gap over [`STALL_MS`].
#[derive(Debug, Default, Clone)]
pub struct Cadence {
    last: Option<f64>,
    gaps: Vec<f64>,
}

pub const STALL_MS: f64 = 100.0;

impl Cadence {
    pub fn push(&mut self, shown_ms: f64) {
        if let Some(l) = self.last {
            self.gaps.push(shown_ms - l);
        }
        self.last = Some(shown_ms);
    }

    pub fn stalls(&self) -> usize {
        self.gaps.iter().filter(|g| **g > STALL_MS).count()
    }

    pub fn last_gap_ms(&self) -> f64 {
        self.gaps.last().copied().unwrap_or(0.0)
    }

    pub fn max_gap_ms(&self) -> f64 {
        self.gaps.iter().copied().fold(0.0, f64::max)
    }

    /// Standard deviation of the frame interval, ms: 0 is perfectly even.
    pub fn judder_ms(&self) -> f64 {
        // Stalls are counted separately; judder is the cadence between them.
        let g: Vec<f64> = self.gaps.iter().copied().filter(|g| *g <= STALL_MS).collect();
        if g.len() < 2 {
            return 0.0;
        }
        let mean = g.iter().sum::<f64>() / g.len() as f64;
        (g.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / g.len() as f64).sqrt()
    }

    pub fn frames(&self) -> usize {
        self.gaps.len() + usize::from(self.last.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1000.0 / 60.0;

    /// 60 fps captured on schedule, arriving after `transit(i)`.
    fn play(n: usize, transit: impl Fn(usize) -> f64) -> (Playout, Cadence, Cadence) {
        let mut p = Playout::new();
        let (mut shown, mut raw) = (Cadence::default(), Cadence::default());
        for i in 0..n {
            let cap = i as f64 * FRAME;
            let arr = cap + transit(i);
            raw.push(arr);
            shown.push(p.on_frame(arr, cap));
        }
        (p, shown, raw)
    }

    #[test]
    fn wired_jitter_leaves_it_off() {
        let (p, shown, raw) = play(600, |i| 3.0 + (i * 7 % 10) as f64 / 10.0);
        assert_eq!(
            p.target_ms(),
            0.0,
            "a millisecond of jitter is not worth a millisecond of delay"
        );
        assert!((shown.judder_ms() - raw.judder_ms()).abs() < 1e-9, "shown exactly as it arrived");
    }

    #[test]
    fn bunched_wifi_arrivals_come_out_even() {
        // Every third frame is held 25 ms by the radio and lands with the next.
        let (p, shown, raw) = play(1200, |i| 5.0 + if i % 3 == 0 { 25.0 } else { (i % 5) as f64 });
        assert!(raw.judder_ms() > 5.0, "raw arrival is uneven: {}", raw.judder_ms());
        assert!(shown.judder_ms() < 1.0, "shown is even: {}", shown.judder_ms());
        assert!(p.target_ms() <= MAX_DELAY_MS && p.target_ms() >= 20.0, "{}", p.target_ms());
    }

    #[test]
    fn the_delay_is_bounded() {
        // 120 ms of jitter: the buffer stops at its bound and frames beyond
        // it are shown late rather than everything being delayed 120 ms.
        let (p, _, _) = play(1200, |i| if i % 2 == 0 { 120.0 } else { 0.0 });
        assert_eq!(p.target_ms(), MAX_DELAY_MS);
        assert!(p.stats.late > 500);
    }

    #[test]
    fn a_stall_is_a_pause_not_a_permanent_delay() {
        // 300 ms with nothing, then the backlog at once.
        let stall = |i: usize| {
            let cap = i as f64 * FRAME;
            let (from, to) = (5000.0, 5300.0);
            if cap >= from && cap < to {
                to - cap + 2.0
            } else {
                2.0
            }
        };
        let (p, shown, _) = play(900, stall);
        assert_eq!(shown.stalls(), 1);
        assert!(shown.max_gap_ms() < 320.0, "{}", shown.max_gap_ms());
        // Two seconds later the stall has left the percentile: no lasting cost.
        assert!(p.target_ms() < ENGAGE_MS, "{}", p.target_ms());
    }

    #[test]
    fn it_shrinks_gradually_and_never_reorders() {
        let mut p = Playout::new();
        let mut last = f64::MIN;
        for i in 0..2000 {
            let cap = i as f64 * FRAME;
            let jitter = if i < 600 {
                if i % 2 == 0 {
                    30.0
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let r = p.on_frame(cap + 1.0 + jitter, cap);
            assert!(r >= last);
            last = r;
            if i == 700 {
                assert!(
                    p.target_ms() > 0.0 && p.target_ms() < 30.0,
                    "still shrinking: {}",
                    p.target_ms()
                );
            }
        }
        assert_eq!(p.target_ms(), 0.0);
    }
}
