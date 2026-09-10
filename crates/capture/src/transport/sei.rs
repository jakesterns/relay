//! In-band capture timestamps: a user_data_unregistered SEI NAL prepended to
//! every encoded access unit. RTP timestamps are owned by the packetizer, so
//! the capture time rides inside the bitstream instead — decoders skip
//! unknown SEI UUIDs, and the receiver reads it back for the glass-to-glass
//! estimate.

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

/// Annex B SEI NAL carrying `timestamp_ns` (sender clock, unix ns).
pub fn timestamp_sei(timestamp_ns: i64) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(32);
    rbsp.push(5); // payload type: user_data_unregistered
    rbsp.push(24); // payload size: 16-byte UUID + 8-byte timestamp
    rbsp.extend_from_slice(&UUID);
    rbsp.extend_from_slice(&timestamp_ns.to_be_bytes());
    rbsp.push(0x80); // rbsp_stop_one_bit

    let mut nal = vec![0, 0, 0, 1, 0x4E, 0x01]; // start code + NUH: PREFIX_SEI (39), tid+1 = 1
    ep_encode(&rbsp, &mut nal);
    nal
}

/// Find Relay's timestamp SEI in an Annex B access unit.
pub fn extract_timestamp(au: &[u8]) -> Option<i64> {
    let mut i = 0;
    while i + 4 <= au.len() {
        // Find the next start code (3- or 4-byte).
        let sc = if au[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if au[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        let nal_start = i + sc;
        if nal_start + 2 > au.len() {
            return None;
        }
        let nal_type = (au[nal_start] >> 1) & 0x3F;
        // Next start code bounds this NAL.
        let mut end = au.len();
        let mut j = nal_start;
        while j + 3 <= au.len() {
            if au[j..].starts_with(&[0, 0, 1]) || au[j..].starts_with(&[0, 0, 0, 1]) {
                end = j;
                break;
            }
            j += 1;
        }
        if nal_type == 39 {
            let rbsp = ep_decode(&au[nal_start + 2..end]);
            if rbsp.len() >= 2 + 24 && rbsp[0] == 5 && rbsp[1] == 24 && rbsp[2..18] == UUID {
                let mut ts = [0u8; 8];
                ts.copy_from_slice(&rbsp[18..26]);
                return Some(i64::from_be_bytes(ts));
            }
        }
        i = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sei_round_trips() {
        for ts in [0i64, 1, -1, 1_700_000_000_000_000_000, i64::MAX, 0x0000_0001_0000_0002] {
            let nal = timestamp_sei(ts);
            assert_eq!(extract_timestamp(&nal), Some(ts), "ts {ts}");
            // Prepended to a fake AU it is still found.
            let mut au = nal.clone();
            au.extend_from_slice(&[0, 0, 0, 1, 0x28, 0x01, 0xaa, 0xbb]);
            assert_eq!(extract_timestamp(&au), Some(ts));
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
        assert_eq!(extract_timestamp(&nal), None);
    }

    #[test]
    fn sei_found_after_other_nals_and_with_3_byte_start_codes() {
        let ts = 1_726_000_000_123_456_789i64;
        // AU: VPS-ish NAL first, then our SEI re-prefixed with a 3-byte start code.
        let mut au = vec![0, 0, 0, 1, 0x40, 0x01, 0x0c];
        let sei = timestamp_sei(ts);
        au.extend_from_slice(&[0, 0, 1]); // 3-byte start code
        au.extend_from_slice(&sei[4..]); // SEI NAL without its 4-byte start code
        au.extend_from_slice(&[0, 0, 0, 1, 0x28, 0x01, 0xaa]);
        assert_eq!(extract_timestamp(&au), Some(ts));
    }

    #[test]
    fn timestamps_with_zero_runs_survive_emulation_prevention() {
        // Big-endian encodings containing 00 00 0x runs, which must be
        // EP-escaped in the NAL and still parse back.
        for ts in [0x0000_0000_0000_0001i64, 0x0100_0000_0200_0003, 0x0000_0100_0000_0200, i64::MIN]
        {
            let nal = timestamp_sei(ts);
            assert_eq!(extract_timestamp(&nal), Some(ts), "ts {ts:#x}");
        }
    }

    #[test]
    fn truncated_and_garbage_input_does_not_panic() {
        let good = timestamp_sei(42);
        for cut in 0..good.len() {
            let _ = extract_timestamp(&good[..cut]); // must not panic
        }
        assert_eq!(extract_timestamp(&[]), None);
        assert_eq!(extract_timestamp(&[0, 0, 0, 1]), None);
        assert_eq!(extract_timestamp(&[0, 0, 1, 0x4E]), None);
        // Deterministic pseudo-random bytes: no crash, no false positive.
        let mut x = 0x12345678u32;
        let junk: Vec<u8> = (0..4096)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect();
        let _ = extract_timestamp(&junk);
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
        assert_eq!(extract_timestamp(&nal), None);
    }
}
