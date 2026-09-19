//! Feedback control for the share: damped bitrate adaptation on the sender.
//! Pure logic, no I/O — the sender loop owns the socket and the timer. The
//! loss figure it is fed comes from the receiver's `reorder` buffer.

/// Bitrate controller driven by the receiver's once-a-second report of
/// *unrepaired* loss: what NACK could not fix in time.
///
/// Damped on purpose. A bitrate that hunts is worse to watch than one that is
/// steadily a little too high, because every step is a visible quality change
/// and every overshoot is another keyframe. So:
///
/// * one bad second changes nothing — a step down needs [`LOSSY_TO_DROP`]
///   consecutive lossy windows;
/// * after any loss the rate holds for [`CLEAN_TO_CLIMB`] clean windows before
///   it climbs at all, then climbs slowly ([`INCREASE_BPS`] every other window);
/// * the rate that failed is remembered: for [`MEMORY_WINDOWS`] the climb
///   stops at 90 % of it instead of walking straight back into the loss, and
///   each further failure doubles how long it is remembered (to
///   [`MEMORY_MAX_WINDOWS`]), so a link with a hard limit is probed ever more
///   rarely rather than every minute and a half for the whole share.
#[derive(Debug, Clone)]
pub struct BitrateControl {
    floor: u32,
    ceiling: u32,
    current: u32,
    lossy_run: u32,
    clean_run: u32,
    /// (cap, windows left) while a failed rate is remembered.
    soft_cap: Option<(u32, u32)>,
    /// How long the next failure is remembered for.
    memory: u32,
}

/// Unrepaired loss above this fraction makes a window lossy. Low, because
/// after NACK any residue costs a keyframe; not zero, so one stray packet in
/// a second of 4,000 does not count.
const LOSS_THRESHOLD: f32 = 0.005;
const LOSSY_TO_DROP: u32 = 2;
const DECREASE: f32 = 0.8;
const CLEAN_TO_CLIMB: u32 = 10;
const INCREASE_BPS: u32 = 1_000_000;
const MEMORY_WINDOWS: u32 = 60;
const MEMORY_MAX_WINDOWS: u32 = 960;
/// Never step below this rate (unless the requested ceiling is lower).
const MIN_FLOOR_BPS: u32 = 8_000_000;

impl BitrateControl {
    /// `initial_bps` is the requested bitrate and the ceiling. The floor is
    /// 1/6 of that, but at least 8 Mb/s — capped at the ceiling so a
    /// sub-8 Mb/s request still yields a valid (degenerate) range.
    pub fn new(initial_bps: u32) -> Self {
        let ceiling = initial_bps;
        let floor = (initial_bps / 6).max(MIN_FLOOR_BPS).min(ceiling);
        Self {
            floor,
            ceiling,
            current: ceiling,
            lossy_run: 0,
            clean_run: 0,
            soft_cap: None,
            memory: MEMORY_WINDOWS,
        }
    }

    pub fn floor(&self) -> u32 {
        self.floor
    }

    pub fn ceiling(&self) -> u32 {
        self.ceiling
    }

    pub fn current(&self) -> u32 {
        self.current
    }

    /// Feed one window of loss; returns the target bitrate.
    pub fn on_window(&mut self, loss_fraction: f32) -> u32 {
        if let Some((cap, left)) = self.soft_cap {
            self.soft_cap = (left > 1).then_some((cap, left - 1));
        }
        if loss_fraction > LOSS_THRESHOLD {
            self.clean_run = 0;
            self.lossy_run += 1;
            if self.lossy_run >= LOSSY_TO_DROP {
                self.lossy_run = 0;
                let failed = self.current;
                self.current = ((failed as f32 * DECREASE) as u32).clamp(self.floor, self.ceiling);
                let cap = ((failed as f32 * 0.9) as u32).max(self.current);
                self.soft_cap = Some((cap, self.memory));
                self.memory = (self.memory * 2).min(MEMORY_MAX_WINDOWS);
            }
        } else {
            self.lossy_run = 0;
            self.clean_run += 1;
            if self.clean_run >= CLEAN_TO_CLIMB && self.clean_run % 2 == 0 {
                let cap = self.soft_cap.map_or(self.ceiling, |(cap, _)| cap.min(self.ceiling));
                self.current = self.current.saturating_add(INCREASE_BPS).min(cap).max(self.current);
                if self.current == self.ceiling {
                    // Back at the requested rate and clean: the trouble is over.
                    self.memory = MEMORY_WINDOWS;
                }
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_bad_second_changes_nothing() {
        let mut c = BitrateControl::new(40_000_000);
        assert_eq!(c.on_window(0.05), 40_000_000);
        assert_eq!(c.on_window(0.0), 40_000_000);
        assert_eq!(c.on_window(0.05), 40_000_000, "not consecutive");
    }

    #[test]
    fn sustained_loss_steps_down_once_per_two_windows() {
        let mut c = BitrateControl::new(40_000_000);
        c.on_window(0.05);
        assert_eq!(c.on_window(0.05), 32_000_000);
        assert_eq!(c.on_window(0.05), 32_000_000);
        assert_eq!(c.on_window(0.05), 25_600_000);
    }

    #[test]
    fn loss_at_the_threshold_is_clean() {
        let mut c = BitrateControl::new(40_000_000);
        for _ in 0..10 {
            assert_eq!(c.on_window(LOSS_THRESHOLD), 40_000_000, "threshold is exclusive");
        }
    }

    #[test]
    fn recovery_holds_then_climbs_slowly_and_stops_short_of_the_failed_rate() {
        let mut c = BitrateControl::new(40_000_000);
        c.on_window(0.05);
        c.on_window(0.05); // 32 Mb/s; 40 failed, so the cap is 36.
        for i in 1..CLEAN_TO_CLIMB {
            assert_eq!(c.on_window(0.0), 32_000_000, "holding, clean window {i}");
        }
        assert_eq!(c.on_window(0.0), 33_000_000);
        assert_eq!(c.on_window(0.0), 33_000_000, "every other window");
        assert_eq!(c.on_window(0.0), 34_000_000);
        let mut bps = 0;
        for _ in 0..30 {
            bps = c.on_window(0.0);
        }
        assert_eq!(bps, 36_000_000, "90 % of the rate that failed, while remembered");
        // The memory runs out; the climb finishes.
        for _ in 0..40 {
            bps = c.on_window(0.0);
        }
        assert_eq!(bps, 40_000_000);
    }

    #[test]
    fn it_does_not_oscillate_on_a_link_that_fails_at_a_fixed_rate() {
        // A link that loses packets above 35 Mb/s. Count direction changes
        // over five minutes: stepping every window flips every few seconds.
        let mut c = BitrateControl::new(40_000_000);
        let (mut last, mut dir, mut flips) = (40_000_000u32, 0i8, 0u32);
        for _ in 0..300 {
            let loss = if c.current() > 35_000_000 { 0.03 } else { 0.0 };
            let bps = c.on_window(loss);
            let d = (bps as i64 - last as i64).signum() as i8;
            if d != 0 && d != dir {
                flips += 1;
                dir = d;
            }
            last = bps;
        }
        assert!(flips <= 8, "{flips} direction changes in 5 minutes");
        assert!(last >= 28_000_000, "and it has not collapsed: {last}");
    }

    #[test]
    fn clamps_to_floor_and_survives_degenerate_ranges() {
        let mut c = BitrateControl::new(60_000_000);
        assert_eq!(c.floor(), 10_000_000);
        for _ in 0..100 {
            c.on_window(0.5);
        }
        assert_eq!(c.current(), c.floor());
        // A 5 Mb/s request: the 8 Mb/s minimum floor caps at the ceiling
        // instead of producing floor > ceiling (which would panic in clamp).
        let mut c = BitrateControl::new(5_000_000);
        assert_eq!((c.floor(), c.ceiling()), (5_000_000, 5_000_000));
        for loss in [0.5, 0.5, 0.0, 0.5] {
            assert_eq!(c.on_window(loss), 5_000_000);
        }
        let mut c = BitrateControl::new(0);
        assert_eq!(c.on_window(1.0), 0);
        assert_eq!(c.on_window(0.0), 0);
    }
}
