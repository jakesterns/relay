//! Pairing and signalling over one TCP connection: newline-delimited JSON.
//!
//! The six-digit code never crosses the wire. Each side proves it knows the
//! code by sending `HMAC-SHA256(code, sdp)` next to its SDP; the SDP contains
//! the DTLS certificate fingerprint, and DTLS itself verifies the certificate
//! against the SDP — so a verified MAC transitively pins the peer. Paired
//! peer fingerprints persist in `%LOCALAPPDATA%\Relay\data\peers.json`.
//!
//! After SDP exchange the sender runs a few NTP-style pings so the receiver
//! can convert the sender's in-band capture timestamps to its own clock.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SigMsg {
    Offer {
        name: String,
        sdp: String,
        mac: String,
    },
    Answer {
        name: String,
        sdp: String,
        mac: String,
    },
    /// Sender→receiver clock probe; `t1_ns` is the sender's send time.
    Ping {
        seq: u32,
        t1_ns: i64,
    },
    /// Receiver reply: `t2_ns` receive time, `t3_ns` send time (receiver clock).
    Pong {
        seq: u32,
        t1_ns: i64,
        t2_ns: i64,
        t3_ns: i64,
    },
    /// Sender's estimate of (receiver_clock − sender_clock), pushed to the
    /// receiver so it can rebase in-band capture timestamps.
    Clock {
        offset_ns: i64,
        rtt_ns: i64,
    },
    /// Receiver→sender health: fraction of expected AUs missing in the last
    /// window (0.0..=1.0). Drives the sender's bitrate step-down.
    Loss {
        fraction: f32,
    },
    Bye,
}

pub struct SigStream {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    line: String,
}

impl SigStream {
    pub fn new(stream: TcpStream) -> Self {
        let (rd, writer) = stream.into_split();
        Self { reader: BufReader::new(rd), writer, line: String::new() }
    }

    pub async fn send(&mut self, msg: &SigMsg) -> Result<()> {
        let mut bytes = serde_json::to_vec(msg)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<SigMsg> {
        use tokio::io::AsyncBufReadExt;
        self.line.clear();
        let n = self.reader.read_line(&mut self.line).await?;
        if n == 0 {
            bail!("signalling connection closed");
        }
        if self.line.len() > 256 * 1024 {
            bail!("signalling message too large");
        }
        Ok(serde_json::from_str(&self.line)?)
    }
}

/// `HMAC-SHA256(code, payload)` as lowercase hex.
pub fn mac(code: &str, payload: &str) -> String {
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(code.as_bytes()).expect("any key size");
    h.update(payload.as_bytes());
    let out = h.finalize().into_bytes();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn verify_mac(code: &str, payload: &str, got: &str) -> bool {
    // Constant-time comparison via the hmac crate.
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(code.as_bytes()).expect("any key size");
    h.update(payload.as_bytes());
    let Ok(raw) = hex_decode(got) else { return false };
    h.verify_slice(&raw).is_ok()
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        bail!("odd hex length");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("bad hex"))
        .collect()
}

/// Random six-digit pairing code (leading zeros allowed).
pub fn pairing_code() -> String {
    use rand::Rng;
    format!("{:06}", rand::rng().random_range(0..1_000_000u32))
}

pub fn unix_now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// `a=fingerprint:` line of an SDP (the DTLS certificate fingerprint).
pub fn sdp_fingerprint(sdp: &str) -> Option<String> {
    sdp.lines().find_map(|l| l.trim().strip_prefix("a=fingerprint:").map(str::to_string))
}

// ---- paired-peers store ------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub name: String,
    pub fingerprint: String,
    pub paired_unix: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Peers {
    #[serde(default)]
    pub peers: Vec<Peer>,
}

pub fn peers_file() -> Result<PathBuf> {
    Ok(relay_core::config::Paths::default_for_user()?.data_dir().join("peers.json"))
}

pub fn remember_peer(name: &str, fingerprint: &str) -> Result<()> {
    let path = peers_file()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut store: Peers = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    store.peers.retain(|p| p.name != name);
    store.peers.push(Peer {
        name: name.to_string(),
        fingerprint: fingerprint.to_string(),
        paired_unix: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&store)?)?;
    Ok(())
}

/// Sender side: run `n` pings, compute the median clock offset and push it to
/// the receiver. Returns (offset_ns, rtt_ns).
pub async fn clock_sync(sig: &mut SigStream, n: u32) -> Result<(i64, i64)> {
    let mut samples: Vec<(i64, i64)> = Vec::new(); // (offset, rtt)
    for seq in 0..n {
        sig.send(&SigMsg::Ping { seq, t1_ns: unix_now_ns() }).await?;
        match sig.recv().await? {
            SigMsg::Pong { t1_ns, t2_ns, t3_ns, .. } => {
                let t4 = unix_now_ns();
                let offset = ((t2_ns - t1_ns) + (t3_ns - t4)) / 2;
                let rtt = (t4 - t1_ns) - (t3_ns - t2_ns);
                samples.push((offset, rtt));
            }
            other => bail!("expected pong, got {other:?}"),
        }
    }
    // The sample with the lowest RTT has the least queueing noise.
    let &(offset, rtt) = samples.iter().min_by_key(|(_, rtt)| *rtt).context("no clock samples")?;
    sig.send(&SigMsg::Clock { offset_ns: offset, rtt_ns: rtt }).await?;
    Ok((offset, rtt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_verifies_and_rejects() {
        let m = mac("123456", "sdp-payload");
        assert!(verify_mac("123456", "sdp-payload", &m));
        assert!(!verify_mac("123457", "sdp-payload", &m));
        assert!(!verify_mac("123456", "other", &m));
        assert!(!verify_mac("123456", "sdp-payload", "zz"));
    }

    #[test]
    fn pairing_code_is_six_digits() {
        for _ in 0..100 {
            let c = pairing_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn fingerprint_is_extracted() {
        let sdp = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\na=fingerprint:sha-256 AA:BB\r\n";
        assert_eq!(sdp_fingerprint(sdp).as_deref(), Some("sha-256 AA:BB"));
    }
}
