//! Source-switch state machine, kept pure so the request → apply/reject
//! lifecycle is unit-testable. The stdin task calls [`Switcher::request`];
//! the video pipeline drains with [`Switcher::take_pending`] at a frame
//! boundary and reports back with `applied`/`rejected`. The encoder, track
//! and peer connection are never touched — only the capture source and the
//! converter change, followed by one forced IDR.

use crate::command::SourceTarget;

#[derive(Debug)]
pub struct Switcher {
    current: SourceTarget,
    pending: Option<SourceTarget>,
}

impl Switcher {
    pub fn new(initial: SourceTarget) -> Self {
        Self { current: initial, pending: None }
    }

    pub fn current(&self) -> SourceTarget {
        self.current
    }

    /// Queue a switch. Returns false (dropped) when the target is already
    /// live and nothing else is queued. A newer request replaces a queued one
    /// — only the latest matters.
    pub fn request(&mut self, target: SourceTarget) -> bool {
        if target == self.current && self.pending.is_none() {
            return false;
        }
        self.pending = Some(target);
        true
    }

    /// The pipeline takes the newest queued target to act on.
    pub fn take_pending(&mut self) -> Option<SourceTarget> {
        self.pending.take()
    }

    /// The new source is live; it becomes current.
    pub fn applied(&mut self, target: SourceTarget) {
        self.current = target;
    }
}

/// Clamp a requested region to the monitor, snapping position down and size
/// up to even pixels (the GPU video processor is happiest on 4:2:0-friendly
/// coordinates). `None` when nothing usable remains.
pub fn clamp_region(
    mon: (u32, u32),
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Option<(u32, u32, u32, u32)> {
    let x = (x & !1).min(mon.0.saturating_sub(2));
    let y = (y & !1).min(mon.1.saturating_sub(2));
    let w = ((w + 1) & !1).min(mon.0 - x);
    let h = ((h + 1) & !1).min(mon.1 - y);
    (w >= 2 && h >= 2).then_some((x, y, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    const D0: SourceTarget = SourceTarget::Display { index: 0 };
    const D1: SourceTarget = SourceTarget::Display { index: 1 };
    const W: SourceTarget = SourceTarget::Window { hwnd: 0xABC };

    #[test]
    fn dedups_requests_for_the_live_source() {
        let mut s = Switcher::new(D0);
        assert!(!s.request(D0), "already live, nothing queued");
        assert_eq!(s.take_pending(), None);

        assert!(s.request(D1));
        assert_eq!(s.take_pending(), Some(D1));
        assert_eq!(s.take_pending(), None, "taken once");
    }

    #[test]
    fn newer_request_replaces_a_queued_one() {
        let mut s = Switcher::new(D0);
        assert!(s.request(D1));
        assert!(s.request(W));
        assert_eq!(s.take_pending(), Some(W), "only the latest target applies");
        assert_eq!(s.take_pending(), None);
    }

    #[test]
    fn requesting_current_cancels_a_queued_switch() {
        // User clicks display 1 then clicks back to display 0 before the
        // pipeline got to it: the pending request becomes "switch to what is
        // already live", which the pipeline applies as a no-op re-create or
        // the caller can skip; either way current stays consistent.
        let mut s = Switcher::new(D0);
        assert!(s.request(D1));
        assert!(s.request(D0), "queued switch exists, so this must be recorded");
        assert_eq!(s.take_pending(), Some(D0));
        s.applied(D0);
        assert_eq!(s.current(), D0);
    }

    #[test]
    fn applied_moves_current_and_failure_keeps_it() {
        let mut s = Switcher::new(D0);
        s.request(W);
        let t = s.take_pending().unwrap();
        // Simulate failure: nothing applied, current unchanged.
        assert_eq!(s.current(), D0);
        // Retry succeeds.
        s.applied(t);
        assert_eq!(s.current(), W);
        assert!(!s.request(W), "now it is live; repeat clicks are dropped");
    }

    #[test]
    fn regions_clamp_to_the_monitor_and_even_pixels() {
        assert_eq!(clamp_region((2560, 1440), 101, 51, 1280, 720), Some((100, 50, 1280, 720)));
        // Odd sizes round up; overflow clamps to the edge.
        assert_eq!(clamp_region((1920, 1080), 0, 0, 1919, 1079), Some((0, 0, 1920, 1080)));
        assert_eq!(clamp_region((1920, 1080), 1900, 1000, 400, 400), Some((1900, 1000, 20, 80)));
        // Degenerate rects are rejected.
        assert_eq!(clamp_region((1920, 1080), 0, 0, 0, 100), None);
        assert_eq!(clamp_region((1920, 1080), 5000, 0, 100, 100), Some((1918, 0, 2, 100)));
    }
}
