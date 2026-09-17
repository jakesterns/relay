//! The receive side: advertise over mDNS, show a pairing code, answer the
//! offer, then depacketize video access units (HEVC or H.264, whichever the
//! answer negotiated) and Opus packets. Rendering and
//! audio playback attach on top (`recv` command); `--headless` just counts
//! and reports latency, which is how the transport is benchmarked.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use tokio::sync::mpsc;
use tracing::{info, warn};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::PeerConnection;

use super::{build_pc, discovery, sei, signal};
use crate::codec::VideoCodec;

#[derive(Debug)]
pub struct RecvOpts {
    /// mDNS instance name; default = hostname.
    pub name: Option<String>,
    /// Transport benchmark mode: no decode, no window, just stats.
    pub headless: bool,
    /// Print the pairing code (the UI reads it from the NDJSON stream).
    pub code: Option<String>,
    /// Mirror decoded video into the Relay virtual camera (opt-in).
    pub vcam: bool,
    /// Render decoded audio to this endpoint id (interim virtual-mic route).
    pub mic_route: Option<String>,
}

/// One depacketized video access unit.
pub struct AccessUnit {
    /// Which codec the bitstream is, from the RTP payload type.
    pub codec: VideoCodec,
    pub data: Vec<u8>,
    /// Sender capture time mapped to this machine's clock (unix ns), when the
    /// in-band SEI was present.
    pub capture_local_ns: Option<i64>,
    pub rtp_timestamp: u32,
}

impl AccessUnit {
    /// A decode PTS in 100 ns ticks derived from the 90 kHz RTP timestamp.
    pub fn pts_or_zero(&self) -> i64 {
        // 90 kHz → 100 ns ticks: ×(10_000_000/90_000).
        self.rtp_timestamp as i64 * 1000 / 9
    }
}

#[derive(Default)]
pub struct RecvStats {
    pub video_bytes: AtomicU64,
    pub video_aus: AtomicU64,
    pub audio_packets: AtomicU64,
    /// Packets on the second (microphone) audio track, 0 when the sender
    /// ships only one.
    pub mic_packets: AtomicU64,
    /// network (+jitter) latency of the last AU: arrival − capture, in µs.
    pub arrival_latency_us_last: AtomicI64,
}

pub async fn run(opts: RecvOpts) -> Result<()> {
    let code = opts.code.clone().unwrap_or_else(signal::pairing_code);
    let name = opts.name.clone().unwrap_or_else(discovery::hostname);

    // What goes in the answer. Headless never decodes, so it accepts whatever
    // the test hook allows; a real receiver offers only what it can decode.
    let codecs = if opts.headless {
        crate::codec::allowed_codecs()
    } else {
        let _mf = crate::probe::MediaFoundation::start()?;
        crate::probe::decodable_codecs()
    };
    if codecs.is_empty() {
        bail!(
            "this PC has neither an H.264 nor an HEVC decoder, so it cannot show a share. \
             H.264 decode is part of Windows; an N edition needs the free Media Feature Pack"
        );
    }
    info!(codecs = ?codecs, "receiver video codecs");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();
    let _ad = discovery::Advertisement::start(&name, port)?;
    info!(%name, port, "advertising receiver");
    eprintln!("\n  Relay receiver \"{name}\" - pairing code: {code}\n");
    println!(
        "{}",
        serde_json::json!({
            "event": "waiting",
            "name": name,
            "port": port,
            "code": code,
            "codecs": codecs,
        })
    );

    let (tcp, from) = listener.accept().await?;
    tcp.set_nodelay(true)?;
    let local_ip = tcp.local_addr()?.ip();
    info!(%from, "sender connected");
    let mut sig = signal::SigStream::new(tcp);

    // Offer first, so we only build the peer connection for a valid code.
    let (offer_json, sender_name) = match sig.recv().await? {
        signal::SigMsg::Offer { name, sdp, mac } => {
            if !signal::verify_mac(&code, &sdp, &mac) {
                let _ = sig.send(&signal::SigMsg::Bye).await;
                bail!("pairing code mismatch from {from}");
            }
            (sdp, name)
        }
        other => bail!("expected offer, got {other:?}"),
    };
    if let Some(fp) = signal::sdp_fingerprint(&offer_json) {
        let _ = signal::remember_peer(&sender_name, &fp);
    }

    let (pc, mut events, runtime) = build_pc(local_ip, &codecs).await?;
    let offer: RTCSessionDescription = serde_json::from_str(&offer_json)?;
    pc.set_remote_description(offer).await?;
    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer).await?;
    let _ = events.gather_done.recv().await;
    let local = pc.local_description().await.context("no local description")?;
    let answer_json = serde_json::to_string(&local)?;
    sig.send(&signal::SigMsg::Answer {
        name: discovery::hostname(),
        sdp: answer_json.clone(),
        mac: signal::mac(&code, &answer_json),
    })
    .await?;

    // Serve clock pings; the sender pushes its offset estimate when done.
    let clock_offset_ns = Arc::new(AtomicI64::new(0));
    // Loss fractions computed by the video loop, forwarded to the sender.
    let (loss_tx, mut loss_rx) = mpsc::channel::<f32>(4);
    // A fatal render error, forwarded to the sender before we close (B3).
    let (abort_tx, mut abort_rx) = mpsc::channel::<(String, tokio::sync::oneshot::Sender<()>)>(1);
    {
        let offset = clock_offset_ns.clone();
        let mut sig = sig;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    incoming = sig.recv() => match incoming {
                        Ok(signal::SigMsg::Ping { seq, t1_ns }) => {
                            let t2 = signal::unix_now_ns();
                            let msg = signal::SigMsg::Pong {
                                seq,
                                t1_ns,
                                t2_ns: t2,
                                t3_ns: signal::unix_now_ns(),
                            };
                            if sig.send(&msg).await.is_err() {
                                break;
                            }
                        }
                        Ok(signal::SigMsg::Clock { offset_ns, rtt_ns }) => {
                            info!(
                                offset_ms = offset_ns as f64 / 1e6,
                                rtt_ms = rtt_ns as f64 / 1e6,
                                "clock offset from sender"
                            );
                            offset.store(offset_ns, Ordering::Relaxed);
                        }
                        Ok(signal::SigMsg::Bye) | Err(_) => break,
                        Ok(_) => {}
                    },
                    Some(fraction) = loss_rx.recv() => {
                        if sig.send(&signal::SigMsg::Loss { fraction }).await.is_err() {
                            break;
                        }
                    }
                    Some((reason, done)) = abort_rx.recv() => {
                        let _ = sig.send(&signal::SigMsg::Abort { reason }).await;
                        let _ = done.send(());
                        break;
                    }
                }
            }
        });
    }

    println!("{}", serde_json::json!({ "event": "paired", "sender": sender_name }));

    // Track fan-out: video AUs and audio packets land on channels.
    let stats = Arc::new(RecvStats::default());
    let (au_tx, mut au_rx) = mpsc::channel::<AccessUnit>(8);
    let (opus_tx, mut opus_rx) = mpsc::channel::<Vec<u8>>(64);
    let (mic_tx, mut mic_rx) = mpsc::channel::<Vec<u8>>(64);
    {
        let stats = stats.clone();
        let offset = clock_offset_ns.clone();
        let runtime2 = runtime.clone();
        tokio::spawn(async move {
            let mut events_tracks = events.tracks;
            // How many audio tracks have arrived, for the arrival-order
            // fallback in `audio_role`.
            let mut audio_seen = 0usize;
            while let Some(track) = events_tracks.recv().await {
                let kind = track.kind().await;
                info!(?kind, "track arrived");
                match kind {
                    RtpCodecKind::Video => {
                        let stats = stats.clone();
                        let offset = offset.clone();
                        let au_tx = au_tx.clone();
                        let loss_tx = loss_tx.clone();
                        runtime2.spawn(Box::pin(video_track_loop(
                            track, stats, offset, au_tx, loss_tx,
                        )));
                    }
                    _ => {
                        let track_id = track.track_id().await;
                        let role = super::audio_role(&track_id, audio_seen);
                        audio_seen += 1;
                        info!(%track_id, ?role, "audio track arrived");
                        let stats = stats.clone();
                        let tx = match role {
                            super::AudioRole::Program => opus_tx.clone(),
                            super::AudioRole::Mic => mic_tx.clone(),
                        };
                        runtime2.spawn(Box::pin(async move {
                            while let Some(ev) = track.poll().await {
                                if let TrackRemoteEvent::OnRtpPacket(p) = ev {
                                    match role {
                                        super::AudioRole::Program => {
                                            stats.audio_packets.fetch_add(1, Ordering::Relaxed);
                                        }
                                        super::AudioRole::Mic => {
                                            stats.mic_packets.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                    if tx.send(p.payload.to_vec()).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }));
                    }
                }
            }
        });
    }

    // Consumers: headless = drain and report; full mode adds decode+present
    // and audio playback (attached by the recv command).
    let stats2 = stats.clone();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    let mut lat = crate::Percentiles::default();
    let mut last_aus = 0u64;
    if opts.headless {
        loop {
            tokio::select! {
                Some(au) = au_rx.recv() => {
                    if let Some(ts) = au.capture_local_ns {
                        lat.push_ms((signal::unix_now_ns() - ts) as f64 / 1e6);
                    }
                }
                Some(_pkt) = opus_rx.recv() => {}
                Some(_pkt) = mic_rx.recv() => {}
                _ = ticker.tick() => {
                    let aus = stats2.video_aus.load(Ordering::Relaxed);
                    let (p50, p99, max) = lat.summary().unwrap_or((0.0, 0.0, 0.0));
                    println!("{}", serde_json::json!({
                        "event": "stats",
                        "aus": aus,
                        "fps": (aus - last_aus) as f64 / 0.5,
                        "video_bytes": stats2.video_bytes.load(Ordering::Relaxed),
                        "audio_packets": stats2.audio_packets.load(Ordering::Relaxed),
                        "mic_packets": stats2.mic_packets.load(Ordering::Relaxed),
                        "capture_to_arrival_ms": { "p50": p50, "p99": p99, "max": max },
                    }));
                    last_aus = aus;
                }
                _ = events.closed.recv() => {
                    warn!("peer connection closed");
                    break;
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        pc.close().await?;
        let (p50, p99, max) = lat.summary().unwrap_or((0.0, 0.0, 0.0));
        println!(
            "{}",
            serde_json::json!({
                "event": "summary",
                "aus": stats2.video_aus.load(Ordering::Relaxed),
                "capture_to_arrival_ms": { "p50": p50, "p99": p99, "max": max, "samples": lat.len() },
            })
        );
        return Ok(());
    }

    // Full receive mode is attached by the caller (decode + present + audio).
    let render_opts =
        crate::render::RenderOpts { vcam: opts.vcam, mic_route: opts.mic_route.clone() };
    crate::render::run(au_rx, opus_rx, mic_rx, stats, events.closed, pc, render_opts, abort_tx)
        .await
}

/// Depacketize one video track into access units (marker bit = AU boundary).
async fn video_track_loop(
    track: Arc<dyn TrackRemote>,
    stats: Arc<RecvStats>,
    clock_offset_ns: Arc<AtomicI64>,
    au_tx: mpsc::Sender<AccessUnit>,
    loss_tx: mpsc::Sender<f32>,
) {
    use super::depay::VideoDepay;

    // Built from the first packet's payload type, and rebuilt if it changes.
    let mut depkt: Option<(VideoCodec, VideoDepay)> = None;
    let mut unknown_pt_logged = false;
    let mut au: Vec<u8> = Vec::with_capacity(256 * 1024);
    // RTP-sequence loss estimation over ~1 s windows.
    let mut loss = super::control::LossWindow::default();
    let mut window_start = std::time::Instant::now();
    while let Some(ev) = track.poll().await {
        let TrackRemoteEvent::OnRtpPacket(pkt) = ev else { continue };
        loss.push(pkt.header.sequence_number);
        if window_start.elapsed() >= std::time::Duration::from_secs(1) {
            let _ = loss_tx.try_send(loss.take_fraction());
            window_start = std::time::Instant::now();
        }
        let Some(codec) = VideoCodec::from_payload_type(pkt.header.payload_type) else {
            if !unknown_pt_logged {
                warn!(pt = pkt.header.payload_type, "video packet with an unknown payload type");
                unknown_pt_logged = true;
            }
            continue;
        };
        if depkt.as_ref().map(|(c, _)| *c) != Some(codec) {
            info!(codec = codec.label(), "video codec");
            println!("{}", serde_json::json!({ "event": "codec", "codec": codec }));
            au.clear();
            depkt = Some((codec, VideoDepay::new(codec)));
        }
        let Some((_, d)) = depkt.as_mut() else { continue };
        d.push(&pkt.payload, &mut au);
        if pkt.header.marker && !au.is_empty() {
            stats.video_bytes.fetch_add(au.len() as u64, Ordering::Relaxed);
            stats.video_aus.fetch_add(1, Ordering::Relaxed);
            let capture_local_ns = sei::extract_timestamp(codec, &au).map(|sender_ns| {
                let local = sender_ns + clock_offset_ns.load(Ordering::Relaxed);
                stats
                    .arrival_latency_us_last
                    .store((signal::unix_now_ns() - local) / 1_000, Ordering::Relaxed);
                local
            });
            let unit = AccessUnit {
                codec,
                data: std::mem::take(&mut au),
                capture_local_ns,
                rtp_timestamp: pkt.header.timestamp,
            };
            if au_tx.send(unit).await.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pts_converts_90khz_to_100ns_ticks() {
        // One second of 90 kHz clock = 10^7 100-ns ticks.
        let au = AccessUnit {
            codec: VideoCodec::Hevc,
            data: vec![],
            capture_local_ns: None,
            rtp_timestamp: 90_000,
        };
        assert_eq!(au.pts_or_zero(), 10_000_000);
        // One 60 fps frame = 1500 ticks of 90 kHz = 166_666 (truncated) 100-ns ticks.
        let au = AccessUnit {
            codec: VideoCodec::Hevc,
            data: vec![],
            capture_local_ns: None,
            rtp_timestamp: 1_500,
        };
        assert_eq!(au.pts_or_zero(), 166_666);
        let au = AccessUnit {
            codec: VideoCodec::Hevc,
            data: vec![],
            capture_local_ns: None,
            rtp_timestamp: 0,
        };
        assert_eq!(au.pts_or_zero(), 0);
        // u32::MAX must not overflow the i64 math.
        let au = AccessUnit {
            codec: VideoCodec::Hevc,
            data: vec![],
            capture_local_ns: None,
            rtp_timestamp: u32::MAX,
        };
        assert_eq!(au.pts_or_zero(), u32::MAX as i64 * 1000 / 9);
    }
}
