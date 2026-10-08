//! S49: the adaptation loop on a simulated clock. Deterministic.
//!
//! `loopback_tests` runs the real transport on real sockets, so anything it
//! asserts about *when* things happen depends on how the machine schedules
//! it — the capacity-drop case passed in CI and failed on a loaded PC. The
//! timing claims live here instead: the same link model (`netsim::Link`),
//! the same receiver measurement (`control::FeedbackMeter`), the same sender
//! decisions (`control::SenderAdapt`: delay+loss control and the ladder),
//! the same pacing, keyframe gate and keyframe hold — all driven by one
//! simulated clock, so a run is a pure function of its inputs.
//!
//! What is simplified: no retransmission (a lost packet damages its frame,
//! which then waits for a keyframe, as the real assembler does after the
//! reorder hold gives up), and the encoder hits its target exactly.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::time::{Duration, Instant};

use super::control::{Cause, FeedbackMeter, SenderAdapt, FEEDBACK_INTERVAL};
use super::ladder::{Ladder, Rung};
use super::netsim::{Fate, Link, Profile};
use super::pacing::{KeyframeGate, Pacer};
use super::playout::Cadence;
use super::reorder::HOLD;
use crate::codec::VideoCodec;

const PACKET: usize = 1200;
/// The real receiver stops withholding after this long without a keyframe.
const MAX_WITHHOLD: Duration = Duration::from_secs(1);
/// Signalling and RTCP latency, sender ↔ receiver, outside the model.
const CONTROL_DELAY: Duration = Duration::from_millis(1);

#[derive(Debug, Default)]
struct Run {
    /// (s, target Mb/s) at every report.
    targets: Vec<(f64, f64)>,
    rungs: Vec<(f64, Rung)>,
    lost: u64,
    shown: usize,
    stalls: usize,
    max_gap_ms: f64,
    keyframes: u64,
    causes: Vec<Cause>,
}

impl Run {
    fn target_at(&self, s: f64) -> f64 {
        self.targets.iter().rev().find(|(t, _)| *t <= s).map_or(0.0, |(_, m)| *m)
    }
    fn min_between(&self, a: f64, b: f64) -> f64 {
        self.targets
            .iter()
            .filter(|(t, _)| (a..=b).contains(t))
            .map(|(_, m)| *m)
            .fold(f64::MAX, f64::min)
    }
}

/// Things that happen at a simulated time, ordered by it (then by sequence,
/// so equal times keep their order).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ev {
    /// Packet `idx` of frame `frame` leaves the sender.
    Send { frame: u64, idx: u32 },
    /// It arrives at the receiver (or the receiver gives it up).
    Arrive { frame: u64, idx: u32, lost: bool },
    /// The sender hears a keyframe request.
    KeyframeRequest,
}

struct Frame {
    n: u32,
    capture: Duration,
    keyframe: bool,
    resolved: u32,
    damaged: bool,
}

fn simulate(profile: Profile, secs: f64, bitrate_bps: u32, top: Rung) -> Run {
    let codec = VideoCodec::H264;
    let base = Instant::now(); // only ever offset by simulated durations
    let mut link = Link::new(profile);
    let mut adapt = SenderAdapt::new(bitrate_bps, Some(Ladder::new(top, codec)));
    let mut meter = FeedbackMeter::new(0);
    let mut pacer = Pacer::new();
    let mut gate = KeyframeGate::default();
    let mut cadence = Cadence::default();

    let (mut target, mut constrained, mut rung) = (bitrate_bps, false, top);
    let mut keyframe_wanted = true;
    let mut keyframe_hold_until = Duration::ZERO;
    let mut frames: HashMap<u64, Frame> = HashMap::new();
    let mut next_shown_frame = 0u64;
    let mut awaiting_since: Option<Duration> = None;
    let mut run = Run::default();

    let mut events: BinaryHeap<Reverse<(Duration, u64, Ev)>> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut push = |h: &mut BinaryHeap<Reverse<(Duration, u64, Ev)>>, at: Duration, ev: Ev| {
        seq += 1;
        h.push(Reverse((at, seq, ev)));
    };

    let end = Duration::from_secs_f64(secs);
    let (mut next_frame_at, mut next_report_at) = (Duration::ZERO, FEEDBACK_INTERVAL);
    let mut frame_no = 0u64;
    loop {
        // The next thing to happen: a frame, a report, or a queued event.
        let next_ev = events.peek().map(|Reverse((t, _, _))| *t);
        let now = [Some(next_frame_at), Some(next_report_at), next_ev]
            .into_iter()
            .flatten()
            .min()
            .unwrap();
        if now >= end {
            break;
        }
        let ns = now.as_nanos() as i64;

        if now == next_frame_at {
            let fps = rung.fps.max(1);
            next_frame_at += Duration::from_secs(1) / fps;
            let idr = keyframe_wanted && now >= keyframe_hold_until;
            if idr {
                keyframe_wanted = false;
                run.keyframes += 1;
            }
            let frame_bytes = (target / 8 / fps) as usize;
            let bytes = if idr { frame_bytes * 6 } else { frame_bytes };
            let n = bytes.div_ceil(PACKET).max(1) as u32;
            let span = pacer.plan(bytes, constrained.then_some(target), fps);
            frames.insert(
                frame_no,
                Frame { n, capture: now, keyframe: idr, resolved: 0, damaged: false },
            );
            for idx in 0..n {
                let at = now + span.mul_f64(f64::from(idx) / f64::from(n));
                push(&mut events, at, Ev::Send { frame: frame_no, idx });
            }
            frame_no += 1;
            continue;
        }

        if now == next_report_at {
            next_report_at += FEEDBACK_INTERVAL;
            let f = meter.take(ns);
            let was = target;
            let d = adapt.on_feedback(&f);
            target = d.target_bps;
            constrained = d.constrained;
            // As the real sender: after a queue cut, a forced keyframe waits
            // for the queue to drain.
            if target < was && d.cause == Cause::Queue {
                keyframe_hold_until =
                    now + Duration::from_millis(f.queue_ms.clamp(50.0, 300.0) as u64);
            }
            if let Some(r) = d.rung {
                rung = r;
                // A new encoder starts with a keyframe.
                keyframe_wanted = true;
                run.rungs.push((now.as_secs_f64(), r));
            }
            run.targets.push((now.as_secs_f64(), f64::from(target) / 1e6));
            run.causes.push(d.cause);
            continue;
        }

        let Some(Reverse((_, _, ev))) = events.pop() else { break };
        match ev {
            Ev::Send { frame, idx } => {
                let len = PACKET;
                match link.on_packet(now, len) {
                    Fate::Deliver(at) => {
                        push(&mut events, at, Ev::Arrive { frame, idx, lost: false })
                    }
                    // The receiver gives a hole up one reorder hold after the
                    // packets behind it start arriving.
                    Fate::QueueDrop | Fate::RadioLoss => {
                        push(&mut events, now + HOLD, Ev::Arrive { frame, idx, lost: true })
                    }
                }
            }
            Ev::KeyframeRequest => {
                if gate.request(base + now, constrained) {
                    keyframe_wanted = true;
                }
            }
            Ev::Arrive { frame, idx, lost } => {
                let Some(f) = frames.get_mut(&frame) else { continue };
                if lost {
                    run.lost += 1;
                    meter.on_lost(1);
                    f.damaged = true;
                } else {
                    meter.on_packet(PACKET);
                    if idx == 0 {
                        // The frame head carries the capture stamp.
                        meter.on_unit(ns, Some(f.capture.as_nanos() as i64));
                    }
                }
                f.resolved += 1;
                // Show whole frames in order, as the decoder takes them.
                while let Some(f) = frames.get(&next_shown_frame) {
                    if f.resolved < f.n {
                        break;
                    }
                    let f = frames.remove(&next_shown_frame).unwrap();
                    next_shown_frame += 1;
                    if f.damaged {
                        if awaiting_since.is_none() {
                            awaiting_since = Some(now);
                        }
                        push(&mut events, now + CONTROL_DELAY, Ev::KeyframeRequest);
                        continue;
                    }
                    // Withheld while a keyframe is awaited, up to the limit.
                    if !f.keyframe && awaiting_since.is_some_and(|t| now - t < MAX_WITHHOLD) {
                        continue;
                    }
                    if f.keyframe {
                        awaiting_since = None;
                    }
                    cadence.push(now.as_secs_f64() * 1e3);
                    run.shown += 1;
                }
            }
        }
    }
    run.stalls = cadence.stalls();
    run.max_gap_ms = cadence.max_gap_ms();
    run
}

const P1440: Rung = Rung { width: 2560, height: 1440, fps: 60 };

/// Wired: the target never moves, the ladder never steps, nothing is lost,
/// the picture never pauses. The S49 machinery stays out of the way.
#[test]
fn wired_never_adapts() {
    let r = simulate(Profile::wired(), 60.0, 40_000_000, P1440);
    assert!(r.targets.iter().all(|(_, m)| *m == 40.0), "target moved");
    assert!(r.rungs.is_empty());
    assert_eq!((r.lost, r.stalls, r.keyframes), (0, 0, 1));
    assert!(r.causes.iter().all(|c| *c == Cause::Steady));
    assert!(r.shown >= 3590, "{} frames", r.shown);
}

/// The rate falls 200 → 15 Mb/s at 12 s for 8 s. Cut below 15 within a
/// second, one bounded freeze, a step down the ladder, climbing again after.
#[test]
fn a_capacity_drop_is_cut_quickly_survived_and_recovered_from() {
    let p = Profile::parse("wired,cap=200:15:20:8,queue=150").unwrap();
    let r = simulate(p, 40.0, 40_000_000, P1440);
    eprintln!("targets: {:?}\nrungs: {:?}", r.targets, r.rungs);
    eprintln!(
        "lost {} shown {} stalls {} max gap {:.0} ms keyframes {}",
        r.lost, r.shown, r.stalls, r.max_gap_ms, r.keyframes
    );
    assert_eq!(r.target_at(11.9), 40.0, "full rate before the drop");
    assert!(r.min_between(12.0, 13.0) <= 15.0, "cut within a second: {:?}", r.targets);
    assert!(r.max_gap_ms < 1500.0, "bounded freeze: {}", r.max_gap_ms);
    assert!(r.stalls <= 4, "{} stalls", r.stalls);
    assert!(!r.rungs.is_empty() && r.rungs[0].1.height < 1440, "stepped down: {:?}", r.rungs);
    assert!(r.min_between(14.0, 20.0) >= 2.5, "never below the floor");
    assert!(r.target_at(40.0) > r.min_between(14.0, 20.0) * 1.3, "climbing back: {:?}", r.targets);
}

/// The same run twice is the same run: no wall clock anywhere.
#[test]
fn the_simulation_is_deterministic() {
    let p = || Profile::parse("wifi-busy,seed=11").unwrap();
    let a = simulate(p(), 30.0, 40_000_000, P1440);
    let b = simulate(p(), 30.0, 40_000_000, P1440);
    assert_eq!(a.targets, b.targets);
    assert_eq!(a.rungs, b.rungs);
    assert_eq!((a.lost, a.shown, a.stalls, a.keyframes), (b.lost, b.shown, b.stalls, b.keyframes));
}

/// Busy home Wi-Fi: it adapts, and never freezes past the withholding limit
/// plus a keyframe's trip.
#[test]
fn busy_wifi_keeps_moving() {
    let r = simulate(Profile::wifi_busy(), 90.0, 40_000_000, P1440);
    eprintln!(
        "busy: lost {} shown {} stalls {} max gap {:.0} ms",
        r.lost, r.shown, r.stalls, r.max_gap_ms
    );
    assert!(r.min_between(0.0, 90.0) < 40.0, "it backed off at some point");
    assert!(r.max_gap_ms < 1500.0, "{}", r.max_gap_ms);
    // No frame count: the simulation has no retransmission, so on a lossy
    // profile most frames are damaged here that NACK repairs in a real share.
}
