//! Feedback control for the share: AIMD bitrate adaptation on the sender and
//! RTP-sequence loss accounting on the receiver. Pure logic, no I/O — the
//! sender/receiver loops own the sockets and timers.

/// AIMD bitrate controller. Multiplicative decrease on sustained loss,
/// additive increase when the window is clean, always clamped to
/// `[floor, ceiling]`.
#[derive(Debug, Clone, Copy)]
pub struct AimdBitrate {
    floor: u32,
    ceiling: u32,
}

/// Loss above this fraction in a window triggers a step-down.
const LOSS_THRESHOLD: f32 = 0.02;
/// Multiplicative decrease factor.
const DECREASE: f32 = 0.8;
/// Additive increase per clean window.
const INCREASE_BPS: u32 = 2_000_000;
/// Never step below this rate (unless the user's ceiling is lower).
const MIN_FLOOR_BPS: u32 = 8_000_000;

impl AimdBitrate {
    /// `initial_bps` is the user's requested bitrate and the ceiling. The
    /// floor is 1/6 of that, but at least 8 Mb/s — capped at the ceiling so a
    /// sub-8 Mb/s request still yields a valid (degenerate) range.
    pub fn new(initial_bps: u32) -> Self {
        let ceiling = initial_bps;
        let floor = (initial_bps / 6).max(MIN_FLOOR_BPS).min(ceiling);
        Self { floor, ceiling }
    }

    pub fn floor(&self) -> u32 {
        self.floor
    }

    pub fn ceiling(&self) -> u32 {
        self.ceiling
    }

    /// Next target given the current target and the last window's loss.
    pub fn next(&self, current_bps: u32, loss_fraction: f32) -> u32 {
        let next = if loss_fraction > LOSS_THRESHOLD {
            (current_bps as f32 * DECREASE) as u32
        } else {
            current_bps.saturating_add(INCREASE_BPS)
        };
        next.clamp(self.floor, self.ceiling)
    }
}

/// RTP-sequence loss accounting over a window. Feed every arriving sequence
/// number; `take_fraction` reports the missing fraction and starts a new
/// window. Sequence numbers are u16 and wrap; a backwards jump (reordered or
/// duplicate packet) is ignored rather than counted as ~65 k losses.
#[derive(Debug, Default)]
pub struct LossWindow {
    last_seq: Option<u16>,
    expected: u64,
    lost: u64,
}

impl LossWindow {
    pub fn push(&mut self, seq: u16) {
        if let Some(prev) = self.last_seq {
            let gap = seq.wrapping_sub(prev);
            if gap == 0 || gap >= 0x8000 {
                // Duplicate or reordered-late packet: not new loss. Keep
                // `last_seq` at the newest sequence we have seen.
                return;
            }
            self.expected += gap as u64;
            self.lost += (gap - 1) as u64;
        }
        self.last_seq = Some(seq);
    }

    /// Missing fraction of the current window (0.0 when nothing arrived),
    /// then reset for the next window. Continuity across windows is kept.
    pub fn take_fraction(&mut self) -> f32 {
        let fraction =
            if self.expected > 0 { self.lost as f32 / self.expected as f32 } else { 0.0 };
        self.expected = 0;
        self.lost = 0;
        fraction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aimd_increases_when_clean() {
        let c = AimdBitrate::new(60_000_000);
        assert_eq!(c.next(40_000_000, 0.0), 42_000_000);
        assert_eq!(c.next(40_000_000, LOSS_THRESHOLD), 42_000_000, "threshold is exclusive");
    }

    #[test]
    fn aimd_decreases_on_loss() {
        let c = AimdBitrate::new(60_000_000);
        assert_eq!(c.next(50_000_000, 0.05), 40_000_000);
    }

    #[test]
    fn aimd_clamps_to_ceiling_and_floor() {
        let c = AimdBitrate::new(60_000_000);
        assert_eq!(c.floor(), 10_000_000);
        assert_eq!(c.ceiling(), 60_000_000);
        // At the ceiling, a clean window stays at the ceiling.
        assert_eq!(c.next(60_000_000, 0.0), 60_000_000);
        // Repeated loss converges to the floor and stops there.
        let mut bps = 60_000_000;
        for _ in 0..40 {
            bps = c.next(bps, 0.5);
        }
        assert_eq!(bps, c.floor());
    }

    #[test]
    fn aimd_low_ceiling_never_panics_or_exceeds() {
        // A 5 Mb/s request: the 8 Mb/s minimum floor must cap at the ceiling
        // instead of producing floor > ceiling (which would panic in clamp).
        let c = AimdBitrate::new(5_000_000);
        assert_eq!(c.floor(), 5_000_000);
        assert_eq!(c.ceiling(), 5_000_000);
        assert_eq!(c.next(5_000_000, 0.5), 5_000_000);
        assert_eq!(c.next(5_000_000, 0.0), 5_000_000);
    }

    #[test]
    fn aimd_zero_bitrate_is_degenerate_but_safe() {
        let c = AimdBitrate::new(0);
        assert_eq!(c.next(0, 0.0), 0);
        assert_eq!(c.next(0, 1.0), 0);
    }

    #[test]
    fn loss_window_clean_sequence_is_zero() {
        let mut w = LossWindow::default();
        for seq in 100u16..200 {
            w.push(seq);
        }
        assert_eq!(w.take_fraction(), 0.0);
    }

    #[test]
    fn loss_window_counts_gaps() {
        let mut w = LossWindow::default();
        w.push(1);
        w.push(2);
        w.push(5); // 3 and 4 lost
        w.push(6);
        // expected 5 (seqs 2..=6), lost 2.
        assert!((w.take_fraction() - 0.4).abs() < 1e-6);
        // Window reset; the next clean packet reports zero.
        w.push(7);
        assert_eq!(w.take_fraction(), 0.0);
    }

    #[test]
    fn loss_window_wraps_u16() {
        let mut w = LossWindow::default();
        w.push(65_534);
        w.push(65_535);
        w.push(0);
        w.push(1);
        assert_eq!(w.take_fraction(), 0.0, "wraparound is not loss");
        // A gap across the wrap still counts.
        let mut w = LossWindow::default();
        w.push(65_535);
        w.push(2); // 0 and 1 lost
        assert!((w.take_fraction() - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn loss_window_ignores_reorder_and_duplicates() {
        let mut w = LossWindow::default();
        w.push(10);
        w.push(12); // 11 momentarily missing
        w.push(11); // …arrives late: must not add ~65 k losses
        w.push(12); // duplicate
        w.push(13);
        let f = w.take_fraction();
        // Only the original gap (1 of 3) remains counted.
        assert!((f - 1.0 / 3.0).abs() < 1e-6, "fraction {f}");
    }

    #[test]
    fn loss_window_empty_is_zero() {
        let mut w = LossWindow::default();
        assert_eq!(w.take_fraction(), 0.0);
        w.push(42); // first packet alone: no expectations yet
        assert_eq!(w.take_fraction(), 0.0);
    }

    #[test]
    fn loss_window_total_loss_capped_at_one() {
        let mut w = LossWindow::default();
        w.push(0);
        w.push(30_000);
        let f = w.take_fraction();
        assert!(f < 1.0 && f > 0.99, "fraction {f}");
    }
}
