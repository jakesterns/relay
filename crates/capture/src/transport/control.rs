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

/// One receiver report (S49), every [`FEEDBACK_INTERVAL`]. Sent only to a
/// sender that announced it understands it; an older sender gets the 1 s
/// `Loss` fraction as before.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Feedback {
    /// Video packets given up on, and delivered, in this window.
    pub lost: u32,
    pub packets: u32,
    /// The standing queue: the *smallest* queueing delay any access unit saw
    /// in this window, ms. A keyframe burst or a stall raises the largest
    /// delay for a moment; only a queue that is not draining raises the
    /// smallest. (CoDel's observation, and why this signal ignores both.)
    pub queue_ms: f32,
    /// The largest, for the log and the playout buffer, not for control.
    pub queue_max_ms: f32,
    /// The first and last unit's queueing delay in the window: falling
    /// means the queue is draining (a stall released), rising means it is
    /// building. Absent from a report that predates them.
    #[serde(default)]
    pub queue_first_ms: f32,
    #[serde(default)]
    pub queue_last_ms: f32,
    /// Video payload delivered in this window, kb/s.
    pub recv_kbps: u32,
    pub window_ms: u32,
}

/// How often the receiver reports. Fast enough to back off within half a
/// second of a queue forming; the loss half still adds these up into the
/// same 1 s windows [`BitrateControl`] has always had.
pub const FEEDBACK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Why the target last moved, for the log and the "what Relay is doing" line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Steady,
    /// A queue is building: the link is carrying less than we send.
    Queue,
    /// Unrepaired loss.
    Loss,
    /// Climbing back.
    Recovering,
}

/// Delay- and loss-based bitrate control (S49).
///
/// The loss half is [`BitrateControl`], untouched and fed the same 1 s
/// windows, so on a wired LAN — where the queue is always empty — the output
/// is exactly what it was before S49. The delay half acts first on Wi-Fi:
///
/// * **Back off quickly.** A standing queue over [`QUEUE_MS`] in two
///   consecutive reports (half a second), or over [`QUEUE_SEVERE_MS`] in one,
///   cuts the target to [`BACKOFF`] of what actually arrived — the link's
///   measured rate, not a guess. A second cut only comes if the queue is not
///   draining, so one capacity drop costs one step, not a staircase.
/// * **Recover slowly.** Nothing climbs for [`HOLD_AFTER_CUT`]; then the
///   target grows [`GROWTH_PER_SEC`] a second, and stops at 95 % of the rate
///   that built a queue for [`REMEMBER`] before probing past it.
#[derive(Debug, Clone)]
pub struct RateControl {
    loss: BitrateControl,
    ceiling: u32,
    floor: u32,
    delay_target: u32,
    over_run: u32,
    prev_queue_ms: f32,
    since_cut: Option<std::time::Duration>,
    /// (rate that built a queue, time left remembered).
    remembered: Option<(u32, std::time::Duration)>,
    acc_lost: u64,
    acc_packets: u64,
    acc_ms: u32,
    last_cause: Cause,
    /// Time since the last congestion event of either kind.
    since_congestion: std::time::Duration,
}

pub const QUEUE_MS: f32 = 15.0;
pub const QUEUE_SEVERE_MS: f32 = 40.0;
const OVERUSE_REPORTS: u32 = 2;
pub const BACKOFF: f32 = 0.85;
/// A cut aims to drain the measured queue within about this long.
pub const DRAIN_MS: f32 = 500.0;
const HOLD_AFTER_CUT: std::time::Duration = std::time::Duration::from_secs(3);
const MIN_CUT_GAP: std::time::Duration = std::time::Duration::from_millis(500);
pub const GROWTH_PER_SEC: f32 = 0.05;
const REMEMBER: std::time::Duration = std::time::Duration::from_secs(30);
/// The delay half may go below the loss half's floor: the resolution ladder
/// needs room down to 1080p30. Never below this.
pub const HARD_FLOOR_BPS: u32 = 2_500_000;

impl RateControl {
    pub fn new(initial_bps: u32) -> Self {
        Self {
            loss: BitrateControl::new(initial_bps),
            ceiling: initial_bps,
            floor: HARD_FLOOR_BPS.min(initial_bps),
            delay_target: initial_bps,
            over_run: 0,
            prev_queue_ms: 0.0,
            since_cut: None,
            remembered: None,
            acc_lost: 0,
            acc_packets: 0,
            acc_ms: 0,
            last_cause: Cause::Steady,
            since_congestion: std::time::Duration::MAX / 4,
        }
    }

    pub fn ceiling(&self) -> u32 {
        self.ceiling
    }

    pub fn current(&self) -> u32 {
        self.loss.current().min(self.delay_target).clamp(self.floor, self.ceiling.max(self.floor))
    }

    /// Why the target is where it is: steady at the ceiling, cut by a queue
    /// or by loss in the last 10 s, or climbing back.
    pub fn cause(&self) -> Cause {
        if self.current() >= self.ceiling {
            Cause::Steady
        } else if self.since_congestion < std::time::Duration::from_secs(10)
            && matches!(self.last_cause, Cause::Queue | Cause::Loss)
        {
            self.last_cause
        } else {
            Cause::Recovering
        }
    }

    /// The target is below the ceiling, or there was congestion lately:
    /// keyframes should be paced and keyframe requests coalesced.
    pub fn constrained(&self) -> bool {
        self.current() < self.ceiling || self.since_congestion < std::time::Duration::from_secs(10)
    }

    /// An older receiver's 1 s loss fraction: the loss half only.
    pub fn on_loss_window(&mut self, fraction: f32) -> u32 {
        self.since_congestion =
            self.since_congestion.saturating_add(std::time::Duration::from_secs(1));
        self.loss_window(fraction)
    }

    fn loss_window(&mut self, fraction: f32) -> u32 {
        let before = self.loss.current();
        let now = self.loss.on_window(fraction);
        if now < before {
            self.last_cause = Cause::Loss;
            self.since_congestion = std::time::Duration::ZERO;
        }
        self.current()
    }

    /// One S49 report; returns the target bitrate.
    pub fn on_feedback(&mut self, f: &Feedback) -> u32 {
        let dt = std::time::Duration::from_millis(u64::from(f.window_ms.clamp(1, 5_000)));
        self.since_congestion = self.since_congestion.saturating_add(dt);
        if let Some(t) = self.since_cut.as_mut() {
            *t += dt;
        }
        if let Some((rate, left)) = self.remembered {
            self.remembered = left.checked_sub(dt).map(|l| (rate, l));
        }

        // Loss: the same 1 s windows as ever.
        self.acc_lost += u64::from(f.lost);
        self.acc_packets += u64::from(f.lost) + u64::from(f.packets);
        self.acc_ms += f.window_ms;
        if self.acc_ms >= 1000 {
            let fraction = if self.acc_packets > 0 {
                self.acc_lost as f32 / self.acc_packets as f32
            } else {
                0.0
            };
            self.loss_window(fraction);
            (self.acc_lost, self.acc_packets, self.acc_ms) = (0, 0, 0);
        }

        // Delay.
        let q = f.queue_ms;
        self.over_run = if q > QUEUE_MS { self.over_run + 1 } else { 0 };
        // A queue draining within the window (its first unit waited longer
        // than its last) is a stall or a spike being released, not a link
        // carrying less than we send: it does not count as severe.
        let draining_in_window = f.queue_first_ms > f.queue_last_ms + 10.0;
        let overuse =
            (q > QUEUE_SEVERE_MS && !draining_in_window) || self.over_run >= OVERUSE_REPORTS;
        let draining = q < self.prev_queue_ms - 2.0;
        self.prev_queue_ms = q;
        let may_cut = match self.since_cut {
            None => true,
            Some(t) => t >= MIN_CUT_GAP && !draining,
        };
        if overuse && may_cut {
            let sending = self.current();
            let measured = f.recv_kbps.saturating_mul(1000);
            // What arrived is what the link carried; never cut to more than
            // we were sending, and never by more than three quarters in one step.
            let base = if measured > 0 { measured.min(sending) } else { sending };
            // Below the link's rate by enough to drain the standing queue in
            // about DRAIN_MS: at the link's own rate it would never drain.
            let factor = BACKOFF.min(1.0 - q / DRAIN_MS);
            let to = ((base as f32 * factor) as u32).max(sending / 4).max(self.floor);
            if to < self.delay_target.min(sending) {
                self.remembered = Some((sending, REMEMBER));
                self.delay_target = to;
                self.since_cut = Some(std::time::Duration::ZERO);
                self.last_cause = Cause::Queue;
                self.since_congestion = std::time::Duration::ZERO;
                self.over_run = 0;
            }
        } else if q <= QUEUE_MS && self.delay_target < self.ceiling {
            let held = self.since_cut.is_some_and(|t| t < HOLD_AFTER_CUT);
            if !held {
                let grow = (self.delay_target as f32 * GROWTH_PER_SEC * dt.as_secs_f32()) as u32;
                let cap = self.remembered.map_or(self.ceiling, |(r, _)| {
                    ((r as f32 * 0.95) as u32).max(self.delay_target)
                });
                let next =
                    self.delay_target.saturating_add(grow.max(50_000)).min(cap).min(self.ceiling);
                if next > self.delay_target {
                    self.delay_target = next;
                    self.last_cause = Cause::Recovering;
                }
            }
        }
        if self.current() >= self.ceiling && self.last_cause == Cause::Recovering {
            self.last_cause = Cause::Steady;
        }
        self.current()
    }
}

/// The receiver's half of [`Feedback`]: turns arrivals into reports.
///
/// Queueing delay is measured per access unit from its *first* packet's
/// arrival to its capture time. The first packet, not the last: the last one
/// also carries the unit's own serialisation (a 600 KB keyframe at 25 Mb/s
/// is 190 ms of it), which is not a queue and must not cut the bitrate. The
/// delay is relative to the fastest transit seen in the last 10 s, so the
/// clock offset between the PCs cancels out.
pub struct FeedbackMeter {
    base: std::collections::VecDeque<(i64, i64)>,
    q_min: f32,
    q_max: f32,
    q_first: Option<f32>,
    q_last: f32,
    lost: u32,
    packets: u32,
    bytes: u64,
    started_ns: i64,
}

const BASE_WINDOW_NS: i64 = 10_000_000_000;

impl FeedbackMeter {
    pub fn new(now_ns: i64) -> Self {
        Self {
            base: Default::default(),
            q_min: f32::MAX,
            q_max: 0.0,
            q_first: None,
            q_last: 0.0,
            lost: 0,
            packets: 0,
            bytes: 0,
            started_ns: now_ns,
        }
    }

    /// A packet delivered (in order, or repaired). Its bytes are what the
    /// link carried: counting only whole units under-reports a lossy link,
    /// where most units are damaged, and halves the cut it deserves.
    pub fn on_packet(&mut self, bytes: usize) {
        self.packets = self.packets.saturating_add(1);
        self.bytes += bytes as u64;
    }

    pub fn on_lost(&mut self, n: u16) {
        self.lost = self.lost.saturating_add(u32::from(n));
    }

    /// A whole unit, first packet in at `first_ns`, captured at
    /// `capture_ns` (both on this PC's clock). Returns its queueing delay, ms.
    pub fn on_unit(&mut self, first_ns: i64, capture_ns: Option<i64>) -> Option<f32> {
        let transit = first_ns - capture_ns?;
        while self.base.back().is_some_and(|&(_, t)| t >= transit) {
            self.base.pop_back();
        }
        self.base.push_back((first_ns, transit));
        while self.base.front().is_some_and(|&(at, _)| first_ns - at > BASE_WINDOW_NS) {
            self.base.pop_front();
        }
        let base = self.base.front().map_or(transit, |&(_, t)| t);
        let q = ((transit - base) as f64 / 1e6) as f32;
        self.q_min = self.q_min.min(q);
        self.q_max = self.q_max.max(q);
        self.q_first.get_or_insert(q);
        self.q_last = q;
        Some(q)
    }

    /// The report for the window since the last one, and start the next.
    pub fn take(&mut self, now_ns: i64) -> Feedback {
        let window_ms = ((now_ns - self.started_ns).max(1) / 1_000_000).max(1) as u32;
        let f = Feedback {
            lost: self.lost,
            packets: self.packets,
            // No unit completed in the window (a stall): no evidence of a
            // standing queue either way, so report none rather than guess.
            queue_ms: if self.q_min == f32::MAX { 0.0 } else { self.q_min },
            queue_max_ms: self.q_max,
            queue_first_ms: self.q_first.unwrap_or(0.0),
            queue_last_ms: self.q_last,
            recv_kbps: (self.bytes * 8 / u64::from(window_ms)) as u32,
            window_ms,
        };
        *self = Self { base: std::mem::take(&mut self.base), ..Self::new(now_ns) };
        f
    }
}

/// The sender's half (S49): the bitrate controller, the resolution ladder
/// and what they tell the pipeline. Pure, so the in-process loopback test
/// drives exactly what a real share does.
pub struct SenderAdapt {
    pub control: RateControl,
    pub ladder: Option<super::ladder::Ladder>,
}

/// What changed after one report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    pub target_bps: u32,
    /// A new rung to switch the encoder to.
    pub rung: Option<super::ladder::Rung>,
    pub cause: Cause,
    pub constrained: bool,
}

impl SenderAdapt {
    /// `ladder`: the rungs, when the receiver takes mid-share size changes.
    pub fn new(bitrate_bps: u32, ladder: Option<super::ladder::Ladder>) -> Self {
        Self { control: RateControl::new(bitrate_bps), ladder }
    }

    fn decision(&self, rung: Option<super::ladder::Rung>) -> Decision {
        Decision {
            target_bps: self.control.current(),
            rung,
            cause: self.control.cause(),
            constrained: self.control.constrained(),
        }
    }

    pub fn on_feedback(&mut self, f: &Feedback) -> Decision {
        let target = self.control.on_feedback(f);
        let ceiling = self.control.ceiling();
        let dt = std::time::Duration::from_millis(u64::from(f.window_ms.clamp(1, 5_000)));
        let rung = self.ladder.as_mut().and_then(|l| l.on_tick(dt, target, ceiling));
        self.decision(rung)
    }

    pub fn on_loss_window(&mut self, fraction: f32) -> Decision {
        self.control.on_loss_window(fraction);
        self.decision(None)
    }
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Steady => "steady",
            Cause::Queue => "queue",
            Cause::Loss => "loss",
            Cause::Recovering => "recovering",
        }
    }

    pub fn parse(s: &str) -> Cause {
        match s {
            "queue" => Cause::Queue,
            "loss" => Cause::Loss,
            "recovering" => Cause::Recovering,
            _ => Cause::Steady,
        }
    }
}

/// The `adapt` object on both ends' `stats` lines (S49).
pub fn adapt_json(
    rung: super::ladder::Rung,
    top: super::ladder::Rung,
    target_bps: u32,
    cause: Cause,
) -> serde_json::Value {
    serde_json::json!({
        "rung": rung.label(),
        "width": rung.width,
        "height": rung.height,
        "fps": rung.fps,
        "top": top.label(),
        "target_mbps": target_bps as f64 / 1e6,
        "cause": cause.as_str(),
        "note": adapt_note(Some(rung), Some(top), cause),
    })
}

/// Words for what Relay is doing, for the screens. Calm, and never about the
/// user: the link is short of room, Relay is adjusting.
pub fn adapt_note(
    rung: Option<super::ladder::Rung>,
    top: Option<super::ladder::Rung>,
    cause: Cause,
) -> Option<String> {
    match (rung, top) {
        (Some(r), Some(t)) if r != t => Some(format!("Lowered to {} to stay smooth", r.label())),
        _ => match cause {
            Cause::Queue | Cause::Loss => Some("Lowered the bitrate to stay smooth".into()),
            Cause::Recovering => Some("Raising quality as the link allows".into()),
            Cause::Steady => None,
        },
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

    fn report(lost: u32, queue_ms: f32, recv_mbps: f32) -> Feedback {
        Feedback {
            lost,
            packets: 1000,
            queue_ms,
            queue_max_ms: queue_ms,
            queue_first_ms: queue_ms,
            queue_last_ms: queue_ms,
            recv_kbps: (recv_mbps * 1000.0) as u32,
            window_ms: 250,
        }
    }

    /// The wired guarantee: with no queue, the S49 controller fed 250 ms
    /// reports gives exactly what S30's gave for the same loss in 1 s windows.
    #[test]
    fn with_no_queue_it_is_the_loss_controller_exactly() {
        let mut old = BitrateControl::new(40_000_000);
        let mut new = RateControl::new(40_000_000);
        // A loss pattern with steps down and the slow climb back.
        let seconds: Vec<u32> = (0..240)
            .map(|s| if (20..26).contains(&s) || (100..103).contains(&s) { 30 } else { 0 })
            .collect();
        for (s, &lost_per_quarter) in seconds.iter().enumerate() {
            let want = old.on_window(lost_per_quarter as f32 / 1030.0);
            let mut got = 0;
            for _ in 0..4 {
                got = new.on_feedback(&report(lost_per_quarter, 0.5, 40.0));
            }
            assert_eq!(got, want, "second {s}");
        }
        assert!(new.current() < 40_000_000 || old.current() == new.current());
    }

    #[test]
    fn a_standing_queue_cuts_to_what_arrived_within_half_a_second() {
        let mut c = RateControl::new(40_000_000);
        assert_eq!(c.on_feedback(&report(0, 2.0, 40.0)), 40_000_000);
        // The link fell to 25 Mb/s: a queue forms, 25 arrive.
        assert_eq!(c.on_feedback(&report(0, 30.0, 25.0)), 40_000_000, "one report is not enough");
        let cut = c.on_feedback(&report(0, 55.0, 25.0));
        assert_eq!(cut, 21_250_000, "85 % of the measured 25 Mb/s");
        assert_eq!(c.cause(), Cause::Queue);
        // The queue is draining: no second cut while it does.
        for q in [70.0, 50.0, 30.0, 16.0] {
            assert_eq!(c.on_feedback(&report(0, q, 21.0)), cut, "draining at {q} ms");
        }
    }

    #[test]
    fn a_deep_queue_is_cut_below_the_link_rate_so_it_drains() {
        // 200 ms queued on a link carrying 20: sending 20 would keep it
        // there for ever, 17 would take seconds. 60 % drains it in ~0.5 s.
        let mut c = RateControl::new(30_000_000);
        assert_eq!(c.on_feedback(&report(0, 200.0, 20.0)), 12_000_000);
        let mut c = RateControl::new(22_000_000);
        assert_eq!(c.on_feedback(&report(0, 200.0, 20.0)), 12_000_000);
    }

    #[test]
    fn a_severe_queue_cuts_on_one_report_but_never_by_more_than_three_quarters() {
        let mut c = RateControl::new(40_000_000);
        assert_eq!(c.on_feedback(&report(0, 120.0, 5.0)), 10_000_000);
    }

    #[test]
    fn a_queue_that_keeps_growing_after_a_cut_cuts_again() {
        let mut c = RateControl::new(40_000_000);
        c.on_feedback(&report(0, 30.0, 25.0));
        let first = c.on_feedback(&report(0, 40.0, 25.0));
        c.on_feedback(&report(0, 45.0, 15.0));
        let second = c.on_feedback(&report(0, 50.0, 15.0));
        assert!(second < first, "{second} after {first}");
        assert_eq!(second, 12_750_000);
    }

    /// Measured on the wifi-busy model: a 300 ms scan stall releases its
    /// backlog in a burst, and a report window that ends mid-burst has a
    /// smallest delay of 100+ ms — a "severe queue" that is really one
    /// draining. Its first unit waited longer than its last, so it is not cut.
    #[test]
    fn a_stall_draining_in_the_window_is_not_a_severe_queue() {
        let mut c = RateControl::new(40_000_000);
        let mut f = report(0, 117.0, 40.0);
        f.queue_first_ms = 290.0;
        f.queue_last_ms = 117.0;
        assert_eq!(c.on_feedback(&f), 40_000_000);
        assert_eq!(c.on_feedback(&report(0, 2.0, 40.0)), 40_000_000);
        // A queue building inside the window is cut at once.
        let mut f = report(0, 80.0, 25.0);
        f.queue_first_ms = 80.0;
        f.queue_last_ms = 140.0;
        assert!(c.on_feedback(&f) < 40_000_000);
    }

    #[test]
    fn stalls_and_keyframes_do_not_move_it() {
        // A 300 ms stall shows as one window with a huge *largest* delay; the
        // standing queue (smallest) stays near zero because the queue drains.
        let mut c = RateControl::new(40_000_000);
        for i in 0..400 {
            let mut f = report(0, 1.0, 40.0);
            if i % 40 == 0 {
                f.queue_max_ms = 300.0;
            }
            assert_eq!(c.on_feedback(&f), 40_000_000);
        }
        assert!(!c.constrained());
    }

    #[test]
    fn recovery_holds_then_climbs_slowly_and_stops_short_of_the_rate_that_queued() {
        let mut c = RateControl::new(40_000_000);
        c.on_feedback(&report(0, 30.0, 20.0));
        let cut = c.on_feedback(&report(0, 30.0, 20.0));
        assert_eq!(cut, 17_000_000, "85 % of the 20 that arrived");
        // Three seconds of holding.
        for i in 0..11 {
            assert_eq!(c.on_feedback(&report(0, 1.0, 20.0)), cut, "report {i}");
        }
        // Then about 5 % a second: 10 s to roughly 1.6x, not a jump.
        let mut bps = 0;
        for _ in 0..40 {
            bps = c.on_feedback(&report(0, 1.0, 20.0));
        }
        assert!(bps > 26_000_000 && bps < 30_000_000, "{bps}");
        for _ in 0..40 {
            bps = c.on_feedback(&report(0, 1.0, 20.0));
        }
        assert_eq!(bps, 38_000_000, "95 % of the 40 that queued, while remembered");
        for _ in 0..120 {
            bps = c.on_feedback(&report(0, 1.0, 20.0));
        }
        assert_eq!(bps, 40_000_000, "and all the way back once forgotten");
        assert_eq!(c.cause(), Cause::Steady);
    }

    #[test]
    fn on_a_link_with_a_fixed_capacity_it_settles_instead_of_hunting() {
        // A 25 Mb/s link: sending more builds a queue at (send-25)/25 s per s.
        let mut c = RateControl::new(40_000_000);
        let (mut queue, mut changes, mut last) = (0.0f32, 0u32, 40_000_000u32);
        let mut min_after_settle = u32::MAX;
        for i in 0..2400 {
            let send = c.current() as f32 / 1e6;
            queue = (queue + (send - 25.0) / 25.0 * 250.0).clamp(0.0, 150.0);
            let recv = send.min(25.0);
            let bps = c.on_feedback(&report(0, queue, recv));
            if bps != last && (bps as i64 - last as i64).abs() > 2_000_000 {
                changes += 1;
            }
            last = bps;
            if i > 400 {
                min_after_settle = min_after_settle.min(bps);
            }
        }
        // Ten minutes: a cut roughly every half minute while the memory runs
        // out, never a step every second.
        assert!(changes <= 40, "{changes} large changes in 10 minutes");
        assert!(min_after_settle >= 18_000_000, "never collapses: {min_after_settle}");
        assert!(last <= 26_000_000, "and stays near the link: {last}");
    }

    #[test]
    fn the_floor_holds_and_sub_floor_requests_survive() {
        let mut c = RateControl::new(40_000_000);
        for _ in 0..200 {
            c.on_feedback(&report(0, 200.0, 0.5));
        }
        assert_eq!(c.current(), HARD_FLOOR_BPS);
        let mut c = RateControl::new(1_000_000);
        assert_eq!(c.on_feedback(&report(0, 200.0, 0.1)), 1_000_000);
    }

    #[test]
    fn an_older_receivers_loss_windows_still_work() {
        let mut c = RateControl::new(40_000_000);
        c.on_loss_window(0.05);
        assert_eq!(c.on_loss_window(0.05), 32_000_000);
        assert_eq!(c.cause(), Cause::Loss);
        assert!(c.constrained());
    }
}
