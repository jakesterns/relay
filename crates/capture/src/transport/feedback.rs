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
