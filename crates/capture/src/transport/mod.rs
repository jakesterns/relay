//! webrtc-rs transport: one HEVC video track, one or two Opus audio tracks,
//! DTLS-SRTP, ICE host candidates only (no STUN/TURN — LAN only by design).
//!
//! `discovery` finds receivers over mDNS, `signal` pairs them with a
//! six-digit code and syncs clocks, `sei` carries capture timestamps in-band,
//! `sender`/`receiver` are the two ends of a share.

pub mod control;
pub mod depay;
pub mod discovery;
pub mod netcheck;
pub mod receiver;
pub mod sei;
pub mod sender;
pub mod signal;

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rtc::interceptor::Registry;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{
    MediaEngine, MIME_TYPE_HEVC, MIME_TYPE_OPUS,
};
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind};
use tokio::sync::mpsc;
use webrtc::media_stream::track_remote::TrackRemote;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState,
};
use webrtc::runtime::{default_runtime, Runtime};

pub const VIDEO_PT: u8 = 98;
pub const AUDIO_PT: u8 = 120;

pub fn video_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_HEVC.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: String::new(),
            rtcp_feedback: vec![],
        },
        payload_type: VIDEO_PT,
    }
}

/// Which of the two audio streams an arriving track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRole {
    /// The desktop mix or one game process: what the share sounds like.
    Program,
    /// The sender microphone.
    Mic,
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
        sender::PROGRAM_TRACK_ID => AudioRole::Program,
        _ if index == 0 => AudioRole::Program,
        _ => AudioRole::Mic,
    }
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

/// Peer connection bound to `local_ip`, host candidates only, HEVC + Opus
/// registered. Returns the connection, its event channels and the runtime.
pub async fn build_pc(
    local_ip: IpAddr,
) -> Result<(impl PeerConnection, PcEvents, Arc<dyn Runtime>)> {
    let mut media_engine = MediaEngine::default();
    media_engine.register_codec(video_codec(), RtpCodecKind::Video)?;
    media_engine.register_codec(audio_codec(), RtpCodecKind::Audio)?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;

    // No ICE servers: host candidates only, STUN off. LAN by construction.
    let config = RTCConfigurationBuilder::new().build();

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

    let runtime = default_runtime().context("webrtc runtime")?;
    let pc = PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(runtime.clone())
        .with_udp_addrs(vec![format!("{local_ip}:0")])
        .build()
        .await?;

    Ok((pc, PcEvents { gather_done, connected, closed, tracks }, runtime))
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
    fn unrecognised_extra_tracks_fall_back_to_arrival_order() {
        assert_eq!(audio_role("something-else", 1), AudioRole::Mic);
        assert_eq!(audio_role("something-else", 2), AudioRole::Mic);
    }
}
