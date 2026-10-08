//! Put the video packets back in order, and say when that is impossible.
//!
//! NACK was always negotiated — the default interceptors add it — but the
//! receive loop fed packets to the depacketizer in arrival order. A
//! retransmission arrives after the packets that followed the hole, so the
//! access unit had already gone to the decoder damaged, and the late packet
//! was then appended to whichever unit was being built. Recovery made the
//! picture worse. (S30; this is the mechanism behind B15's 10–15 s smears.)
//!
//! [`Reorder`] holds packets that arrive past a hole until the hole is filled
//! or [`HOLD`] runs out. In order, nothing is held and nothing is copied: the
//! packet goes straight through. When a hole times out the buffer skips it and
//! reports [`Step::Lost`], which is the receiver's cue to drop the damaged
//! unit, stop feeding the decoder, and ask for a keyframe.
//!
//! Pure: packets and instants in, packets out.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long to wait for a retransmission before giving a packet up.
///
/// The NACK generator runs every [`super::NACK_INTERVAL`] (10 ms) and the LAN
/// round trip is under a millisecond, so a first retransmission lands within
/// ~12 ms and a second, if that one is lost too, within ~22 ms. 40 ms allows
/// for both plus scheduling noise. It is latency spent only while a packet is
/// actually missing; the alternative is a keyframe, which costs more than
/// 40 ms to arrive and a visible freeze.
pub const HOLD: Duration = Duration::from_millis(40);

/// Most packets parked behind a hole. 40 ms at 80 Mb/s is ~340 packets, and a
/// keyframe burst can be 550; past this the hole is abandoned rather than
/// letting memory follow a dead link.
const MAX_HELD: usize = 2048;

#[derive(Debug, PartialEq, Eq)]
pub enum Step<P> {
    /// The next packet in sequence.
    Packet(P),
    /// `n` packets were given up on; the stream resumes after them.
    Lost(u16),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReorderStats {
    /// Holes filled in time: a loss the viewer never saw. Counted when the
    /// packet that closes the hole releases the ones parked behind it --
    /// once per hole, not per packet: a single packet arriving early parks
    /// only itself while hundreds go straight through in order, and counting
    /// each of those read "Repaired 570" on a clean wired LAN (r33 soak).
    pub recovered: u64,
    /// Packets given up on.
    pub lost: u64,
    /// Holes given up on (one keyframe request each).
    pub unrecovered_gaps: u64,
    /// Arrived after being given up on, or twice. Dropped.
    pub late_or_duplicate: u64,
}

pub struct Reorder<P> {
    /// The sequence number owed to the consumer next.
    next: Option<u16>,
    /// Packets past the hole, keyed by distance from `next` at insert time
    /// folded into a wrapping-safe extended sequence.
    held: BTreeMap<u64, (Instant, P)>,
    /// Extended sequence of `next`, so the map orders across the u16 wrap.
    next_ext: u64,
    /// When the current hole was first seen.
    hole_since: Option<Instant>,
    /// How long a hole is waited for: [`HOLD`] unless [`HoldTuner`] says
    /// the link's retransmissions need longer (S49).
    hold: Duration,
    /// How long the hole that last closed had been open: one repair's
    /// latency, for the tuner. Taken by the caller.
    pub last_repair: Option<Duration>,
    pub stats: ReorderStats,
}

impl<P> Reorder<P> {
    pub fn new() -> Self {
        Self {
            next: None,
            held: BTreeMap::new(),
            next_ext: 1 << 32,
            hole_since: None,
            hold: HOLD,
            last_repair: None,
            stats: ReorderStats::default(),
        }
    }

    pub fn hold(&self) -> Duration {
        self.hold
    }

    /// Change the hold. An open hole keeps its start, so its deadline moves.
    pub fn set_hold(&mut self, hold: Duration) {
        self.hold = hold;
    }

    /// Whether a hole is open, i.e. [`Reorder::poll`] needs calling on a timer.
    pub fn waiting(&self) -> bool {
        self.hole_since.is_some()
    }

    /// When the open hole expires.
    pub fn deadline(&self) -> Option<Instant> {
        self.hole_since.map(|t| t + self.hold)
    }

    /// Take an arriving packet; `out` receives whatever is now deliverable.
    pub fn push(&mut self, seq: u16, packet: P, now: Instant, out: &mut Vec<Step<P>>) {
        let Some(next) = self.next else {
            self.next = Some(seq.wrapping_add(1));
            self.next_ext += 1;
            out.push(Step::Packet(packet));
            return;
        };
        let ahead = seq.wrapping_sub(next);
        if ahead >= 0x8000 {
            // Behind `next`: already delivered or already given up on.
            self.stats.late_or_duplicate += 1;
            return;
        }
        if ahead == 0 {
            out.push(Step::Packet(packet));
            self.advance(1);
            if self.held.contains_key(&self.next_ext) {
                self.stats.recovered += 1;
                self.last_repair = self.hole_since.map(|t| now.saturating_duration_since(t));
            }
            self.drain(now, out);
            return;
        }
        // Past a hole: park it.
        let key = self.next_ext + u64::from(ahead);
        if self.held.insert(key, (now, packet)).is_some() {
            self.stats.late_or_duplicate += 1;
        }
        self.hole_since.get_or_insert(now);
        if self.held.len() > MAX_HELD {
            self.give_up(now, out);
        }
    }

    /// Give up on a hole that has outlived [`HOLD`]. Call when
    /// [`Reorder::deadline`] passes.
    pub fn poll(&mut self, now: Instant, out: &mut Vec<Step<P>>) {
        while self.hole_since.is_some_and(|t| now.saturating_duration_since(t) >= self.hold) {
            self.give_up(now, out);
        }
    }

    fn advance(&mut self, n: u16) {
        self.next = self.next.map(|s| s.wrapping_add(n));
        self.next_ext += u64::from(n);
    }

    /// Skip to the first held packet, reporting the hole.
    fn give_up(&mut self, now: Instant, out: &mut Vec<Step<P>>) {
        let Some((&first, _)) = self.held.iter().next() else {
            self.hole_since = None;
            return;
        };
        let missing = (first - self.next_ext) as u16;
        self.stats.lost += u64::from(missing);
        self.stats.unrecovered_gaps += 1;
        out.push(Step::Lost(missing));
        self.advance(missing);
        self.drain(now, out);
    }

    /// Deliver the run of held packets that is now contiguous.
    fn drain(&mut self, now: Instant, out: &mut Vec<Step<P>>) {
        while let Some((_, p)) = self.held.remove(&self.next_ext) {
            out.push(Step::Packet(p));
            self.advance(1);
        }
        // Anything still held sits behind a *new* hole, which has been
        // visible — and asked for — since the first packet past it arrived.
        // Its hold runs from then, not from now. Until S49 it ran from now,
        // so under burst loss the holds queued up end to end: on the
        // capacity-drop model thirty holes held the picture for 1.2 s, each
        // waiting its 40 ms only after the one before had given up.
        self.hole_since = self.held.values().next().map(|(at, _)| (*at).min(now));
    }
}

impl<P> Default for Reorder<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// Longest the hold may grow to on a slow link (S49). Past this a keyframe
/// is cheaper than the wait.
pub const MAX_HOLD: Duration = Duration::from_millis(150);

/// Fits [`Reorder`]'s hold to the link (S49).
///
/// [`HOLD`] was sized for a wired LAN, where a retransmission is back in
/// ~12 ms. Over Wi-Fi a NACK and its answer can each sit behind a 30-80 ms
/// radio hold, so a repair often lands after 40 ms — and arrives to find the
/// hole already given up on and a keyframe already requested. That is a
/// direct signal: a retransmission arriving late (`late_or_duplicate`
/// climbing) means the hold is too short, so it grows by half, to
/// [`MAX_HOLD`]. When repairs have all been landing well inside the hold for
/// a while, it shrinks back toward [`HOLD`]. On a wired LAN nothing is ever
/// late and the hold never moves.
pub struct HoldTuner {
    hold: Duration,
    repairs: std::collections::VecDeque<Duration>,
    last_late: u64,
    since_grow: Duration,
    since_review: Duration,
}

const GROW_GAP: Duration = Duration::from_millis(500);
const REVIEW: Duration = Duration::from_secs(5);

impl HoldTuner {
    pub fn new() -> Self {
        Self {
            hold: HOLD,
            repairs: Default::default(),
            last_late: 0,
            since_grow: GROW_GAP,
            since_review: Duration::ZERO,
        }
    }

    pub fn hold(&self) -> Duration {
        self.hold
    }

    pub fn on_repair(&mut self, took: Duration) {
        if self.repairs.len() == 64 {
            self.repairs.pop_front();
        }
        self.repairs.push_back(took);
    }

    /// Advance by `dt` with the reorder buffer's running `late_or_duplicate`
    /// count. Returns the new hold when it changes.
    pub fn tick(&mut self, dt: Duration, late_total: u64) -> Option<Duration> {
        self.since_grow += dt;
        self.since_review += dt;
        let late = late_total.saturating_sub(self.last_late);
        self.last_late = late_total;
        if late > 0 {
            // Still late: no shrinking for a full review period from now.
            self.since_review = Duration::ZERO;
        }
        if late > 0 && self.since_grow >= GROW_GAP && self.hold < MAX_HOLD {
            self.since_grow = Duration::ZERO;
            self.since_review = Duration::ZERO;
            self.hold = (self.hold * 3 / 2).min(MAX_HOLD);
            return Some(self.hold);
        }
        if self.since_review >= REVIEW {
            self.since_review = Duration::ZERO;
            let mut r: Vec<Duration> = self.repairs.iter().copied().collect();
            r.sort();
            let p95 = r.get(r.len() * 95 / 100).copied().unwrap_or(Duration::ZERO);
            if self.hold > HOLD && self.since_grow >= REVIEW && p95 < self.hold * 2 / 5 {
                self.hold = (self.hold * 4 / 5).max(HOLD);
                return Some(self.hold);
            }
        }
        None
    }
}

impl Default for HoldTuner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(r: &mut Reorder<u16>, seqs: &[u16], now: Instant) -> Vec<Step<u16>> {
        let mut out = Vec::new();
        for &s in seqs {
            r.push(s, s, now, &mut out);
        }
        out
    }

    fn packets(steps: &[Step<u16>]) -> Vec<u16> {
        steps.iter().filter_map(|s| if let Step::Packet(p) = s { Some(*p) } else { None }).collect()
    }

    #[test]
    fn in_order_passes_straight_through() {
        let mut r = Reorder::new();
        let out = feed(&mut r, &[10, 11, 12, 13], Instant::now());
        assert_eq!(packets(&out), vec![10, 11, 12, 13]);
        assert!(!r.waiting());
        assert_eq!(r.stats, ReorderStats::default());
    }

    #[test]
    fn a_retransmission_in_time_is_invisible() {
        let t = Instant::now();
        let mut r = Reorder::new();
        let out = feed(&mut r, &[1, 2, 4, 5], t);
        assert_eq!(packets(&out), vec![1, 2], "4 and 5 wait behind the hole");
        assert!(r.waiting());
        let mut out = Vec::new();
        r.push(3, 3, t + Duration::from_millis(12), &mut out);
        assert_eq!(packets(&out), vec![3, 4, 5]);
        assert!(!r.waiting());
        assert_eq!(r.stats.recovered, 1);
        assert_eq!(r.stats.lost, 0);
    }

    #[test]
    fn a_hole_that_outlives_the_hold_is_reported_and_skipped() {
        let t = Instant::now();
        let mut r = Reorder::new();
        feed(&mut r, &[1, 2, 5, 6], t);
        let mut out = Vec::new();
        r.poll(t + HOLD - Duration::from_millis(1), &mut out);
        assert!(out.is_empty(), "not yet");
        r.poll(t + HOLD, &mut out);
        assert_eq!(out, vec![Step::Lost(2), Step::Packet(5), Step::Packet(6)]);
        assert_eq!(r.stats.lost, 2);
        assert_eq!(r.stats.unrecovered_gaps, 1);
        // The retransmission finally turns up: too late, dropped.
        let mut out = Vec::new();
        r.push(3, 3, t + HOLD * 2, &mut out);
        assert!(out.is_empty());
        assert_eq!(r.stats.late_or_duplicate, 1);
    }

    #[test]
    fn a_second_hole_is_held_from_when_it_was_seen() {
        let t = Instant::now();
        let mut r = Reorder::new();
        // 5 arrives at t: from then on 4 is known to be missing too.
        feed(&mut r, &[1, 3, 5], t);
        let mut out = Vec::new();
        let t1 = t + Duration::from_millis(30);
        r.push(2, 2, t1, &mut out);
        assert_eq!(packets(&out), vec![2, 3]);
        assert_eq!(r.deadline(), Some(t + HOLD), "the hole at 4 has been open since t");
    }

    /// S49: burst loss leaves many holes at once. Each must expire HOLD after
    /// it was seen, not HOLD after the previous one gave up, or thirty holes
    /// freeze the picture for thirty holds.
    #[test]
    fn many_holes_expire_together_not_end_to_end() {
        let t = Instant::now();
        let mut r = Reorder::new();
        let mut seqs: Vec<u16> = vec![0];
        // 30 holes: every third packet missing.
        for k in 0..30u16 {
            seqs.push(3 * k + 1);
            seqs.push(3 * k + 2);
        }
        feed(&mut r, &seqs, t);
        let mut out = Vec::new();
        r.poll(t + HOLD, &mut out);
        let lost = out.iter().filter(|s| matches!(s, Step::Lost(_))).count();
        assert_eq!(
            lost, 29,
            "every hole behind a packet given up at once, one hold after it was seen"
        );
        assert!(!r.waiting());
    }

    /// r33 soak: one packet ~565 ahead of the rest counted every in-order
    /// packet after it as a repair. Nothing was lost; nothing was repaired.
    #[test]
    fn one_early_packet_is_not_hundreds_of_repairs() {
        let t = Instant::now();
        let mut r = Reorder::new();
        let mut seqs: Vec<u16> = vec![1, 600];
        seqs.extend(2..600);
        seqs.push(601);
        let out = feed(&mut r, &seqs, t);
        assert_eq!(packets(&out), (1..=601).collect::<Vec<u16>>());
        assert_eq!(r.stats.lost, 0);
        assert!(r.stats.recovered <= 1, "recovered = {}", r.stats.recovered);
    }

    #[test]
    fn duplicates_are_dropped() {
        let t = Instant::now();
        let mut r = Reorder::new();
        let out = feed(&mut r, &[1, 2, 2, 1, 3], t);
        assert_eq!(packets(&out), vec![1, 2, 3]);
        assert_eq!(r.stats.late_or_duplicate, 2);
    }

    #[test]
    fn order_survives_the_u16_wrap() {
        let t = Instant::now();
        let mut r = Reorder::new();
        let out = feed(&mut r, &[65_533, 65_534, 0, 1, 65_535], t);
        assert_eq!(packets(&out), vec![65_533, 65_534, 65_535, 0, 1]);
        assert_eq!(r.stats.recovered, 1);
    }

    #[test]
    fn a_longer_hold_waits_longer_and_repairs_report_their_latency() {
        let t = Instant::now();
        let mut r = Reorder::new();
        r.set_hold(Duration::from_millis(100));
        feed(&mut r, &[1, 2, 4], t);
        let mut out = Vec::new();
        r.poll(t + Duration::from_millis(60), &mut out);
        assert!(out.is_empty(), "40 ms would have given up; 100 does not");
        r.push(3, 3, t + Duration::from_millis(70), &mut out);
        assert_eq!(packets(&out), vec![3, 4]);
        assert_eq!(r.last_repair, Some(Duration::from_millis(70)));
    }

    #[test]
    fn the_hold_grows_on_late_repairs_and_comes_back_when_they_stop() {
        let mut h = HoldTuner::new();
        let tick = Duration::from_millis(250);
        // Wired: repairs at 12 ms, nothing late. Never moves.
        for _ in 0..400 {
            h.on_repair(Duration::from_millis(12));
            assert_eq!(h.tick(tick, 0), None);
        }
        // Wi-Fi: retransmissions keep arriving after the give-up.
        let mut late = 0;
        let mut holds = Vec::new();
        for _ in 0..40 {
            late += 3;
            if let Some(d) = h.tick(tick, late) {
                holds.push(d.as_millis());
            }
        }
        assert_eq!(holds, [60, 90, 135, 150], "grows by half, at most every 500 ms, to the cap");
        // Repairs land fast again: it shrinks back, slowly, to 40.
        for _ in 0..400 {
            h.on_repair(Duration::from_millis(15));
            h.tick(tick, late);
        }
        assert_eq!(h.hold(), HOLD);
    }

    #[test]
    fn a_dead_link_cannot_grow_the_buffer_without_bound() {
        let t = Instant::now();
        let mut r = Reorder::new();
        let mut out = Vec::new();
        r.push(0, 0, t, &mut out);
        for s in 2..(MAX_HELD as u16 + 10) {
            r.push(s, s, t, &mut out);
        }
        assert!(out.contains(&Step::Lost(1)), "the hole was abandoned on size, before its hold");
        assert!(r.held.len() <= MAX_HELD);
    }
}
