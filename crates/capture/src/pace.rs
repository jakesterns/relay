//! Frame pacing: hold the encode rate to the share's target fps.
//!
//! Capture delivers frames at display refresh — 60, 144, 240 Hz — but the
//! encoder is told the share's fps and its rate control budgets bits per frame
//! on that basis. Feeding it every captured frame spends a 30 fps bit budget
//! on 60 frames a second (bug B1: `video_up` said 30, every `stats` line read
//! ~60). So frames are admitted by capture timestamp against a running
//! deadline, and the rest are dropped before any conversion.
//!
//! The deadline advances by one interval per admitted frame rather than being
//! reset to "now", so the admitted rate cannot drift. A quarter-interval
//! tolerance lets a slightly early frame through, which is what makes a 60 Hz
//! source at 30 fps alternate cleanly instead of undershooting to 20. If the
//! source stalls past a whole interval, the deadline resynchronises instead of
//! admitting a burst to catch up.
//!
//! Pure: timestamps in, decisions out.

pub struct FramePacer {
    interval_100ns: i64,
    next_100ns: Option<i64>,
}

impl FramePacer {
    pub fn new(fps: u32) -> Self {
        Self { interval_100ns: 10_000_000 / i64::from(fps.max(1)), next_100ns: None }
    }

    /// Whether the frame captured at `t_100ns` should be encoded.
    pub fn admit(&mut self, t_100ns: i64) -> bool {
        let interval = self.interval_100ns;
        let Some(next) = self.next_100ns else {
            self.next_100ns = Some(t_100ns + interval);
            return true;
        };
        if t_100ns + interval / 4 < next {
            return false;
        }
        // Behind by more than a whole interval (source stall, or a switch):
        // start again from this frame rather than admit a catch-up burst.
        self.next_100ns =
            Some(if t_100ns >= next + interval { t_100ns + interval } else { next + interval });
        true
    }

    /// Forget the deadline; the next frame is admitted. For a source switch,
    /// where timestamps from the new source bear no relation to the old.
    pub fn reset(&mut self) {
        self.next_100ns = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Admitted count over `secs` of a `hz` source with per-frame `jitter`
    /// (alternating ±), for a pacer at `fps`.
    fn run(hz: f64, fps: u32, secs: f64, jitter_100ns: i64) -> (usize, Vec<i64>) {
        let mut p = FramePacer::new(fps);
        let period = 10_000_000.0 / hz;
        let n = (secs * hz) as usize;
        let mut admitted = Vec::new();
        for i in 0..n {
            let j = if i % 2 == 0 { jitter_100ns } else { -jitter_100ns };
            let t = (i as f64 * period) as i64 + j;
            if p.admit(t) {
                admitted.push(t);
            }
        }
        (admitted.len(), admitted)
    }

    #[test]
    fn a_60hz_source_at_30fps_alternates_cleanly() {
        let (count, times) = run(60.0, 30, 10.0, 0);
        assert_eq!(count, 300);
        for w in times.windows(2) {
            let gap = w[1] - w[0];
            assert!((333_000..=334_000).contains(&gap), "gap {gap}");
        }
        // Still clean with a millisecond of capture jitter either way.
        let (count, _) = run(60.0, 30, 10.0, 10_000);
        assert_eq!(count, 300);
    }

    #[test]
    fn the_target_rate_holds_for_common_refresh_rates() {
        for hz in [60.0, 75.0, 120.0, 144.0, 165.0, 240.0] {
            for fps in [30u32, 60] {
                let (count, _) = run(hz, fps, 20.0, 0);
                let want = (20 * fps).min((20.0 * hz) as u32) as f64;
                let got = count as f64;
                assert!(
                    (got - want).abs() / want <= 0.02,
                    "{hz} Hz at {fps} fps: {got} frames in 20 s, want ~{want}"
                );
            }
        }
    }

    #[test]
    fn a_source_slower_than_the_target_passes_every_frame() {
        let (count, _) = run(30.0, 60, 10.0, 0);
        assert_eq!(count, 300);
        let (count, _) = run(60.0, 60, 10.0, 15_000);
        assert_eq!(count, 600, "60 Hz at 60 fps with jitter drops nothing");
    }

    #[test]
    fn a_stall_resynchronises_instead_of_bursting() {
        let mut p = FramePacer::new(30);
        let step = 166_667; // 60 Hz
        assert!(p.admit(0));
        // Two seconds of nothing, then 60 Hz again.
        let resume = 20_000_000;
        let admitted = (0..60).filter(|i| p.admit(resume + i * step)).count();
        assert_eq!(admitted, 30, "one second at 30 fps, no catch-up burst");
    }

    #[test]
    fn reset_admits_the_next_frame_whatever_its_timestamp() {
        let mut p = FramePacer::new(30);
        assert!(p.admit(1_000_000_000));
        assert!(!p.admit(1_000_000_001));
        p.reset();
        assert!(p.admit(5), "a new source's clock can be anywhere");
    }
}
