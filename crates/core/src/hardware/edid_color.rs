//! What a panel says about its own colour, read out of its EDID.
//!
//! There is no AutoEQ for monitors, and there does not need to be: unlike a
//! headphone, a display reports its own characteristics. The base EDID block
//! carries the measured chromaticity of the primaries and white point plus a
//! gamma exponent, and the CTA-861 extension block carries the colorimetry
//! standards and HDR formats the panel accepts. That is per-unit, exact, free
//! of licensing, and needs no network — strictly better than a lookup table
//! keyed on model name.
//!
//! What EDID does *not* say is the panel technology. Nothing in the spec
//! distinguishes OLED from IPS from VA from mini-LED, and the proxies that
//! look promising do not hold up — see [`PanelGuess`]. So technology stays a
//! user-supplied field on [`super::Monitor`], and this module reports only
//! what the panel actually measured about itself.
//!
//! References: VESA E-EDID 1.4 §3.7 (chromaticity), CTA-861-G §7.5.5
//! (colorimetry data block) and §7.5.13 (HDR static metadata).

use serde::{Deserialize, Serialize};

/// CIE 1931 xy chromaticity.
pub type Xy = (f32, f32);

/// The panel's own primaries and white point, from EDID bytes 25..=34.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Chromaticity {
    pub red: Xy,
    pub green: Xy,
    pub blue: Xy,
    pub white: Xy,
}

/// Colour standards the panel advertises in its CTA colorimetry data block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colorimetry {
    pub bt2020_rgb: bool,
    pub bt2020_ycc: bool,
    pub bt2020_cycc: bool,
    pub adobe_rgb: bool,
    pub adobe_ycc: bool,
    pub s_ycc601: bool,
    pub xv_ycc709: bool,
    pub xv_ycc601: bool,
}

/// HDR formats and luminance, from the CTA HDR static metadata block plus the
/// two vendor blocks that carry the dynamic formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Hdr {
    /// SMPTE ST 2084 (PQ) — what "HDR10" means on a display.
    pub hdr10: bool,
    /// Hybrid Log-Gamma.
    pub hlg: bool,
    /// Traditional gamma HDR (rare, and not the same as HDR10).
    pub hdr_gamma: bool,
    /// Dolby Vision, from the Dolby vendor-specific data block.
    pub dolby_vision: bool,
    /// HDR10+, from the Samsung vendor-specific data block.
    pub hdr10_plus: bool,
    /// Peak luminance, cd/m².
    pub max_nits: Option<f32>,
    /// Maximum frame-average luminance, cd/m².
    pub max_frame_avg_nits: Option<f32>,
    /// Black level, cd/m². Near zero on an emissive panel.
    pub min_nits: Option<f32>,
}

impl Hdr {
    pub fn any(&self) -> bool {
        self.hdr10 || self.hlg || self.dolby_vision || self.hdr10_plus
    }
}

/// Everything the EDID says about this panel's colour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chromaticity: Option<Chromaticity>,
    /// Display gamma from byte 23, when provided (0xFF means "see DisplayID").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma: Option<f32>,
    /// Bits per primary colour, digital inputs only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    pub digital: bool,
    pub colorimetry: Colorimetry,
    pub hdr: Hdr,
    /// Fraction of each standard gamut the primaries enclose, 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Coverage>,
}

/// Gamut coverage, as a fraction of each reference triangle's area that the
/// panel's own triangle covers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub srgb: f32,
    pub dci_p3: f32,
    pub bt2020: f32,
}

/// What EDID can honestly say about a panel's colour capability.
///
/// Note what this is *not*: OLED / IPS / VA / mini-LED. Nothing in EDID
/// identifies the technology, and the obvious-looking proxies do not work.
/// Minimum luminance looks like a black-level giveaway, but the LG UltraGear
/// this was developed against — an LCD — encodes minimum-luminance code 1,
/// which the CTA formula turns into 9×10⁻⁵ cd/m². Peak luminance separates
/// tiers, not technologies. Reporting "OLED" from those would be a confident
/// lie, so technology stays a user-supplied field on [`super::Monitor`] and
/// this enum covers only the colour class, which the numbers do support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelGuess {
    /// P3-class primaries and a PQ transfer function.
    HdrWideGamut,
    /// P3-class primaries, no HDR signalling.
    WideGamut,
    /// Primaries close to sRGB.
    StandardGamut,
    #[default]
    Unknown,
}

impl PanelGuess {
    pub fn label(self) -> &'static str {
        match self {
            PanelGuess::HdrWideGamut => "Wide gamut · HDR",
            PanelGuess::WideGamut => "Wide gamut · SDR",
            PanelGuess::StandardGamut => "Standard gamut (sRGB)",
            PanelGuess::Unknown => "Unknown",
        }
    }
}

// Reference primaries, CIE 1931 xy.
const SRGB: [Xy; 3] = [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)];
const DCI_P3: [Xy; 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];
const BT2020: [Xy; 3] = [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)];

/// Parse the colour-relevant parts of a full EDID blob (base block plus any
/// extension blocks). Returns `None` only if the base block is unusable;
/// missing extensions simply leave their fields at their defaults.
pub fn parse(bytes: &[u8]) -> Option<ColorInfo> {
    if bytes.len() < 128 {
        return None;
    }

    // Byte 20: bit 7 set = digital input, bits 6..4 = bit depth code.
    let digital = bytes[20] & 0x80 != 0;
    let bit_depth = if digital {
        match (bytes[20] >> 4) & 0x07 {
            1 => Some(6),
            2 => Some(8),
            3 => Some(10),
            4 => Some(12),
            5 => Some(14),
            6 => Some(16),
            // 0 = undefined, 7 = reserved.
            _ => None,
        }
    } else {
        None
    };

    // Byte 23: (gamma * 100) - 100. 0xFF means the value lives elsewhere.
    let gamma = (bytes[23] != 0xFF).then(|| (bytes[23] as f32 + 100.0) / 100.0);

    let chromaticity = chromaticity(bytes);
    let coverage = chromaticity.map(|c| Coverage {
        srgb: coverage_of(&c, &SRGB),
        dci_p3: coverage_of(&c, &DCI_P3),
        bt2020: coverage_of(&c, &BT2020),
    });

    let mut colorimetry = Colorimetry::default();
    let mut hdr = Hdr::default();
    for block in cta_blocks(bytes) {
        apply_cta_block(block, &mut colorimetry, &mut hdr);
    }

    Some(ColorInfo { chromaticity, gamma, bit_depth, digital, colorimetry, hdr, coverage })
}

/// Bytes 25..=34 pack ten 10-bit values: two low bits each in bytes 25 and 26,
/// the high eight in bytes 27..=34.
fn chromaticity(b: &[u8]) -> Option<Chromaticity> {
    let lo_rg = b[25];
    let lo_bw = b[26];
    let v = |hi: u8, lo: u8| -> f32 { (((hi as u16) << 2) | (lo as u16 & 0x03)) as f32 / 1024.0 };
    let c = Chromaticity {
        red: (v(b[27], lo_rg >> 6), v(b[28], lo_rg >> 4)),
        green: (v(b[29], lo_rg >> 2), v(b[30], lo_rg)),
        blue: (v(b[31], lo_bw >> 6), v(b[32], lo_bw >> 4)),
        white: (v(b[33], lo_bw >> 2), v(b[34], lo_bw)),
    };
    // All-zero chromaticity means the panel declined to report it.
    let zero = |p: Xy| p.0 == 0.0 && p.1 == 0.0;
    (!(zero(c.red) && zero(c.green) && zero(c.blue))).then_some(c)
}

/// How much of `reference` the panel's triangle actually encloses: the area of
/// the intersection over the area of the reference.
///
/// Not the area ratio. A panel whose triangle is the right *size* but pushed
/// towards, say, cyan would score 100 % on a ratio while missing a slice of
/// deep red — which is exactly the error that makes a "100 % DCI-P3" claim
/// worthless. Intersection is the definition monitor reviews mean, and the
/// only one worth showing a user.
fn coverage_of(p: &Chromaticity, reference: &[Xy; 3]) -> f32 {
    let reference_area = polygon_area(reference);
    if reference_area <= 0.0 {
        return 0.0;
    }
    let panel = [p.red, p.green, p.blue];
    let overlap = polygon_area(&clip_to_triangle(reference, &panel));
    (overlap / reference_area).clamp(0.0, 1.0)
}

/// Shoelace area of a simple polygon.
fn polygon_area(poly: &[Xy]) -> f32 {
    if poly.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..poly.len() {
        let (x1, y1) = poly[i];
        let (x2, y2) = poly[(i + 1) % poly.len()];
        sum += x1 * y2 - x2 * y1;
    }
    sum.abs() / 2.0
}

/// Sutherland–Hodgman: clip `subject` against each edge of the triangle
/// `clip`. Both are convex, so the result is the convex intersection.
fn clip_to_triangle(subject: &[Xy], clip: &[Xy; 3]) -> Vec<Xy> {
    // Orient the clip triangle counter-clockwise so "inside" is consistent.
    let mut tri = *clip;
    let cross = (tri[1].0 - tri[0].0) * (tri[2].1 - tri[0].1)
        - (tri[2].0 - tri[0].0) * (tri[1].1 - tri[0].1);
    if cross < 0.0 {
        tri.swap(1, 2);
    }

    let mut out: Vec<Xy> = subject.to_vec();
    for i in 0..3 {
        let a = tri[i];
        let b = tri[(i + 1) % 3];
        // Positive = left of the directed edge a->b = inside.
        let side = |p: Xy| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let input = std::mem::take(&mut out);
        if input.is_empty() {
            return out;
        }
        for j in 0..input.len() {
            let cur = input[j];
            let prev = input[(j + input.len() - 1) % input.len()];
            let (sc, sp) = (side(cur), side(prev));
            if sc >= 0.0 {
                if sp < 0.0 {
                    if let Some(x) = segment_edge_intersection(prev, cur, a, b) {
                        out.push(x);
                    }
                }
                out.push(cur);
            } else if sp >= 0.0 {
                if let Some(x) = segment_edge_intersection(prev, cur, a, b) {
                    out.push(x);
                }
            }
        }
    }
    out
}

/// Where segment `p→q` crosses the infinite line through `a→b`.
fn segment_edge_intersection(p: Xy, q: Xy, a: Xy, b: Xy) -> Option<Xy> {
    let (rx, ry) = (q.0 - p.0, q.1 - p.1);
    let (sx, sy) = (b.0 - a.0, b.1 - a.1);
    let denom = rx * sy - ry * sx;
    if denom.abs() < 1e-12 {
        return None; // Parallel.
    }
    let t = ((a.0 - p.0) * sy - (a.1 - p.1) * sx) / denom;
    Some((p.0 + t * rx, p.1 + t * ry))
}

/// Every data block in the CTA-861 extension, as `(tag, payload)` where the
/// payload excludes the header byte.
fn cta_blocks(bytes: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    // Byte 126 counts extension blocks; each is 128 bytes after the base.
    let extensions = bytes[126] as usize;
    for i in 0..extensions {
        let start = 128 * (i + 1);
        let Some(block) = bytes.get(start..start + 128) else { break };
        // Tag 0x02 = CTA-861. Revision must be 3+ for the data block
        // collection to exist; byte 2 is where the detailed timings start,
        // and 0 or <4 means there are no data blocks at all.
        if block[0] != 0x02 || block[1] < 3 {
            continue;
        }
        let dtd_start = block[2] as usize;
        if !(4..=128).contains(&dtd_start) {
            continue;
        }
        let mut p = 4usize;
        while p < dtd_start {
            let header = block[p];
            let len = (header & 0x1F) as usize;
            let tag = header >> 5;
            let Some(payload) = block.get(p + 1..p + 1 + len) else { break };
            out.push((tag, payload));
            p += 1 + len;
            if len == 0 {
                break; // Malformed: a zero-length block would loop forever.
            }
        }
    }
    out
}

/// CTA tag 7 = "use extended tag", where the colour blocks live. Tag 3 is the
/// vendor-specific block that carries Dolby Vision and HDR10+.
fn apply_cta_block(block: (u8, &[u8]), colorimetry: &mut Colorimetry, hdr: &mut Hdr) {
    let (tag, payload) = block;
    match tag {
        3 => {
            // Vendor-specific: first three bytes are the OUI, little-endian.
            if payload.len() >= 3 {
                let oui = u32::from(payload[0])
                    | (u32::from(payload[1]) << 8)
                    | (u32::from(payload[2]) << 16);
                match oui {
                    // Dolby Laboratories, then Samsung.
                    0x00_D046 => hdr.dolby_vision = true,
                    0x90_848B => hdr.hdr10_plus = true,
                    _ => {}
                }
            }
        }
        7 => {
            let Some((&ext, rest)) = payload.split_first() else { return };
            match ext {
                // Colorimetry data block: one flags byte, then MD flags.
                0x05 => {
                    if let Some(&f) = rest.first() {
                        colorimetry.xv_ycc601 = f & 0x01 != 0;
                        colorimetry.xv_ycc709 = f & 0x02 != 0;
                        colorimetry.s_ycc601 = f & 0x04 != 0;
                        colorimetry.adobe_ycc = f & 0x08 != 0;
                        colorimetry.adobe_rgb = f & 0x10 != 0;
                        colorimetry.bt2020_cycc = f & 0x20 != 0;
                        colorimetry.bt2020_ycc = f & 0x40 != 0;
                        colorimetry.bt2020_rgb = f & 0x80 != 0;
                    }
                }
                // HDR static metadata: EOTF flags, descriptor flags, then up
                // to three optional luminance codes.
                0x06 => {
                    if let Some(&eotf) = rest.first() {
                        hdr.hdr_gamma = eotf & 0x02 != 0;
                        hdr.hdr10 = eotf & 0x04 != 0;
                        hdr.hlg = eotf & 0x08 != 0;
                    }
                    hdr.max_nits = rest.get(2).map(|&c| luminance(c));
                    hdr.max_frame_avg_nits = rest.get(3).map(|&c| luminance(c));
                    // Minimum is expressed relative to the maximum.
                    if let (Some(&min), Some(max)) = (rest.get(4), hdr.max_nits) {
                        let f = min as f32 / 255.0;
                        hdr.min_nits = Some(max * f * f / 100.0);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// CTA-861 luminance coding: `50 · 2^(code/32)` cd/m².
fn luminance(code: u8) -> f32 {
    50.0 * (code as f32 / 32.0).exp2()
}

/// Classify the panel's colour capability from what the EDID reports, with
/// the reason, so the UI can show its working rather than an oracle's verdict.
///
/// Returns [`PanelGuess::Unknown`] rather than inventing an answer when the
/// panel reports no chromaticity: this is shown to the user as a starting
/// point, and a confident wrong guess is worse than none.
pub fn guess_panel(info: &ColorInfo) -> (PanelGuess, &'static str) {
    let Some(p3) = info.coverage.map(|c| c.dci_p3) else {
        return (PanelGuess::Unknown, "the panel reports no chromaticity");
    };
    match (p3 >= 0.85, info.hdr.any()) {
        (true, true) => {
            (PanelGuess::HdrWideGamut, "P3-class primaries and an HDR transfer function")
        }
        (true, false) => {
            (PanelGuess::WideGamut, "primaries cover most of DCI-P3, but no HDR is signalled")
        }
        (false, _) => (PanelGuess::StandardGamut, "primaries are close to sRGB"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 384-byte EDID from this machine's LG UltraGear: base block,
    /// a CTA-861 rev 3 extension, and a DisplayID extension.
    const LG_FULL: &[u8] = include_bytes!("../../tests/fixtures/edid-gsm5c7c-full.bin");
    /// The same panel's base block only, as M1 captured it.
    const LG_BASE: &[u8] = include_bytes!("../../tests/fixtures/edid-gsm5c7c.bin");

    #[test]
    fn reads_the_panels_own_primaries() {
        let c = parse(LG_FULL).unwrap().chromaticity.unwrap();
        // Decoded by hand from bytes 25..=34 of the fixture.
        let close = |a: f32, b: f32| (a - b).abs() < 0.002;
        assert!(close(c.red.0, 0.677) && close(c.red.1, 0.321), "red {:?}", c.red);
        assert!(close(c.green.0, 0.249) && close(c.green.1, 0.685), "green {:?}", c.green);
        assert!(close(c.blue.0, 0.146) && close(c.blue.1, 0.057), "blue {:?}", c.blue);
        // White point is D65 (0.3127, 0.3290) to within the 10-bit grid.
        assert!(close(c.white.0, 0.3135) && close(c.white.1, 0.3291), "white {:?}", c.white);
    }

    #[test]
    fn a_wide_gamut_panel_reads_as_wide_gamut() {
        let cov = parse(LG_FULL).unwrap().coverage.unwrap();
        assert!(cov.srgb > 0.99, "encloses essentially all of sRGB: {cov:?}");
        assert!(cov.dci_p3 > 0.95, "near-complete P3: {cov:?}");
        assert!(cov.bt2020 < 0.85, "but nowhere near BT.2020: {cov:?}");
    }

    /// Coverage must mean containment, not area. A triangle with the same
    /// area as DCI-P3 but rotated away from it covers far less of it, and a
    /// ratio-based metric would wrongly call that 100 %.
    #[test]
    fn coverage_is_containment_not_area() {
        // DCI-P3 translated bodily up and to the right: identical area, so an
        // area ratio would score it a perfect 100 %, but it misses a large
        // part of the real gamut.
        let shift = |p: Xy| (p.0 + 0.09, p.1 + 0.06);
        let shifted = Chromaticity {
            red: shift(DCI_P3[0]),
            green: shift(DCI_P3[1]),
            blue: shift(DCI_P3[2]),
            white: (0.3127, 0.3290),
        };
        let area_ratio =
            polygon_area(&[shifted.red, shifted.green, shifted.blue]) / polygon_area(&DCI_P3);
        assert!((area_ratio - 1.0).abs() < 1e-4, "same area by construction: {area_ratio}");

        let covered = coverage_of(&shifted, &DCI_P3);
        assert!(covered < 0.75, "containment must expose the miss, got {covered}");
        assert!(covered > 0.0, "but they do overlap");
    }

    #[test]
    fn a_triangle_covers_itself_completely() {
        let p3 = Chromaticity {
            red: DCI_P3[0],
            green: DCI_P3[1],
            blue: DCI_P3[2],
            white: (0.3127, 0.3290),
        };
        assert!((coverage_of(&p3, &DCI_P3) - 1.0).abs() < 1e-3);
        // And a strictly larger gamut still only scores 100 %, never more.
        let wide = Chromaticity {
            red: BT2020[0],
            green: BT2020[1],
            blue: BT2020[2],
            white: (0.3127, 0.3290),
        };
        assert!((coverage_of(&wide, &DCI_P3) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn disjoint_gamuts_cover_nothing() {
        let tiny = Chromaticity {
            red: (0.9, 0.05),
            green: (0.92, 0.06),
            blue: (0.91, 0.04),
            white: (0.3127, 0.3290),
        };
        assert_eq!(coverage_of(&tiny, &DCI_P3), 0.0);
    }

    #[test]
    fn reads_gamma_bit_depth_and_input_type() {
        let i = parse(LG_FULL).unwrap();
        assert!(i.digital, "DisplayPort panel");
        // Byte 20 is 0xC5: bit-depth code 4, which is 12 bits, not the 10 a
        // DisplayPort panel is usually assumed to carry.
        assert_eq!(i.bit_depth, Some(12));
        assert_eq!(i.gamma, Some(2.2));
    }

    #[test]
    fn reads_hdr_support_from_the_cta_extension() {
        let hdr = parse(LG_FULL).unwrap().hdr;
        assert!(hdr.hdr10, "the EOTF byte sets the ST 2084 bit");
        assert!(!hdr.hlg);
        // 50 * 2^(115/32) ≈ 603 cd/m².
        let max = hdr.max_nits.expect("peak luminance");
        assert!((max - 603.0).abs() < 5.0, "peak {max}");
        assert!(hdr.max_frame_avg_nits.unwrap() < max);
        assert!(hdr.min_nits.unwrap() < 0.001);
    }

    #[test]
    fn reads_advertised_colorimetry_standards() {
        let c = parse(LG_FULL).unwrap().colorimetry;
        assert!(c.bt2020_rgb && c.bt2020_ycc, "panel advertises BT.2020: {c:?}");
    }

    #[test]
    fn a_base_only_edid_still_yields_colour_without_hdr() {
        // Windows stores base-only EDID for some entries; colour must still
        // work, just without anything the extension would have carried.
        let i = parse(LG_BASE).unwrap();
        assert!(i.chromaticity.is_some(), "primaries live in the base block");
        assert_eq!(i.gamma, Some(2.2));
        assert!(!i.hdr.any(), "no extension, so no HDR claims");
        assert_eq!(i.colorimetry, Colorimetry::default());
    }

    #[test]
    fn rejects_input_too_short_to_be_an_edid() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 64]).is_none());
    }

    #[test]
    fn a_truncated_extension_count_does_not_panic() {
        // Claim two extensions but supply none: the walker must not index
        // past the end. Windows really does store blobs like this.
        let mut b = LG_FULL[..128].to_vec();
        b[126] = 2;
        let i = parse(&b).expect("base block still parses");
        assert!(!i.hdr.any());
    }

    #[test]
    fn this_panel_classes_as_wide_gamut_hdr() {
        let i = parse(LG_FULL).unwrap();
        let (guess, why) = guess_panel(&i);
        assert_eq!(guess, PanelGuess::HdrWideGamut, "{why}");
    }

    #[test]
    fn a_wide_gamut_panel_without_hdr_is_not_called_hdr() {
        let mut i = parse(LG_FULL).unwrap();
        i.hdr = Hdr::default();
        assert_eq!(guess_panel(&i).0, PanelGuess::WideGamut);
    }

    #[test]
    fn srgb_primaries_class_as_standard_gamut() {
        let mut i = parse(LG_FULL).unwrap();
        i.coverage = Some(Coverage { srgb: 1.0, dci_p3: 0.72, bt2020: 0.5 });
        assert_eq!(guess_panel(&i).0, PanelGuess::StandardGamut);
    }

    /// The trap this module exists to avoid. A tiny minimum-luminance figure
    /// looks like an emissive black level but is what this very LCD reports,
    /// so nothing may conclude "OLED" from it.
    #[test]
    fn a_near_zero_black_level_does_not_imply_oled() {
        let i = parse(LG_FULL).unwrap();
        assert!(i.hdr.min_nits.unwrap() < 0.001, "the LCD really does report ~9e-5 cd/m²");
        assert_eq!(
            guess_panel(&i).0,
            PanelGuess::HdrWideGamut,
            "classed by gamut, not by black level"
        );
    }

    #[test]
    fn no_chromaticity_means_no_guess() {
        let i = ColorInfo {
            chromaticity: None,
            gamma: None,
            bit_depth: None,
            digital: true,
            colorimetry: Colorimetry::default(),
            hdr: Hdr::default(),
            coverage: None,
        };
        assert_eq!(guess_panel(&i).0, PanelGuess::Unknown);
    }
}
