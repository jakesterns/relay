//! The send side of a share: capture → NV12 → HEVC or H.264 (negotiated) →
//! SEI stamp → webrtc track, plus Opus audio. Emits NDJSON stats on stdout every 500 ms for the
//! core to relay to the instrument strip; `stop` on stdin tears down.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::Track;
use webrtc::peer_connection::PeerConnection;

use super::{audio_codec, build_pc, discovery, sei, signal};
use crate::audio::{AudioSource, OpusProfile, OpusStream};
use crate::codec::{self, VideoCodec};
use crate::command::{self, EngineCmd, SourceTarget};
use crate::encode::mf::{EncoderConfig, EncoderEvent, MfEncoder};
use crate::record::{budget::DiskBudget, RecordConfig, Recorder};
use crate::source::switch::{self, Switcher};
use crate::time;
use relay_core::share::RecordingContainer;
use std::path::PathBuf;
use std::sync::{Mutex as StdMutex, OnceLock};

/// r54: before a share captures one process's audio, check the PID is still
/// the process the core pinned. A recycled PID would send another app's
/// sound to the receiver, unasked; then the share goes on without program
/// audio (never the whole desktop mix instead, which could carry a call) and
/// the core is told why.
pub fn guard_audio_pid(opts: &mut SendOpts) {
    let Some(crate::audio::AudioSource::Process { pid }) = opts.audio else { return };
    if let Err(why) =
        relay_core::proc_identity::target_ok(pid, opts.audio_image.as_deref(), opts.audio_created)
    {
        warn!(pid, why, "the shared app's audio is not captured");
        println!("{}", serde_json::json!({ "event": "audio_off", "reason": why }));
        opts.audio = None;
        opts.rest = false;
    }
}

#[derive(Debug)]
pub struct SendOpts {
    /// Receiver instance name (mDNS) or `ip:port`; `None` = first discovered.
    pub peer: Option<String>,
    /// Six-digit code from the receiver. Empty when `trusted` is set.
    pub code: String,
    /// Connect without a code, as a PC the receiver remembers (S35). The
    /// value is the receiver's expected DTLS fingerprint: the answer must
    /// carry it, or we are talking to the wrong machine and stop before any
    /// media flows. The core resolves this from a peer id; the engine never
    /// chooses it.
    pub trusted: Option<String>,
    pub bitrate_bps: u32,
    pub fps: u32,
    /// The program-mix track: `None` = no program audio; `Some(Desktop)` is
    /// the default share audio, `Some(Process)` game-only.
    pub audio: Option<AudioSource>,
    /// Also send the default microphone as a *second* Opus track, alongside
    /// the program mix rather than instead of it. The receiver sums them;
    /// see `docs/dev/dual-audio-decision.md`.
    pub mic: bool,
    /// Also send everything on the PC *except* the shared app, as a third
    /// track (S37). Only meaningful with a `Process` program source; with
    /// the desktop mix there is no "rest", and it is ignored.
    pub rest: bool,
    pub cursor: bool,
    /// Encode size cap `(w, h)`; `None` = native capture size. The capture is
    /// GPU-scaled, so changing source never renegotiates the connection.
    pub size: Option<(u32, u32)>,
    /// Recording folder; `None` disables recording and the replay ring.
    pub record_dir: Option<PathBuf>,
    pub container: RecordingContainer,
    /// Start continuous recording as soon as the share is up.
    pub record: bool,
    /// Replay ring window; 0 = ring off.
    pub replay_secs: u32,
    /// Emit a JPEG thumbnail of the capture this many times a second, as
    /// preview events on stdout, so the app window can show what is being
    /// shared. 0 = off, which costs nothing at all.
    pub preview_fps: u32,
    /// Also present the captured picture as "Relay Camera" on this PC (S36),
    /// so a streaming program here can pick it as a webcam. Video only: the
    /// program captures the game's audio itself. The core decides this from
    /// the preset, consent and registration; the engine never chooses it.
    pub vcam: bool,
    /// Also publish the share as an NDI® source, "Relay share" (S51). A
    /// `ndi` command changes it live.
    pub ndi: bool,
    /// The microphone endpoint id (S40); `None` = the System default, which
    /// the track then follows if it changes mid-share.
    pub mic_device: Option<String>,
    /// Where the call coming back plays (S40); `None` = the System default.
    pub output_device: Option<String>,
    /// Which process an `AudioSource::Process` PID must be (r54): image path
    /// and creation time from the core. See [`guard_audio_pid`].
    pub audio_image: Option<String>,
    pub audio_created: Option<u64>,
    /// What to capture from the first frame; `None` = the primary display.
    /// A reconnect resumes the source the share was on (r59: it restarted on
    /// the desktop after the user had switched to one window). A source that
    /// cannot be opened ends the video pipeline; it never falls back to the
    /// desktop.
    pub source: Option<SourceTarget>,
}

/// Rate-limited JPEG thumbnails of the capture, emitted as `preview` events.
///
/// Off unless `preview_fps > 0`, and even then it only touches the GPU once
/// per interval — the share path is on a latency budget and a preview is
/// worth nothing if it costs frames. A failure here is logged once and then
/// the tap disables itself: a thumbnail is never a reason to interrupt a
/// share that is otherwise working.
/// The rate is read from a shared cell on every frame rather than captured at
/// construction, so Ctrl+Alt+P (a `preview` command on stdin) can turn the tap
/// on and off mid-share without disturbing the encoder.
struct PreviewTap {
    fps: Arc<AtomicU32>,
    last: Option<Instant>,
    inner: Option<crate::preview::Preview>,
    failed: bool,
}

impl PreviewTap {
    fn new(fps: Arc<AtomicU32>) -> Self {
        Self { fps, last: None, inner: None, failed: false }
    }

    /// Drop the scaler so the next frame rebuilds it at the new source size.
    fn invalidate(&mut self) {
        self.inner = None;
    }

    fn interval(&self) -> Option<Duration> {
        let fps = self.fps.load(Ordering::Relaxed);
        (fps > 0).then(|| Duration::from_micros(1_000_000 / fps.min(30) as u64))
    }

    fn due(&self) -> bool {
        match (self.interval(), self.last) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(i), Some(last)) => last.elapsed() >= i,
        }
    }
}

/// One preview frame, if one is due. Errors disable the tap rather than
/// propagating: see [`PreviewTap`].
fn emit_preview(
    tap: &mut PreviewTap,
    gpu: &crate::d3d::Gpu,
    texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    in_size: (u32, u32),
) {
    if tap.failed {
        return;
    }
    if tap.interval().is_none() {
        // Switched off mid-share: give the scaler and its staging texture back.
        tap.inner = None;
        return;
    }
    if !tap.due() {
        return;
    }
    tap.last = Some(Instant::now());

    if tap.inner.is_none() {
        match crate::preview::Preview::new(gpu, in_size) {
            Ok(p) => tap.inner = Some(p),
            Err(e) => {
                warn!(error = %e, "preview disabled");
                tap.failed = true;
                return;
            }
        }
    }
    let Some(p) = tap.inner.as_mut() else { return };
    match p.jpeg(gpu, texture) {
        Ok(jpeg) => {
            use base64::Engine as _;
            let (w, h) = p.size();
            println!(
                "{}",
                serde_json::json!({
                    "event": "preview",
                    "width": w,
                    "height": h,
                    "jpeg": base64::engine::general_purpose::STANDARD.encode(&jpeg),
                })
            );
        }
        Err(e) => {
            warn!(error = %e, "preview frame failed; disabling");
            tap.failed = true;
        }
    }
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
    /// Counters for the mic track, when a second track is being sent.
    pub mic_packets: AtomicU64,
    pub mic_peak_milli: AtomicU32,
    /// Counters for the rest-of-PC track (S37), when it is being sent.
    pub rest_packets: AtomicU64,
    pub rest_peak_milli: AtomicU32,
    /// Frames handed to "Relay Camera" on this PC (S36); 0 when it is off.
    pub vcam_frames: AtomicU64,
    /// The call coming back from the receiver (S19): packets received, and
    /// the peak of what was played here. Both 0 until the receiver sends it.
    pub return_packets: AtomicU64,
    pub return_peak_milli: AtomicU32,
}

/// What adaptation decided, shared between the feedback task (writes), the
/// pipeline (rung), the writer (pacing), the PLI handler and the stats line.
#[derive(Default)]
pub struct AdaptState {
    /// The bitrate controller has backed off or saw congestion lately.
    pub constrained: AtomicBool,
    /// `control::Cause as u8`.
    pub cause: std::sync::atomic::AtomicU8,
    /// A rung the pipeline should switch the encoder to.
    pub rung_wanted: StdMutex<Option<super::ladder::Rung>>,
    /// The rung the encoder runs at now; the first one is the share's top.
    pub rung_now: StdMutex<Option<super::ladder::Rung>>,
    pub top: OnceLock<super::ladder::Rung>,
    /// Encoder rebuilds for a rung change, and how long the last one took.
    pub rung_changes: AtomicU64,
    pub rebuild_ms_last: AtomicU64,
    /// No forced keyframe before this: a queue is draining (S49).
    keyframe_hold_until: StdMutex<Option<Instant>>,
}

impl AdaptState {
    pub fn hold_keyframes_for(&self, d: Duration) {
        *self.keyframe_hold_until.lock().unwrap() = Some(Instant::now() + d);
    }

    /// Take a pending keyframe request, unless a hold says not yet; a held
    /// request stays pending and is taken once the hold ends.
    pub fn take_keyframe(&self, wanted: &AtomicBool) -> bool {
        let held = self.keyframe_hold_until.lock().unwrap().is_some_and(|t| Instant::now() < t);
        !held && wanted.swap(false, Ordering::Relaxed)
    }

    pub fn cause(&self) -> super::control::Cause {
        use super::control::Cause;
        match self.cause.load(Ordering::Relaxed) {
            x if x == Cause::Queue as u8 => Cause::Queue,
            x if x == Cause::Loss as u8 => Cause::Loss,
            x if x == Cause::Recovering as u8 => Cause::Recovering,
            _ => Cause::Steady,
        }
    }
}

struct VideoAu {
    data: Vec<u8>,
    keyframe: bool,
    /// RTP clock ticks this unit lasts: 90 kHz / the fps it was encoded at.
    samples: u32,
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

/// Pick a discovered receiver: by (case-insensitive) name when given, else
/// the first found. Pure selection so it is unit-testable.
fn pick_discovered<'a>(
    found: &'a [discovery::Discovered],
    want: &Option<String>,
) -> Result<&'a discovery::Discovered> {
    if found.is_empty() {
        bail!("no Relay receivers found on the LAN (is the other PC on the Receive screen?)");
    }
    match want {
        Some(name) => found.iter().find(|d| d.name.eq_ignore_ascii_case(name)).with_context(|| {
            format!(
                "receiver `{name}` not found; saw: {:?}",
                found.iter().map(|d| d.name.clone()).collect::<Vec<_>>()
            )
        }),
        None => Ok(&found[0]),
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
    let pick = pick_discovered(&found, &want)?;
    Ok((SocketAddr::new(pick.addr, pick.port), pick.name.clone()))
}

/// msid track ids for the two audio tracks. These are the wire contract the
/// receiver classifies on; see [`super::audio_role`].
pub const PROGRAM_TRACK_ID: &str = "relay-audio";
pub const MIC_TRACK_ID: &str = "relay-audio-mic";
/// Everything on the sending PC except the shared app (S37).
pub const REST_TRACK_ID: &str = "relay-audio-rest";

type AudioTrackPair = (Arc<TrackLocalStaticSample>, Arc<dyn webrtc::rtp_transceiver::RtpSender>);

/// Add one Opus track to the peer connection and return it with its sender.
async fn add_audio_track(
    pc: &impl PeerConnection,
    stream_id: &str,
    track_id: &str,
    label: &str,
) -> Result<AudioTrackPair> {
    let track = Arc::new(TrackLocalStaticSample::new(MediaStreamTrack::new(
        stream_id.into(),
        track_id.into(),
        label.into(),
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
    Ok((track, sender))
}

/// Video codecs this sender can offer, in preference order: the ones the
/// primary monitor's GPU has a hardware encoder for, narrowed by
/// `RELAY_VIDEO_CODECS`. Enumeration only — no encoder is created here.
fn offerable_codecs() -> Result<Vec<VideoCodec>> {
    let _mf = crate::probe::MediaFoundation::start()?;
    let (luid, adapter) = crate::d3d::adapter_luid_for_monitor(crate::d3d::primary_monitor())?;
    let offered = codec::filter_supported(&codec::allowed_codecs(), |c| {
        crate::encode::mf::hardware_encoder_available(c, luid)
    });
    if offered.is_empty() {
        bail!(crate::encode::mf::no_encoder_error(&adapter));
    }
    Ok(offered)
}

/// The one video track's description. The SSRC and ids stay fixed so setting
/// the codec after the answer is a `replace_track`, not a new m-line.
///
/// `None` leaves the encoding's codec empty, which is what makes webrtc-rs put
/// *every* registered video codec on the offer's m-line. A track described
/// with a codec narrows the offer to that one codec, and the receiver could
/// then only take HEVC or reject video outright.
pub(crate) fn video_stream_track(codec: Option<VideoCodec>, ssrc: u32) -> MediaStreamTrack {
    MediaStreamTrack::new(
        "relay-video-stream".into(),
        "relay-video".into(),
        "Relay Video".into(),
        RtpCodecKind::Video,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            },
            codec: codec.map(|c| super::video_codec(c).rtp_codec).unwrap_or_default(),
            ..Default::default()
        }],
    )
}

pub async fn run(opts: SendOpts) -> Result<()> {
    let offered = tokio::task::spawn_blocking(offerable_codecs).await??;
    info!(offered = ?offered, "video codecs this GPU can encode");
    let (peer_addr, peer_name) = resolve_peer(&opts.peer).await?;
    info!(%peer_addr, %peer_name, "connecting to receiver");
    let tcp = tokio::net::TcpStream::connect(peer_addr).await.context("signalling connect")?;
    tcp.set_nodelay(true)?;
    let local_ip = tcp.local_addr()?.ip();
    let mut sig = signal::SigStream::new(tcp);
    // S49: which link the share leaves over, sent in the offer.
    let local_link = super::netcheck::link_info_for(local_ip)
        .unwrap_or_else(|_| super::netcheck::LinkInfo::unknown());

    let (pc, mut events, runtime) = build_pc(local_ip, &offered).await?;

    // Video + audio tracks. The video track is raw RTP: the codec is not
    // known until the answer arrives, so we packetize ourselves once it is.
    let video_ssrc = rand::random::<u32>();
    let video_track = Arc::new(TrackLocalStaticRTP::new(video_stream_track(None, video_ssrc)));
    let video_sender = pc.add_track(video_track.clone() as Arc<dyn TrackLocal>).await?;

    // Up to two audio tracks. The msid track ids are the contract the
    // receiver classifies on: see `super::audio_role`.
    let mut audio_track = None;
    if opts.audio.is_some() {
        audio_track = Some(
            add_audio_track(&pc, "relay-audio-stream", PROGRAM_TRACK_ID, "Relay Audio").await?,
        );
    }
    let mut mic_track = None;
    if opts.mic {
        mic_track =
            Some(add_audio_track(&pc, "relay-mic-stream", MIC_TRACK_ID, "Relay Microphone").await?);
    }
    // The rest of the PC only exists as a track when there is an app to be
    // the rest *of*; with the whole desktop mix it would be a second copy.
    let rest_pid = match (&opts.audio, opts.rest) {
        (Some(AudioSource::Process { pid }), true) => Some(*pid),
        _ => None,
    };
    let mut rest_track = None;
    if rest_pid.is_some() {
        rest_track =
            Some(add_audio_track(&pc, "relay-rest-stream", REST_TRACK_ID, "Relay Rest").await?);
    }

    // Room for the call to come back (S19): one audio m-line we only receive
    // on. Added *after* our own audio tracks, because `add_track` binds to
    // the first audio transceiver without a sender and this one must not be
    // it. Costs nothing when the receiver has no call app to return.
    pc.add_transceiver_from_kind(
        RtpCodecKind::Audio,
        Some(rtc::rtp_transceiver::RTCRtpTransceiverInit {
            direction: rtc::rtp_transceiver::RTCRtpTransceiverDirection::Recvonly,
            streams: vec![],
            send_encodings: vec![],
        }),
    )
    .await
    .context("add the return-audio transceiver")?;

    // Offer / answer, both MAC'd with the pairing code.
    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;
    let _ = events.gather_done.recv().await;
    let local = pc.local_description().await.context("no local description")?;
    let offer_json = serde_json::to_string(&local)?;
    // A remembered receiver gets no MAC — there is no code to MAC with. It
    // recognises this PC by the DTLS fingerprint in the SDP, and DTLS then
    // proves we hold that certificate's key (S35).
    let trusted = opts.trusted.is_some();
    sig.send(&signal::SigMsg::Offer {
        name: discovery::hostname(),
        sdp: offer_json.clone(),
        mac: if trusted { String::new() } else { signal::mac(&opts.code, &offer_json) },
        trusted,
        link: Some(local_link.clone()),
    })
    .await?;

    let peer_link;
    let answer_json = match (sig.recv().await?, opts.trusted.as_deref()) {
        (signal::SigMsg::Answer { name, sdp, link, .. }, Some(expected)) => {
            peer_link = link;
            // Mutual: the receiver checked us, we check it. A name is not an
            // identity — anything on the LAN can call itself `studio-pc` —
            // so the answer must carry the fingerprint we remembered, and
            // DTLS holds it to that.
            let got = signal::sdp_fingerprint(&sdp).unwrap_or_default();
            if relay_core::peers::norm(&got) != relay_core::peers::norm(expected) {
                return Err(super::Refused(format!(
                    "{name} is not the PC this one remembers by that name; \
                     pair with its code to trust the new one"
                ))
                .into());
            }
            sdp
        }
        (signal::SigMsg::Bye, Some(_)) => {
            return Err(super::Refused(format!(
                "{peer_name} does not remember this PC; pair with its code once and it will"
            ))
            .into());
        }
        (signal::SigMsg::Answer { name, sdp, mac, link }, None) => {
            peer_link = link;
            if !signal::verify_mac(&opts.code, &sdp, &mac) {
                bail!("pairing code mismatch - the receiver used a different code");
            }
            if let Some(fp) = signal::sdp_fingerprint(&sdp) {
                // Never silent: a store that cannot be written is why the
                // next no-code share would fail (B17 hid behind a `let _ =`).
                if let Err(e) =
                    relay_core::peers::remember(&name, &fp, relay_core::peers::Direction::SentTo)
                {
                    warn!(error = %e, receiver = %name, "could not remember this receiver");
                }
            } else {
                warn!(receiver = %name, "answer carries no DTLS fingerprint; nothing to remember");
            }
            sdp
        }
        // The receiver hangs up on a wrong code. Final: the same code will
        // be wrong again, and every retry would cost the receiver its code.
        (signal::SigMsg::Bye, None) => {
            return Err(super::Refused(format!(
                "{peer_name} did not accept that code; check the six digits on the receiving PC"
            ))
            .into());
        }
        (other, _) => bail!("expected answer, got {other:?}"),
    };
    let answer: RTCSessionDescription = serde_json::from_str(&answer_json)?;
    debug!(offer = %local.sdp, answer = %answer.sdp, "sdp exchange");
    let answered = codec::sdp_video_codecs(&answer.sdp);
    let codec = codec::pick(&offered, &answered).with_context(|| {
        format!(
            "the receiver cannot decode any video codec this PC can encode \
             (offered {offered:?}, receiver kept {answered:?})"
        )
    })?;
    // Now describe the track with the negotiated codec (same SSRC and ids)
    // *before* the remote description starts the senders, so the RTCP/NACK
    // interceptors bind to it.
    video_sender
        .replace_track(Arc::new(TrackLocalStaticRTP::new(video_stream_track(
            Some(codec),
            video_ssrc,
        ))) as Arc<dyn TrackLocal>)
        .await?;
    info!(codec = codec.label(), ?offered, ?answered, "video codec negotiated");
    println!(
        "{}",
        serde_json::json!({ "event": "codec", "codec": codec, "offered": offered, "answered": answered })
    );
    pc.set_remote_description(answer).await?;

    let (offset_ns, rtt_ns) = signal::clock_sync(&mut sig, 7).await?;
    info!(offset_ms = offset_ns as f64 / 1e6, rtt_ms = rtt_ns as f64 / 1e6, "clocks synced");

    tokio::select! {
        _ = events.connected.recv() => {}
        _ = events.closed.recv() => bail!("peer connection failed before connecting"),
        _ = tokio::time::sleep(Duration::from_secs(15)) => bail!("timed out waiting for DTLS/ICE"),
    }
    info!("connected; starting media");
    if let Some(fp) = opts.trusted.as_deref() {
        // DTLS has now proved the receiver is the PC we remembered.
        match relay_core::peers::touch(fp, relay_core::peers::Direction::SentTo) {
            Ok(true) => {}
            Ok(false) => warn!("trusted receiver connected but is no longer in the store"),
            Err(e) => warn!(error = %e, "could not update the remembered receiver"),
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "event": "connected",
            "peer": peer_name,
            "rtt_ms": rtt_ns as f64 / 1e6,
            "trusted": trusted,
        })
    );

    // Which links the share runs over, both ends (S49). An older receiver
    // reports none; then only this end is known.
    let peer_s49 = peer_link.is_some();
    let wifi_note = super::netcheck::wifi_note(&local_link, peer_link.as_ref());
    println!(
        "{}",
        serde_json::json!({
            "event": "link",
            "kind": local_link.kind,
            "label": local_link.label(),
            "peer": peer_link.as_ref().map(super::receiver::link_json),
            "recommendation": wifi_note,
        })
    );
    info!(
        local = %local_link.label(),
        receiver = %peer_link.as_ref().map_or("not reported".into(), |l| l.label()),
        "links"
    );
    if let Some(note) = &wifi_note {
        warn!("{note}");
    }

    let stats = Arc::new(Stats::default());
    let stop = Arc::new(AtomicBool::new(false));
    // Per-track gain and mute, set from stdin, read by each audio thread (S37).
    let faders = crate::mixer::Faders::shared();
    // Which endpoint the mic and the call-return output use, set live from
    // stdin (S40). Empty = the System default, followed if it changes.
    let device_slots = Arc::new(crate::devices::DeviceSlots::new(
        opts.mic_device.clone(),
        opts.output_device.clone(),
    ));
    // The call coming back (S19). The receiver sends at most one track our
    // way; when it arrives, play it on the default endpoint behind the Call
    // fader. The playback thread starts only then, so a share with nothing
    // coming back opens no render stream here.
    let return_stop = std::sync::Mutex::new(None::<std::sync::mpsc::Sender<()>>);
    {
        let mut tracks = events.tracks;
        let stats = stats.clone();
        let faders = faders.clone();
        let output_slot = device_slots.output.clone();
        let runtime2 = runtime.clone();
        runtime.spawn(Box::pin(async move {
            while let Some(track) = tracks.recv().await {
                if track.kind().await != RtpCodecKind::Audio {
                    continue;
                }
                let track_id = track.track_id().await;
                info!(%track_id, "return audio track arrived from the receiver");
                let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
                let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
                *return_stop.lock().unwrap() = Some(stop_tx);
                let play_stats = Arc::new(crate::playback::PlaybackStats::default());
                let play_stats2 = play_stats.clone();
                let faders2 = faders.clone();
                let output_slot2 = output_slot.clone();
                let spawned = std::thread::Builder::new()
                    .name("relay-return-playback".into())
                    .spawn(move || {
                        if let Err(e) = crate::playback::run(
                            vec![(rx, crate::mixer::Track::Call)],
                            stop_rx,
                            output_slot2,
                            play_stats2,
                            faders2,
                            // The call coming back is not part of the share.
                            None,
                        ) {
                            warn!(error = %e, "return audio playback stopped");
                        }
                    });
                if let Err(e) = spawned {
                    warn!(error = %e, "could not start return audio playback");
                    continue;
                }
                let stats = stats.clone();
                runtime2.spawn(Box::pin(async move {
                    use webrtc::media_stream::track_remote::TrackRemoteEvent;
                    while let Some(ev) = track.poll().await {
                        if let TrackRemoteEvent::OnRtpPacket(p) = ev {
                            stats.return_packets.fetch_add(1, Ordering::Relaxed);
                            stats.return_peak_milli.store(
                                play_stats.peak_milli.load(Ordering::Relaxed),
                                Ordering::Relaxed,
                            );
                            if tx.send(p.payload.to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                }));
            }
        }));
    }
    let keyframe_wanted = Arc::new(AtomicBool::new(false));
    // Adaptive target bitrate, read by the pipeline each frame.
    let target_bps = Arc::new(AtomicU32::new(opts.bitrate_bps));
    // S49: the rest of what adaptation decides, shared with the pipeline
    // (rung), the writer (pacing) and the PLI handler (coalescing).
    let adapt_state = Arc::new(AdaptState::default());
    // The ladder needs a receiver that takes a mid-share size change (S49),
    // and a sending PC whose own Relay Camera is off: that camera is fed the
    // encoder's input, at a size fixed for the share.
    let ladder_allowed =
        peer_s49 && !opts.vcam && std::env::var("RELAY_LADDER").map_or(true, |v| v != "0");
    if peer_s49 && !ladder_allowed {
        info!(vcam = opts.vcam, "resolution ladder off for this share; bitrate still adapts");
    }

    // Loss feedback from the receiver → the damped bitrate control; and the
    // receiver's reason, if it stops on a fatal error. The same task owns the
    // signalling socket, so it also keeps the clock offset current (B14): one
    // ping every `CLOCK_RESYNC_INTERVAL`, filtered, pushed to the receiver.
    // Measured once at connect, the offset was 11 ms stale within four
    // minutes and the receiver's latency readout went negative.
    let (abort_tx, mut abort_rx) = mpsc::channel::<String>(1);
    // Fired when the share stops: the task says `Bye` on the socket it owns,
    // so the receiver ends now instead of when ICE gives up on us (B8).
    let (bye_tx, bye_rx) = tokio::sync::oneshot::channel::<()>();
    let (bye_sent_tx, bye_sent_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let target = target_bps.clone();
        let retx = events.retransmit_budget.clone();
        let state = adapt_state.clone();
        // S30's loss controller, wrapped by S49's delay half and ladder
        // (`control::SenderAdapt`). With an older receiver only the loss
        // half runs, exactly as before.
        let mut adapt = super::control::SenderAdapt::new(opts.bitrate_bps, None);
        let mut announced: Option<(super::ladder::Rung, super::control::Cause, u32)> = None;
        let mut last_report: Option<super::control::Feedback> = None;
        let mut clock = signal::ClockFilter::seeded(offset_ns, rtt_ns);
        let mut bye_rx = bye_rx;
        // Test hook: the pre-S33 behaviour, to measure the drift it leaves.
        let resync = std::env::var_os("RELAY_CLOCK_SYNC_ONCE").is_none();
        runtime.spawn(Box::pin(async move {
            let mut ping = tokio::time::interval(signal::CLOCK_RESYNC_INTERVAL);
            ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut seq = 1000u32;
            loop {
                tokio::select! {
                    incoming = sig.recv() => {
                        let decision = match incoming {
                            Ok(signal::SigMsg::Loss { fraction }) => Some(adapt.on_loss_window(fraction)),
                            Ok(signal::SigMsg::Feedback { report }) => {
                                if adapt.ladder.is_none() && ladder_allowed {
                                    if let Some(top) = state.top.get() {
                                        let l = super::ladder::Ladder::new(*top, codec);
                                        info!(rungs = ?l.rungs().iter().map(|r| r.label()).collect::<Vec<_>>(), "resolution ladder ready");
                                        adapt.ladder = Some(l);
                                    }
                                }
                                debug!(
                                    queue_ms = report.queue_ms,
                                    queue_max_ms = report.queue_max_ms,
                                    recv_mbps = report.recv_kbps as f64 / 1e3,
                                    lost = report.lost,
                                    "feedback"
                                );
                                last_report = Some(report);
                                Some(adapt.on_feedback(&report))
                            }
                            Ok(signal::SigMsg::Pong { seq: got, t1_ns, t2_ns, t3_ns }) if got == seq => {
                                let (offset, rtt) =
                                    signal::clock_sample(t1_ns, t2_ns, t3_ns, signal::unix_now_ns());
                                match clock.push(offset, rtt) {
                                    Some(offset_ns) => {
                                        debug!(
                                            offset_ms = offset_ns as f64 / 1e6,
                                            sample_ms = offset as f64 / 1e6,
                                            rtt_ms = rtt as f64 / 1e6,
                                            "clock offset updated"
                                        );
                                        let msg = signal::SigMsg::Clock { offset_ns, rtt_ns: rtt };
                                        if sig.send(&msg).await.is_err() {
                                            break;
                                        }
                                    }
                                    None => debug!(
                                        rtt_ms = rtt as f64 / 1e6,
                                        "clock sample ignored: slow round trip"
                                    ),
                                }
                                None
                            }
                            Ok(signal::SigMsg::Abort { reason }) => {
                                let _ = abort_tx.send(reason).await;
                                break;
                            }
                            Ok(_) => None,
                            Err(_) => break,
                        };
                        let Some(d) = decision else { continue };
                        let was = target.swap(d.target_bps, Ordering::Relaxed);
                        if d.target_bps < was && d.cause == super::control::Cause::Queue {
                            // A keyframe asked for now would go into the queue that
                            // is being drained, and be damaged by it: wait it out.
                            let q = last_report.map_or(100.0, |r| r.queue_ms);
                            state.hold_keyframes_for(Duration::from_millis(q.clamp(50.0, 300.0) as u64));
                        }
                        if d.target_bps != was {
                            info!(
                                from_mbps = was as f64 / 1e6,
                                to_mbps = d.target_bps as f64 / 1e6,
                                cause = d.cause.as_str(),
                                queue_ms = last_report.map(|r| r.queue_ms),
                                queue_max_ms = last_report.map(|r| r.queue_max_ms),
                                recv_mbps = last_report.map(|r| r.recv_kbps as f64 / 1e3),
                                "bitrate target changed"
                            );
                        }
                        state.constrained.store(d.constrained, Ordering::Relaxed);
                        retx.store(
                            super::feedback::retransmit_budget_for(d.constrained, d.target_bps),
                            Ordering::Relaxed,
                        );
                        state.cause.store(d.cause as u8, Ordering::Relaxed);
                        if let Some(r) = d.rung {
                            info!(rung = %r.label(), target_mbps = d.target_bps as f64 / 1e6, "resolution step");
                            *state.rung_wanted.lock().unwrap() = Some(r);
                        }
                        // Tell an S49 receiver what we are doing, when it
                        // changes enough to read differently on its screen.
                        let Some(top) = state.top.get().copied() else { continue };
                        if !peer_s49 {
                            continue;
                        }
                        let rung = adapt.ladder.as_ref().map_or(top, |l| l.current());
                        let news = announced.is_none_or(|(r, c, t)| {
                            r != rung
                                || c != d.cause
                                || (f64::from(t) - f64::from(d.target_bps)).abs() > f64::from(t) * 0.1
                        });
                        if news {
                            announced = Some((rung, d.cause, d.target_bps));
                            let msg = signal::SigMsg::Adapt {
                                width: rung.width,
                                height: rung.height,
                                fps: rung.fps,
                                target_bps: d.target_bps,
                                cause: d.cause.as_str().into(),
                            };
                            if sig.send(&msg).await.is_err() {
                                break;
                            }
                        }
                    }
                    _ = &mut bye_rx => {
                        let _ = sig.send(&signal::SigMsg::Bye).await;
                        let _ = bye_sent_tx.send(());
                        break;
                    }
                    _ = ping.tick(), if resync => {
                        seq = seq.wrapping_add(1);
                        let msg = signal::SigMsg::Ping { seq, t1_ns: signal::unix_now_ns() };
                        if sig.send(&msg).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }));
    }

    // PLI from the receiver → force an IDR. On a constrained link a burst of
    // requests (one per damaged frame) is answered with one keyframe, not one
    // each (S49); wired answers every request at once, as before.
    {
        let kf = keyframe_wanted.clone();
        let vt = video_track.clone();
        let state = adapt_state.clone();
        runtime.spawn(Box::pin(async move {
            let mut gate = super::pacing::KeyframeGate::default();
            while let Some(ev) = vt.poll().await {
                let TrackLocalEvent::OnRtcpPacket(packets) = ev;
                for p in packets {
                    if p.as_any()
                        .downcast_ref::<rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication>()
                        .is_some()
                    {
                        let constrained = state.constrained.load(Ordering::Relaxed);
                        if gate.request(std::time::Instant::now(), constrained) {
                            info!("the receiver asked for a keyframe");
                            kf.store(true, Ordering::Relaxed);
                        } else {
                            debug!(coalesced = gate.coalesced, "keyframe request coalesced with the last one");
                        }
                    }
                }
            }
        }));
    }

    // NDI output (S51): made here, the runtime loaded only when it is turned
    // on. The video and program-audio pipelines tee into it.
    let ndi = crate::ndi::NdiOutput::with_runtime(relay_core::ndi::share_source_name());
    if opts.ndi {
        let n = ndi.clone();
        tokio::task::spawn_blocking(move || n.apply(true));
    }

    // Source-switch mailbox: stdin queues, the pipeline applies at a frame
    // boundary. The share starts on `opts.source`, else the primary display.
    let switcher = Arc::new(StdMutex::new(Switcher::new(
        opts.source.unwrap_or(SourceTarget::Display { index: 0 }),
    )));

    // Recorder slot: created inside the video pipeline once the capture size
    // is known; every other reader treats "not there yet" as "off".
    let recorder: Arc<OnceLock<Recorder>> = Arc::new(OnceLock::new());
    let record_setup = opts.record_dir.clone().map(|dir| RecordSetup {
        dir,
        record_on_start: opts.record,
        replay_secs: opts.replay_secs,
        audio: opts.audio.is_some(),
        mic: opts.mic,
        container: opts.container,
    });

    // Preview rate, shared so the `preview` stdin command can retune it live.
    let preview_fps = Arc::new(AtomicU32::new(opts.preview_fps));

    // Video pipeline thread (blocking): capture → convert → encode → channel.
    let (vtx, mut vrx) = mpsc::channel::<VideoAu>(4);
    let video_join = {
        let stats = stats.clone();
        let stop = stop.clone();
        let kf = keyframe_wanted.clone();
        let bitrate = opts.bitrate_bps;
        let fps = opts.fps;
        let cursor = opts.cursor;
        let out_size = opts.size;
        let preview_fps = preview_fps.clone();
        let target = target_bps.clone();
        let rec = recorder.clone();
        let sw = switcher.clone();
        let vcam = opts.vcam;
        let ndi = ndi.clone();
        let adapt = adapt_state.clone();
        std::thread::Builder::new().name("relay-video-pipeline".into()).spawn(move || {
            if let Err(e) = video_pipeline(
                codec,
                bitrate,
                fps,
                cursor,
                out_size,
                preview_fps,
                vtx,
                stats,
                stop,
                kf,
                target,
                rec,
                record_setup,
                sw,
                vcam,
                ndi,
                adapt,
            ) {
                warn!(error = %e, "video pipeline stopped");
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "where": "video", "message": e.to_string() })
                );
            }
        })?
    };

    // Audio threads: WASAPI → Opus → channel, one per track. They stay
    // independent all the way to the wire; nothing on this side mixes.
    let (atx, arx) = mpsc::channel::<(Vec<u8>, Duration)>(16);
    let audio_join = if let Some(source) = opts.audio.clone() {
        let stats = stats.clone();
        let stop = stop.clone();
        let rec = recorder.clone();
        let faders = faders.clone();
        let ndi = Some(ndi.clone());
        Some(std::thread::Builder::new().name("relay-audio-pipeline".into()).spawn(move || {
            // The program mix follows the default output (S40); no pick.
            if let Err(e) = audio_pipeline(
                source,
                AudioTrack::Program,
                None,
                atx,
                stats,
                stop,
                rec,
                faders,
                ndi,
            ) {
                warn!(error = %e, "audio pipeline stopped");
            }
        })?)
    } else {
        None
    };

    let (mtx, mrx) = mpsc::channel::<(Vec<u8>, Duration)>(16);
    let mic_join = if opts.mic {
        let stats = stats.clone();
        let stop = stop.clone();
        let rec = recorder.clone();
        let faders = faders.clone();
        let mic_slot = device_slots.mic.clone();
        // A mic-only share publishes the mic; otherwise NDI carries the
        // program mix, as the one stream a production would want.
        let mic_ndi = opts.audio.is_none().then(|| ndi.clone());
        Some(std::thread::Builder::new().name("relay-mic-pipeline".into()).spawn(move || {
            // A missing or exclusively-held microphone must not take the
            // share down with it: video and the program mix carry on.
            if let Err(e) = audio_pipeline(
                AudioSource::Microphone,
                AudioTrack::Mic,
                Some(mic_slot),
                mtx,
                stats,
                stop,
                rec,
                faders,
                mic_ndi,
            ) {
                warn!(error = %e, "microphone pipeline stopped");
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "where": "mic", "message": e.to_string() })
                );
            }
        })?)
    } else {
        None
    };

    // The rest of the PC (S37): the same process-loopback activation with the
    // exclude flag. Best-effort like the mic -- the game and the mic carry on
    // if it fails, and the failure is reported, not hidden.
    let (rtx, rrx) = mpsc::channel::<(Vec<u8>, Duration)>(16);
    let rest_join = if let Some(pid) = rest_pid {
        let stats = stats.clone();
        let stop = stop.clone();
        let rec = recorder.clone();
        let faders = faders.clone();
        Some(std::thread::Builder::new().name("relay-rest-pipeline".into()).spawn(move || {
            if let Err(e) = audio_pipeline(
                AudioSource::Rest { pid },
                AudioTrack::Rest,
                None,
                rtx,
                stats,
                stop,
                rec,
                faders,
                None,
            ) {
                warn!(error = %e, "rest-of-PC audio pipeline stopped");
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "where": "rest", "message": e.to_string() })
                );
            }
        })?)
    } else {
        None
    };
    let _ = &rest_join;

    // Async writers: channels → tracks.
    {
        use rtc::rtp::packetizer::Packetizer as _;
        let track = video_track.clone();
        let sender = video_sender.clone();
        let stats = stats.clone();
        let state = adapt_state.clone();
        let target = target_bps.clone();
        let fps = opts.fps;
        let payloader = super::video_codec(codec).rtp_codec.payloader()?;
        runtime.spawn(Box::pin(async move {
            let Ok(params) = sender.get_parameters().await else { return };
            // The negotiated payload type for our codec (the answer echoes the
            // offer's, but read it rather than assume it).
            let pt = params
                .rtp_parameters
                .codecs
                .iter()
                .find(|c| c.rtp_codec.mime_type.eq_ignore_ascii_case(codec.mime()))
                .map(|c| c.payload_type)
                .unwrap_or(codec.payload_type());
            let mut packetizer = rtc::rtp::packetizer::new_packetizer(
                RTP_OUTBOUND_MTU,
                pt,
                video_ssrc,
                payloader,
                Box::new(rtc::rtp::sequence::new_random_sequencer()),
                90_000,
            );
            // S49: on a constrained link a frame much larger than the rest
            // (a keyframe) leaves at a measured rate instead of as one burst.
            let mut pacer = super::pacing::Pacer::new();
            let mut hires: Option<super::pacing::HiResTimer> = None;
            'aus: while let Some(au) = vrx.recv().await {
                let n = au.data.len() as u64;
                let t0 = std::time::Instant::now();
                let packets = match packetizer.packetize(&Bytes::from(au.data), au.samples) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "packetize failed");
                        break;
                    }
                };
                let burst = packets.len();
                let constrained = state.constrained.load(Ordering::Relaxed);
                let span = pacer.plan(
                    n as usize,
                    constrained.then(|| target.load(Ordering::Relaxed)),
                    fps,
                );
                // Millisecond sleeps need millisecond timers; held only
                // while the link is constrained (see `HiResTimer`).
                if constrained && hires.is_none() {
                    hires = super::pacing::HiResTimer::acquire();
                } else if !constrained {
                    hires = None;
                }
                if let Err(e) = send_paced(&track, packets, t0, span).await {
                    tracing::warn!(error = %e, "write_rtp failed");
                    break 'aus;
                }
                if !span.is_zero() {
                    debug!(
                        packets = burst,
                        span_ms = span.as_millis() as u64,
                        took_ms = t0.elapsed().as_millis() as u64,
                        keyframe = au.keyframe,
                        "large frame paced"
                    );
                }
                // Nothing paces this: the whole access unit is queued at once
                // and leaves at line rate. Record the big ones (S30).
                if burst >= 150 {
                    tracing::info!(
                        packets = burst,
                        bytes = n,
                        keyframe = au.keyframe,
                        queue_us = t0.elapsed().as_micros() as u64,
                        "large video burst"
                    );
                }
                let took = t0.elapsed();
                if took > Duration::from_millis(100) {
                    tracing::info!(
                        ms = took.as_millis() as u64,
                        packets = burst,
                        bytes = n,
                        queued = vrx.len(),
                        "slow video write"
                    );
                } else if took > Duration::from_millis(30) {
                    tracing::debug!(ms = took.as_millis() as u64, "slow video write");
                }

                stats.video_bytes.fetch_add(n, Ordering::Relaxed);
                stats.video_frames.fetch_add(1, Ordering::Relaxed);
                if au.keyframe {
                    stats.keyframes.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for (slot, rx) in [(audio_track, arx), (mic_track, mrx), (rest_track, rrx)] {
        let Some((track, sender)) = slot else { continue };
        let mut rx = rx;
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
                let mut line = serde_json::json!({
                    "event": "stats",
                    "codec": codec,
                    "bitrate_mbps": (bytes - last_bytes) as f64 * 8.0 / 0.5 / 1e6,
                    "fps": (frames - last_frames) as f64 / 0.5,
                    "frames": frames,
                    "keyframes": stats.keyframes.load(Ordering::Relaxed),
                    "dropped": stats.dropped.load(Ordering::Relaxed),
                    "encode_ms": stats.encode_us_last.load(Ordering::Relaxed) as f64 / 1e3,
                    "capture_to_send_ms": stats.capture_to_send_us_last.load(Ordering::Relaxed) as f64 / 1e3,
                    "audio_packets": stats.audio_packets.load(Ordering::Relaxed),
                    "audio_peak": stats.audio_peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
                    "mic_packets": stats.mic_packets.load(Ordering::Relaxed),
                    "mic_peak": stats.mic_peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
                    "rest_packets": stats.rest_packets.load(Ordering::Relaxed),
                    "rest_peak": stats.rest_peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
                    "vcam_frames": stats.vcam_frames.load(Ordering::Relaxed),
                    "ndi": ndi.status_json(),
                    "return_packets": stats.return_packets.load(Ordering::Relaxed),
                    "return_peak": stats.return_peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
                    "cpu_percent": fp.cpu_percent,
                    "rss_mb": fp.rss_bytes as f64 / 1e6,
                });
                if let Some(rs) = recorder.get().map(|r| r.stats()) {
                    line["recording"] = rs.recording.load(Ordering::Relaxed).into();
                    line["rec_mb"] = (rs.bytes_written.load(Ordering::Relaxed) as f64 / 1e6).into();
                    line["rec_dropped"] = rs.dropped.load(Ordering::Relaxed).into();
                    line["replay_fill"] = (rs.ring_fill_milli.load(Ordering::Relaxed) as f64 / 1e3).into();
                    line["replays_saved"] = rs.replays_saved.load(Ordering::Relaxed).into();
                    line["rec_stopped_disk"] = rs.stopped_for_disk.load(Ordering::Relaxed).into();
                }
                // S49: the links and what Relay is doing about them.
                line["link"] = super::receiver::link_json(&local_link);
                if let Some(l) = &peer_link {
                    line["peer_link"] = super::receiver::link_json(l);
                }
                if let Some(top) = adapt_state.top.get().copied() {
                    let rung = adapt_state.rung_now.lock().unwrap().unwrap_or(top);
                    line["adapt"] = super::control::adapt_json(
                        rung,
                        top,
                        target_bps.load(Ordering::Relaxed),
                        adapt_state.cause(),
                    );
                    line["adapt"]["rung_changes"] = adapt_state.rung_changes.load(Ordering::Relaxed).into();
                    line["adapt"]["rebuild_ms_last"] = adapt_state.rebuild_ms_last.load(Ordering::Relaxed).into();
                }
                println!("{line}");
                last_bytes = bytes;
                last_frames = frames;
            }
            line = stdin_lines.next_line(), if stdin_open => {
                match line {
                    Ok(Some(l)) => match command::parse_line(&l) {
                        Some(EngineCmd::Stop) => {
                            info!("stop command received");
                            break Ok(());
                        }
                        Some(EngineCmd::Record { on }) => {
                            if let Some(r) = recorder.get() {
                                r.set_recording(on);
                                if on {
                                    // The file starts at the next keyframe;
                                    // don't make the user wait out the GOP.
                                    keyframe_wanted.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                        Some(EngineCmd::ReplaySave) => {
                            if let Some(r) = recorder.get() {
                                r.save_replay();
                            }
                        }
                        Some(EngineCmd::Preview { fps }) => {
                            preview_fps.store(fps.min(30), Ordering::Relaxed);
                            debug!(fps, "preview rate set");
                        }
                        Some(EngineCmd::Switch { target }) => {
                            if switcher.lock().unwrap().request(target) {
                                debug!(?target, "switch queued");
                            } else {
                                debug!(?target, "switch dropped (already live)");
                            }
                        }
                        Some(EngineCmd::Mixer { faders: set }) => {
                            faders.apply(&set);
                            debug!(?set, "mixer set");
                        }
                        Some(EngineCmd::Device { track, device }) => {
                            let changed = device_slots.apply(track, device.clone());
                            info!(?track, device = device.as_deref().unwrap_or("System default"), changed, "audio device set");
                        }
                        Some(EngineCmd::Host { .. } | EngineCmd::LocalShare { .. }) => {
                            debug!("host is a receiver command; ignored by the sender");
                        }
                        // S36's "Relay Camera here" stays decided at spawn:
                        // the core arbitrates the one ring writer between a
                        // share and a receive, so a live toggle is receiver
                        // only (S43b; docs/plans/S43-vcam-win10.md).
                        Some(EngineCmd::Vcam { on }) => {
                            info!(on, "vcam is a receiver command; ignored by the sender");
                        }
                        Some(EngineCmd::Return { .. }) => {
                            debug!("return is a receiver command; ignored by the sender");
                        }
                        Some(EngineCmd::Ndi { on }) => {
                            info!(on, "ndi command received");
                            let n = ndi.clone();
                            tokio::task::spawn_blocking(move || n.apply(on));
                        }
                        None => {
                            debug!(line = %l, "unrecognised stdin line ignored");
                        }
                    },
                    _ if spawned_by_core => {
                        info!("stdin closed; core went away");
                        break Ok(());
                    }
                    _ => stdin_open = false,
                }
            }
            Some(reason) = abort_rx.recv() => {
                warn!(%reason, "receiver stopped");
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "where": "receiver", "message": reason })
                );
                break Err(anyhow::anyhow!("receiver stopped: {reason}"));
            }
            _ = events.closed.recv() => break Err(anyhow::anyhow!("peer connection lost")),
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c");
                break Ok(());
            }
        }
    };

    info!("stopping share");
    let stopping = std::time::Instant::now();
    stop.store(true, Ordering::Relaxed);
    // Tell the receiver first: its picture should end when ours does, not
    // after our pipelines have wound down. Bounded, in case the task has
    // already ended with the connection.
    let _ = bye_tx.send(());
    let _ = tokio::time::timeout(Duration::from_millis(200), bye_sent_rx).await;
    let _ = video_join.join();
    for j in [audio_join, mic_join].into_iter().flatten() {
        let _ = j.join();
    }
    info!(ms = stopping.elapsed().as_secs_f64() * 1e3, "pipelines stopped");
    super::close_bounded(&pc, "sender").await;
    println!("{}", serde_json::json!({ "event": "stopped" }));
    result
}

/// Write one access unit's packets, spread evenly over `span` from `t0`
/// (zero: one burst, as always on a wired link). See `pacing::Pacer`.
pub(crate) async fn send_paced(
    track: &TrackLocalStaticRTP,
    packets: Vec<rtc::rtp::Packet>,
    t0: Instant,
    span: Duration,
) -> Result<()> {
    let n = packets.len();
    for (i, p) in packets.into_iter().enumerate() {
        let due = super::pacing::Pacer::due(t0, span, i, n);
        if due > Instant::now() + Duration::from_millis(1) {
            tokio::time::sleep_until(due.into()).await;
        }
        track.write_rtp(p).await?;
    }
    Ok(())
}

/// What `video_pipeline` needs to bring up the recorder once the capture
/// size is known.
struct RecordSetup {
    dir: PathBuf,
    record_on_start: bool,
    replay_secs: u32,
    audio: bool,
    mic: bool,
    container: RecordingContainer,
}

/// webrtc-rs's own outbound MTU for sample tracks; kept identical so the
/// packet sizes on the wire did not change when the video track went raw.
const RTP_OUTBOUND_MTU: usize = 1200;

/// "Relay Camera" on the sending PC (S36), if this Windows has the API and
/// the camera is registered. Every failure is reported as a `vcam_error` line
/// and answered with `None`: a webcam is never a reason to lose the share.
fn start_vcam_sink(size: (u32, u32), fps: u32) -> Option<crate::vcam_sink::VcamSink> {
    // `VcamSink::start` picks the path: the frame-server camera where the API
    // exists, the DirectShow ring (S43) on Windows 10. It never calls the
    // delay-loaded MFCreateVirtualCamera where the export is missing.
    match crate::vcam_sink::VcamSink::start(size.0, size.1, fps) {
        Ok(sink) => {
            println!(
                "{}",
                serde_json::json!({ "event": "vcam_up", "width": size.0, "height": size.1 })
            );
            Some(sink)
        }
        Err(e) => {
            warn!(error = %e, "Relay Camera unavailable; sharing without it");
            println!("{}", serde_json::json!({ "event": "vcam_error", "message": e.to_string() }));
            None
        }
    }
}

/// Blocking pipeline: WGC/DXGI capture → GPU NV12 → HEVC/H.264 MFT → SEI → channel.
#[allow(clippy::too_many_arguments)]
fn video_pipeline(
    codec: VideoCodec,
    bitrate_bps: u32,
    fps: u32,
    cursor: bool,
    out_size: Option<(u32, u32)>,
    preview_fps: Arc<AtomicU32>,
    tx: mpsc::Sender<VideoAu>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    keyframe_wanted: Arc<AtomicBool>,
    target_bps: Arc<AtomicU32>,
    recorder: Arc<OnceLock<Recorder>>,
    record_setup: Option<RecordSetup>,
    switcher: Arc<StdMutex<Switcher>>,
    vcam: bool,
    ndi: Arc<crate::ndi::NdiOutput>,
    adapt: Arc<AdaptState>,
) -> Result<()> {
    // WGC's free-threaded FrameArrived callbacks are delivered on an MTA
    // threadpool thread; without a process MTA they stop after the first
    // frame. Establish it on this thread for the life of the pipeline.
    // SAFETY: balanced by CoUninitialize when the guard drops.
    let _com = ComMta::init()?;
    let _mf = crate::probe::MediaFoundation::start()?;
    let hmon = crate::d3d::primary_monitor();
    let gpu = crate::d3d::device_for_monitor(hmon)?;
    let initial = switcher.lock().unwrap().current();
    let (mut src, initial_crop) = if initial == (SourceTarget::Display { index: 0 }) {
        (crate::source::create(&gpu, hmon, cursor)?, None)
    } else {
        // Fail closed: a resumed window that is gone must not become the
        // desktop.
        create_target_source(&gpu, initial, cursor)
            .with_context(|| format!("the share's source ({initial:?}) could not be opened"))?
    };
    let in_size = src.size();
    // The encoder's output size is fixed for the life of the share; sources
    // of any size are GPU-scaled into it, so switching never renegotiates.
    let mut size = out_size.unwrap_or(in_size);
    let mut conv = crate::encode::convert::Converter::new(&gpu, in_size, size)?;
    // Changed only by a resolution/frame-rate step (S49).
    let mut cur_fps = fps;
    let mut enc = MfEncoder::new(
        &gpu,
        &EncoderConfig { codec, width: size.0, height: size.1, fps, bitrate_bps },
    )?;
    if let Some(rs) = record_setup {
        // Ring RAM ≈ bitrate × (window + one GOP + margin), hard-capped.
        let ring_max =
            ((bitrate_bps as u64 / 8) * (rs.replay_secs as u64 + 15)).min(1_500_000_000) as usize;
        match Recorder::start(RecordConfig {
            codec,
            dir: rs.dir,
            width: size.0,
            height: size.1,
            audio: rs.audio,
            mic: rs.mic,
            replay_secs: rs.replay_secs,
            ring_max_bytes: ring_max,
            budget: DiskBudget::default(),
            roll_secs: 3600,
            record_on_start: rs.record_on_start,
            container: rs.container,
        }) {
            Ok(r) => {
                let _ = recorder.set(r);
            }
            Err(e) => {
                warn!(error = %e, "recorder unavailable; sharing without recording");
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "where": "record", "message": e.to_string() })
                );
            }
        }
    }
    info!(
        encoder = %enc.name,
        codec = codec.label(),
        w = size.0,
        h = size.1,
        fps,
        bitrate_bps,
        "video pipeline up"
    );
    println!(
        "{}",
        serde_json::json!({
            "event": "video_up",
            "encoder": enc.name,
            "codec": codec,
            "width": size.0,
            "height": size.1,
            "fps": fps,
            "bitrate_bps": bitrate_bps,
            "adapter": gpu.adapter_name,
        })
    );

    // S36: the same picture as "Relay Camera" on this PC. Started at the
    // encode size, which is fixed for the share -- a source switch scales
    // into it -- so the camera never has to change format. Best-effort like
    // the receiver's: a failure is reported once and the share goes on.
    let mut vcam_sink = if vcam { start_vcam_sink(size, fps) } else { None };
    // S51: and as an NDI source, when that is on. The same NV12, read back
    // a frame later so the encoder never waits for it.
    let mut ndi_video = crate::ndi::video::VideoProducer::new(ndi, fps);

    // S49: the share's top rung is what it starts at; the ladder never goes
    // above it.
    let top = super::ladder::Rung { width: size.0, height: size.1, fps };
    let _ = adapt.top.set(top);
    *adapt.rung_now.lock().unwrap() = Some(top);

    let mut inflight: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut applied_bps = bitrate_bps;
    let mut conv_in = in_size;
    // Built on the first frame rather than here: a source swap can change the
    // input size, and rebuilding from the frame keeps one code path.
    let mut preview = PreviewTap::new(preview_fps.clone());
    let mut crop: Option<(u32, u32, u32, u32)> = initial_crop;
    conv.set_source_rect(crop);
    let mut pacer = crate::pace::FramePacer::new(fps);
    // The last NV12 handed to the encoder. WGC and DXGI deliver nothing while
    // the screen is still, and a receiver hearing nothing for 3 s treats the
    // share as ended, so a still screen is re-encoded every empty 250 ms wait
    // (4 fps of near-empty P-frames). Converter ring textures stay valid
    // until further converts, and none happen while this is repeating.
    let mut last_nv12: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> = None;
    while !stop.load(Ordering::Relaxed) {
        match enc.next_event()? {
            EncoderEvent::NeedInput => {
                // Apply any adaptive bitrate change from the loss controller.
                let want = target_bps.load(Ordering::Relaxed);
                if want != applied_bps && enc.set_bitrate(want).is_ok() {
                    applied_bps = want;
                    tracing::debug!(bps = want, "bitrate adjusted");
                }
                // A resolution / frame-rate step from the ladder (S49): a new
                // encoder at the new size, the converter scaling into it, and
                // the recorder rolling to a file of that size. The new encoder
                // starts with an IDR, which is what the receiver needs to
                // switch its decoder over.
                let step = adapt.rung_wanted.lock().unwrap().take();
                if let Some(r) =
                    step.filter(|r| (r.width, r.height, r.fps) != (size.0, size.1, cur_fps))
                {
                    let t0 = Instant::now();
                    let cfg = EncoderConfig {
                        codec,
                        width: r.width,
                        height: r.height,
                        fps: r.fps,
                        bitrate_bps: applied_bps,
                    };
                    match MfEncoder::new(&gpu, &cfg).and_then(|e| {
                        Ok((
                            e,
                            crate::encode::convert::Converter::new(
                                &gpu,
                                conv_in,
                                (r.width, r.height),
                            )?,
                        ))
                    }) {
                        Ok((new_enc, mut new_conv)) => {
                            new_conv.set_source_rect(crop);
                            enc = new_enc;
                            conv = new_conv;
                            size = (r.width, r.height);
                            cur_fps = r.fps;
                            pacer = crate::pace::FramePacer::new(r.fps);
                            inflight.clear();
                            last_nv12 = None;
                            if let Some(rec) = recorder.get() {
                                rec.resize(r.width, r.height);
                            }
                            let ms = t0.elapsed().as_millis() as u64;
                            *adapt.rung_now.lock().unwrap() = Some(r);
                            adapt.rung_changes.fetch_add(1, Ordering::Relaxed);
                            adapt.rebuild_ms_last.store(ms, Ordering::Relaxed);
                            info!(
                                w = r.width,
                                h = r.height,
                                fps = r.fps,
                                ms,
                                "encoder rebuilt for a new rung"
                            );
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "rung", "width": r.width, "height": r.height,
                                    "fps": r.fps, "rebuild_ms": ms,
                                })
                            );
                            continue;
                        }
                        Err(e) => {
                            warn!(error = %e, rung = %r.label(), "could not switch rung; staying")
                        }
                    }
                }
                // Swap the capture source if a switch is queued. Encoder,
                // track and peer connection stay as they are — the new
                // source scales into the same encode size, then one IDR.
                let pending = switcher.lock().unwrap().take_pending();
                if let Some(t) = pending {
                    match create_target_source(&gpu, t, cursor) {
                        Ok((new_src, new_crop)) => {
                            src = new_src;
                            conv_in = src.size();
                            conv = crate::encode::convert::Converter::new(&gpu, conv_in, size)?;
                            last_nv12 = None;
                            conv.set_source_rect(new_crop);
                            crop = new_crop;
                            preview.invalidate();
                            pacer.reset();
                            let _ = enc.request_keyframe();
                            switcher.lock().unwrap().applied(t);
                            info!(target = ?t, w = conv_in.0, h = conv_in.1, "source switched");
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "source",
                                    "target": t,
                                    "width": conv_in.0,
                                    "height": conv_in.1,
                                })
                            );
                        }
                        Err(e) => {
                            warn!(target = ?t, error = %e, "switch failed; keeping current source");
                            println!(
                                "{}",
                                serde_json::json!({ "event": "error", "where": "switch", "message": e.to_string() })
                            );
                        }
                    }
                }
                // Capture runs at display refresh; admit only frames on the
                // share's fps schedule. A skipped frame is dropped here, before
                // conversion, so its pool slot goes straight back (bug B1).
                let frame = loop {
                    let Some(f) = src.next(Duration::from_millis(250))? else { break None };
                    if pacer.admit(f.qpc_100ns) {
                        break Some(f);
                    }
                    drop(f);
                    if stop.load(Ordering::Relaxed) {
                        break None;
                    }
                };
                let Some(frame) = frame else {
                    tracing::debug!("no capture frame in 250ms");
                    if let Some(nv12) = last_nv12.as_ref() {
                        // A keyframe asked for on a still screen is owed now,
                        // not at the next screen change: the receiver sat
                        // paused through five requests for 2.3 s (r34, 2b).
                        if adapt.take_keyframe(&keyframe_wanted) {
                            let _ = enc.request_keyframe();
                        }
                        let pts = time::qpc_now_100ns();
                        enc.submit(nv12, pts)?;
                        inflight.insert(pts, pts);
                    }
                    continue;
                };
                // A captured window can resize (or a display can change
                // mode): rebuild the source so the frame pool matches. The
                // encode size never changes, so nothing renegotiates.
                if (frame.width, frame.height) != conv_in && crop.is_none() && frame.width > 0 {
                    let t = switcher.lock().unwrap().current();
                    tracing::debug!(target = ?t, w = frame.width, h = frame.height, "source resized");
                    drop(frame); // stale-sized; the rebuilt source delivers the next one
                    match create_target_source(&gpu, t, cursor) {
                        Ok((new_src, new_crop)) => {
                            src = new_src;
                            conv_in = src.size();
                            conv = crate::encode::convert::Converter::new(&gpu, conv_in, size)?;
                            last_nv12 = None;
                            conv.set_source_rect(new_crop);
                            crop = new_crop;
                            preview.invalidate();
                            pacer.reset();
                            let _ = enc.request_keyframe();
                        }
                        Err(e) => warn!(error = %e, "rebuilding resized source failed"),
                    }
                    continue;
                }
                if adapt.take_keyframe(&keyframe_wanted) {
                    let _ = enc.request_keyframe();
                }
                emit_preview(&mut preview, &gpu, &frame.texture, conv_in);
                let nv12 = conv.convert(&frame.texture)?;
                if let Some(sink) = vcam_sink.as_mut() {
                    // The NV12 the encoder is about to take, copied to the
                    // ring first; the copy is queued on the same immediate
                    // context, so it completes before the encoder's read.
                    let cam = crate::decode::mf::DecodedFrame {
                        texture: nv12.clone(),
                        subresource: 0,
                        pts_100ns: frame.qpc_100ns,
                        width: size.0,
                        height: size.1,
                    };
                    match sink.push(&gpu.device, &gpu.context, &cam) {
                        Ok(()) => {
                            stats.vcam_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            warn!(error = %e, "Relay Camera stopped; the share carries on");
                            println!(
                                "{}",
                                serde_json::json!({ "event": "error", "where": "vcam", "message": e.to_string() })
                            );
                            vcam_sink = None;
                        }
                    }
                }
                let tee = crate::decode::mf::DecodedFrame {
                    texture: nv12.clone(),
                    subresource: 0,
                    pts_100ns: frame.qpc_100ns,
                    width: size.0,
                    height: size.1,
                };
                if let Err(e) = ndi_video.push(&gpu.device, &gpu.context, &tee) {
                    warn!(error = %e, "NDI video stopped; the share carries on");
                    ndi_video.output().fail(format!("NDI video stopped: {e}"));
                    ndi_video.reset();
                }
                enc.submit(&nv12, frame.qpc_100ns)?;
                last_nv12 = Some(nv12);
                inflight.insert(frame.qpc_100ns, time::qpc_now_100ns());
                stats.dropped.store(src.dropped(), Ordering::Relaxed);
            }
            EncoderEvent::Output(out) => {
                let now_qpc = time::qpc_now_100ns();
                if let Some(t_in) = inflight.remove(&out.pts_100ns) {
                    stats.encode_us_last.store(((now_qpc - t_in) / 10) as u64, Ordering::Relaxed);
                }
                // Tee the pre-SEI bitstream to the recorder; never blocks.
                if let Some(r) = recorder.get() {
                    r.push_video(&out.data, out.pts_100ns, out.keyframe);
                }
                // Capture time on the wall clock, for the receiver's estimate.
                let capture_unix_ns =
                    signal::unix_now_ns() - (now_qpc - out.pts_100ns).max(0) * 100;
                stats
                    .capture_to_send_us_last
                    .store(((now_qpc - out.pts_100ns) / 10).max(0) as u64, Ordering::Relaxed);
                let mut data = sei::timestamp_sei(codec, capture_unix_ns);
                data.extend_from_slice(&out.data);
                let au = VideoAu { data, keyframe: out.keyframe, samples: 90_000 / cur_fps.max(1) };
                if tx.blocking_send(au).is_err() {
                    break; // writer gone
                }
            }
        }
    }
    Ok(())
}

/// A live capture source plus the crop rect a `Region` target needs.
type SourceAndCrop = (Box<dyn crate::source::FrameSource>, Option<(u32, u32, u32, u32)>);

/// Build the capture source (and optional crop rect) for a switch target on
/// the share's existing GPU device.
fn create_target_source(
    gpu: &crate::d3d::Gpu,
    target: SourceTarget,
    cursor: bool,
) -> Result<SourceAndCrop> {
    use crate::source;
    match target {
        SourceTarget::Display { index } => {
            let mons = crate::d3d::monitors();
            let hmon = *mons
                .get(index)
                .with_context(|| format!("no display {index} (this PC has {})", mons.len()))?;
            Ok((source::create(gpu, hmon, cursor)?, None))
        }
        SourceTarget::Window { hwnd } => {
            let hwnd = windows::Win32::Foundation::HWND(hwnd as usize as *mut _);
            Ok((Box::new(source::wgc::WgcCapture::window(gpu, hwnd, cursor)?), None))
        }
        SourceTarget::Region { display, x, y, w, h } => {
            let mons = crate::d3d::monitors();
            let hmon = *mons
                .get(display)
                .with_context(|| format!("no display {display} (this PC has {})", mons.len()))?;
            let src = source::create(gpu, hmon, cursor)?;
            let crop = switch::clamp_region(src.size(), x, y, w, h)
                .context("region lies outside the monitor")?;
            Ok((src, Some(crop)))
        }
    }
}

/// Which of the two audio tracks a pipeline feeds. The discriminant doubles
/// as the recorder audio-track index, so wire and file agree by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTrack {
    Program = 0,
    Mic = 1,
    /// The rest of the PC (S37). Index 2 is beyond what the muxers carry,
    /// and they drop a track they do not have, so recordings keep the app
    /// and the mic -- said in the UI, not discovered afterwards.
    Rest = 2,
}

#[allow(clippy::too_many_arguments)]
fn audio_pipeline(
    source: AudioSource,
    which: AudioTrack,
    device: Option<Arc<crate::devices::DeviceSlot>>,
    tx: mpsc::Sender<(Vec<u8>, Duration)>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    recorder: Arc<OnceLock<Recorder>>,
    faders: Arc<crate::mixer::Faders>,
    ndi: Option<Arc<crate::ndi::NdiOutput>>,
) -> Result<()> {
    let profile = match which {
        AudioTrack::Program | AudioTrack::Rest => OpusProfile::program(),
        AudioTrack::Mic => OpusProfile::voice(),
    };
    let mut stream = OpusStream::new_on(source, profile, device)?;
    stream.set_faders(
        faders,
        match which {
            AudioTrack::Program => crate::mixer::Track::App,
            AudioTrack::Mic => crate::mixer::Track::Mic,
            AudioTrack::Rest => crate::mixer::Track::Rest,
        },
    );
    if let Some(n) = ndi {
        stream.set_ndi(n);
    }
    // Per track, not per share: the program mix and the microphone are
    // different endpoints and can run at different rates, so each one reports
    // its own conversion.
    tracing::info!(
        track = ?which,
        rate = stream.endpoint_rate(),
        channels = stream.endpoint_channels(),
        conversion = stream.conversion.as_deref().unwrap_or("none"),
        "audio pipeline up"
    );
    let (packets, peak) = match which {
        AudioTrack::Program => (&stats.audio_packets, &stats.audio_peak_milli),
        AudioTrack::Mic => (&stats.mic_packets, &stats.mic_peak_milli),
        AudioTrack::Rest => (&stats.rest_packets, &stats.rest_peak_milli),
    };
    while !stop.load(Ordering::Relaxed) {
        let Some(p) = stream.next(Duration::from_millis(200))? else { continue };
        packets.fetch_add(1, Ordering::Relaxed);
        peak.store((stream.peak * 1e3) as u32, Ordering::Relaxed);
        if let Some(r) = recorder.get() {
            r.push_audio(
                which as usize,
                &p.data,
                p.qpc_100ns,
                (p.duration.as_nanos() / 100) as i64,
            );
        }
        if tx.blocking_send((p.data, p.duration)).is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::discovery::Discovered;

    fn found() -> Vec<Discovered> {
        vec![
            Discovered {
                name: "gaming-pc".into(),
                addr: "192.168.1.10".parse().unwrap(),
                port: 7001,
            },
            Discovered { name: "Laptop".into(), addr: "192.168.1.11".parse().unwrap(), port: 7002 },
        ]
    }

    #[test]
    fn pick_first_when_no_name_given() {
        let f = found();
        let d = pick_discovered(&f, &None).unwrap();
        assert_eq!(d.name, "gaming-pc");
    }

    #[test]
    fn pick_by_name_is_case_insensitive() {
        let f = found();
        let d = pick_discovered(&f, &Some("LAPTOP".into())).unwrap();
        assert_eq!(d.port, 7002);
    }

    #[test]
    fn pick_unknown_name_errors_and_lists_candidates() {
        let f = found();
        let err = pick_discovered(&f, &Some("den-pc".into())).unwrap_err().to_string();
        assert!(err.contains("den-pc"), "{err}");
        assert!(err.contains("gaming-pc"), "should list what was seen: {err}");
    }

    #[test]
    fn pick_from_empty_errors() {
        let err = pick_discovered(&[], &None).unwrap_err().to_string();
        assert!(err.contains("no Relay receivers"), "{err}");
    }
}
