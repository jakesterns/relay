//! S49 integration: a whole share's transport, in-process, on loopback,
//! through the Wi-Fi link model.
//!
//! Two real peer connections (ICE, DTLS-SRTP, the NACK and RTCP
//! interceptors, the sized and simulated sockets), the receiver's real video
//! loop (reorder, hold tuner, assembler, keyframe requests, 250 ms
//! feedback), the real playout buffer, and the sender's real adaptation
//! (`SenderAdapt`: delay+loss control, ladder), pacing, keyframe gate and
//! retransmission budget. Only capture and the hardware encoder are
//! replaced, by a synthetic encoder that produces H.264-shaped access units
//! of exactly the size the target bitrate asks for — so this runs anywhere,
//! CI included, with no GPU.
//!
//! The GPU end of the same thing (capture → NVENC → this transport →
//! headless receiver) is `scripts/wifi-sim-check.sh`; both are reported in
//! `docs/plans/S49-wifi-hardening.md`.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::rtp::packetizer::Packetizer as _;
use tokio::sync::mpsc;
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::peer_connection::PeerConnection;

use super::control::SenderAdapt;
use super::ladder::{Ladder, Rung};
use super::netsim::Profile;
use super::playout::{Cadence, Playout};
use super::receiver::{AccessUnit, RecvStats, Report};
use crate::codec::VideoCodec;

/// Targets (s, Mb/s) and rung changes (s, rung), as the sender logged them.
type Log = (Vec<(f64, f64)>, Vec<(f64, Rung)>);

#[derive(Debug, Default)]
struct Outcome {
    frames: usize,
    stalls: usize,
    max_gap_ms: f64,
    judder_ms: f64,
    shown_p99_ms: f64,
    lost: u64,
    gaps: u64,
    recovered: u64,
    withheld: u64,
    keyframes: u64,
    /// (seconds since start, target Mb/s) at every report.
    targets: Vec<(f64, f64)>,
    rungs: Vec<(f64, Rung)>,
    playout_ms_end: f64,
}

impl Outcome {
    fn target_at(&self, secs: f64) -> f64 {
        self.targets.iter().rev().find(|(t, _)| *t <= secs).map_or(0.0, |(_, m)| *m)
    }
    fn min_target_between(&self, a: f64, b: f64) -> f64 {
        self.targets
            .iter()
            .filter(|(t, _)| *t >= a && *t <= b)
            .map(|(_, m)| *m)
            .fold(f64::MAX, f64::min)
    }
}

fn local_ip() -> std::net::IpAddr {
    // The interface a real share would use; nothing is sent to this address.
    super::local_ip_towards("192.0.2.1".parse().unwrap())
        .unwrap_or_else(|_| "127.0.0.1".parse().unwrap())
}

/// One synthetic access unit: the SEI capture stamp the real sender adds,
/// then one H.264 slice of `bytes` (IDR or not).
fn synthetic_au(capture_ns: i64, bytes: usize, idr: bool, n: u64) -> Vec<u8> {
    let mut au = super::sei::timestamp_sei(VideoCodec::H264, capture_ns);
    au.extend_from_slice(&[0, 0, 0, 1, if idr { 0x65 } else { 0x41 }]);
    // Never a zero byte, so nothing in the payload reads as a start code.
    au.extend((0..bytes as u64).map(|i| ((i.wrapping_mul(2_654_435_761) ^ n) as u8) | 1));
    au
}

async fn run_share(sim: Option<Profile>, secs: f64, bitrate_bps: u32, top: Rung) -> Outcome {
    // The synthetic encoder paces frames with sleeps; at the default 15.6 ms
    // timer tick that is judder the test would then measure.
    let _hires = super::pacing::HiResTimer::acquire();
    let ip = local_ip();
    let codecs = [VideoCodec::H264];
    let (spc, mut sev, _srt) = super::build_pc_with(ip, &codecs, sim.clone()).await.unwrap();
    let (rpc, mut rev, rrt) = super::build_pc_with(ip, &codecs, sim).await.unwrap();

    let ssrc = rand::random::<u32>();
    let track = Arc::new(TrackLocalStaticRTP::new(super::sender::video_stream_track(
        Some(VideoCodec::H264),
        ssrc,
    )));
    spc.add_track(track.clone() as Arc<dyn TrackLocal>).await.unwrap();

    // Offer / answer in-process: what the signalling socket carries.
    let offer = spc.create_offer(None).await.unwrap();
    spc.set_local_description(offer).await.unwrap();
    let _ = sev.gather_done.recv().await;
    let offer: RTCSessionDescription = spc.local_description().await.unwrap();
    rpc.set_remote_description(offer).await.unwrap();
    let answer = rpc.create_answer(None).await.unwrap();
    rpc.set_local_description(answer).await.unwrap();
    let _ = rev.gather_done.recv().await;
    let answer = rpc.local_description().await.unwrap();
    spc.set_remote_description(answer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), sev.connected.recv()).await.expect("connected");

    // Receiver: the real video loop, feeding a playout buffer on paper.
    let stats = Arc::new(RecvStats::default());
    let (au_tx, mut au_rx) = mpsc::channel::<AccessUnit>(64);
    let (report_tx, mut report_rx) = mpsc::channel::<Report>(8);
    // The track appears with its first packet, so this waits alongside the
    // encoder below rather than before it.
    {
        let mut tracks = rev.tracks;
        let stats = stats.clone();
        let rrt2 = rrt.clone();
        tokio::spawn(async move {
            let Some(remote) = tracks.recv().await else { return };
            rrt2.spawn(Box::pin(super::receiver::video_track_loop(
                remote,
                stats,
                Arc::new(AtomicI64::new(0)),
                au_tx,
                report_tx,
                true,
            )));
        });
    }
    let shown =
        Arc::new(Mutex::new((Playout::new(), Cadence::default(), crate::Percentiles::default())));
    {
        let shown = shown.clone();
        tokio::spawn(async move {
            while let Some(au) = au_rx.recv().await {
                let Some(cap) = au.capture_local_ns else { continue };
                let mut s = shown.lock().unwrap();
                let at = s.0.on_frame(au.arrival_local_ns as f64 / 1e6, cap as f64 / 1e6);
                s.1.push(at);
                s.2.push_ms(at - cap as f64 / 1e6);
            }
        });
    }

    // Sender: adaptation from the receiver's reports.
    let start = Instant::now();
    let target = Arc::new(AtomicU32::new(bitrate_bps));
    let constrained = Arc::new(AtomicBool::new(false));
    let rung_now = Arc::new(Mutex::new(top));
    let log: Arc<Mutex<Log>> = Default::default();
    {
        let (target, constrained, rung_now, log) =
            (target.clone(), constrained.clone(), rung_now.clone(), log.clone());
        let retx = sev.retransmit_budget.clone();
        let codec = VideoCodec::H264;
        tokio::spawn(async move {
            let mut adapt = SenderAdapt::new(bitrate_bps, Some(Ladder::new(top, codec)));
            while let Some(r) = report_rx.recv().await {
                let Report::Feedback(f) = r else { continue };
                let d = adapt.on_feedback(&f);
                target.store(d.target_bps, Ordering::Relaxed);
                constrained.store(d.constrained, Ordering::Relaxed);
                retx.store(
                    super::feedback::retransmit_budget_for(d.constrained, d.target_bps),
                    Ordering::Relaxed,
                );
                let t = start.elapsed().as_secs_f64();
                let mut l = log.lock().unwrap();
                l.0.push((t, f64::from(d.target_bps) / 1e6));
                if let Some(rung) = d.rung {
                    *rung_now.lock().unwrap() = rung;
                    l.1.push((t, rung));
                }
            }
        });
    }
    let keyframe = Arc::new(AtomicBool::new(true));
    {
        let (keyframe, constrained, track) = (keyframe.clone(), constrained.clone(), track.clone());
        tokio::spawn(async move {
            let mut gate = super::pacing::KeyframeGate::default();
            while let Some(ev) = track.poll().await {
                let TrackLocalEvent::OnRtcpPacket(packets) = ev;
                for p in packets {
                    if p.as_any()
                        .is::<rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication>()
                        && gate.request(Instant::now(), constrained.load(Ordering::Relaxed))
                    {
                        keyframe.store(true, Ordering::Relaxed);
                    }
                }
            }
        });
    }

    // The synthetic encoder: one unit per frame at the rung's rate, sized by
    // the target; keyframes six times a frame, on request.
    let payloader = super::video_codec(VideoCodec::H264).rtp_codec.payloader().unwrap();
    let mut packetizer = rtc::rtp::packetizer::new_packetizer(
        1200,
        VideoCodec::H264.payload_type(),
        ssrc,
        payloader,
        Box::new(rtc::rtp::sequence::new_random_sequencer()),
        90_000,
    );
    let mut pacer = super::pacing::Pacer::new();
    let mut keyframes = 0;
    let mut n = 0u64;
    let mut next = Instant::now();
    while start.elapsed().as_secs_f64() < secs {
        let rung = *rung_now.lock().unwrap();
        let fps = rung.fps.max(1);
        next += Duration::from_secs(1) / fps;
        tokio::time::sleep_until(next.into()).await;
        let t = target.load(Ordering::Relaxed);
        let idr = keyframe.swap(false, Ordering::Relaxed);
        keyframes += u64::from(idr);
        let frame = (t / 8 / fps) as usize;
        let bytes = if idr { frame * 6 } else { frame };
        n += 1;
        let au = synthetic_au(crate::signal_now_ns(), bytes, idr, n);
        let len = au.len();
        let packets = packetizer.packetize(&Bytes::from(au), 90_000 / fps).unwrap();
        let c = constrained.load(Ordering::Relaxed);
        let span = pacer.plan(len, c.then_some(t), fps);
        let t0 = Instant::now();
        if super::sender::send_paced(&track, packets, t0, span).await.is_err() {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    super::close_bounded(&spc, "test sender").await;
    super::close_bounded(&rpc, "test receiver").await;

    let s = shown.lock().unwrap();
    let l = log.lock().unwrap();
    let (_, p99, _) = s.2.summary().unwrap_or((0.0, 0.0, 0.0));
    Outcome {
        frames: s.1.frames(),
        stalls: s.1.stalls(),
        max_gap_ms: s.1.max_gap_ms(),
        judder_ms: s.1.judder_ms(),
        shown_p99_ms: p99,
        lost: stats.video_lost_packets.load(Ordering::Relaxed),
        gaps: stats.video_gaps.load(Ordering::Relaxed),
        recovered: stats.video_recovered.load(Ordering::Relaxed),
        withheld: stats.video_aus_dropped.load(Ordering::Relaxed),
        keyframes,
        targets: l.0.clone(),
        rungs: l.1.clone(),
        playout_ms_end: s.0.target_ms(),
    }
}

const P1440: Rung = Rung { width: 2560, height: 1440, fps: 60 };

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

/// Wired: the regression baseline. Nothing lost, nothing held, the target
/// never moves, the ladder never steps, the playout buffer stays off.
#[test]
fn wired_loopback_is_untouched() {
    let o = rt().block_on(run_share(None, 8.0, 40_000_000, P1440));
    eprintln!("wired: {o:?}");
    assert!(o.frames > 400, "{} frames", o.frames);
    assert_eq!((o.lost, o.gaps, o.withheld), (0, 0, 0));
    assert_eq!(o.stalls, 0);
    assert!(o.targets.iter().all(|(_, m)| *m == 40.0), "target moved on a clean link");
    assert!(o.rungs.is_empty());
    assert_eq!(o.playout_ms_end, 0.0, "no jitter worth buffering");
    // Playout off means frames are shown exactly as they arrive: no added
    // delay and no smoothing, as before S49.
    assert_eq!(o.keyframes, 1, "only the first: nothing asked for another");
    assert!(o.shown_p99_ms < 50.0, "p99 {}", o.shown_p99_ms);
}

/// A good 5 GHz link: loss is repaired, jitter is smoothed, the share stays
/// at full rate and shows every frame on time.
#[test]
fn good_wifi_stays_smooth_at_full_rate() {
    let o = rt().block_on(run_share(Some(Profile::wifi_good()), 15.0, 40_000_000, P1440));
    eprintln!("wifi-good: {o:?}");
    assert!(o.frames > 800);
    assert!(o.stalls <= 2, "{} stalls", o.stalls);
    assert!(o.max_gap_ms < 400.0, "max gap {}", o.max_gap_ms);
    assert!(o.target_at(15.0) >= 32.0, "target {}", o.target_at(15.0));
    assert!(o.shown_p99_ms < 120.0, "p99 {}", o.shown_p99_ms);
    assert!(
        o.recovered >= o.gaps,
        "most holes repaired: {} repaired, {} given up",
        o.recovered,
        o.gaps
    );
    assert!(o.judder_ms < 15.0, "judder {}", o.judder_ms);
}

/// The rate falls from 200 to 15 Mb/s for 8 s (12-20 s in): the target is
/// cut below 15 within about a second, the freeze that costs is bounded, the
/// ladder steps down, and once the link recovers the target climbs again.
#[test]
fn a_capacity_drop_is_cut_quickly_survived_and_recovered_from() {
    let p = Profile::parse("wired,cap=200:15:20:8,queue=150").unwrap();
    let o = rt().block_on(run_share(Some(p), 32.0, 40_000_000, P1440));
    eprintln!("capacity-drop: {o:?}");
    assert!(o.target_at(9.0) >= 39.0, "full rate before the drop: {}", o.target_at(9.0));
    assert!(o.min_target_between(10.0, 14.5) <= 15.0, "cut fast: {:?}", o.targets);
    assert!(o.max_gap_ms < 1500.0, "bounded freeze: {}", o.max_gap_ms);
    assert!(o.stalls <= 4, "{} stalls", o.stalls);
    assert!(!o.rungs.is_empty() && o.rungs[0].1.height < 1440, "stepped down: {:?}", o.rungs);
    assert!(
        o.target_at(32.0) > o.min_target_between(14.0, 20.0) * 1.3,
        "climbing back: {:?}",
        o.targets
    );
}

/// The busy-home profile for a minute: a soak, run by hand
/// (`cargo test -p relay-capture --lib loopback -- --ignored --nocapture`).
#[test]
#[ignore]
fn busy_wifi_soak() {
    let o = rt().block_on(run_share(Some(Profile::wifi_busy()), 60.0, 40_000_000, P1440));
    eprintln!("wifi-busy: {o:?}");
    assert!(o.max_gap_ms < 1500.0);
}
