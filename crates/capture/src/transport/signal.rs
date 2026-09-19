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
    /// Receiver→sender: the receiver hit a fatal error and is stopping. Sent
    /// before it closes the peer connection, so the sender can say why at
    /// once instead of reporting a lost connection when ICE times out (B3).
    /// An older sender fails to parse it and ends its feedback loop, which is
    /// what the closing connection would have done a moment later anyway.
    Abort {
        reason: String,
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

/// Unix nanoseconds for latency math: the wall clock read *once*, then
/// advanced by the monotonic clock. Every stamp that crosses the wire and
/// every ping comes from here, so an NTP step or a user changing the time
/// mid-share cannot move the latency readout; what is left between two PCs is
/// a constant plus crystal drift, which [`ClockFilter`] follows (B14).
///
/// `RELAY_CLOCK_SKEW_PPM` makes this process's clock run fast or slow by that
/// much. Test hook: two processes on one PC share a crystal, so drift has to
/// be manufactured to be measured.
pub fn unix_now_ns() -> i64 {
    static ANCHOR: std::sync::OnceLock<(std::time::Instant, i64, f64)> = std::sync::OnceLock::new();
    let (t0, wall0, skew) = ANCHOR.get_or_init(|| {
        let wall =
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0);
        let ppm = std::env::var("RELAY_CLOCK_SKEW_PPM")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        (std::time::Instant::now(), wall, ppm / 1e6)
    });
    let elapsed = t0.elapsed().as_nanos() as i64;
    wall0 + elapsed + (elapsed as f64 * skew) as i64
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
    remember_peer_at(&peers_file()?, name, fingerprint)
}

/// Upsert `name` in the peer store at `path` (one entry per name; re-pairing
/// replaces the fingerprint). Tolerates a missing or corrupt store.
pub fn remember_peer_at(path: &std::path::Path, name: &str, fingerprint: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut store: Peers = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    store.peers.retain(|p| p.name != name);
    store.peers.push(Peer {
        name: name.to_string(),
        fingerprint: fingerprint.to_string(),
        paired_unix: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    });
    std::fs::write(path, serde_json::to_vec_pretty(&store)?)?;
    Ok(())
}

/// How often the sender re-measures the offset during a share. Two crystals
/// 50 ppm apart move 0.1 ms in this long, which is inside the ping's noise.
pub const CLOCK_RESYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// One NTP-style exchange → (offset, rtt), both ns. Offset is
/// receiver clock − sender clock.
pub fn clock_sample(t1_ns: i64, t2_ns: i64, t3_ns: i64, t4_ns: i64) -> (i64, i64) {
    (((t2_ns - t1_ns) + (t3_ns - t4_ns)) / 2, (t4_ns - t1_ns) - (t3_ns - t2_ns))
}

/// Turns a stream of noisy (offset, rtt) samples into the offset the receiver
/// should use. A sample whose round trip was much longer than the recent best
/// sat in a queue somewhere, and half of that wait lands in its offset, so it
/// is ignored; the rest are smoothed so the readout does not twitch. A jump
/// far outside the noise is taken at once rather than chased.
#[derive(Debug, Default)]
pub struct ClockFilter {
    estimate_ns: Option<f64>,
    recent_rtt_ns: std::collections::VecDeque<i64>,
    pub accepted: u64,
    pub rejected: u64,
}

impl ClockFilter {
    const RTT_WINDOW: usize = 16;
    const SMOOTHING: f64 = 0.5;
    const STEP_NS: f64 = 20e6;

    pub fn seeded(offset_ns: i64, rtt_ns: i64) -> Self {
        let mut f = Self::default();
        f.push(offset_ns, rtt_ns);
        f
    }

    /// Feed one sample; returns the new estimate when the sample was used.
    pub fn push(&mut self, offset_ns: i64, rtt_ns: i64) -> Option<i64> {
        let best = self.recent_rtt_ns.iter().copied().min();
        if self.recent_rtt_ns.len() == Self::RTT_WINDOW {
            self.recent_rtt_ns.pop_front();
        }
        self.recent_rtt_ns.push_back(rtt_ns);
        if best.is_some_and(|best| rtt_ns > best * 2 + 200_000) {
            self.rejected += 1;
            return None;
        }
        let sample = offset_ns as f64;
        let next = match self.estimate_ns {
            Some(est) if (sample - est).abs() < Self::STEP_NS => {
                est + Self::SMOOTHING * (sample - est)
            }
            _ => sample,
        };
        self.estimate_ns = Some(next);
        self.accepted += 1;
        Some(next as i64)
    }
}

/// Sender side: run `n` pings, take the clock offset from the quickest one
/// and push it to the receiver. Returns (offset_ns, rtt_ns).
pub async fn clock_sync(sig: &mut SigStream, n: u32) -> Result<(i64, i64)> {
    let mut samples: Vec<(i64, i64)> = Vec::new(); // (offset, rtt)
    for seq in 0..n {
        sig.send(&SigMsg::Ping { seq, t1_ns: unix_now_ns() }).await?;
        match sig.recv().await? {
            SigMsg::Pong { t1_ns, t2_ns, t3_ns, .. } => {
                samples.push(clock_sample(t1_ns, t2_ns, t3_ns, unix_now_ns()));
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
    fn clock_sample_recovers_offset_and_rtt() {
        // Receiver 5 s ahead, 1 ms each way, 2 ms turnaround.
        let (t1, t2) = (1_000_000_000, 6_001_000_000);
        let (t3, t4) = (6_003_000_000, 1_004_000_000);
        assert_eq!(clock_sample(t1, t2, t3, t4), (5_000_000_000, 2_000_000));
    }

    #[test]
    fn filter_follows_drift_and_never_trails_far() {
        // 100 ppm for ten minutes at the resync interval: 60 ms of drift.
        let mut f = ClockFilter::seeded(0, 250_000);
        let mut worst = 0i64;
        for k in 1..=300i64 {
            let truth = k * 200_000; // 0.2 ms per 2 s
            let noise = if k % 2 == 0 { 60_000 } else { -60_000 };
            let est = f.push(truth + noise, 250_000).unwrap();
            worst = worst.max((est - truth).abs());
        }
        assert!(worst < 300_000, "estimate trailed the truth by {worst} ns");
        assert_eq!(f.rejected, 0);
    }

    #[test]
    fn filter_ignores_a_sample_that_queued() {
        let mut f = ClockFilter::seeded(1_000_000, 250_000);
        // 40 ms round trip: its offset is off by up to 20 ms.
        assert_eq!(f.push(19_000_000, 40_000_000), None);
        assert_eq!(f.push(1_000_000, 260_000), Some(1_000_000));
        assert_eq!((f.accepted, f.rejected), (2, 1));
    }

    #[test]
    fn filter_takes_a_real_step_at_once() {
        let mut f = ClockFilter::seeded(0, 250_000);
        assert_eq!(f.push(3_000_000_000, 250_000), Some(3_000_000_000));
    }

    #[test]
    fn a_slow_link_is_not_rejected_forever() {
        // Wi-Fi: every round trip is 8-12 ms. Nothing here is an outlier.
        let mut f = ClockFilter::default();
        for k in 0..50i64 {
            assert!(f.push(0, 8_000_000 + (k % 5) * 1_000_000).is_some());
        }
    }

    #[test]
    fn latency_clock_is_monotonic_and_near_the_wall_clock() {
        let a = unix_now_ns();
        let b = unix_now_ns();
        assert!(b >= a);
        let wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as i64;
        assert!((wall - b).abs() < 5_000_000_000, "anchored to the wall clock at start");
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

    #[test]
    fn fingerprint_absent_is_none() {
        assert_eq!(sdp_fingerprint("v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 98\r\n"), None);
        assert_eq!(sdp_fingerprint(""), None);
    }

    #[test]
    fn mac_accepts_uppercase_hex() {
        let m = mac("123456", "payload").to_uppercase();
        assert!(verify_mac("123456", "payload", &m));
    }

    #[test]
    fn mac_rejects_odd_and_truncated_hex() {
        let m = mac("123456", "payload");
        assert!(!verify_mac("123456", "payload", &m[..m.len() - 1])); // odd length
        assert!(!verify_mac("123456", "payload", &m[..m.len() - 2])); // truncated
        assert!(!verify_mac("123456", "payload", ""));
    }

    fn temp_store(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("relay-test-peers-{tag}-{}.json", std::process::id()))
    }

    #[test]
    fn peer_store_round_trips_and_dedupes_by_name() {
        let path = temp_store("dedupe");
        let _ = std::fs::remove_file(&path);
        remember_peer_at(&path, "gaming-pc", "sha-256 AA").unwrap();
        remember_peer_at(&path, "laptop", "sha-256 BB").unwrap();
        // Re-pairing the same name replaces its fingerprint, no duplicate row.
        remember_peer_at(&path, "gaming-pc", "sha-256 CC").unwrap();

        let store: Peers = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(store.peers.len(), 2);
        let gp = store.peers.iter().find(|p| p.name == "gaming-pc").unwrap();
        assert_eq!(gp.fingerprint, "sha-256 CC");
        assert!(gp.paired_unix > 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn peer_store_survives_corrupt_file() {
        let path = temp_store("corrupt");
        std::fs::write(&path, b"{ not json !!").unwrap();
        remember_peer_at(&path, "laptop", "sha-256 DD").unwrap();
        let store: Peers = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(store.peers.len(), 1);
        assert_eq!(store.peers[0].name, "laptop");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sig_msg_wire_format_is_stable() {
        // The receiver of the other version must parse these exact shapes.
        let m = SigMsg::Loss { fraction: 0.25 };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"type":"loss","fraction":0.25}"#);
        let m: SigMsg =
            serde_json::from_str(r#"{"type":"offer","name":"pc","sdp":"v=0","mac":"ab"}"#).unwrap();
        assert!(matches!(m, SigMsg::Offer { .. }));
        let m = SigMsg::Abort { reason: "no H.264 decoder".into() };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"abort","reason":"no H.264 decoder"}"#
        );
        let m: SigMsg = serde_json::from_str(r#"{"type":"bye"}"#).unwrap();
        assert!(matches!(m, SigMsg::Bye));
        let m: SigMsg =
            serde_json::from_str(r#"{"type":"clock","offset_ns":-5,"rtt_ns":9}"#).unwrap();
        assert!(matches!(m, SigMsg::Clock { offset_ns: -5, rtt_ns: 9 }));
    }

    /// A connected localhost TCP pair wrapped in SigStreams.
    async fn sig_pair() -> (SigStream, SigStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (a, b) = tokio::join!(tokio::net::TcpStream::connect(addr), async {
            listener.accept().await.map(|(s, _)| s)
        });
        (SigStream::new(a.unwrap()), SigStream::new(b.unwrap()))
    }

    #[tokio::test]
    async fn sig_stream_round_trips_every_variant() {
        let (mut a, mut b) = sig_pair().await;
        a.send(&SigMsg::Offer { name: "pc".into(), sdp: "v=0".into(), mac: "ab".into() })
            .await
            .unwrap();
        a.send(&SigMsg::Ping { seq: 3, t1_ns: 42 }).await.unwrap();
        a.send(&SigMsg::Bye).await.unwrap();
        assert!(matches!(b.recv().await.unwrap(), SigMsg::Offer { .. }));
        assert!(matches!(b.recv().await.unwrap(), SigMsg::Ping { seq: 3, t1_ns: 42 }));
        assert!(matches!(b.recv().await.unwrap(), SigMsg::Bye));
    }

    #[tokio::test]
    async fn sig_stream_errors_on_close_garbage_and_oversize() {
        // Closed connection.
        let (a, mut b) = sig_pair().await;
        drop(a);
        assert!(b.recv().await.is_err());

        // Non-JSON line.
        let (mut a, mut b) = sig_pair().await;
        {
            use tokio::io::AsyncWriteExt;
            a.writer.write_all(b"not json\n").await.unwrap();
        }
        assert!(b.recv().await.is_err());

        // A message over the 256 KB cap is rejected even if it is valid JSON.
        let (mut a, mut b) = sig_pair().await;
        let big =
            SigMsg::Offer { name: "pc".into(), sdp: "x".repeat(300 * 1024), mac: "ab".into() };
        let send = a.send(&big);
        let recv = b.recv();
        let (sent, got) = tokio::join!(send, recv);
        sent.unwrap();
        assert!(got.unwrap_err().to_string().contains("too large"));
    }

    #[tokio::test]
    async fn clock_sync_recovers_a_simulated_offset() {
        const SKEW_NS: i64 = 250_000_000; // receiver clock runs 250 ms ahead
        let (mut sender, mut receiver) = sig_pair().await;

        // Fake receiver: answer pings with a skewed clock, then hand back the
        // Clock message the sender pushes.
        let receiver_task = tokio::spawn(async move {
            loop {
                match receiver.recv().await.unwrap() {
                    SigMsg::Ping { seq, t1_ns } => {
                        let t = unix_now_ns() + SKEW_NS;
                        receiver
                            .send(&SigMsg::Pong { seq, t1_ns, t2_ns: t, t3_ns: t })
                            .await
                            .unwrap();
                    }
                    SigMsg::Clock { offset_ns, rtt_ns } => return (offset_ns, rtt_ns),
                    other => panic!("unexpected {other:?}"),
                }
            }
        });

        let (offset, rtt) = clock_sync(&mut sender, 5).await.unwrap();
        let (pushed_offset, pushed_rtt) = receiver_task.await.unwrap();
        assert_eq!(offset, pushed_offset, "sender pushes its estimate to the receiver");
        assert_eq!(rtt, pushed_rtt);
        // Localhost RTT is far under the tolerance, so the estimate must land
        // within a few ms of the simulated skew, and RTT must be sane.
        assert!((offset - SKEW_NS).abs() < 10_000_000, "offset {} vs skew {SKEW_NS}", offset);
        assert!((0..1_000_000_000).contains(&rtt), "rtt {rtt}");
    }

    #[tokio::test]
    async fn clock_sync_rejects_wrong_reply() {
        let (mut sender, mut receiver) = sig_pair().await;
        let feeder = tokio::spawn(async move {
            let _ = receiver.recv().await; // swallow the ping
            let _ = receiver.send(&SigMsg::Bye).await;
            receiver
        });
        assert!(clock_sync(&mut sender, 1).await.is_err());
        let _ = feeder.await;
    }
}
