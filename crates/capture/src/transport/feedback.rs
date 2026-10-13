//! Let keyframe requests reach the application.
//!
//! webrtc-rs runs received RTCP through the interceptor chain and then drops
//! it: the chain's terminal `NoopInterceptor` forwards RTP only ("RTCP message
//! read must end here. If any rtcp packet needs to be forwarded to
//! PeerConnection, just add a new interceptor"). NACK and receiver reports are
//! consumed inside the chain, so nothing was missing them — but a PLI is for
//! the encoder, which lives in the application. The sender has polled its
//! track for `PictureLossIndication` since M4 and, measured in S30, never
//! received one: 21 sent by the receiver, 0 seen.
//!
//! This is the interceptor that comment asks for. It forwards PLI and FIR and
//! nothing else, so the per-second reports and the NACK traffic still end in
//! the chain instead of waking the application.

use std::collections::VecDeque;

use rtc::interceptor::{interceptor, Interceptor, Packet, StreamInfo, TaggedPacket};
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::sansio;
use rtc::shared::error::Error;

#[derive(Interceptor)]
pub struct KeyframeRequestForwarder<P> {
    #[next]
    inner: P,
    read_queue: VecDeque<TaggedPacket>,
}

/// Registry builder: `Registry::new().with(keyframe_request_forwarder())`.
pub fn keyframe_request_forwarder<P>() -> impl FnOnce(P) -> KeyframeRequestForwarder<P> {
    |inner| KeyframeRequestForwarder { inner, read_queue: VecDeque::new() }
}

#[interceptor]
impl<P: Interceptor> KeyframeRequestForwarder<P> {
    #[overrides]
    fn handle_read(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(packets) = &msg.message {
            // Route on the request alone: the endpoint picks the track from
            // the first packet in the compound, which is usually a report.
            let requests: Vec<_> = packets
                .iter()
                .filter(|p| {
                    let any = p.as_any();
                    any.is::<PictureLossIndication>() || any.is::<FullIntraRequest>()
                })
                .map(|p| p.cloned())
                .collect();
            if !requests.is_empty() {
                self.read_queue.push_back(TaggedPacket {
                    now: msg.now,
                    transport: msg.transport,
                    message: Packet::Rtcp(requests),
                });
            }
        }
        self.inner.handle_read(msg)
    }

    #[overrides]
    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.read_queue.pop_front().or_else(|| self.inner.poll_read())
    }
}

// ---------------------------------------------------------------------------
// S49: a budget for retransmissions on a constrained link.
// ---------------------------------------------------------------------------

use rtc::rtcp::transport_feedbacks::transport_layer_nack::{NackPair, TransportLayerNack};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Retransmissions allowed while the link is constrained, as a share of the
/// target bitrate. The rest of what NACKs ask for is not sent.
pub const RETRANSMIT_SHARE: f64 = 0.15;
/// Never below this, so a single hole is still repaired at the floor.
pub const RETRANSMIT_MIN_BPS: u32 = 1_000_000;
const PACKET_BITS: f64 = 1200.0 * 8.0;

/// The budget for a constrained link at `target_bps`; 0 (unlimited) when
/// not constrained, which is what a wired share always has.
pub fn retransmit_budget_for(constrained: bool, target_bps: u32) -> u32 {
    if !constrained {
        return 0;
    }
    ((f64::from(target_bps) * RETRANSMIT_SHARE) as u32).max(RETRANSMIT_MIN_BPS)
}

/// Cuts NACK requests down to what the budget allows (S49).
///
/// Measured on the loopback link model: when Wi-Fi capacity halves, the
/// access point drops what does not fit, the receiver NACKs every hole up to
/// four times, and the sender's NACK responder answers each one. On the
/// capacity-drop profile that was 85 Mb/s arriving against a 2.5 Mb/s
/// target — retransmissions crowding out the video they were meant to
/// repair: congestion collapse. Pure: the interceptor below feeds it.
pub struct NackTrimmer {
    tokens: f64,
    last: Option<std::time::Instant>,
    /// Packets asked for and not retransmitted.
    pub trimmed: u64,
}

impl NackTrimmer {
    pub fn new() -> Self {
        Self { tokens: 0.0, last: None, trimmed: 0 }
    }

    /// The pairs to pass on to the responder. `budget_bps == 0`: all of them.
    pub fn trim(
        &mut self,
        now: std::time::Instant,
        budget_bps: u32,
        nacks: &[NackPair],
    ) -> Vec<NackPair> {
        if budget_bps == 0 {
            self.last = Some(now);
            return nacks.to_vec();
        }
        let rate = f64::from(budget_bps) / PACKET_BITS;
        // Room for 50 ms of repairs at once, at least a few packets.
        let depth = (rate * 0.05).max(4.0);
        let dt =
            self.last.map_or(f64::INFINITY, |l| now.saturating_duration_since(l).as_secs_f64());
        self.last = Some(now);
        self.tokens = (self.tokens + dt * rate).min(depth);
        let mut out: Vec<NackPair> = Vec::new();
        for pair in nacks {
            for seq in pair.packet_list() {
                if self.tokens < 1.0 {
                    self.trimmed += 1;
                    continue;
                }
                self.tokens -= 1.0;
                match out.last_mut() {
                    Some(p) if seq.wrapping_sub(p.packet_id).wrapping_sub(1) < 16 => {
                        p.lost_packets |= 1 << seq.wrapping_sub(p.packet_id).wrapping_sub(1);
                    }
                    _ => out.push(NackPair::new(seq)),
                }
            }
        }
        out
    }
}

impl Default for NackTrimmer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Interceptor)]
pub struct RetransmitBudget<P> {
    #[next]
    inner: P,
    budget: Arc<AtomicU32>,
    trimmer: NackTrimmer,
}

/// Registry builder; must wrap (come after) the NACK responder so it sees
/// the requests first. `budget` is bits per second, 0 for unlimited.
pub fn retransmit_budget<P>(budget: Arc<AtomicU32>) -> impl FnOnce(P) -> RetransmitBudget<P> {
    move |inner| RetransmitBudget { inner, budget, trimmer: NackTrimmer::new() }
}

#[interceptor]
impl<P: Interceptor> RetransmitBudget<P> {
    #[overrides]
    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        let budget = self.budget.load(Ordering::Relaxed);
        if budget > 0 {
            let now = msg.now;
            if let Packet::Rtcp(packets) = &mut msg.message {
                let mut kept: Vec<Box<dyn rtc::rtcp::Packet>> = Vec::with_capacity(packets.len());
                for p in packets.drain(..) {
                    let Some(n) = p.as_any().downcast_ref::<TransportLayerNack>() else {
                        kept.push(p);
                        continue;
                    };
                    let nacks = self.trimmer.trim(now, budget, &n.nacks);
                    if !nacks.is_empty() {
                        kept.push(Box::new(TransportLayerNack {
                            sender_ssrc: n.sender_ssrc,
                            media_ssrc: n.media_ssrc,
                            nacks,
                        }));
                    }
                }
                *packets = kept;
            }
        }
        self.inner.handle_read(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn pairs(seqs: &[u16]) -> Vec<NackPair> {
        seqs.iter().map(|&s| NackPair::new(s)).collect()
    }

    fn seqs(p: &[NackPair]) -> Vec<u16> {
        p.iter().flat_map(|p| p.packet_list()).collect()
    }

    #[test]
    fn unlimited_passes_everything_untouched() {
        let mut t = NackTrimmer::new();
        let all: Vec<u16> = (100..400).collect();
        assert_eq!(seqs(&t.trim(Instant::now(), 0, &pairs(&all))), all);
        assert_eq!(t.trimmed, 0);
        assert_eq!(retransmit_budget_for(false, 40_000_000), 0, "wired: never limited");
        assert_eq!(retransmit_budget_for(true, 40_000_000), 6_000_000);
        assert_eq!(retransmit_budget_for(true, 2_500_000), RETRANSMIT_MIN_BPS);
    }

    #[test]
    fn a_budget_caps_retransmissions_at_its_rate() {
        // 1 Mb/s is ~104 packets a second, 5 at once.
        let mut t = NackTrimmer::new();
        let t0 = Instant::now();
        let mut passed = 0;
        for i in 0..100u32 {
            let now = t0 + Duration::from_millis(10 * u64::from(i));
            let ask: Vec<u16> = (0..20).map(|k| (i * 20 + k) as u16).collect();
            passed += seqs(&t.trim(now, 1_000_000, &pairs(&ask))).len();
        }
        // One second of asking for 2,000 packets: about 104 get through.
        assert!((95..=115).contains(&passed), "{passed}");
        assert_eq!(t.trimmed as usize, 2000 - passed);
    }

    #[test]
    fn kept_packets_are_re_encoded_as_compact_pairs() {
        let mut t = NackTrimmer::new();
        let mut p = NackPair::new(1000);
        p.lost_packets = 0b101; // 1001 and 1003
        let out = t.trim(Instant::now(), 100_000_000, &[p, NackPair::new(1030)]);
        assert_eq!(seqs(&out), [1000, 1001, 1003, 1030]);
        assert_eq!(out.len(), 2, "1000-1003 fit one pair; 1030 is too far");
    }
}
