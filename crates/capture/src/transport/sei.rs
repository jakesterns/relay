//! In-band capture timestamps: a user_data_unregistered SEI NAL prepended to
//! every encoded access unit. RTP timestamps are owned by the packetizer, so
//! the capture time rides inside the bitstream instead — decoders skip
//! unknown SEI UUIDs, and the receiver reads it back for the glass-to-glass
//! estimate.

use crate::codec::{split_nalus, VideoCodec};

/// Relay's SEI UUID (random, fixed).
const UUID: [u8; 16] = [
    0x9a, 0x21, 0x0e, 0x5b, 0x77, 0x4d, 0x41, 0x2a, 0xb1, 0x6c, 0x03, 0x8e, 0xd1, 0xc3, 0x55, 0x27,
];

/// Insert emulation-prevention bytes (0x03 after any 00 00 before 00/01/02/03).
fn ep_encode(raw: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0u32;
    for &b in raw {
        if zeros >= 2 && b <= 3 {
            out.push(0x03);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

/// Strip emulation-prevention bytes.
fn ep_decode(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut zeros = 0u32;
    for &b in raw {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue; // emulation prevention byte
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Annex B SEI NAL carrying `timestamp_ns` (sender clock, unix ns), in the
/// NAL syntax of `codec`.
pub fn timestamp_sei(codec: VideoCodec, timestamp_ns: i64) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(32);
    rbsp.push(5); // payload type: user_data_unregistered
    rbsp.push(24); // payload size: 16-byte UUID + 8-byte timestamp
    rbsp.extend_from_slice(&UUID);
    rbsp.extend_from_slice(&timestamp_ns.to_be_bytes());
    rbsp.push(0x80); // rbsp_stop_one_bit

    let mut nal = vec![0, 0, 0, 1];
    nal.extend_from_slice(codec.sei_header());
    ep_encode(&rbsp, &mut nal);
    nal
}

/// Find Relay's timestamp SEI in an Annex B access unit of `codec`.
pub fn extract_timestamp(codec: VideoCodec, au: &[u8]) -> Option<i64> {
    for nal in split_nalus(au) {
        if codec.nal_type(nal) != codec.sei_type() {
            continue;
        }
        let Some(body) = nal.get(codec.nal_header_len()..) else { continue };
        let rbsp = ep_decode(body);
        if rbsp.len() >= 2 + 24 && rbsp[0] == 5 && rbsp[1] == 24 && rbsp[2..18] == UUID {
            let mut ts = [0u8; 8];
            ts.copy_from_slice(&rbsp[18..26]);
            return Some(i64::from_be_bytes(ts));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::PREFERENCE;
    const HEVC: VideoCodec = VideoCodec::Hevc;

    #[test]
    fn sei_round_trips() {
        for ts in [0i64, 1, -1, 1_700_000_000_000_000_000, i64::MAX, 0x0000_0001_0000_0002] {
            let nal = timestamp_sei(HEVC, ts);
            assert_eq!(extract_timestamp(HEVC, &nal), Some(ts), "ts {ts}");
            // Prepended to a fake AU it is still found.
            let mut au = nal.clone();
            au.extend_from_slice(&[0, 0, 0, 1, 0x28, 0x01, 0xaa, 0xbb]);
            assert_eq!(extract_timestamp(HEVC, &au), Some(ts));
        }
    }

    #[test]
    fn emulation_prevention_round_trips() {
        let nasty: Vec<u8> = vec![0, 0, 0, 0, 1, 2, 3, 0, 0, 2, 0xff, 0, 0, 0];
        let mut enc = Vec::new();
        ep_encode(&nasty, &mut enc);
        assert_eq!(ep_decode(&enc), nasty);
        // No illegal 00 00 0x sequences survive.
        for w in enc.windows(3) {
            assert!(!(w[0] == 0 && w[1] == 0 && w[2] <= 2), "{enc:?}");
        }
    }

    #[test]
    fn foreign_sei_is_ignored() {
        let mut nal = vec![0, 0, 0, 1, 0x4E, 0x01, 5, 24];
        nal.extend_from_slice(&[0u8; 24]);
        nal.push(0x80);
        assert_eq!(extract_timestamp(HEVC, &nal), None);
    }

    #[test]
    fn sei_found_after_other_nals_and_with_3_byte_start_codes() {
        let ts = 1_726_000_000_123_456_789i64;
        // AU: VPS-ish NAL first, then our SEI re-prefixed with a 3-byte start code.
        let mut au = vec![0, 0, 0, 1, 0x40, 0x01, 0x0c];
        let sei = timestamp_sei(HEVC, ts);
        au.extend_from_slice(&[0, 0, 1]); // 3-byte start code
        au.extend_from_slice(&sei[4..]); // SEI NAL without its 4-byte start code
        au.extend_from_slice(&[0, 0, 0, 1, 0x28, 0x01, 0xaa]);
        assert_eq!(extract_timestamp(HEVC, &au), Some(ts));
    }

    #[test]
    fn timestamps_with_zero_runs_survive_emulation_prevention() {
        // Big-endian encodings containing 00 00 0x runs, which must be
        // EP-escaped in the NAL and still parse back.
        for ts in [0x0000_0000_0000_0001i64, 0x0100_0000_0200_0003, 0x0000_0100_0000_0200, i64::MIN]
        {
            let nal = timestamp_sei(HEVC, ts);
            assert_eq!(extract_timestamp(HEVC, &nal), Some(ts), "ts {ts:#x}");
        }
    }

    #[test]
    fn truncated_and_garbage_input_does_not_panic() {
        let good = timestamp_sei(HEVC, 42);
        for cut in 0..good.len() {
            let _ = extract_timestamp(HEVC, &good[..cut]); // must not panic
        }
        assert_eq!(extract_timestamp(HEVC, &[]), None);
        assert_eq!(extract_timestamp(HEVC, &[0, 0, 0, 1]), None);
        assert_eq!(extract_timestamp(HEVC, &[0, 0, 1, 0x4E]), None);
        // Deterministic pseudo-random bytes: no crash, no false positive.
        let mut x = 0x12345678u32;
        let junk: Vec<u8> = (0..4096)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect();
        let _ = extract_timestamp(HEVC, &junk);
    }

    #[test]
    fn wrong_payload_size_is_ignored() {
        // Same UUID but a payload size that is not 24: not ours.
        let mut rbsp = vec![5u8, 23];
        rbsp.extend_from_slice(&UUID);
        rbsp.extend_from_slice(&[0u8; 7]);
        rbsp.push(0x80);
        let mut nal = vec![0, 0, 0, 1, 0x4E, 0x01];
        ep_encode(&rbsp, &mut nal);
        assert_eq!(extract_timestamp(HEVC, &nal), None);
    }

    #[test]
    fn h264_sei_round_trips_and_codecs_do_not_cross_read() {
        let ts = 1_726_000_000_123_456_789i64;
        let nal = timestamp_sei(VideoCodec::H264, ts);
        assert_eq!(&nal[..5], &[0, 0, 0, 1, 0x06], "one-byte H.264 SEI header");
        let mut au = nal.clone();
        au.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84]); // IDR slice
        assert_eq!(extract_timestamp(VideoCodec::H264, &au), Some(ts));
        // Each codec reads only its own SEI syntax.
        assert_eq!(extract_timestamp(VideoCodec::Hevc, &au), None);
        assert_eq!(extract_timestamp(VideoCodec::H264, &timestamp_sei(HEVC, ts)), None);
        for c in PREFERENCE {
            for v in [0i64, -1, i64::MIN, 0x0000_0100_0000_0200] {
                assert_eq!(extract_timestamp(c, &timestamp_sei(c, v)), Some(v), "{c:?} {v:#x}");
            }
        }
    }
}
