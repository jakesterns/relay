//! The two video codecs a share can run on, and the rule that picks one.
//!
//! HEVC gives better quality per bit, but on Windows its decoder is only free
//! on PCs whose manufacturer licensed it; everyone else is asked to pay. H.264
//! decode ships with every Windows install, and every GPU that encodes HEVC
//! also encodes H.264. So the sender offers both on one video m-line, the
//! receiver registers only the codecs it can actually decode, and the answer
//! decides: HEVC when both ends can do it, H.264 otherwise. It is a
//! capability, never a user setting.
//!
//! Pure — no OS dependencies — so the negotiation, the SDP parse and the
//! bitstream helpers are unit-tested on any machine.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    Hevc,
    H264,
}

/// Preference order. The negotiated codec is the first entry both ends
/// support, so this is the whole policy.
pub const PREFERENCE: [VideoCodec; 2] = [VideoCodec::Hevc, VideoCodec::H264];

impl VideoCodec {
    /// Wire/JSON name, and the token `RELAY_VIDEO_CODECS` accepts.
    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::Hevc => "hevc",
            VideoCodec::H264 => "h264",
        }
    }

    /// How the UI and the logs print it.
    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::Hevc => "HEVC",
            VideoCodec::H264 => "H.264",
        }
    }

    /// RTP mime type, matching webrtc-rs's `MIME_TYPE_HEVC` / `MIME_TYPE_H264`.
    pub fn mime(self) -> &'static str {
        match self {
            VideoCodec::Hevc => "video/H265",
            VideoCodec::H264 => "video/H264",
        }
    }

    /// Fixed dynamic payload types. HEVC keeps the 98 it has always had, so an
    /// older peer that only knows HEVC still negotiates against a new one.
    pub fn payload_type(self) -> u8 {
        match self {
            VideoCodec::Hevc => 98,
            VideoCodec::H264 => 102,
        }
    }

    /// fmtp line. Both ends register the identical string, so webrtc-rs's
    /// fmtp matcher (packetization-mode + profile) always finds an exact match.
    /// High profile, level 5.2: 4K60 needs 5.2, and every H.264 decoder that
    /// ships with Windows decodes High.
    pub fn fmtp(self) -> &'static str {
        match self {
            VideoCodec::Hevc => "",
            VideoCodec::H264 => {
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640034"
            }
        }
    }

    pub fn from_payload_type(pt: u8) -> Option<Self> {
        PREFERENCE.into_iter().find(|c| c.payload_type() == pt)
    }

    /// Case-insensitive; accepts the rtpmap encoding name (`H265`, `H264`)
    /// as well as a full mime type.
    pub fn from_encoding_name(name: &str) -> Option<Self> {
        let n = name.trim().rsplit('/').next().unwrap_or("").to_ascii_uppercase();
        match n.as_str() {
            "H265" | "HEVC" => Some(VideoCodec::Hevc),
            "H264" | "AVC" => Some(VideoCodec::H264),
            _ => None,
        }
    }

    fn from_token(t: &str) -> Option<Self> {
        match t.trim().to_ascii_lowercase().as_str() {
            "hevc" | "h265" | "h.265" => Some(VideoCodec::Hevc),
            "h264" | "h.264" | "avc" => Some(VideoCodec::H264),
            _ => None,
        }
    }

    /// NAL unit type from the first byte(s) of a NAL.
    pub fn nal_type(self, nal: &[u8]) -> u8 {
        match (self, nal.first()) {
            (_, None) => 0xFF,
            (VideoCodec::Hevc, Some(b)) => (b >> 1) & 0x3F,
            (VideoCodec::H264, Some(b)) => b & 0x1F,
        }
    }

    /// NAL header length in bytes.
    pub fn nal_header_len(self) -> usize {
        match self {
            VideoCodec::Hevc => 2,
            VideoCodec::H264 => 1,
        }
    }

    pub fn sps_type(self) -> u8 {
        match self {
            VideoCodec::Hevc => 33,
            VideoCodec::H264 => 7,
        }
    }

    pub fn pps_type(self) -> u8 {
        match self {
            VideoCodec::Hevc => 34,
            VideoCodec::H264 => 8,
        }
    }

    /// Parameter sets and access-unit delimiters: they live in the container's
    /// decoder configuration, not in MP4/Matroska samples.
    pub fn is_out_of_band(self, nal_type: u8) -> bool {
        match self {
            VideoCodec::Hevc => matches!(nal_type, 32..=35),
            VideoCodec::H264 => matches!(nal_type, 7..=9),
        }
    }

    /// Does this access unit start a decodable sequence?
    ///
    /// A decoder fed inter-coded frames before its first keyframe predicts
    /// from nothing and paints garbage. On a real receiver that showed as a
    /// smeared picture for the moment between the video track arriving and the
    /// next keyframe — which, with a 10 s GOP, can be a long moment.
    ///
    /// H.264: NAL type 5 is an IDR slice. HEVC: 16..=21 covers BLA, IDR and
    /// CRA, all of which are valid random-access points.
    pub fn is_keyframe(self, au: &[u8]) -> bool {
        split_nalus(au).into_iter().any(|n| {
            let t = self.nal_type(n);
            match self {
                VideoCodec::H264 => t == 5,
                VideoCodec::Hevc => (16..=21).contains(&t),
            }
        })
    }

    /// The SEI NAL type Relay's timestamp rides in, and that NAL's header.
    pub fn sei_type(self) -> u8 {
        match self {
            VideoCodec::Hevc => 39, // PREFIX_SEI
            VideoCodec::H264 => 6,
        }
    }

    pub fn sei_header(self) -> &'static [u8] {
        match self {
            VideoCodec::Hevc => &[0x4E, 0x01], // PREFIX_SEI, layer 0, tid+1 = 1
            VideoCodec::H264 => &[0x06],       // nal_ref_idc 0, SEI
        }
    }

    /// Coded (width, height) in luma samples from the SPS in an Annex B access
    /// unit. H.264 applies the frame cropping window (1080p is coded as 1088
    /// rows and cropped); HEVC reports the coded size.
    pub fn dimensions(self, au: &[u8]) -> Option<(u32, u32)> {
        let sps = split_nalus(au).into_iter().find(|n| self.nal_type(n) == self.sps_type())?;
        let rbsp = rbsp_unescape(sps.get(self.nal_header_len()..)?);
        let (w, h) = match self {
            VideoCodec::Hevc => hevc_sps_dimensions(&rbsp)?,
            VideoCodec::H264 => h264_sps_dimensions(&rbsp)?,
        };
        (w > 0 && h > 0 && w <= 16384 && h <= 16384).then_some((w, h))
    }
}

/// The codecs Relay may use on this machine, before hardware is consulted.
/// `RELAY_VIDEO_CODECS=h264` (comma list) narrows it — a test hook for the
/// benchmarks and for simulating a receiver with no HEVC decoder, not a
/// setting: nothing in the app writes it.
pub fn allowed_codecs() -> Vec<VideoCodec> {
    match std::env::var("RELAY_VIDEO_CODECS") {
        Ok(v) => parse_codec_list(&v),
        Err(_) => PREFERENCE.to_vec(),
    }
}

/// Parse a comma list into codecs in *preference* order, deduplicated.
/// Unknown tokens are ignored; a list that names nothing known means "all".
pub fn parse_codec_list(s: &str) -> Vec<VideoCodec> {
    let named: Vec<VideoCodec> = s.split(',').filter_map(VideoCodec::from_token).collect();
    if named.is_empty() {
        return PREFERENCE.to_vec();
    }
    PREFERENCE.into_iter().filter(|c| named.contains(c)).collect()
}

/// Keep `candidates` that `supported` says yes to, in preference order.
pub fn filter_supported(
    candidates: &[VideoCodec],
    mut supported: impl FnMut(VideoCodec) -> bool,
) -> Vec<VideoCodec> {
    PREFERENCE.into_iter().filter(|c| candidates.contains(c) && supported(*c)).collect()
}

/// The negotiated codec: the first in [`PREFERENCE`] that the sender offered
/// and the receiver kept in its answer.
pub fn pick(offered: &[VideoCodec], answered: &[VideoCodec]) -> Option<VideoCodec> {
    PREFERENCE.into_iter().find(|c| offered.contains(c) && answered.contains(c))
}

/// Video codecs listed on the first `m=video` section of an SDP, in m-line
/// payload order. Rejected sections (port 0) list nothing. Only codecs Relay
/// knows are returned.
pub fn sdp_video_codecs(sdp: &str) -> Vec<VideoCodec> {
    let mut in_video = false;
    let mut seen_video = false;
    let mut pts: Vec<u8> = Vec::new();
    let mut rtpmap: Vec<(u8, VideoCodec)> = Vec::new();
    for line in sdp.lines().map(str::trim) {
        if let Some(m) = line.strip_prefix("m=") {
            if seen_video {
                break; // only the first video section
            }
            let mut parts = m.split_whitespace();
            in_video = parts.next() == Some("video");
            if in_video {
                seen_video = true;
                let port = parts.next().unwrap_or("0");
                let _proto = parts.next();
                if port != "0" {
                    pts = parts.filter_map(|p| p.parse().ok()).collect();
                }
            }
            continue;
        }
        if !in_video {
            continue;
        }
        if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            let mut it = rest.splitn(2, ' ');
            let (Some(pt), Some(enc)) = (it.next(), it.next()) else { continue };
            let Ok(pt) = pt.trim().parse::<u8>() else { continue };
            let name = enc.split('/').next().unwrap_or("");
            if let Some(c) = VideoCodec::from_encoding_name(name) {
                rtpmap.push((pt, c));
            }
        }
    }
    let mut out = Vec::new();
    for pt in pts {
        if let Some(&(_, c)) = rtpmap.iter().find(|(p, _)| *p == pt) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
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

/// Remove emulation-prevention bytes (00 00 03 → 00 00) from a NAL payload.
pub fn rbsp_unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0u32;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// MSB-first bit reader over an RBSP. Reads past the end yield `None`.
struct Bits<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn u(&mut self, n: u32) -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = *self.data.get(self.bit / 8)?;
            v = (v << 1) | ((byte >> (7 - self.bit % 8)) & 1) as u64;
            self.bit += 1;
        }
        Some(v)
    }

    fn flag(&mut self) -> Option<bool> {
        self.u(1).map(|b| b == 1)
    }

    /// Unsigned Exp-Golomb.
    fn ue(&mut self) -> Option<u64> {
        let mut zeros = 0;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u64 << zeros) - 1 + self.u(zeros)?)
    }

    /// Signed Exp-Golomb.
    fn se(&mut self) -> Option<i64> {
        let k = self.ue()?;
        Some(if k % 2 == 1 { k.div_ceil(2) as i64 } else { -((k / 2) as i64) })
    }
}

/// HEVC `seq_parameter_set_rbsp` up to the picture size.
fn hevc_sps_dimensions(rbsp: &[u8]) -> Option<(u32, u32)> {
    let mut r = Bits::new(rbsp);
    r.u(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.u(3)? as usize;
    r.u(1)?; // temporal_id_nesting
    r.u(88)?; // general profile (space/tier/idc/compat/constraints)
    r.u(8)?; // general_level_idc
    if max_sub_layers_minus1 > 0 {
        let mut profile_present = [false; 8];
        let mut level_present = [false; 8];
        for i in 0..max_sub_layers_minus1 {
            profile_present[i] = r.flag()?;
            level_present[i] = r.flag()?;
        }
        for _ in max_sub_layers_minus1..8 {
            r.u(2)?;
        }
        for i in 0..max_sub_layers_minus1 {
            if profile_present[i] {
                r.u(88)?;
            }
            if level_present[i] {
                r.u(8)?;
            }
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    if r.ue()? == 3 {
        r.u(1)?; // separate_colour_plane_flag
    }
    let w = r.ue()? as u32;
    let h = r.ue()? as u32;
    Some((w, h))
}

/// H.264 `seq_parameter_set_data` up to the frame cropping window.
fn h264_sps_dimensions(rbsp: &[u8]) -> Option<(u32, u32)> {
    let mut r = Bits::new(rbsp);
    let profile_idc = r.u(8)?;
    r.u(8)?; // constraint flags + reserved
    r.u(8)?; // level_idc
    r.ue()?; // seq_parameter_set_id
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = false;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = r.ue()?;
        if chroma_format_idc == 3 {
            separate_colour_plane = r.flag()?;
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.u(1)?; // qpprime_y_zero_transform_bypass_flag
        if r.flag()? {
            // seq_scaling_matrix_present_flag
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.flag()? {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i64, 8i64);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256).rem_euclid(256);
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            r.u(1)?; // delta_pic_order_always_zero_flag
            r.se()?; // offset_for_non_ref_pic
            r.se()?; // offset_for_top_to_bottom_field
            for _ in 0..r.ue()?.min(255) {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.u(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = r.ue()? + 1;
    let height_map_units = r.ue()? + 1;
    let frame_mbs_only = r.flag()?;
    if !frame_mbs_only {
        r.u(1)?; // mb_adaptive_frame_field_flag
    }
    r.u(1)?; // direct_8x8_inference_flag
    let field_factor = if frame_mbs_only { 1 } else { 2 };
    let mut w = width_mbs * 16;
    let mut h = height_map_units * 16 * field_factor;
    if r.flag()? {
        // frame_cropping_flag
        let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        let chroma_array_type = if separate_colour_plane { 0 } else { chroma_format_idc };
        let (sub_w, sub_h) = match chroma_array_type {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let crop_x = if chroma_array_type == 0 { 1 } else { sub_w };
        let crop_y = if chroma_array_type == 0 { field_factor } else { sub_h * field_factor };
        w = w.checked_sub(crop_x * (left + right))?;
        h = h.checked_sub(crop_y * (top + bottom))?;
    }
    Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A bit writer for building SPS fixtures, with Exp-Golomb.
    #[derive(Default)]
    pub(crate) struct BitWriter {
        bytes: Vec<u8>,
        bit: usize,
    }

    impl BitWriter {
        pub(crate) fn u(&mut self, n: u32, v: u64) -> &mut Self {
            for i in (0..n).rev() {
                if self.bit % 8 == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= (((v >> i) & 1) as u8) << (7 - self.bit % 8);
                self.bit += 1;
            }
            self
        }
        pub(crate) fn ue(&mut self, v: u64) -> &mut Self {
            let x = v + 1;
            let bits = 64 - x.leading_zeros();
            self.u(bits - 1, 0);
            self.u(bits, x)
        }
        pub(crate) fn se(&mut self, v: i64) -> &mut Self {
            let k = if v > 0 { (v as u64) * 2 - 1 } else { (-v) as u64 * 2 };
            self.ue(k)
        }
        pub(crate) fn finish(&mut self) -> Vec<u8> {
            self.u(1, 1); // rbsp_stop_one_bit
            self.bytes.clone()
        }
    }

    /// An H.264 High-profile SPS NAL (with header) for a `w`×`h` 4:2:0 frame,
    /// cropped the way a hardware encoder codes 1080p.
    pub(crate) fn h264_sps(w: u32, h: u32) -> Vec<u8> {
        let mbs_w = w.div_ceil(16);
        let mbs_h = h.div_ceil(16);
        let crop_bottom = (mbs_h * 16 - h) / 2;
        let crop_right = (mbs_w * 16 - w) / 2;
        let mut b = BitWriter::default();
        b.u(8, 100).u(8, 0).u(8, 52).ue(0); // High, level 5.2, sps id 0
        b.ue(1).ue(0).ue(0).u(1, 0).u(1, 0); // 4:2:0, 8-bit, no scaling matrix
        b.ue(0).ue(0).ue(0); // frame_num bits, poc type 0, poc lsb bits
        b.ue(1).u(1, 0); // one ref frame, no gaps
        b.ue(mbs_w as u64 - 1).ue(mbs_h as u64 - 1);
        b.u(1, 1).u(1, 1); // frame_mbs_only, direct_8x8
        if crop_bottom > 0 || crop_right > 0 {
            b.u(1, 1).ue(0).ue(crop_right as u64).ue(0).ue(crop_bottom as u64);
        } else {
            b.u(1, 0);
        }
        b.u(1, 0); // vui_parameters_present_flag
        let mut nal = vec![0x67];
        nal.extend(b.finish());
        nal
    }

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn hevc_wins_only_when_both_ends_have_it() {
        use VideoCodec::*;
        assert_eq!(pick(&[Hevc, H264], &[Hevc, H264]), Some(Hevc));
        // The case S27 exists for: a receiver with no HEVC decoder.
        assert_eq!(pick(&[Hevc, H264], &[H264]), Some(H264));
        // A sender whose GPU only encodes H.264.
        assert_eq!(pick(&[H264], &[Hevc, H264]), Some(H264));
        // Answer order does not override the preference.
        assert_eq!(pick(&[Hevc, H264], &[H264, Hevc]), Some(Hevc));
        // An older HEVC-only receiver still works against a new sender.
        assert_eq!(pick(&[Hevc, H264], &[Hevc]), Some(Hevc));
        assert_eq!(pick(&[Hevc], &[H264]), None);
        assert_eq!(pick(&[], &[Hevc]), None);
    }

    #[test]
    fn codec_list_env_parses_in_preference_order() {
        use VideoCodec::*;
        assert_eq!(parse_codec_list("h264"), vec![H264]);
        assert_eq!(parse_codec_list("H264, hevc"), vec![Hevc, H264]);
        assert_eq!(parse_codec_list("h265"), vec![Hevc]);
        assert_eq!(parse_codec_list("hevc,hevc"), vec![Hevc]);
        // Nothing recognisable must not silently disable video.
        assert_eq!(parse_codec_list("vp9"), vec![Hevc, H264]);
        assert_eq!(parse_codec_list(""), vec![Hevc, H264]);
        assert_eq!(filter_supported(&[Hevc, H264], |c| c == H264), vec![H264]);
        assert_eq!(filter_supported(&[Hevc], |c| c == H264), vec![]);
    }

    #[test]
    fn payload_types_and_names_round_trip() {
        for c in PREFERENCE {
            assert_eq!(VideoCodec::from_payload_type(c.payload_type()), Some(c));
            assert_eq!(VideoCodec::from_encoding_name(c.mime()), Some(c));
            assert_eq!(VideoCodec::from_token(c.name()), Some(c));
        }
        assert_eq!(VideoCodec::Hevc.payload_type(), 98, "HEVC pt is the pre-S27 wire contract");
        assert_ne!(VideoCodec::Hevc.payload_type(), VideoCodec::H264.payload_type());
        assert_eq!(VideoCodec::from_payload_type(111), None);
        assert_eq!(serde_json::to_string(&VideoCodec::H264).unwrap(), r#""h264""#);
    }

    #[test]
    fn answer_sdp_lists_the_video_codecs_it_kept() {
        let both = "v=0\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 120\r\n\
            a=rtpmap:120 opus/48000/2\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 98 102\r\n\
            a=rtpmap:98 H265/90000\r\n\
            a=rtpmap:102 H264/90000\r\n\
            a=fmtp:102 packetization-mode=1\r\n";
        assert_eq!(sdp_video_codecs(both), vec![VideoCodec::Hevc, VideoCodec::H264]);

        let h264_only = "v=0\nm=video 9 UDP/TLS/RTP/SAVPF 102\na=rtpmap:102 H264/90000\n";
        assert_eq!(sdp_video_codecs(h264_only), vec![VideoCodec::H264]);

        // Rejected video section: nothing negotiated.
        let rejected = "v=0\r\nm=video 0 UDP/TLS/RTP/SAVPF 0\r\n";
        assert!(sdp_video_codecs(rejected).is_empty());
        // rtpmap lines under the audio section never count as video.
        let audio_only = "v=0\r\nm=audio 9 RTP 102\r\na=rtpmap:102 H264/90000\r\n";
        assert!(sdp_video_codecs(audio_only).is_empty());
        // A payload type listed without an rtpmap, or an unknown codec, is skipped.
        let odd = "m=video 9 RTP 96 97 102\r\na=rtpmap:96 VP8/90000\r\na=rtpmap:102 h264/90000\r\n";
        assert_eq!(sdp_video_codecs(odd), vec![VideoCodec::H264]);
        assert!(sdp_video_codecs("").is_empty());
    }

    #[test]
    fn nal_helpers_are_per_codec() {
        assert_eq!(VideoCodec::H264.nal_type(&[0x67]), 7);
        assert_eq!(VideoCodec::H264.nal_type(&[0x65]), 5);
        assert_eq!(VideoCodec::Hevc.nal_type(&[0x42, 0x01]), 33);
        assert_eq!(VideoCodec::Hevc.nal_type(&[]), 0xFF);
        for t in [7, 8, 9] {
            assert!(VideoCodec::H264.is_out_of_band(t));
        }
        assert!(!VideoCodec::H264.is_out_of_band(5));
        assert!(!VideoCodec::H264.is_out_of_band(6), "SEI stays in the sample");
        for t in [32, 33, 34, 35] {
            assert!(VideoCodec::Hevc.is_out_of_band(t));
        }
        assert!(!VideoCodec::Hevc.is_out_of_band(19));
        assert_eq!(VideoCodec::H264.nal_type(VideoCodec::H264.sei_header()), 6);
        assert_eq!(VideoCodec::Hevc.nal_type(VideoCodec::Hevc.sei_header()), 39);
    }

    #[test]
    fn h264_sps_dimensions_apply_cropping() {
        for (w, h) in [(1920, 1080), (2560, 1440), (3840, 2160), (1280, 720), (640, 360)] {
            let sps = h264_sps(w, h);
            let au = annexb(&[&sps, &[0x68, 0xce], &[0x65, 0x88]]);
            assert_eq!(VideoCodec::H264.dimensions(&au), Some((w, h)), "{w}x{h}");
        }
        // No SPS in a P-frame AU.
        assert_eq!(VideoCodec::H264.dimensions(&annexb(&[&[0x41, 0x9a]])), None);
        // An HEVC reader never mistakes an H.264 SPS for its own.
        assert_eq!(VideoCodec::Hevc.dimensions(&annexb(&[&h264_sps(1920, 1080)])), None);
    }

    #[test]
    fn h264_sps_with_scaling_matrix_and_poc_type_1_still_parses() {
        let mut b = BitWriter::default();
        b.u(8, 100).u(8, 0).u(8, 42).ue(0);
        b.ue(1).ue(0).ue(0).u(1, 0);
        b.u(1, 1); // seq_scaling_matrix_present_flag
        b.u(1, 1); // list 0 present
        b.se(-8); // next becomes 0 → rest of list uses last
        for _ in 1..8 {
            b.u(1, 0);
        }
        b.ue(0).ue(1); // log2_max_frame_num, poc type 1
        b.u(1, 0).se(-2).se(3).ue(2).se(1).se(-1);
        b.ue(1).u(1, 0);
        b.ue(119).ue(67); // 1920 x 1088
        b.u(1, 1).u(1, 1);
        b.u(1, 1).ue(0).ue(0).ue(0).ue(4); // crop 8 rows
        let mut nal = vec![0x67];
        nal.extend(b.finish());
        assert_eq!(VideoCodec::H264.dimensions(&annexb(&[&nal])), Some((1920, 1080)));
    }

    #[test]
    fn truncated_sps_is_none_not_a_panic() {
        let sps = h264_sps(1920, 1080);
        for cut in 0..sps.len() {
            let _ = VideoCodec::H264.dimensions(&annexb(&[&sps[..cut]]));
        }
        assert_eq!(VideoCodec::H264.dimensions(&annexb(&[&sps[..4]])), None);
    }

    #[test]
    fn exp_golomb_round_trips() {
        let mut b = BitWriter::default();
        for v in [0u64, 1, 2, 7, 255, 65_535] {
            b.ue(v);
        }
        for v in [0i64, 1, -1, 5, -128] {
            b.se(v);
        }
        let bytes = b.finish();
        let mut r = Bits::new(&bytes);
        for v in [0u64, 1, 2, 7, 255, 65_535] {
            assert_eq!(r.ue(), Some(v));
        }
        for v in [0i64, 1, -1, 5, -128] {
            assert_eq!(r.se(), Some(v));
        }
    }
}
