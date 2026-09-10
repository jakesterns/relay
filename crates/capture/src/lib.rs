//! Relay capture — the share engine. Spun up per share as the `relay-share`
//! process, torn down fully afterwards.
//!
//! Layout:
//! - `probe`     MFTEnumEx / WGC capability checks (`relay-share probe`).
//! - `source/`   Windows.Graphics.Capture (primary), DXGI Desktop Duplication (fallback).
//! - `encode/`   Media Foundation HEVC hardware MFT (NVENC/QSV/AMF). No CPU path.
//! - `transport/` webrtc-rs, mDNS discovery, pairing, DTLS-SRTP. LAN only.
//! - `stats`     the instrument-strip feed: bitrate, latency, drops, load, audio level.
//!
//! Everything GPU-side stays GPU-side: the capture texture is converted to
//! NV12 with the D3D11 video processor and handed to the encoder as a DXGI
//! surface. No frame ever crosses to system memory on the send path.

#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
pub mod decode;
#[cfg(windows)]
pub mod encode;
#[cfg(windows)]
pub mod playback;
#[cfg(windows)]
pub mod probe;
#[cfg(windows)]
pub mod render;
#[cfg(windows)]
pub mod source;
#[cfg(windows)]
pub mod time;
#[cfg(windows)]
pub mod transport;

/// Wall-clock unix nanoseconds (receiver present-latency math).
#[cfg(windows)]
pub fn signal_now_ns() -> i64 {
    transport::signal::unix_now_ns()
}

/// Timing helper: p50/p99 over a recorded series of durations, in milliseconds.
#[derive(Debug, Default)]
pub struct Percentiles {
    samples_ms: Vec<f64>,
}

impl Percentiles {
    pub fn push(&mut self, d: std::time::Duration) {
        self.samples_ms.push(d.as_secs_f64() * 1e3);
    }

    /// Signed sample in milliseconds (capture timestamps can sit slightly in
    /// the future of the receive time: WGC stamps the DWM present slot).
    pub fn push_ms(&mut self, ms: f64) {
        self.samples_ms.push(ms);
    }

    pub fn len(&self) -> usize {
        self.samples_ms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples_ms.is_empty()
    }

    /// (p50, p99, max) in milliseconds. `None` when empty.
    pub fn summary(&self) -> Option<(f64, f64, f64)> {
        if self.samples_ms.is_empty() {
            return None;
        }
        let mut s = self.samples_ms.clone();
        s.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize];
        Some((at(0.50), at(0.99), *s.last().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn percentiles_pick_the_right_samples() {
        let mut p = Percentiles::default();
        for i in 1..=100 {
            p.push(Duration::from_millis(i));
        }
        let (p50, p99, max) = p.summary().unwrap();
        assert!((p50 - 50.0).abs() <= 1.0, "p50 {p50}");
        assert!((p99 - 99.0).abs() <= 1.0, "p99 {p99}");
        assert_eq!(max, 100.0);
    }

    #[test]
    fn percentiles_edge_cases() {
        let p = Percentiles::default();
        assert!(p.summary().is_none());
        assert!(p.is_empty());

        let mut p = Percentiles::default();
        p.push_ms(7.5);
        assert_eq!(p.summary(), Some((7.5, 7.5, 7.5)));
        assert_eq!(p.len(), 1);

        // Negative samples (WGC future-stamped frames) sort correctly.
        let mut p = Percentiles::default();
        p.push_ms(-3.0);
        p.push_ms(1.0);
        p.push_ms(-1.0);
        let (p50, _, max) = p.summary().unwrap();
        assert_eq!(p50, -1.0);
        assert_eq!(max, 1.0);
    }
}
