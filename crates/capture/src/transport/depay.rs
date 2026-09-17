//! Video RTP → Annex B reassembly, for both codecs a share can negotiate.
//!
//! H265 (RFC 7798). webrtc-rs parses payloads but
//! `H265Packet::depacketize` hands back the raw RTP payload, so we rebuild the
//! access unit ourselves: single NAL, aggregation (AP, type 48) and
//! fragmentation (FU, type 49). PACI (type 50) is not emitted by the encoders
//! we drive and is skipped.
//!
//! Output is Annex B (each NAL prefixed with `00 00 00 01`) so it feeds both
//! the SEI timestamp reader and the Media Foundation decoder unchanged.

use crate::codec::VideoCodec;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// The depacketizer for whichever codec the share negotiated.
pub enum VideoDepay {
    H265(H265Depay),
    H264(H264Depay),
}

impl VideoDepay {
    pub fn new(codec: VideoCodec) -> Self {
        match codec {
            VideoCodec::Hevc => VideoDepay::H265(H265Depay::default()),
            VideoCodec::H264 => VideoDepay::H264(H264Depay::default()),
        }
    }

    pub fn push(&mut self, payload: &[u8], au: &mut Vec<u8>) {
        match self {
            VideoDepay::H265(d) => d.push(payload, au),
            VideoDepay::H264(d) => d.push(payload, au),
        }
    }
}

/// H264 RTP → Annex B (RFC 6184, packetization-mode 1): single NAL (1–23),
/// STAP-A (24) and FU-A (28). webrtc-rs's payloader emits exactly these —
/// SPS and PPS ride in a STAP-A ahead of the IDR. STAP-B/MTAP/FU-B are
/// interleaved-mode only and are dropped.
#[derive(Default)]
pub struct H264Depay {
    fu: Vec<u8>,
    fu_header: Option<u8>,
}

impl H264Depay {
    pub fn push(&mut self, payload: &[u8], au: &mut Vec<u8>) {
        let Some(&first) = payload.first() else { return };
        match first & 0x1F {
            1..=23 => {
                au.extend_from_slice(&START_CODE);
                au.extend_from_slice(payload);
            }
            24 => {
                let mut i = 1;
                while i + 2 <= payload.len() {
                    let size = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
                    i += 2;
                    if size == 0 || i + size > payload.len() {
                        break;
                    }
                    au.extend_from_slice(&START_CODE);
                    au.extend_from_slice(&payload[i..i + size]);
                    i += size;
                }
            }
            28 => {
                if payload.len() < 2 {
                    return;
                }
                let fu = payload[1];
                if fu & 0x80 != 0 {
                    self.fu.clear();
                    // F and NRI from the indicator, type from the FU header.
                    self.fu_header = Some((first & 0xE0) | (fu & 0x1F));
                }
                if self.fu_header.is_none() {
                    return; // lost the start fragment
                }
                self.fu.extend_from_slice(&payload[2..]);
                if fu & 0x40 != 0 {
                    if let Some(hdr) = self.fu_header.take() {
                        au.extend_from_slice(&START_CODE);
                        au.push(hdr);
                        au.extend_from_slice(&self.fu);
                    }
                    self.fu.clear();
                }
            }
            _ => {}
        }
    }
}

#[derive(Default)]
pub struct H265Depay {
    /// Partially reassembled fragmentation unit (without its NAL header).
    fu: Vec<u8>,
    fu_header: Option<[u8; 2]>,
}

impl H265Depay {
    /// Feed one RTP payload; append any completed NALs (Annex B) to `au`.
    pub fn push(&mut self, payload: &[u8], au: &mut Vec<u8>) {
        if payload.len() < 2 {
            return;
        }
        let nal_type = (payload[0] >> 1) & 0x3F;
        match nal_type {
            49 => self.push_fu(payload, au),
            48 => self.push_ap(payload, au),
            50 => {} // PACI: not produced by our encoders
            _ => {
                // Single NAL unit packet: the whole payload is one NAL.
                au.extend_from_slice(&START_CODE);
                au.extend_from_slice(payload);
            }
        }
    }

    fn push_fu(&mut self, payload: &[u8], au: &mut Vec<u8>) {
        // 2-byte PayloadHdr + 1-byte FU header (+ optional DONL, unused here).
        if payload.len() < 3 {
            return;
        }
        let fu_header = payload[2];
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let fu_type = (fu_header & 0x3F) as u16;

        if start {
            self.fu.clear();
            // Reconstruct the original NAL header from the FU's payload header.
            let layer_id = ((payload[0] & 0x01) << 5) | (payload[1] >> 3);
            let tid = payload[1] & 0x07;
            let b0 = (payload[0] & 0x80) | ((fu_type as u8) << 1) | (layer_id >> 5);
            let b1 = (layer_id << 3) | tid;
            self.fu_header = Some([b0, b1]);
        }
        if self.fu_header.is_none() {
            return; // mid-fragment without a start: drop
        }
        self.fu.extend_from_slice(&payload[3..]);
        if end {
            if let Some(hdr) = self.fu_header.take() {
                au.extend_from_slice(&START_CODE);
                au.extend_from_slice(&hdr);
                au.extend_from_slice(&self.fu);
            }
            self.fu.clear();
        }
    }

    fn push_ap(&mut self, payload: &[u8], au: &mut Vec<u8>) {
        // 2-byte PayloadHdr, then [16-bit size][NAL] repeated.
        let mut i = 2;
        while i + 2 <= payload.len() {
            let size = u16::from_be_bytes([payload[i], payload[i + 1]]) as usize;
            i += 2;
            if i + size > payload.len() {
                break;
            }
            au.extend_from_slice(&START_CODE);
            au.extend_from_slice(&payload[i..i + size]);
            i += size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_nal_gets_start_code() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        // type 32 (VPS): (32<<1)=0x40.
        d.push(&[0x40, 0x01, 0xaa, 0xbb], &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x40, 0x01, 0xaa, 0xbb]);
    }

    #[test]
    fn aggregation_splits_two_nals() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        // AP header type 48 = 0x60,0x01; then [len][nal] twice.
        let mut p = vec![0x60, 0x01];
        p.extend_from_slice(&[0, 2, 0x40, 0x01]);
        p.extend_from_slice(&[0, 3, 0x42, 0x01, 0x05]);
        d.push(&p, &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x40, 0x01, /**/ 0, 0, 0, 1, 0x42, 0x01, 0x05]);
    }

    #[test]
    fn fragmentation_reassembles() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        // Original NAL type 33 (SPS), header bytes 0x42,0x01.
        // FU packets: PayloadHdr type 49 = 0x62,0x01; FU header start/end.
        let fu_type = 33u8;
        d.push(&[0x62, 0x01, 0x80 | fu_type, 0xde, 0xad], &mut au); // start
        d.push(&[0x62, 0x01, fu_type, 0xbe, 0xef], &mut au); // middle
        d.push(&[0x62, 0x01, 0x40 | fu_type, 0x00], &mut au); // end
        assert_eq!(&au[..4], &START_CODE);
        assert_eq!(au[4], 0x42, "reconstructed NAL header b0");
        assert_eq!(au[5], 0x01, "reconstructed NAL header b1");
        assert_eq!(&au[6..], &[0xde, 0xad, 0xbe, 0xef, 0x00]);
    }

    #[test]
    fn fu_without_start_is_dropped() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        // Middle and end fragments with no preceding start (lost packet).
        d.push(&[0x62, 0x01, 33, 0xbe, 0xef], &mut au);
        d.push(&[0x62, 0x01, 0x40 | 33, 0x00], &mut au);
        assert!(au.is_empty(), "orphan fragments must not emit a NAL");
        // A fresh complete FU afterwards still reassembles.
        d.push(&[0x62, 0x01, 0x80 | 33, 0xaa], &mut au);
        d.push(&[0x62, 0x01, 0x40 | 33, 0xbb], &mut au);
        assert_eq!(&au[6..], &[0xaa, 0xbb]);
    }

    #[test]
    fn new_fu_start_discards_stale_fragment() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        d.push(&[0x62, 0x01, 0x80 | 33, 0x11, 0x22], &mut au); // start, never ended
        d.push(&[0x62, 0x01, 0x80 | 33, 0x33], &mut au); // new start
        d.push(&[0x62, 0x01, 0x40 | 33, 0x44], &mut au); // end
        assert_eq!(au.len(), 4 + 2 + 2, "only the second FU's payload survives");
        assert_eq!(&au[6..], &[0x33, 0x44]);
    }

    #[test]
    fn truncated_and_tiny_payloads_are_ignored() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        d.push(&[], &mut au);
        d.push(&[0x40], &mut au); // 1 byte: below the 2-byte NAL header
        d.push(&[0x62, 0x01], &mut au); // FU with no FU header
        assert!(au.is_empty());
    }

    #[test]
    fn aggregation_with_truncated_size_stops_cleanly() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        // First NAL complete, second claims 200 bytes but has 1.
        let mut p = vec![0x60, 0x01];
        p.extend_from_slice(&[0, 2, 0x40, 0x01]);
        p.extend_from_slice(&[0, 200, 0xff]);
        d.push(&p, &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x40, 0x01], "only the complete NAL is emitted");
    }

    #[test]
    fn paci_is_skipped() {
        let mut d = H265Depay::default();
        let mut au = Vec::new();
        // Type 50 = PACI: (50<<1)=0x64.
        d.push(&[0x64, 0x01, 0xaa, 0xbb], &mut au);
        assert!(au.is_empty());
    }

    #[test]
    fn h264_single_nal_and_stap_a() {
        let mut d = H264Depay::default();
        let mut au = Vec::new();
        d.push(&[0x06, 5, 24], &mut au); // SEI, single NAL
        assert_eq!(au, vec![0, 0, 0, 1, 0x06, 5, 24]);

        let mut au = Vec::new();
        // STAP-A (0x78) carrying SPS (0x67 ..) and PPS (0x68 ..), as webrtc-rs emits.
        d.push(&[0x78, 0, 3, 0x67, 0x64, 0x00, 0, 2, 0x68, 0xce], &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x67, 0x64, 0x00, 0, 0, 0, 1, 0x68, 0xce]);
    }

    #[test]
    fn h264_fu_a_reassembles_with_the_original_header() {
        let mut d = H264Depay::default();
        let mut au = Vec::new();
        // IDR (type 5, NRI 3): indicator 0x7C (NRI 3, type 28), FU header S/E + type 5.
        d.push(&[0x7C, 0x85, 0xaa, 0xbb], &mut au);
        d.push(&[0x7C, 0x05, 0xcc], &mut au);
        assert!(au.is_empty(), "nothing until the end fragment");
        d.push(&[0x7C, 0x45, 0xdd], &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x65, 0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[test]
    fn h264_orphan_fragments_truncation_and_unsupported_types_are_dropped() {
        let mut d = H264Depay::default();
        let mut au = Vec::new();
        d.push(&[0x7C, 0x05, 0xcc], &mut au); // middle, no start
        d.push(&[0x7C, 0x45, 0xdd], &mut au); // end, no start
        d.push(&[], &mut au);
        d.push(&[0x7C], &mut au); // FU-A with no FU header
        d.push(&[0x19, 0, 0], &mut au); // STAP-B
        d.push(&[0x1D, 0x80, 1], &mut au); // FU-B
        d.push(&[0x78, 0, 200, 0x67], &mut au); // STAP-A claiming too much
        assert!(au.is_empty(), "{au:?}");
        // Recovers on the next complete FU.
        d.push(&[0x7C, 0x85, 1], &mut au);
        d.push(&[0x7C, 0x45, 2], &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x65, 1, 2]);
    }

    #[test]
    fn video_depay_dispatches_on_codec() {
        let mut au = Vec::new();
        VideoDepay::new(VideoCodec::H264).push(&[0x78, 0, 1, 0x67, 0, 1, 0x68], &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x68]);
        let mut au = Vec::new();
        VideoDepay::new(VideoCodec::Hevc).push(&[0x40, 0x01, 0xaa], &mut au);
        assert_eq!(au, vec![0, 0, 0, 1, 0x40, 0x01, 0xaa]);
    }
}
