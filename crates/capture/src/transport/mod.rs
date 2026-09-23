//! webrtc-rs transport: one video track (HEVC or H.264, negotiated — see
//! `crate::codec`), one or two Opus audio tracks,
//! DTLS-SRTP, ICE host candidates only (no STUN/TURN — LAN only by design).
//!
//! `discovery` finds receivers over mDNS, `signal` pairs them with a
//! six-digit code and syncs clocks, `sei` carries capture timestamps in-band,
//! `sender`/`receiver` are the two ends of a share.

pub mod control;
pub mod depay;
pub mod discovery;
pub mod feedback;
pub mod identity;
pub mod netcheck;
pub mod netio;
pub mod receiver;
pub mod reorder;
pub mod sei;
pub mod sender;
pub mod signal;

use std::net::IpAddr;
use std::sync::Arc;

use std::time::Duration;
use tracing::warn;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rtc::interceptor::Registry;
use rtc::interceptor::{NackGeneratorBuilder, NackResponderBuilder};
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::{
    configure_rtcp_reports, configure_simulcast_extension_headers, configure_twcc_receiver_only,
};
use rtc::peer_connection::configuration::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use rtc::peer_connection::configuration::setting_engine::SettingEngine;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind,
};
use tokio::sync::mpsc;
use webrtc::media_stream::track_remote::TrackRemote;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState,
};
use webrtc::runtime::{default_runtime, Runtime};

use crate::codec::VideoCodec;

pub const AUDIO_PT: u8 = 120;

/// How often the receiver asks again for what is missing.
///
/// The library default is 100 ms — six frames at 60 fps, by which time the
/// damaged frame has long been shown. On a LAN a retransmission costs one
/// packet and under a millisecond of round trip, so ask early: 10 ms puts the
/// first retransmission inside the same frame interval and leaves room for
/// three more inside [`reorder::HOLD`].
pub const NACK_INTERVAL: Duration = Duration::from_millis(10);

/// NACKs per missing packet. After [`reorder::HOLD`] the receiver has moved
/// on and asked for a keyframe; anything the sender retransmits past that is
/// wasted bandwidth, and unlimited is the library default.
const NACKS_PER_PACKET: u16 = 4;

/// SRTP anti-replay window, in packets. The library default is 64, which at
/// 4,000 packets a second is 16 ms: every NACK retransmission of a video
/// packet is older than that by the time it arrives, and SRTP rejects it as a
/// replay ("duplicated") before anything above sees it. Measured in S30 —
/// 1,627 rejections in 6.6 s, 14 holes repaired out of 124. It has to cover
/// [`reorder::HOLD`] at the highest rate plus a keyframe burst: 40 ms at
/// 80 Mb/s is ~340 packets, a 4K keyframe up to ~2,000. 4096 is a 512-byte
/// bitmask and still rejects true replays.
const SRTP_REPLAY_WINDOW: usize = 4096;

/// Packets each end remembers for NACK: the receiver's arrival log and the
/// sender's retransmission buffer. 2048 is 250 ms at 80 Mb/s and four
/// worst-case keyframes; the defaults (512 / 1024) are shorter than one 4K
/// keyframe burst. Must be a power of two.
const NACK_HISTORY: u16 = 2048;

/// RTP codec parameters for `codec`. The feedback list is empty here and
/// filled in by `build_pc`: `register_feedback` appends `nack`, `nack pli`
/// and `transport-cc` to every registered video codec, which is what reaches
/// the SDP. Sender and receiver build these from the
/// same table, so the fmtp lines match exactly.
pub fn video_codec(codec: VideoCodec) -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: codec.mime().to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: codec.fmtp().to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: codec.payload_type(),
    }
}

/// Which of the two audio streams an arriving track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRole {
    /// The desktop mix or one game process: what the share sounds like.
    Program,
    /// The sender microphone.
    Mic,
    /// Everything on the sending PC except the shared app (S37).
    Rest,
}

/// Classify an arriving audio track. `track_id` is the msid track id the
/// sender set ([`sender::PROGRAM_TRACK_ID`] / [`sender::MIC_TRACK_ID`]);
/// `index` is how many audio tracks arrived before this one.
///
/// The id is authoritative when we recognise it. Everything else falls back
/// to arrival order, which is what keeps an older peer — one that sends a
/// single unnamed audio track, or names it something else — working: its one
/// track is the program mix, and there is nothing to confuse it with.
pub fn audio_role(track_id: &str, index: usize) -> AudioRole {
    match track_id {
        sender::MIC_TRACK_ID => AudioRole::Mic,
        sender::REST_TRACK_ID => AudioRole::Rest,
        sender::PROGRAM_TRACK_ID => AudioRole::Program,
        _ if index == 0 => AudioRole::Program,
        _ => AudioRole::Mic,
    }
}

/// The msid track id of the audio the *receiver* sends back (S19): the call
/// app's output, heard on the sender. The only track that travels that way.
pub const RETURN_TRACK_ID: &str = "relay-audio-return";

/// Does this offer leave room for the return track — an audio m-line the
/// sender only receives on? A receiver adds its return track only when it
/// does; against an older sender the answer would otherwise grow an m-line
/// the offer never had, and the whole share would fail rather than just the
/// return. Takes the raw SDP or the JSON description that is actually sent.
pub fn sdp_offers_return(sdp: &str) -> bool {
    let inner = serde_json::from_str::<serde_json::Value>(sdp)
        .ok()
        .and_then(|v| v.get("sdp")?.as_str().map(str::to_string));
    let text = inner.as_deref().unwrap_or(sdp);
    let mut in_audio = false;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("m=") {
            in_audio = rest.starts_with("audio ");
        } else if in_audio && line == "a=recvonly" {
            return true;
        }
    }
    false
}

/// One Opus track description, the shape both ends use for anything they
/// send: `stream_id`/`track_id` are the msid the other end classifies on.
pub fn audio_stream_track(stream_id: &str, track_id: &str, label: &str) -> MediaStreamTrack {
    use rtc::rtp_transceiver::rtp_sender::{RTCRtpCodingParameters, RTCRtpEncodingParameters};
    MediaStreamTrack::new(
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
    )
}

pub fn audio_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: AUDIO_PT,
    }
}

/// Events surfaced by the peer-connection handler.
pub struct PcEvents {
    pub gather_done: mpsc::Receiver<()>,
    pub connected: mpsc::Receiver<()>,
    pub closed: mpsc::Receiver<()>,
    pub tracks: mpsc::UnboundedReceiver<Arc<dyn TrackRemote>>,
}

struct Handler {
    gather_done: mpsc::Sender<()>,
    connected: mpsc::Sender<()>,
    closed: mpsc::Sender<()>,
    tracks: mpsc::UnboundedSender<Arc<dyn TrackRemote>>,
}

#[async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gather_done.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::info!(?state, "peer connection");
        match state {
            RTCPeerConnectionState::Connected => {
                let _ = self.connected.try_send(());
            }
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => {
                let _ = self.closed.try_send(());
            }
            _ => {}
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let _ = self.tracks.send(track);
    }
}

/// Peer connection bound to `local_ip`, host candidates only, `video` codecs
/// (preference order) + Opus registered. The sender registers what it can
/// encode and the receiver what it can decode, so the answer is the
/// intersection. Returns the connection, its event channels and the runtime.
pub async fn build_pc(
    local_ip: IpAddr,
    video: &[VideoCodec],
) -> Result<(impl PeerConnection, PcEvents, Arc<dyn Runtime>)> {
    let mut media_engine = MediaEngine::default();
    for &c in video {
        media_engine.register_codec(video_codec(c), RtpCodecKind::Video)?;
    }
    media_engine.register_codec(audio_codec(), RtpCodecKind::Audio)?;
    // `register_default_interceptors`, unrolled so NACK can be tuned for a
    // LAN (see `NACK_INTERVAL`). Same set otherwise: NACK both ways, RTCP
    // reports, transport-cc feedback from the receiving end.
    for parameter in ["", "pli"] {
        media_engine.register_feedback(
            RTCPFeedback { typ: "nack".to_owned(), parameter: parameter.to_owned() },
            RtpCodecKind::Video,
        );
    }
    let registry = Registry::new()
        .with(feedback::keyframe_request_forwarder())
        .with(
            NackGeneratorBuilder::new()
                .with_size(NACK_HISTORY)
                .with_interval(NACK_INTERVAL)
                .with_max_nacks_per_packet(NACKS_PER_PACKET)
                .build(),
        )
        .with(NackResponderBuilder::new().with_size(NACK_HISTORY).build());
    let registry = configure_rtcp_reports(registry);
    configure_simulcast_extension_headers(&mut media_engine)?;
    let registry = configure_twcc_receiver_only(registry, &mut media_engine)?;

    // No ICE servers: host candidates only, STUN off. LAN by construction.
    //
    // The certificate is this installation's lasting identity (S35) rather
    // than the throwaway one webrtc-rs would mint per connection. Without it
    // our DTLS fingerprint changes every run, and a remembered peer can never
    // be recognised — which is why `peers.json` was written but never read.
    // A failure here is not fatal: fall back to a generated certificate so a
    // share still works, and say so, because the symptom (peers stop being
    // recognised) is otherwise unexplainable.
    let config = match identity::certificate() {
        Ok(cert) => RTCConfigurationBuilder::new().with_certificates(vec![cert]).build(),
        Err(e) => {
            warn!(error = %e, "no lasting DTLS identity; peers will not recognise this PC");
            RTCConfigurationBuilder::new().build()
        }
    };

    let (gather_tx, gather_done) = mpsc::channel(1);
    let (conn_tx, connected) = mpsc::channel(1);
    let (closed_tx, closed) = mpsc::channel(1);
    let (track_tx, tracks) = mpsc::unbounded_channel();
    let handler = Arc::new(Handler {
        gather_done: gather_tx,
        connected: conn_tx,
        closed: closed_tx,
        tracks: track_tx,
    });

    let (runtime, net) = netio::TunedRuntime::wrap(default_runtime().context("webrtc runtime")?);
    netio::log_every_second(&net);
    let mut setting_engine = SettingEngine::default();
    setting_engine.set_srtp_replay_protection_window(SRTP_REPLAY_WINDOW);
    let pc = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_setting_engine(setting_engine)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(runtime.clone())
        .with_udp_addrs(vec![format!("{local_ip}:0")])
        .build()
        .await?;

    Ok((pc, PcEvents { gather_done, connected, closed, tracks }, runtime))
}

/// How long closing a peer connection may take before we stop waiting. It is
/// tidiness: by the time anyone calls this the share is over and the peer has
/// been told on the signalling socket, so nothing the user sees depends on it.
pub const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Close `pc`, but never wait on it for longer than [`CLOSE_GRACE`] (B8), and
/// say how long it took so a slow close shows up in the log as a number.
pub async fn close_bounded(pc: &impl PeerConnection, who: &str) {
    let started = std::time::Instant::now();
    let finished = tokio::time::timeout(CLOSE_GRACE, pc.close()).await.is_ok();
    let ms = started.elapsed().as_secs_f64() * 1e3;
    if finished {
        tracing::info!(who, ms, "peer connection closed");
    } else {
        tracing::warn!(who, ms, "peer connection did not close in time; moving on");
    }
}

/// The local address the OS would use to reach `peer` — the right interface
/// for ICE host candidates without binding to everything.
pub fn local_ip_towards(peer: IpAddr) -> Result<IpAddr> {
    let s = std::net::UdpSocket::bind(if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    s.connect((peer, 9)).context("no route to peer")?;
    Ok(s.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_tracks_classify_by_id_whatever_the_order() {
        assert_eq!(audio_role(sender::PROGRAM_TRACK_ID, 0), AudioRole::Program);
        assert_eq!(audio_role(sender::MIC_TRACK_ID, 1), AudioRole::Mic);
        // SDP m-line order is not guaranteed to survive the answer, so the
        // id has to win over the index when we recognise it.
        assert_eq!(audio_role(sender::MIC_TRACK_ID, 0), AudioRole::Mic);
        assert_eq!(audio_role(sender::PROGRAM_TRACK_ID, 1), AudioRole::Program);
    }

    /// An older sender ships one audio track with whatever msid it likes.
    /// It must land on the program mix, not the mic.
    #[test]
    fn an_older_peer_single_track_is_the_program_mix() {
        assert_eq!(audio_role("audio", 0), AudioRole::Program);
        assert_eq!(audio_role("", 0), AudioRole::Program);
        assert_eq!(audio_role("6f2e1b3a-audio", 0), AudioRole::Program);
    }

    #[test]
    fn an_offer_with_a_receive_only_audio_line_has_room_for_the_return() {
        // The S19 sender: three send-only audio lines, then the one it
        // only receives on. Attributes of *other* lines must not count.
        let offer = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 98\r\na=sendonly\r\n\
                     m=audio 9 UDP/TLS/RTP/SAVPF 120\r\na=sendonly\r\n\
                     m=audio 9 UDP/TLS/RTP/SAVPF 120\r\na=recvonly\r\n";
        assert!(sdp_offers_return(offer));
        // And in the JSON form that actually travels (B17's lesson).
        let json = serde_json::json!({ "type": "offer", "sdp": offer }).to_string();
        assert!(sdp_offers_return(&json));

        // A pre-S19 sender: audio lines all send-only. No room, no track.
        let old = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 98\r\na=sendonly\r\n\
                   m=audio 9 UDP/TLS/RTP/SAVPF 120\r\na=sendonly\r\n";
        assert!(!sdp_offers_return(old));
        // A receive-only *video* line is not an audio return either.
        let video_only = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 98\r\na=recvonly\r\n\
                          m=audio 9 UDP/TLS/RTP/SAVPF 120\r\na=sendonly\r\n";
        assert!(!sdp_offers_return(video_only));
        assert!(!sdp_offers_return(""));
    }

    #[test]
    fn unrecognised_extra_tracks_fall_back_to_arrival_order() {
        assert_eq!(audio_role("something-else", 1), AudioRole::Mic);
        assert_eq!(audio_role("something-else", 2), AudioRole::Mic);
    }
}
