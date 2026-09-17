//! Annex B ↔ MP4 helpers for the recorded bitstream (HEVC or H.264): start-code
//! splitting, length-prefix conversion, parameter-set extraction and the
//! minimal SPS fields the `hvcC` / `avcC` boxes need. Pure — no OS dependencies.
//!
//! The unsuffixed functions are the HEVC forms the recorder has always used;
//! the `_for` forms take the share's codec.

use crate::codec::VideoCodec;

pub use crate::codec::{rbsp_unescape, split_nalus};

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

/// Parameter sets pulled out of a keyframe access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamSets {
    /// HEVC only; empty for H.264, which has no VPS.
    pub vps: Vec<u8>,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

/// Extract VPS/SPS/PPS from an HEVC Annex B access unit (first of each kind).
pub fn extract_param_sets(annexb: &[u8]) -> Option<ParamSets> {
    extract_param_sets_for(VideoCodec::Hevc, annexb)
}

/// Extract the parameter sets `codec` needs from an Annex B access unit:
/// VPS/SPS/PPS for HEVC, SPS/PPS for H.264.
pub fn extract_param_sets_for(codec: VideoCodec, annexb: &[u8]) -> Option<ParamSets> {
    if codec == VideoCodec::H264 {
        let mut sps = None;
        let mut pps = None;
        for nal in split_nalus(annexb) {
            match codec.nal_type(nal) {
                7 if sps.is_none() => sps = Some(nal.to_vec()),
                8 if pps.is_none() => pps = Some(nal.to_vec()),
                _ => {}
            }
        }
        return Some(ParamSets { vps: Vec::new(), sps: sps?, pps: pps? });
    }
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
    to_mp4_sample_for(VideoCodec::Hevc, annexb)
}

/// [`to_mp4_sample`] for either codec: parameter sets and AUDs of `codec`
/// are dropped, everything else is length-prefixed.
pub fn to_mp4_sample_for(codec: VideoCodec, annexb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(annexb.len() + 16);
    for nal in split_nalus(annexb) {
        if codec.is_out_of_band(codec.nal_type(nal)) {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// The three SPS bytes `avcC` repeats: profile_idc, the constraint flags
/// (`profile_compatibility`) and level_idc. They sit at fixed positions right
/// after the one-byte NAL header, before any Exp-Golomb field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvcSummary {
    pub profile_idc: u8,
    pub profile_compatibility: u8,
    pub level_idc: u8,
}

pub fn parse_avc_sps_summary(sps_nal: &[u8]) -> Option<AvcSummary> {
    let r = rbsp_unescape(sps_nal);
    let b = r.get(1..4)?;
    Some(AvcSummary { profile_idc: b[0], profile_compatibility: b[1], level_idc: b[2] })
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

    #[test]
    fn h264_param_sets_and_samples() {
        let sps = vec![0x67, 100, 0, 52, 0xAC];
        let pps = vec![0x68, 0xEE, 0x3C];
        let aud = vec![0x09, 0xF0];
        let sei = vec![0x06, 5, 1];
        let idr = vec![0x65, 0x88, 0x84];
        let mut au = Vec::new();
        for n in [&aud, &sps, &pps, &sei, &idr] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(n);
        }
        let ps = extract_param_sets_for(VideoCodec::H264, &au).unwrap();
        assert_eq!(ps, ParamSets { vps: vec![], sps: sps.clone(), pps: pps.clone() });
        // The HEVC reader finds nothing it recognises in an H.264 AU.
        assert!(extract_param_sets(&au).is_none());

        let sample = to_mp4_sample_for(VideoCodec::H264, &au);
        let mut want = Vec::new();
        for n in [&sei, &idr] {
            want.extend_from_slice(&(n.len() as u32).to_be_bytes());
            want.extend_from_slice(n);
        }
        assert_eq!(sample, want, "SPS, PPS and AUD live in avcC, not the sample");

        let s = parse_avc_sps_summary(&sps).unwrap();
        assert_eq!((s.profile_idc, s.profile_compatibility, s.level_idc), (100, 0, 52));
        assert!(parse_avc_sps_summary(&[0x67, 100]).is_none());
        // No PPS: not a usable keyframe.
        let mut bare = vec![0, 0, 0, 1];
        bare.extend_from_slice(&sps);
        assert!(extract_param_sets_for(VideoCodec::H264, &bare).is_none());
    }
}
