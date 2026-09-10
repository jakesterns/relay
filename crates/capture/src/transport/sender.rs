//! The send side of a share: capture → NV12 → HEVC → SEI stamp → webrtc
//! track, plus Opus audio. Emits NDJSON stats on stdout every 500 ms for the
//! core to relay to the instrument strip; `stop` on stdin tears down.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::Track;
use webrtc::peer_connection::PeerConnection;

use super::{audio_codec, build_pc, discovery, sei, signal, video_codec};
use crate::audio::{AudioSource, OpusStream};
use crate::encode::mf::{EncoderConfig, EncoderEvent, MfHevcEncoder};
use crate::time;

pub struct SendOpts {
    /// Receiver instance name (mDNS) or `ip:port`; `None` = first discovered.
    pub peer: Option<String>,
    pub code: String,
    pub bitrate_bps: u32,
    pub fps: u32,
    /// `None` = no audio; `Some(Desktop)` is the default share audio.
    pub audio: Option<AudioSource>,
    pub mic: bool,
    pub cursor: bool,
}

/// Live counters the stats task samples every 500 ms.
#[derive(Default)]
pub struct Stats {
    pub video_bytes: AtomicU64,
    pub video_frames: AtomicU64,
    pub keyframes: AtomicU64,
    pub dropped: AtomicU64,
    pub encode_us_last: AtomicU64,
    pub capture_to_send_us_last: AtomicU64,
    pub audio_packets: AtomicU64,
    /// Audio peak scaled by 1e3.
    pub audio_peak_milli: AtomicU32,
}

struct VideoAu {
    data: Vec<u8>,
    keyframe: bool,
}

/// RAII COM MTA membership for a worker thread.
struct ComMta;
impl ComMta {
    fn init() -> Result<Self> {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        // SAFETY: initialising COM for this thread; balanced in Drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self)
    }
}
impl Drop for ComMta {
    fn drop(&mut self) {
        // SAFETY: balances the CoInitializeEx above.
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

/// Resolve the receiver: explicit `ip:port`, or mDNS by (optional) name.
async fn resolve_peer(peer: &Option<String>) -> Result<(SocketAddr, String)> {
    if let Some(p) = peer {
        if let Ok(sa) = p.parse::<SocketAddr>() {
            return Ok((sa, p.clone()));
        }
    }
    let want = peer.clone();
    let found =
        tokio::task::spawn_blocking(move || discovery::browse(Duration::from_secs(3))).await??;
    if found.is_empty() {
        bail!("no Relay receivers found on the LAN (is the other PC on the Receive screen?)");
    }
    let pick = match &want {
        Some(name) => {
            found.iter().find(|d| d.name.eq_ignore_ascii_case(name)).with_context(|| {
                format!(
                    "receiver `{name}` not found; saw: {:?}",
                    found.iter().map(|d| d.name.clone()).collect::<Vec<_>>()
                )
            })?
        }
        None => &found[0],
    };
    Ok((SocketAddr::new(pick.addr, pick.port), pick.name.clone()))
}

pub async fn run(opts: SendOpts) -> Result<()> {
    let (peer_addr, peer_name) = resolve_peer(&opts.peer).await?;
    info!(%peer_addr, %peer_name, "connecting to receiver");
    let tcp = tokio::net::TcpStream::connect(peer_addr).await.context("signalling connect")?;
    tcp.set_nodelay(true)?;
    let local_ip = tcp.local_addr()?.ip();
    let mut sig = signal::SigStream::new(tcp);

    let (pc, mut events, runtime) = build_pc(local_ip).await?;

    // Video + audio tracks.
    let video_track = Arc::new(TrackLocalStaticSample::new(MediaStreamTrack::new(
        "relay-video-stream".into(),
        "relay-video".into(),
        "Relay Video".into(),
        RtpCodecKind::Video,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(rand::random::<u32>()),
                ..Default::default()
            },
            codec: video_codec().rtp_codec,
            ..Default::default()
        }],
    ))?);
    let video_sender = pc.add_track(video_track.clone() as Arc<dyn TrackLocal>).await?;

    let audio_track = if opts.audio.is_some() {
        let track = Arc::new(TrackLocalStaticSample::new(MediaStreamTrack::new(
            "relay-audio-stream".into(),
            "relay-audio".into(),
            "Relay Audio".into(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(rand::random::<u32>()),
                    ..Default::default()
                },
                codec: audio_codec().rtp_codec,
                ..Default::default()
            }],
        ))?);
        let sender = pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
        Some((track, sender))
    } else {
        None
    };

    // Offer / answer, both MAC'd with the pairing code.
    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;
    let _ = events.gather_done.recv().await;
    let local = pc.local_description().await.context("no local description")?;
    let offer_json = serde_json::to_string(&local)?;
    sig.send(&signal::SigMsg::Offer {
        name: discovery::hostname(),
        sdp: offer_json.clone(),
        mac: signal::mac(&opts.code, &offer_json),
    })
    .await?;

    let answer_json = match sig.recv().await? {
        signal::SigMsg::Answer { name, sdp, mac } => {
            if !signal::verify_mac(&opts.code, &sdp, &mac) {
                bail!("pairing code mismatch — the receiver used a different code");
            }
            if let Some(fp) = signal::sdp_fingerprint(&sdp) {
                let _ = signal::remember_peer(&name, &fp);
            }
            sdp
        }
        other => bail!("expected answer, got {other:?}"),
    };
    let answer: RTCSessionDescription = serde_json::from_str(&answer_json)?;
    pc.set_remote_description(answer).await?;

    let (offset_ns, rtt_ns) = signal::clock_sync(&mut sig, 7).await?;
    info!(offset_ms = offset_ns as f64 / 1e6, rtt_ms = rtt_ns as f64 / 1e6, "clocks synced");

    tokio::select! {
        _ = events.connected.recv() => {}
        _ = events.closed.recv() => bail!("peer connection failed before connecting"),
        _ = tokio::time::sleep(Duration::from_secs(15)) => bail!("timed out waiting for DTLS/ICE"),
    }
    info!("connected; starting media");
    println!(
        "{}",
        serde_json::json!({ "event": "connected", "peer": peer_name, "rtt_ms": rtt_ns as f64 / 1e6 })
    );

    // Warn if the route to the peer leaves over Wi-Fi.
    match super::netcheck::link_kind_for(local_ip) {
        Ok(kind) => {
            println!(
                "{}",
                serde_json::json!({
                    "event": "link",
                    "kind": kind,
                    "recommendation": kind.recommendation(),
                })
            );
            if let Some(rec) = kind.recommendation() {
                warn!("{rec}");
            }
        }
        Err(e) => debug!(error = %e, "link check failed"),
    }

    let stats = Arc::new(Stats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let keyframe_wanted = Arc::new(AtomicBool::new(false));
    // Adaptive target bitrate, read by the pipeline each frame.
    let target_bps = Arc::new(AtomicU32::new(opts.bitrate_bps));

    // Loss feedback from the receiver → AIMD bitrate control.
    {
        let target = target_bps.clone();
        let floor = (opts.bitrate_bps / 6).max(8_000_000); // never below ~8 Mb/s or 1/6 ceiling
        let ceiling = opts.bitrate_bps;
        runtime.spawn(Box::pin(async move {
            loop {
                match sig.recv().await {
                    Ok(signal::SigMsg::Loss { fraction }) => {
                        let cur = target.load(Ordering::Relaxed);
                        let next = if fraction > 0.02 {
                            // Multiplicative decrease on sustained loss.
                            ((cur as f32) * 0.8) as u32
                        } else {
                            // Additive increase (~2 Mb/s) when clean.
                            cur + 2_000_000
                        };
                        target.store(next.clamp(floor, ceiling), Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }));
    }

    // PLI from the receiver → force an IDR.
    {
        let kf = keyframe_wanted.clone();
        let vt = video_track.clone();
        runtime.spawn(Box::pin(async move {
            while let Some(ev) = vt.poll().await {
                let TrackLocalEvent::OnRtcpPacket(packets) = ev;
                for p in packets {
                    if p.as_any()
                        .downcast_ref::<rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication>()
                        .is_some()
                    {
                        kf.store(true, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    // Video pipeline thread (blocking): capture → convert → encode → channel.
    let (vtx, mut vrx) = mpsc::channel::<VideoAu>(4);
    let video_join = {
        let stats = stats.clone();
        let stop = stop.clone();
        let kf = keyframe_wanted.clone();
        let bitrate = opts.bitrate_bps;
        let fps = opts.fps;
        let cursor = opts.cursor;
        let target = target_bps.clone();
        std::thread::Builder::new().name("relay-video-pipeline".into()).spawn(move || {
            if let Err(e) = video_pipeline(bitrate, fps, cursor, vtx, stats, stop, kf, target) {
                warn!(error = %e, "video pipeline stopped");
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "where": "video", "message": e.to_string() })
                );
            }
        })?
    };

    // Audio thread: WASAPI → Opus → channel.
    let (atx, mut arx) = mpsc::channel::<(Vec<u8>, Duration)>(16);
    let audio_join = if let Some(source) = opts.audio.clone() {
        let stats = stats.clone();
        let stop = stop.clone();
        Some(std::thread::Builder::new().name("relay-audio-pipeline".into()).spawn(move || {
            if let Err(e) = audio_pipeline(source, atx, stats, stop) {
                warn!(error = %e, "audio pipeline stopped");
            }
        })?)
    } else {
        None
    };

    // Async writers: channels → tracks.
    {
        let track = video_track.clone();
        let sender = video_sender.clone();
        let stats = stats.clone();
        let frame = Duration::from_micros(1_000_000 / opts.fps as u64);
        runtime.spawn(Box::pin(async move {
            let Ok(params) = sender.get_parameters().await else { return };
            let Some(pt) = params.rtp_parameters.codecs.first().map(|c| c.payload_type) else {
                return;
            };
            let ssrcs = track.ssrcs().await;
            let Some(&ssrc) = ssrcs.first() else { return };
            while let Some(au) = vrx.recv().await {
                let n = au.data.len() as u64;
                let t0 = std::time::Instant::now();
                let res = track
                    .sample_writer(ssrc, pt)
                    .write_sample(&Sample {
                        data: Bytes::from(au.data),
                        duration: frame,
                        ..Default::default()
                    })
                    .await;
                if t0.elapsed() > Duration::from_millis(30) {
                    tracing::debug!(ms = t0.elapsed().as_millis() as u64, "slow write_sample");
                }
                if let Err(e) = res {
                    tracing::warn!(error = %e, "write_sample failed");
                    break;
                }
                stats.video_bytes.fetch_add(n, Ordering::Relaxed);
                stats.video_frames.fetch_add(1, Ordering::Relaxed);
                if au.keyframe {
                    stats.keyframes.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    if let Some((track, sender)) = audio_track {
        runtime.spawn(Box::pin(async move {
            let Ok(params) = sender.get_parameters().await else { return };
            let Some(pt) = params.rtp_parameters.codecs.first().map(|c| c.payload_type) else {
                return;
            };
            let ssrcs = track.ssrcs().await;
            let Some(&ssrc) = ssrcs.first() else { return };
            while let Some((data, dur)) = arx.recv().await {
                let res = track
                    .sample_writer(ssrc, pt)
                    .write_sample(&Sample {
                        data: Bytes::from(data),
                        duration: dur,
                        ..Default::default()
                    })
                    .await;
                if res.is_err() {
                    break;
                }
            }
        }));
    }

    // Stats every 500 ms + stdin stop + connection watch.
    let mut meter = relay_core::footprint::FootprintMeter::new();
    meter.sample();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    let mut stdin_lines = {
        use tokio::io::AsyncBufReadExt;
        tokio::io::BufReader::new(tokio::io::stdin()).lines()
    };
    let mut last_bytes = 0u64;
    let mut last_frames = 0u64;
    // When the core spawned us, a closed stdin means the core died and this
    // process must not outlive it. A manual CLI run has no stdin to watch.
    let spawned_by_core = std::env::var("RELAY_SPAWNED").is_ok();
    let mut stdin_open = true;
    let result: Result<()> = loop {
        tokio::select! {
            _ = ticker.tick() => {
                let bytes = stats.video_bytes.load(Ordering::Relaxed);
                let frames = stats.video_frames.load(Ordering::Relaxed);
                let fp = meter.sample();
                println!("{}", serde_json::json!({
                    "event": "stats",
                    "bitrate_mbps": (bytes - last_bytes) as f64 * 8.0 / 0.5 / 1e6,
                    "fps": (frames - last_frames) as f64 / 0.5,
                    "frames": frames,
                    "keyframes": stats.keyframes.load(Ordering::Relaxed),
                    "dropped": stats.dropped.load(Ordering::Relaxed),
                    "encode_ms": stats.encode_us_last.load(Ordering::Relaxed) as f64 / 1e3,
                    "capture_to_send_ms": stats.capture_to_send_us_last.load(Ordering::Relaxed) as f64 / 1e3,
                    "audio_packets": stats.audio_packets.load(Ordering::Relaxed),
                    "audio_peak": stats.audio_peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
                    "cpu_percent": fp.cpu_percent,
                    "rss_mb": fp.rss_bytes as f64 / 1e6,
                }));
                last_bytes = bytes;
                last_frames = frames;
            }
            line = stdin_lines.next_line(), if stdin_open => {
                match line {
                    Ok(Some(l)) if l.trim() == "stop" => break Ok(()),
                    Ok(Some(_)) => {}
                    _ if spawned_by_core => break Ok(()), // core went away
                    _ => stdin_open = false,
                }
            }
            _ = events.closed.recv() => break Err(anyhow::anyhow!("peer connection lost")),
            _ = tokio::signal::ctrl_c() => break Ok(()),
        }
    };

    info!("stopping share");
    stop.store(true, Ordering::Relaxed);
    let _ = video_join.join();
    if let Some(j) = audio_join {
        let _ = j.join();
    }
    // `sig` is owned by the loss-feedback task; closing the peer connection and
    // exiting closes the TCP, which the receiver reads as end-of-share.
    pc.close().await?;
    println!("{}", serde_json::json!({ "event": "stopped" }));
    result
}

/// Blocking pipeline: WGC/DXGI capture → GPU NV12 → HEVC MFT → SEI → channel.
#[allow(clippy::too_many_arguments)]
fn video_pipeline(
    bitrate_bps: u32,
    fps: u32,
    cursor: bool,
    tx: mpsc::Sender<VideoAu>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    keyframe_wanted: Arc<AtomicBool>,
    target_bps: Arc<AtomicU32>,
) -> Result<()> {
    // WGC's free-threaded FrameArrived callbacks are delivered on an MTA
    // threadpool thread; without a process MTA they stop after the first
    // frame. Establish it on this thread for the life of the pipeline.
    // SAFETY: balanced by CoUninitialize when the guard drops.
    let _com = ComMta::init()?;
    let _mf = crate::probe::MediaFoundation::start()?;
    let hmon = crate::d3d::primary_monitor();
    let gpu = crate::d3d::device_for_monitor(hmon)?;
    let mut src = crate::source::create(&gpu, hmon, cursor)?;
    let size = src.size();
    let mut conv = crate::encode::convert::Converter::new(&gpu, size, size)?;
    let enc = MfHevcEncoder::new(
        &gpu,
        &EncoderConfig { width: size.0, height: size.1, fps, bitrate_bps },
    )?;
    info!(encoder = %enc.name, w = size.0, h = size.1, fps, bitrate_bps, "video pipeline up");
    println!(
        "{}",
        serde_json::json!({
            "event": "video_up",
            "encoder": enc.name,
            "width": size.0,
            "height": size.1,
            "fps": fps,
            "bitrate_bps": bitrate_bps,
            "adapter": gpu.adapter_name,
        })
    );

    let mut inflight: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut applied_bps = bitrate_bps;
    while !stop.load(Ordering::Relaxed) {
        match enc.next_event()? {
            EncoderEvent::NeedInput => {
                // Apply any adaptive bitrate change from the loss controller.
                let want = target_bps.load(Ordering::Relaxed);
                if want != applied_bps && enc.set_bitrate(want).is_ok() {
                    applied_bps = want;
                    tracing::debug!(bps = want, "bitrate adjusted");
                }
                let Some(frame) = src.next(Duration::from_millis(250))? else {
                    tracing::debug!("no capture frame in 250ms");
                    continue;
                };
                if keyframe_wanted.swap(false, Ordering::Relaxed) {
                    let _ = enc.request_keyframe();
                }
                let nv12 = conv.convert(&frame.texture)?;
                enc.submit(&nv12, frame.qpc_100ns)?;
                inflight.insert(frame.qpc_100ns, time::qpc_now_100ns());
                stats.dropped.store(src.dropped(), Ordering::Relaxed);
            }
            EncoderEvent::Output(out) => {
                let now_qpc = time::qpc_now_100ns();
                if let Some(t_in) = inflight.remove(&out.pts_100ns) {
                    stats.encode_us_last.store(((now_qpc - t_in) / 10) as u64, Ordering::Relaxed);
                }
                // Capture time on the wall clock, for the receiver's estimate.
                let capture_unix_ns =
                    signal::unix_now_ns() - (now_qpc - out.pts_100ns).max(0) * 100;
                stats
                    .capture_to_send_us_last
                    .store(((now_qpc - out.pts_100ns) / 10).max(0) as u64, Ordering::Relaxed);
                let mut data = sei::timestamp_sei(capture_unix_ns);
                data.extend_from_slice(&out.data);
                let au = VideoAu { data, keyframe: out.keyframe };
                if tx.blocking_send(au).is_err() {
                    break; // writer gone
                }
            }
        }
    }
    Ok(())
}

fn audio_pipeline(
    source: AudioSource,
    tx: mpsc::Sender<(Vec<u8>, Duration)>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut stream = OpusStream::new(source, 160_000)?;
    while !stop.load(Ordering::Relaxed) {
        let Some(p) = stream.next(Duration::from_millis(200))? else { continue };
        stats.audio_packets.fetch_add(1, Ordering::Relaxed);
        stats.audio_peak_milli.store((stream.peak * 1e3) as u32, Ordering::Relaxed);
        if tx.blocking_send((p.data, p.duration)).is_err() {
            break;
        }
    }
    Ok(())
}
