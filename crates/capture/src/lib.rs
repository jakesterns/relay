//! Relay capture — spun up per share, torn down fully afterwards.
//!
//! Planned layout:
//! - `source/`   DXGI Desktop Duplication (primary) and Windows.Graphics.Capture.
//! - `encode/`   hardware-only encoders: NVENC, QSV, AMF. HEVC 4K60, 40–80 Mb/s.
//! - `transport/` webrtc-rs with ICE/STUN, mDNS discovery, DTLS-SRTP. LAN-first.
//! - `record/`   local high-bitrate recording + replay buffer.
//! - `stats`     the instrument-strip feed: bitrate, latency, drops, load, audio level.

#![forbid(unsafe_code)]

pub const CRATE: &str = "relay-capture";
