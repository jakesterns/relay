//! Annex B ↔ MP4 helpers for the recorded HEVC bitstream: start-code
//! splitting, length-prefix conversion, parameter-set extraction and the
//! minimal SPS fields the `hvcC` box needs. Pure — no OS dependencies.

/// H.265 NAL unit types (nal_unit_header, 6-bit type).
pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
pub const NAL_AUD: u8 = 35;
pub const NAL_SEI_PREFIX: u8 = 39;

/// The 6-bit H.265 NAL type of a NAL unit's first byte.
pub fn nal_type(nal: &[u8]) -> u8 {
    if nal.is_empty() {
        return 0xFF;
    }
    (nal[0] >> 1) & 0x3F
}

/// Split an Annex B stream into NAL units (3- and 4-byte start codes).
pub fn split_nalus(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
    while i + 3 <= data.len() {
        let three = data[i..i + 3] == [0, 0, 1];
        let four = i + 4 <= data.len() && data[i..i + 4] == [0, 0, 0, 1];
        if three || four {
            if let Some(s) = start {
                out.push(&data[s..i]);
            }
            i += if four { 4 } else { 3 };
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < data.len() {
            out.push(&data[s..]);
        }
    }
    out
}

/// Parameter sets pulled out of a keyframe access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamSets {
    pub vps: Vec<u8>,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

/// Extract VPS/SPS/PPS from an Annex B access unit (first of each kind).
pub fn extract_param_sets(annexb: &[u8]) -> Option<ParamSets> {
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for nal in split_nalus(annexb) {
        match nal_type(nal) {
            NAL_VPS if vps.is_none() => vps = Some(nal.to_vec()),
            NAL_SPS if sps.is_none() => sps = Some(nal.to_vec()),
            NAL_PPS if pps.is_none() => pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    Some(ParamSets { vps: vps?, sps: sps?, pps: pps? })
}

/// Convert an Annex B access unit into length-prefixed (4-byte) MP4 sample
/// data, dropping parameter sets and AUDs (they live in `hvcC`, not samples).
pub fn to_mp4_sample(annexb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(annexb.len() + 16);
    for nal in split_nalus(annexb) {
        match nal_type(nal) {
            NAL_VPS | NAL_SPS | NAL_PPS | NAL_AUD => continue,
            _ => {
                out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                out.extend_from_slice(nal);
            }
        }
    }
    out
}

/// Remove emulation-prevention bytes (00 00 03 → 00 00) from a NAL payload.
pub fn rbsp_unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0u32;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue; // emulation-prevention byte
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// The profile_tier_level fields `hvcC` repeats, read from the SPS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpsSummary {
    pub general_profile_space: u8,
    pub general_tier_flag: u8,
    pub general_profile_idc: u8,
    pub general_profile_compatibility_flags: u32,
    pub general_constraint_indicator_flags: u64, // 48 bits
    pub general_level_idc: u8,
}

/// Parse the fixed-position profile_tier_level from an SPS NAL. Layout after
/// the 2-byte NAL header: sps_video_parameter_set_id(4) +
/// sps_max_sub_layers_minus1(3) + sps_temporal_id_nesting_flag(1) = 1 byte,
/// then 12 bytes of profile_tier_level (no sub-layer fields needed — the
/// encoder emits a single temporal layer).
pub fn parse_sps_summary(sps_nal: &[u8]) -> Option<SpsSummary> {
    let r = rbsp_unescape(sps_nal);
    let ptl = r.get(3..15)?;
    Some(SpsSummary {
        general_profile_space: ptl[0] >> 6,
        general_tier_flag: (ptl[0] >> 5) & 1,
        general_profile_idc: ptl[0] & 0x1F,
        general_profile_compatibility_flags: u32::from_be_bytes(ptl[1..5].try_into().ok()?),
        general_constraint_indicator_flags: {
            let mut b = [0u8; 8];
            b[2..8].copy_from_slice(&ptl[5..11]);
            u64::from_be_bytes(b)
        },
        general_level_idc: ptl[11],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(ty: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![ty << 1, 0x01];
        v.extend_from_slice(body);
        v
    }

    fn annexb(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut v = Vec::new();
        for (i, n) in nals.iter().enumerate() {
            v.extend_from_slice(if i == 0 { &[0, 0, 0, 1] } else { &[0, 0, 1] });
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn split_handles_both_start_code_lengths_and_garbage() {
        let a = nal(19, b"idr");
        let b = nal(1, b"p");
        let data = annexb(&[a.clone(), b.clone()]);
        assert_eq!(split_nalus(&data), vec![a.as_slice(), b.as_slice()]);

        assert!(split_nalus(b"").is_empty());
        assert!(split_nalus(b"no start code here").is_empty());
        // Trailing empty NAL (start code at the very end) is dropped.
        assert_eq!(split_nalus(&[0, 0, 1]).len(), 0);
    }

    #[test]
    fn param_sets_extracted_and_stripped_from_samples() {
        let vps = nal(NAL_VPS, b"v");
        let sps = nal(NAL_SPS, b"s");
        let pps = nal(NAL_PPS, b"p");
        let idr = nal(19, b"frame-data");
        let au = annexb(&[vps.clone(), sps.clone(), pps.clone(), idr.clone()]);

        let ps = extract_param_sets(&au).unwrap();
        assert_eq!(ps, ParamSets { vps, sps, pps });

        let sample = to_mp4_sample(&au);
        let mut want = (idr.len() as u32).to_be_bytes().to_vec();
        want.extend_from_slice(&idr);
        assert_eq!(sample, want, "sample keeps only the slice NAL, length-prefixed");

        // A non-key AU without parameter sets yields none.
        assert!(extract_param_sets(&annexb(&[nal(1, b"p")])).is_none());
    }

    #[test]
    fn rbsp_unescape_removes_emulation_prevention() {
        assert_eq!(rbsp_unescape(&[0, 0, 3, 1]), vec![0, 0, 1]);
        assert_eq!(rbsp_unescape(&[0, 0, 3, 0, 0, 3, 3]), vec![0, 0, 0, 0, 3]);
        // A 3 not after two zeros is data.
        assert_eq!(rbsp_unescape(&[1, 3, 0, 3]), vec![1, 3, 0, 3]);
    }

    #[test]
    fn sps_summary_reads_profile_tier_level() {
        // Header 0x42 0x01, then 1 byte (vps_id/sub_layers/nesting), then PTL:
        // profile_space 0, tier 0, profile_idc 1 (Main); compat 0x60000000;
        // constraints 0x900000000000; level 123.
        let mut sps = vec![0x42, 0x01, 0x01];
        sps.push(0x01); // space/tier/idc
        sps.extend_from_slice(&0x6000_0000u32.to_be_bytes());
        sps.extend_from_slice(&[0x90, 0, 0, 0, 0, 0]);
        sps.push(123);
        let s = parse_sps_summary(&sps).unwrap();
        assert_eq!(s.general_profile_idc, 1);
        assert_eq!(s.general_profile_compatibility_flags, 0x6000_0000);
        assert_eq!(s.general_constraint_indicator_flags, 0x9000_0000_0000);
        assert_eq!(s.general_level_idc, 123);
        assert!(parse_sps_summary(&[0x42, 0x01]).is_none(), "truncated SPS");
    }
}
