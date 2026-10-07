//! The receive side: advertise over mDNS, show a pairing code, answer the
//! offer, then depacketize video access units (HEVC or H.264, whichever the
//! answer negotiated) and Opus packets. Rendering and
//! audio playback attach on top (`recv` command); `--headless` just counts
//! and reports latency, which is how the transport is benchmarked.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use tokio::sync::mpsc;
use tracing::{info, warn};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::media_stream::Track;
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
    /// The app window's HWND: create the stream window embedded in it (S29)
    /// rather than as a window of its own.
    pub host: Option<u64>,
    /// Send this process tree's audio — the call app's output, i.e. the
    /// other participants — back to the sender as one more Opus track
    /// (S19). The service sets it from the user's pick; never inferred.
    pub return_pid: Option<u32>,
    /// Play received audio on this endpoint (S40); `None` = the System
    /// default, followed if it changes. `mic_route` wins when both are set.
    pub output_device: Option<String>,
    /// What this same PC is sharing at spawn (S50), from the core's
    /// `--local-share`; later changes come as `local_share` commands. The
    /// stream window is hidden from capture only while this covers it.
    pub local_share: Option<crate::command::SourceTarget>,
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
    /// Frames actually presented to the window. Separate from `video_aus` on
    /// purpose: when a receiver froze mid-share there was no way to tell
    /// whether access units had stopped arriving, stopped decoding, or stopped
    /// reaching the screen. Comparing the two answers that in one line.
    pub video_presented: AtomicU64,
    pub audio_packets: AtomicU64,
    /// Packets on the second (microphone) audio track, 0 when the sender
    /// ships only one.
    pub mic_packets: AtomicU64,
    /// Packets on the rest-of-PC track (S37), 0 when it is not sent.
    pub rest_packets: AtomicU64,
    /// The call audio sent *back* to the sender (S19): packets, and the
    /// peak of the last encoded frame ×1e3. 0 when the route is off.
    pub return_packets: AtomicU64,
    pub return_peak_milli: AtomicU32,
    /// network (+jitter) latency of the last AU: arrival − capture, in µs.
    pub arrival_latency_us_last: AtomicI64,
    /// Presentation timestamp of the last decoded frame, and which slice of
    /// the DXVA texture array it came from. Diagnostics for a freeze that
    /// reports no stall: both counters can climb while the screen does not
    /// change, and these two say whether the decoder stopped advancing or we
    /// kept presenting one surface.
    pub last_pts_100ns: AtomicI64,
    pub last_subresource: AtomicU64,
    /// RTP sequence gaps seen on the video track, and the packets they
    /// swallowed. A gap hands the decoder a damaged access unit; until the
    /// next keyframe the picture smears (B15), while `presented` keeps
    /// climbing as if nothing were wrong. These say when that happened.
    pub video_gaps: AtomicU64,
    pub video_lost_packets: AtomicU64,
    /// Since S30 the two above count only what the reorder buffer gave up
    /// on. This is the other side: holes a retransmission filled in time.
    pub video_recovered: AtomicU64,
    /// Keyframe requests (PLI) sent, and whole frames withheld from the
    /// decoder while waiting for one or because it fell behind.
    pub keyframe_requests: AtomicU64,
    pub video_aus_dropped: AtomicU64,
}

/// Raises the flag when the receive ends, whichever way it ends, so the
/// return-capture thread sees it and lets go of the call app's audio.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Capture the call app's output and encode it for the return track (S19).
/// The program profile, not voice: it carries other people's voices as the
/// call app rendered them, and a second voice-grade pass would only cost.
fn return_pipeline(
    pid: u32,
    tx: mpsc::Sender<(Vec<u8>, Duration)>,
    stats: Arc<RecvStats>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    use crate::audio::{AudioSource, OpusProfile, OpusStream};
    let mut stream = OpusStream::new(AudioSource::Process { pid }, OpusProfile::program())?;
    info!(
        pid,
        rate = stream.endpoint_rate(),
        channels = stream.endpoint_channels(),
        conversion = stream.conversion.as_deref().unwrap_or("none"),
        "return audio pipeline up"
    );
    while !stop.load(Ordering::Relaxed) {
        let Some(p) = stream.next(Duration::from_millis(200))? else { continue };
        stats.return_packets.fetch_add(1, Ordering::Relaxed);
        stats.return_peak_milli.store((stream.peak * 1e3) as u32, Ordering::Relaxed);
        if tx.blocking_send((p.data, p.duration)).is_err() {
            break;
        }
    }
    Ok(())
}

pub async fn run(opts: RecvOpts) -> Result<()> {
    let mut code = opts.code.clone().unwrap_or_else(signal::pairing_code);
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

    // Offer first, so we only build the peer connection for a valid code —
    // or for a PC we remember. A no-code offer from a PC this one does not
    // remember is refused and the receiver keeps waiting on the same code:
    // it must not be able to make the receiver restart and rotate the code
    // the user is reading off the screen. A wrong code still ends the wait
    // (and so rotates the code), which is what keeps guessing expensive.
    // The core's commands, read from here on: while waiting a `stop` ends
    // the wait (the installer's shutdown used to time out here and kill the
    // engine, which read as a crash on every update), and a `host` updates
    // where the stream window will go.
    let own_name = name.clone();
    let mut stdin_lines = crate::render::stdin_lines();
    let mut stdin_open = true;
    let mut host = opts.host;
    // Relay Camera can be switched on or off while waiting (S43b): the
    // choice is carried into the render thread when a sender connects.
    let mut vcam = opts.vcam;
    // What this PC is sharing (S50), kept up to date while waiting so the
    // window is created with the right capture decision.
    let mut local_share = opts.local_share;
    // Where received audio plays (S40). Made here so a pick while waiting
    // for a sender is kept; the virtual-mic route, when set, is the output.
    let output = crate::devices::DeviceSlot::shared(
        opts.mic_route.clone().or_else(|| opts.output_device.clone()),
    );
    let (mut sig, local_ip, offer_json, sender_name, trusted) = loop {
        let (tcp, from) = loop {
            tokio::select! {
                r = listener.accept() => break r?,
                line = stdin_lines.next_line(), if stdin_open => match line {
                    Ok(Some(l)) => match crate::command::parse_line(&l) {
                        Some(crate::command::EngineCmd::Stop) => {
                            info!("stop command received while waiting for a sender");
                            return Ok(());
                        }
                        Some(crate::command::EngineCmd::LocalShare { target }) => {
                            info!(?target, "local share changed while waiting for a sender");
                            local_share = target;
                        }
                        Some(crate::command::EngineCmd::Host { mode, owner, .. }) => {
                            host = match mode {
                                crate::command::HostMode::Embedded if owner != 0 => Some(owner),
                                _ => None,
                            };
                        }
                        Some(crate::command::EngineCmd::Device { track, device }) => {
                            crate::render::apply_device(&output, opts.mic_route.is_some(), track, device);
                        }
                        Some(crate::command::EngineCmd::Vcam { on }) => {
                            info!(on, "vcam command received while waiting for a sender");
                            vcam = on;
                        }
                        _ => {}
                    },
                    _ => stdin_open = false,
                },
            }
        };
        tcp.set_nodelay(true)?;
        let local_ip = tcp.local_addr()?.ip();
        info!(%from, "sender connected");
        let mut sig = signal::SigStream::new(tcp);
        let (offer_json, sender_name, trusted) = match sig.recv().await? {
            signal::SigMsg::Offer { name, sdp, trusted: true, .. } => {
                // No code: the sender says we remember it. Its claim is the
                // fingerprint in its SDP, which anyone could have copied from an
                // earlier exchange — so this decides only whether to *proceed*.
                // DTLS decides whether the claim is true, and "paired" is not
                // reported until it has. That this PC is in Start receiving at
                // all is the consent: remembering removes the code, not the
                // consent (trust model §5, the owner's decision).
                let fp = signal::sdp_fingerprint(&sdp);
                let known =
                    fp.as_deref().and_then(|f| relay_core::peers::recognise(f).ok().flatten());
                match known {
                    Some(peer) => {
                        info!(sender = %name, remembered_as = %peer.name, "remembered PC connecting without a code");
                        (sdp, name, true)
                    }
                    None => {
                        let _ = sig.send(&signal::SigMsg::Bye).await;
                        warn!(
                            sender = %name, %from, fingerprint = fp.as_deref().unwrap_or("none"),
                            "refused: asked to connect without a code, but this PC does not remember it"
                        );
                        println!(
                            "{}",
                            serde_json::json!({ "event": "refused_peer", "name": name })
                        );
                        continue;
                    }
                }
            }
            signal::SigMsg::Offer { name, sdp, mac, trusted: false } => {
                if !signal::verify_mac(&code, &sdp, &mac) {
                    let _ = sig.send(&signal::SigMsg::Bye).await;
                    // Not fatal: one mistyped digit on the other PC used to
                    // stop this one receiving, and any PC on the LAN could
                    // keep it out of receive mode. The wait goes on under a
                    // new code, so a guess still costs the code it guessed.
                    code = signal::pairing_code();
                    warn!(sender = %name, %from, "pairing code mismatch; waiting on a new code");
                    println!("{}", serde_json::json!({ "event": "wrong_code", "name": name }));
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "waiting",
                            "name": own_name,
                            "port": port,
                            "code": code,
                            "codecs": codecs,
                        })
                    );
                    continue;
                }
                // The code binds the SDP, fingerprint included, so this is a
                // consented pairing worth remembering.
                if let Some(fp) = signal::sdp_fingerprint(&sdp) {
                    // Not fatal — the share goes on — but never silent: a store
                    // that cannot be written is why "remembered" would fail next
                    // time (B17 hid behind a `let _ =` here).
                    if let Err(e) = relay_core::peers::remember(
                        &name,
                        &fp,
                        relay_core::peers::Direction::ReceivedFrom,
                    ) {
                        warn!(error = %e, sender = %name, "could not remember this sender");
                    }
                } else {
                    warn!(sender = %name, "offer carries no DTLS fingerprint; nothing to remember");
                }
                (sdp, name, false)
            }
            other => bail!("expected offer, got {other:?}"),
        };
        break (sig, local_ip, offer_json, sender_name, trusted);
    };
    let offer_fp = signal::sdp_fingerprint(&offer_json);

    let (pc, mut events, runtime) = build_pc(local_ip, &codecs).await?;

    // The call going back (S19). Our track must sit on the offer's
    // receive-only audio m-line and nowhere else: a send-only transceiver
    // added *before* the offer is applied is what the library pairs with a
    // remote `recvonly` line (and never with the sender's own audio lines,
    // which want a receive-only partner). An older sender has no such line;
    // then the track is not added at all, because an answer with an extra
    // m-line fails the whole share, not just the return.
    let mut return_track = None;
    if let Some(pid) = opts.return_pid {
        if super::sdp_offers_return(&offer_json) {
            let track = Arc::new(TrackLocalStaticSample::new(super::audio_stream_track(
                "relay-return-stream",
                super::RETURN_TRACK_ID,
                "Relay Call",
            ))?);
            let transceiver = pc
                .add_transceiver_from_track(
                    track.clone() as Arc<dyn TrackLocal>,
                    Some(rtc::rtp_transceiver::RTCRtpTransceiverInit {
                        direction: rtc::rtp_transceiver::RTCRtpTransceiverDirection::Sendonly,
                        streams: vec![],
                        send_encodings: vec![],
                    }),
                )
                .await
                .context("add the return-audio track")?;
            let sender = transceiver
                .sender()
                .await?
                .context("the return-audio transceiver has no sender")?;
            info!(pid, "returning the call app's audio to the sender");
            return_track = Some((track, sender, pid));
        } else {
            warn!(pid, "the sender's Relay predates the return route; the call is not sent back");
        }
    }

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
    // The sender said goodbye, or its end of the signalling socket closed
    // (B8). Either is the end of the share, known at once; ICE takes about
    // four seconds to reach the same conclusion from silence.
    let (gone_tx, mut gone_rx) = mpsc::channel::<()>(1);
    {
        let offset = clock_offset_ns.clone();
        let mut sig = sig;
        tokio::spawn(async move {
            let mut clock_updates = 0u64;
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
                            // The sender re-measures every couple of seconds
                            // (B14); only the first and any real move are news.
                            let moved_ns = offset_ns - offset.load(Ordering::Relaxed);
                            if clock_updates == 0 || moved_ns.abs() > 1_000_000 {
                                info!(
                                    offset_ms = offset_ns as f64 / 1e6,
                                    rtt_ms = rtt_ns as f64 / 1e6,
                                    moved_ms = moved_ns as f64 / 1e6,
                                    clock_updates,
                                    "clock offset from sender"
                                );
                            }
                            clock_updates += 1;
                            offset.store(offset_ns, Ordering::Relaxed);
                        }
                        Ok(signal::SigMsg::Bye) => {
                            info!("sender said goodbye");
                            println!("{}", serde_json::json!({ "event": "sender_stopped" }));
                            let _ = gone_tx.try_send(());
                            break;
                        }
                        Err(e) => {
                            info!(error = %e, "signalling closed; the sender is gone");
                            let _ = gone_tx.try_send(());
                            break;
                        }
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

    if trusted {
        // The fingerprint was only a claim. Only the holder of that
        // certificate's private key can finish DTLS with it, so a trusted
        // pairing is announced when DTLS has — and a PC that presented a
        // remembered identity it could not prove is reported as exactly that,
        // never as "paired". The code path does not wait here: its MAC
        // already bound the SDP, and its timing is what two PCs verified.
        tokio::select! {
            _ = events.connected.recv() => {}
            _ = events.closed.recv() => bail!(
                "`{sender_name}` presented a remembered identity it could not prove; refused"
            ),
            _ = tokio::time::sleep(Duration::from_secs(15)) => bail!(
                "timed out waiting for `{sender_name}` to prove its identity"
            ),
        }
        if let Some(fp) = offer_fp.as_deref() {
            match relay_core::peers::touch(fp, relay_core::peers::Direction::ReceivedFrom) {
                Ok(true) => {}
                Ok(false) => warn!("trusted sender connected but is no longer in the store"),
                Err(e) => warn!(error = %e, "could not update the remembered sender"),
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({ "event": "paired", "sender": sender_name, "trusted": trusted })
    );

    // Feed the return track (S19): process loopback of the call app, Opus,
    // straight onto the track. Best effort — a call app that is not playing
    // delivers no blocks and so no packets, which the stats show as 0.
    let stats = Arc::new(RecvStats::default());
    let return_stop = Arc::new(AtomicBool::new(false));
    if let Some((track, sender, pid)) = return_track {
        let (tx, mut rx) = mpsc::channel::<(Vec<u8>, Duration)>(64);
        let stop = return_stop.clone();
        let stats = stats.clone();
        std::thread::Builder::new().name("relay-return-capture".into()).spawn(move || {
            if let Err(e) = return_pipeline(pid, tx, stats, stop) {
                warn!(error = %e, pid, "return audio capture stopped");
            }
        })?;
        runtime.spawn(Box::pin(async move {
            let Ok(params) = sender.get_parameters().await else { return };
            let Some(pt) = params.rtp_parameters.codecs.first().map(|c| c.payload_type) else {
                return;
            };
            let ssrcs = track.ssrcs().await;
            let Some(&ssrc) = ssrcs.first() else { return };
            while let Some((data, dur)) = rx.recv().await {
                let res = track
                    .sample_writer(ssrc, pt)
                    .write_sample(&rtc::media::Sample {
                        data: bytes::Bytes::from(data),
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
    let _return_guard = StopOnDrop(return_stop);

    // Track fan-out: video AUs and audio packets land on channels.
    // Deep enough to ride out decoder start-up (a few hundred ms) without
    // the track loop dropping units; see `video_track_loop` for why it must
    // never wait here.
    let (au_tx, mut au_rx) = mpsc::channel::<AccessUnit>(64);
    let (opus_tx, mut opus_rx) = mpsc::channel::<Vec<u8>>(64);
    let (mic_tx, mut mic_rx) = mpsc::channel::<Vec<u8>>(64);
    let (rest_tx, mut rest_rx) = mpsc::channel::<Vec<u8>>(64);
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
                            super::AudioRole::Rest => rest_tx.clone(),
                        };
                        runtime2.spawn(Box::pin(async move {
                            while let Some(ev) = track.poll().await {
                                if let TrackRemoteEvent::OnRtpPacket(p) = ev {
                                    match role {
                                        super::AudioRole::Program => {
                                            stats.audio_packets.fetch_add(1, Ordering::Relaxed);
                                        }
                                        super::AudioRole::Rest => {
                                            stats.rest_packets.fetch_add(1, Ordering::Relaxed);
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
                Some(_pkt) = rest_rx.recv() => {}
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
                        "rest_packets": stats2.rest_packets.load(Ordering::Relaxed),
                        "return_packets": stats2.return_packets.load(Ordering::Relaxed),
                        "return_peak": stats2.return_peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
                        "rtp_gaps": stats2.video_gaps.load(Ordering::Relaxed),
                        "rtp_lost": stats2.video_lost_packets.load(Ordering::Relaxed),
                        "rtp_recovered": stats2.video_recovered.load(Ordering::Relaxed),
                        "keyframe_requests": stats2.keyframe_requests.load(Ordering::Relaxed),
                        "frames_withheld": stats2.video_aus_dropped.load(Ordering::Relaxed),
                        "capture_to_arrival_ms": { "p50": p50, "p99": p99, "max": max },
                        // The percentiles are over the whole run, which hides
                        // drift (B14); this is the newest access unit alone.
                        "capture_to_arrival_last_ms":
                            stats2.arrival_latency_us_last.load(Ordering::Relaxed) as f64 / 1e3,
                    }));
                    last_aus = aus;
                }
                _ = events.closed.recv() => {
                    warn!("peer connection closed");
                    break;
                }
                _ = gone_rx.recv() => break,
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        super::close_bounded(&pc, "receiver").await;
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
    let render_opts = crate::render::RenderOpts {
        vcam,
        mic_route: opts.mic_route.clone(),
        host,
        output: output.clone(),
        local_share,
        sender: Some(sender_name.clone()),
    };
    // The render loop ends on either: the transport closing, or the sender
    // going away on the signalling socket.
    let (end_tx, end_rx) = mpsc::channel::<()>(1);
    {
        let mut closed = events.closed;
        tokio::spawn(async move {
            tokio::select! {
                _ = closed.recv() => {}
                _ = gone_rx.recv() => {}
            }
            let _ = end_tx.send(()).await;
        });
    }
    crate::render::run(
        au_rx,
        opus_rx,
        mic_rx,
        rest_rx,
        stats,
        end_rx,
        pc,
        render_opts,
        abort_tx,
        stdin_lines,
    )
    .await
}

/// What the reorder buffer's output does to the access unit being built.
/// Pure, so the loss rules are testable without a peer connection.
struct Assembler {
    depkt: Option<(VideoCodec, super::depay::VideoDepay)>,
    au: Vec<u8>,
    /// Packets with this RTP timestamp belong to a unit that lost a packet.
    discard_ts: Option<u32>,
    /// The first packet after a loss names the unit to discard.
    after_loss: bool,
    /// The reference chain is broken: drop units until a keyframe arrives.
    /// Showing them is what smears the picture; holding the last good frame
    /// for the ~100-200 ms a keyframe takes is the lesser evil.
    await_keyframe: bool,
    /// A keyframe has been asked for and has not arrived. Outlives
    /// `await_keyframe` when the withholding limit is hit: the picture is
    /// shown again, damaged, but the asking goes on.
    need_keyframe: bool,
}

struct Unit {
    codec: VideoCodec,
    data: Vec<u8>,
    rtp_timestamp: u32,
}

/// A whole unit withheld because the stream is waiting for a keyframe.
struct Withheld;

impl Assembler {
    fn new() -> Self {
        Self {
            depkt: None,
            au: Vec::with_capacity(256 * 1024),
            discard_ts: None,
            after_loss: false,
            await_keyframe: false,
            need_keyframe: false,
        }
    }

    /// Packets were given up on: whatever is half-built is damaged.
    fn lost(&mut self) {
        self.au.clear();
        if let Some((codec, d)) = self.depkt.as_mut() {
            *d = super::depay::VideoDepay::new(*codec);
        }
        self.after_loss = true;
        self.break_chain();
    }

    /// The reference chain is broken. Withhold — unless withholding was
    /// already called off while waiting for this same keyframe: on a link bad
    /// enough for that, every further loss would otherwise buy another second
    /// of frozen picture, and the share would be still more often than not.
    fn break_chain(&mut self) {
        if !self.need_keyframe {
            self.await_keyframe = true;
            self.need_keyframe = true;
        }
    }

    /// The consumer could not take a unit: same consequence as a loss.
    fn unit_dropped(&mut self) {
        self.break_chain();
    }

    /// The keyframe is not coming soon enough: show what arrives. A smear
    /// is bad; a picture that has stopped is worse, and on the first two-PC
    /// run of this code it also tripped the no-frames-for-3-s rule and ended
    /// the share.
    fn stop_withholding(&mut self) {
        self.await_keyframe = false;
    }

    fn packet(
        &mut self,
        codec: VideoCodec,
        timestamp: u32,
        marker: bool,
        payload: &[u8],
    ) -> Result<Option<Unit>, Withheld> {
        if self.after_loss {
            // Either the rest of the unit the loss hit, or a unit whose head
            // may have been in the loss. Both are unusable.
            self.after_loss = false;
            self.discard_ts = Some(timestamp);
        }
        if self.discard_ts == Some(timestamp) {
            return Ok(None);
        }
        self.discard_ts = None;
        if self.depkt.as_ref().map(|(c, _)| *c) != Some(codec) {
            info!(codec = codec.label(), "video codec");
            println!("{}", serde_json::json!({ "event": "codec", "codec": codec }));
            self.au.clear();
            self.depkt = Some((codec, super::depay::VideoDepay::new(codec)));
        }
        let Some((_, d)) = self.depkt.as_mut() else { return Ok(None) };
        d.push(payload, &mut self.au);
        if !marker || self.au.is_empty() {
            return Ok(None);
        }
        if self.need_keyframe && codec.is_keyframe(&self.au) {
            self.await_keyframe = false;
            self.need_keyframe = false;
        }
        if self.await_keyframe {
            self.au.clear();
            return Err(Withheld);
        }
        Ok(Some(Unit { codec, data: std::mem::take(&mut self.au), rtp_timestamp: timestamp }))
    }
}

/// While waiting for a keyframe, ask again this often: the request is one
/// unacknowledged RTCP packet and can be lost like anything else.
const KEYFRAME_RETRY: Duration = Duration::from_millis(500);

/// Longest the picture is held still waiting for a keyframe. A request is
/// normally answered in well under 200 ms; a second covers one lost request
/// and its retry. Past that the link is in worse trouble than a freeze can
/// hide, so frames are shown again while the requests continue.
const MAX_WITHHOLD: Duration = Duration::from_secs(1);

/// Reorder, depacketize into access units (marker bit = AU boundary), and ask
/// for a keyframe when a loss could not be repaired (B15).
///
/// This loop must never wait on the decoder. webrtc-rs hands packets over
/// through a 256-slot queue with `try_send`, so while this task is parked the
/// driver silently drops everything past the 256th packet. On the S30 two-PC
/// run that was the actual source of loss, with zero packets lost on the
/// wire. Units go out with `try_send`; a full channel drops the unit and
/// costs a keyframe, which is cheaper than losing half a keyframe of packets.
async fn video_track_loop(
    track: Arc<dyn TrackRemote>,
    stats: Arc<RecvStats>,
    clock_offset_ns: Arc<AtomicI64>,
    au_tx: mpsc::Sender<AccessUnit>,
    loss_tx: mpsc::Sender<f32>,
) {
    use super::reorder::{Reorder, Step};
    use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
    use std::time::Instant;

    let mut reorder = Reorder::new();
    let mut steps = Vec::new();
    let mut asm = Assembler::new();
    let mut unknown_pt_logged = false;
    let mut media_ssrc = 0u32;
    let mut last_keyframe_request: Option<Instant> = None;
    // When the current wait for a keyframe began, to log how long it took.
    let mut key_wait_since: Option<Instant> = None;
    let mut withholding_since: Option<Instant> = None;
    // Unrepaired loss over ~1 s windows, for the sender's bitrate control.
    let mut window_start = Instant::now();
    let (mut window_lost, mut window_delivered) = (0u64, 0u64);

    loop {
        let deadline = reorder.deadline();
        let ev = tokio::select! {
            ev = track.poll() => match ev {
                Some(ev) => Some(ev),
                None => break,
            },
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now).into()),
                if deadline.is_some() => None,
        };
        let now = Instant::now();
        match ev {
            Some(TrackRemoteEvent::OnRtpPacket(pkt)) => {
                media_ssrc = pkt.header.ssrc;
                reorder.push(pkt.header.sequence_number, pkt, now, &mut steps);
            }
            Some(_) => continue,
            None => reorder.poll(now, &mut steps),
        }

        for step in steps.drain(..) {
            let pkt = match step {
                Step::Packet(pkt) => pkt,
                Step::Lost(lost) => {
                    window_lost += u64::from(lost);
                    stats.video_gaps.fetch_add(1, Ordering::Relaxed);
                    stats.video_lost_packets.fetch_add(u64::from(lost), Ordering::Relaxed);
                    warn!(lost, "video packets not recovered in time; waiting for a keyframe");
                    asm.lost();
                    // A fresh loss after the last request -- usually inside the
                    // keyframe that answered it -- means that keyframe is broken
                    // too: ask again now, not at the 500 ms retry (r35: every
                    // double loss took 534 ms against a 17 ms norm). 20 ms keeps
                    // one burst's losses to one request.
                    if last_keyframe_request
                        .is_some_and(|t| now.duration_since(t) >= Duration::from_millis(20))
                    {
                        last_keyframe_request = None;
                    }
                    continue;
                }
            };
            window_delivered += 1;
            let Some(codec) = VideoCodec::from_payload_type(pkt.header.payload_type) else {
                if !unknown_pt_logged {
                    warn!(
                        pt = pkt.header.payload_type,
                        "video packet with an unknown payload type"
                    );
                    unknown_pt_logged = true;
                }
                continue;
            };
            let unit =
                match asm.packet(codec, pkt.header.timestamp, pkt.header.marker, &pkt.payload) {
                    Ok(Some(unit)) => unit,
                    Ok(None) => continue,
                    Err(Withheld) => {
                        stats.video_aus_dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
            stats.video_bytes.fetch_add(unit.data.len() as u64, Ordering::Relaxed);
            stats.video_aus.fetch_add(1, Ordering::Relaxed);
            let capture_local_ns =
                sei::extract_timestamp(unit.codec, &unit.data).map(|sender_ns| {
                    let local = sender_ns + clock_offset_ns.load(Ordering::Relaxed);
                    stats
                        .arrival_latency_us_last
                        .store((signal::unix_now_ns() - local) / 1_000, Ordering::Relaxed);
                    local
                });
            let unit = AccessUnit {
                codec: unit.codec,
                data: unit.data,
                capture_local_ns,
                rtp_timestamp: unit.rtp_timestamp,
            };
            match au_tx.try_send(unit) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    stats.video_aus_dropped.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "the decoder is not keeping up; dropped a frame and asked for a keyframe"
                    );
                    asm.unit_dropped();
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
        stats.video_recovered.store(reorder.stats.recovered, Ordering::Relaxed);

        if asm.await_keyframe {
            let since = *withholding_since.get_or_insert(now);
            if now.duration_since(since) >= MAX_WITHHOLD {
                warn!("no keyframe after a second of asking; showing frames again");
                asm.stop_withholding();
            }
        } else {
            withholding_since = None;
        }

        if asm.need_keyframe {
            key_wait_since.get_or_insert(now);
        }
        if !asm.need_keyframe {
            last_keyframe_request = None;
            if let Some(t) = key_wait_since.take() {
                info!(
                    ms = now.duration_since(t).as_millis() as u64,
                    "keyframe arrived; the picture is clean again"
                );
            }
        } else if last_keyframe_request.is_none_or(|t| now.duration_since(t) >= KEYFRAME_RETRY) {
            last_keyframe_request = Some(now);
            stats.keyframe_requests.fetch_add(1, Ordering::Relaxed);
            let pli = PictureLossIndication { sender_ssrc: 0, media_ssrc };
            if let Err(e) = track.write_rtcp(vec![Box::new(pli)]).await {
                warn!(error = %e, "could not send the keyframe request");
            }
        }

        if window_start.elapsed() >= Duration::from_secs(1) {
            let total = window_lost + window_delivered;
            let fraction = if total > 0 { window_lost as f32 / total as f32 } else { 0.0 };
            let _ = loss_tx.try_send(fraction);
            (window_lost, window_delivered) = (0, 0);
            window_start = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One single-NAL H.264 packet: type 5 is an IDR slice, type 1 is not.
    fn nal(idr: bool) -> [u8; 4] {
        [if idr { 0x65 } else { 0x41 }, 1, 2, 3]
    }

    /// Feed a one-packet unit; `Some(true)` delivered, `Some(false)` withheld.
    fn unit(a: &mut Assembler, ts: u32, idr: bool) -> Option<bool> {
        match a.packet(VideoCodec::H264, ts, true, &nal(idr)) {
            Ok(Some(_)) => Some(true),
            Ok(None) => None,
            Err(Withheld) => Some(false),
        }
    }

    #[test]
    fn after_a_loss_nothing_reaches_the_decoder_until_a_keyframe() {
        let mut a = Assembler::new();
        assert_eq!(unit(&mut a, 1000, true), Some(true));
        assert_eq!(unit(&mut a, 2000, false), Some(true));
        // First half of unit 3000 arrives, then the hole is given up on.
        assert!(matches!(a.packet(VideoCodec::H264, 3000, false, &nal(false)), Ok(None)));
        a.lost();
        assert!(a.await_keyframe);
        // The tail of 3000 — marker and all — is part of the damaged unit.
        assert_eq!(unit(&mut a, 3000, false), None);
        // Whole, but it references the damaged one: withheld, not shown.
        assert_eq!(unit(&mut a, 4000, false), Some(false));
        assert_eq!(unit(&mut a, 5000, true), Some(true), "the keyframe ends it");
        assert!(!a.await_keyframe);
        assert_eq!(unit(&mut a, 6000, false), Some(true));
    }

    #[test]
    fn a_unit_whose_head_may_have_been_lost_is_discarded_even_if_it_is_a_keyframe() {
        let mut a = Assembler::new();
        assert_eq!(unit(&mut a, 1000, true), Some(true));
        a.lost();
        // The loss may have eaten the first packets of this unit; its tail
        // alone would parse as a keyframe and decode as garbage.
        assert_eq!(unit(&mut a, 2000, true), None);
        assert!(a.await_keyframe, "still waiting: the retry timer asks again");
        assert_eq!(unit(&mut a, 3000, true), Some(true));
    }

    #[test]
    fn a_frame_the_decoder_could_not_take_also_waits_for_a_keyframe() {
        let mut a = Assembler::new();
        assert_eq!(unit(&mut a, 1000, true), Some(true));
        a.unit_dropped();
        assert_eq!(unit(&mut a, 2000, false), Some(false));
        assert_eq!(unit(&mut a, 3000, true), Some(true));
    }

    #[test]
    fn withholding_can_be_called_off_without_forgetting_the_keyframe() {
        let mut a = Assembler::new();
        assert_eq!(unit(&mut a, 1000, true), Some(true));
        a.unit_dropped();
        assert_eq!(unit(&mut a, 2000, false), Some(false));
        a.stop_withholding();
        assert_eq!(unit(&mut a, 3000, false), Some(true), "shown, damaged or not");
        assert!(a.need_keyframe, "and the requests go on");
        a.lost();
        assert!(!a.await_keyframe, "a further loss does not freeze the picture again");
        assert_eq!(unit(&mut a, 3500, false), None, "though the damaged unit still goes");
        assert_eq!(unit(&mut a, 4000, true), Some(true));
        assert!(!a.need_keyframe);
    }

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
