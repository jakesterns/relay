//! Hardware-only HEVC decode (the receiver side). Media Foundation async MFT,
//! bound to the D3D11 device that owns the presentation window, so decoded
//! NV12 stays on the GPU all the way to the swapchain.

pub mod mf;

/// Minimal Annex B bit reader for parsing an HEVC SPS.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }
    fn u(&mut self, n: u32) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.bit / 8;
            if byte >= self.data.len() {
                return v << (n - (v.leading_zeros())); // ran out; return what we have
            }
            let shift = 7 - (self.bit % 8);
            let b = (self.data[byte] >> shift) & 1;
            v = (v << 1) | b as u64;
            self.bit += 1;
        }
        v
    }
    /// Unsigned Exp-Golomb.
    fn ue(&mut self) -> u64 {
        let mut zeros = 0;
        while self.bit / 8 < self.data.len() && self.u(1) == 0 {
            zeros += 1;
            if zeros > 31 {
                return 0;
            }
        }
        if zeros == 0 {
            return 0;
        }
        (1u64 << zeros) - 1 + self.u(zeros)
    }
}

/// Strip emulation-prevention bytes from an RBSP.
fn deemulate(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Parse (width, height) in luma samples from the SPS in an Annex B AU.
/// Ignores conformance-window cropping (good enough to size the window).
pub fn probe_dimensions(au: &[u8]) -> Option<(u32, u32)> {
    let sps = find_nal(au, 33)?;
    let rbsp = deemulate(&sps[2..]); // skip 2-byte NAL header
    let mut r = BitReader::new(&rbsp);
    let _sps_vps_id = r.u(4);
    let max_sub_layers_minus1 = r.u(3) as u32;
    let _nesting = r.u(1);
    // profile_tier_level: 88 bits general + 8 bits general_level_idc.
    r.u(88);
    r.u(8);
    // Sub-layer present flags (only when max_sub_layers_minus1 > 0).
    if max_sub_layers_minus1 > 0 {
        let mut profile_present = [false; 8];
        let mut level_present = [false; 8];
        for i in 0..max_sub_layers_minus1 as usize {
            profile_present[i] = r.u(1) == 1;
            level_present[i] = r.u(1) == 1;
        }
        if max_sub_layers_minus1 > 0 {
            for _ in max_sub_layers_minus1..8 {
                r.u(2);
            }
        }
        for i in 0..max_sub_layers_minus1 as usize {
            if profile_present[i] {
                r.u(88);
            }
            if level_present[i] {
                r.u(8);
            }
        }
    }
    let _sps_id = r.ue();
    let chroma_format_idc = r.ue();
    if chroma_format_idc == 3 {
        r.u(1);
    }
    let width = r.ue() as u32;
    let height = r.ue() as u32;
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return None;
    }
    Some((width, height))
}

/// Return the NAL body (including 2-byte header) of the first NAL of `nal_type`.
fn find_nal(au: &[u8], nal_type: u8) -> Option<Vec<u8>> {
    let mut i = 0;
    while i + 4 <= au.len() {
        let sc = if au[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if au[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        let start = i + sc;
        if start + 2 > au.len() {
            return None;
        }
        let t = (au[start] >> 1) & 0x3F;
        let mut end = au.len();
        let mut j = start;
        while j + 3 <= au.len() {
            if au[j..].starts_with(&[0, 0, 1]) || au[j..].starts_with(&[0, 0, 0, 1]) {
                end = j;
                break;
            }
            j += 1;
        }
        if t == nal_type {
            return Some(au[start..end].to_vec());
        }
        i = end;
    }
    None
}
