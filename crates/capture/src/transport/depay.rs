//! H265 RTP → Annex B reassembly (RFC 7798). webrtc-rs parses payloads but
//! `H265Packet::depacketize` hands back the raw RTP payload, so we rebuild the
//! access unit ourselves: single NAL, aggregation (AP, type 48) and
//! fragmentation (FU, type 49). PACI (type 50) is not emitted by the encoders
//! we drive and is skipped.
//!
//! Output is Annex B (each NAL prefixed with `00 00 00 01`) so it feeds both
//! the SEI timestamp reader and the Media Foundation decoder unchanged.

const START_CODE: [u8; 4] = [0, 0, 0, 1];

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
}
