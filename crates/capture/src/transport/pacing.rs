//! Keyframe pacing and keyframe-request coalescing on a weak link (S49).
//!
//! The sender hands each access unit to the wire in one burst. On a wired
//! LAN that is right — a 600 KB keyframe leaves in 5 ms and the receive
//! buffer was sized for it (S30). On Wi-Fi at 25 Mb/s the same keyframe is
//! 190 ms of airtime dumped into an access-point queue that holds 150: the
//! tail is dropped, the receiver asks for another keyframe, and a recovery
//! keyframe becomes the cause of the next loss.
//!
//! [`Pacer`] spreads only the frames that are much larger than the rest, and
//! only while the bitrate controller is constrained. Ordinary frames are never
//! delayed — the first cut of this paced *every* packet through a token
//! bucket, and on the loopback model it throttled the sender below what the
//! encoder produced: frames queued on the sender, the receiver measured that
//! as link delay, and the controller cut the rate to the floor chasing a
//! queue it had made itself. A large frame is spread at [`PACE_FACTOR`]
//! times the target, but never over more than [`MAX_SPAN_FRAMES`] frame
//! intervals, so what waits behind it is bounded.
//!
//! [`KeyframeGate`] stops a burst of keyframe requests (one per damaged
//! frame, each answered with another large frame) from becoming a burst of
//! keyframes. Wired keeps answering every request at once.

use std::time::{Duration, Instant};

/// A large frame leaves at this multiple of the target bitrate.
pub const PACE_FACTOR: f64 = 2.0;
/// A frame this many times the recent average is "large".
pub const LARGE: f64 = 3.0;
/// Never spread one frame over more than this many frame intervals.
pub const MAX_SPAN_FRAMES: u32 = 3;

#[derive(Default)]
pub struct Pacer {
    /// Recent ordinary frame size, bytes (EWMA).
    avg_bytes: f64,
}

impl Pacer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Plan one access unit of `bytes`. `target_bps` is `Some` while the
    /// link is constrained. Returns the span to spread its packets over;
    /// zero means send it as one burst, as always.
    pub fn plan(&mut self, bytes: usize, target_bps: Option<u32>, fps: u32) -> Duration {
        let b = bytes as f64;
        let large = self.avg_bytes > 0.0 && b > self.avg_bytes * LARGE;
        // A keyframe must not drag the average up and hide the next one.
        let sample = if self.avg_bytes > 0.0 { b.min(self.avg_bytes * LARGE) } else { b };
        self.avg_bytes =
            if self.avg_bytes > 0.0 { self.avg_bytes * 0.94 + sample * 0.06 } else { sample };
        let Some(t) = target_bps.filter(|t| *t > 0) else { return Duration::ZERO };
        if !large {
            return Duration::ZERO;
        }
        let natural = Duration::from_secs_f64(b * 8.0 / (f64::from(t) * PACE_FACTOR));
        natural.min(Duration::from_secs(1) * MAX_SPAN_FRAMES / fps.max(1))
    }

    /// When packet `i` of `n` is due, for a unit started at `start`.
    pub fn due(start: Instant, span: Duration, i: usize, n: usize) -> Instant {
        if n == 0 || span.is_zero() {
            return start;
        }
        start + span.mul_f64(i as f64 / n as f64)
    }
}

/// Minimum spacing of keyframes forced by requests while constrained. A
/// paced keyframe on a 20 Mb/s link takes ~50 ms to leave and more to
/// arrive; a second one requested meanwhile would only queue behind it.
pub const KEYFRAME_MIN_GAP: Duration = Duration::from_millis(400);

#[derive(Default)]
pub struct KeyframeGate {
    last: Option<Instant>,
    /// Requests absorbed, for the log.
    pub coalesced: u64,
}

impl KeyframeGate {
    /// A request arrived at `now`. `true` means force a keyframe now. When
    /// not `constrained`, always `true`: wired behaviour is unchanged.
    pub fn request(&mut self, now: Instant, constrained: bool) -> bool {
        if constrained
            && self.last.is_some_and(|l| now.saturating_duration_since(l) < KEYFRAME_MIN_GAP)
        {
            self.coalesced += 1;
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// 1 ms timer resolution for this process while held (`timeBeginPeriod`).
///
/// Windows sleeps in 15.6 ms ticks by default, and since Windows 10 2004 the
/// resolution is per process. Pacing a keyframe over 30 ms in 15.6 ms steps
/// is barely pacing, so the sender holds this while it paces, and the
/// loopback link model holds it so its millisecond delays are milliseconds.
/// Released on drop; nothing outside this process is affected.
pub struct HiResTimer(());

impl HiResTimer {
    #[cfg(windows)]
    pub fn acquire() -> Option<Self> {
        // SAFETY: a plain call; balanced by timeEndPeriod in Drop.
        (unsafe { windows::Win32::Media::timeBeginPeriod(1) } == 0).then_some(Self(()))
    }

    #[cfg(not(windows))]
    pub fn acquire() -> Option<Self> {
        None
    }
}

impl Drop for HiResTimer {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: balances the timeBeginPeriod in `acquire`.
        unsafe {
            windows::Win32::Media::timeEndPeriod(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn nothing_is_paced_when_not_constrained() {
        let mut p = Pacer::new();
        for i in 0..200 {
            let bytes = if i % 50 == 0 { 600_000 } else { 40_000 };
            assert_eq!(p.plan(bytes, None, 60), Duration::ZERO);
        }
    }

    #[test]
    fn ordinary_frames_are_never_delayed_even_when_constrained() {
        let mut p = Pacer::new();
        for i in 0..600 {
            // Sizes vary 2x around the mean, as real P-frames do.
            let bytes = 30_000 + (i * 7919 % 30_000);
            assert_eq!(p.plan(bytes, Some(20_000_000), 60), Duration::ZERO, "frame {i}");
        }
    }

    #[test]
    fn a_keyframe_is_spread_at_twice_the_target_and_bounded() {
        let mut p = Pacer::new();
        for _ in 0..60 {
            p.plan(40_000, Some(20_000_000), 60);
        }
        // 200 KB at 40 Mb/s: 40 ms.
        let span = p.plan(200_000, Some(20_000_000), 60);
        assert!((span.as_secs_f64() - 0.040).abs() < 1e-3, "{span:?}");
        // 1 MB would take 200 ms: capped at three frames.
        let span = p.plan(1_000_000, Some(20_000_000), 60);
        assert_eq!(span, Duration::from_millis(50));
        // And the keyframes did not teach it that frames are large.
        assert!(p.plan(250_000, Some(20_000_000), 60) > Duration::ZERO);
    }

    #[test]
    fn packets_are_due_evenly_across_the_span() {
        let t = Instant::now();
        assert_eq!(Pacer::due(t, 40 * MS, 0, 100), t);
        assert_eq!(Pacer::due(t, 40 * MS, 50, 100), t + 20 * MS);
        assert_eq!(Pacer::due(t, Duration::ZERO, 50, 100), t);
    }

    #[test]
    fn requests_are_coalesced_only_when_constrained() {
        let mut g = KeyframeGate::default();
        let t = Instant::now();
        assert!(g.request(t, false));
        assert!(g.request(t + MS, false), "wired: every request answered");
        assert!(!g.request(t + 100 * MS, true));
        assert!(!g.request(t + 300 * MS, true));
        assert!(g.request(t + 450 * MS, true));
        assert_eq!(g.coalesced, 2);
    }

    #[cfg(windows)]
    #[test]
    fn the_timer_resolution_is_taken_and_given_back() {
        let a = HiResTimer::acquire();
        assert!(a.is_some());
        drop(a);
    }
}
