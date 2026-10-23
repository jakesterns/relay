//! HDR look learning (owner 2026-10-23: "users who wish to play with HDR on
//! will still have some sort of Relay tuning").
//!
//! In HDR, Windows hands out scRGB frames (linear light, BT.709 primaries,
//! 1.0 = 80 nits) and ignores the gamma ramp, so the SDR path can neither
//! read the picture nor apply most of what it learns. This module is the
//! HDR half, kept pure like the rest of `learn`:
//!
//! - [`to_display_codes`] turns a small scRGB frame into the same packed
//!   8-bit frame the [`super::Analyser`] already reads: each channel relative
//!   to the panel's peak, through a 2.2 power curve. Saturation measured as
//!   (max − min) / max on power-law values does not depend on the scale, so
//!   the SDR saturation rules mean the same thing here. Clipping means "at
//!   or past what this panel can show".
//! - [`crush_threshold`] is the shadow line for that encoding: a fixed light
//!   level ([`HDR_CRUSH_NITS`]), not a code value, because 5 % of a
//!   1000-nit curve would be most of a dark room.
//! - [`realize_hdr`] keeps only what still works in HDR — vendor vibrance —
//!   and turns shadow and highlight findings into advice for the game's own
//!   HDR settings, which Relay never touches.
//!
//! SDR and HDR looks are learned and kept apart (the core stores HDR under
//! its own key), so switching HDR on or off never mixes the two.

use super::derive::{Adjustments, LookTargets, PanelCaps, PanelKind};

/// scRGB reference: 1.0 is 80 nits.
pub const SCRGB_NITS: f32 = 80.0;
/// Shadows below this many nits count as crushed in HDR.
pub const HDR_CRUSH_NITS: f32 = 0.1;
/// The power curve the display codes are encoded with.
pub const HDR_CODE_GAMMA: f32 = 2.2;
/// Peak used when the panel reports none (EDID without HDR metadata).
pub const DEFAULT_PEAK_NITS: f32 = 1000.0;
/// A reported peak below this is not an HDR panel's; use the default.
pub const MIN_PEAK_NITS: f32 = 300.0;
/// Vibrance points above neutral for full saturation help in HDR. Smaller
/// than SDR: HDR games are graded wide already, and the panel is in its
/// wide-gamut mode.
pub const HDR_MAX_VIBRANCE_BOOST: i32 = 6;

/// Advice shown when an HDR game crushes its shadows.
pub const ADVICE_SHADOWS: &str = "Shadows are crushed. Raise the game's HDR black level or \
shadow brightness one step: in HDR, Relay cannot lift shadows without greying the blacks.";
/// Advice shown when an HDR game sends highlights past the panel's peak.
pub const ADVICE_HIGHLIGHTS: &str = "Highlights go past what your monitor can show. Lower the \
game's HDR peak brightness until the brightest sky just keeps its detail.";
/// Always present on an HDR look, so the UI can say what HDR tuning is.
pub const NOTE_HDR_ONLY: &str = "HDR look: colour vibrance only; brightness and shadows are \
left to the game's HDR settings";

/// The panel's peak in nits, from `DXGI_OUTPUT_DESC1::MaxLuminance`.
pub fn peak_nits(max_luminance: f32) -> f32 {
    if max_luminance.is_finite() && max_luminance >= MIN_PEAK_NITS {
        max_luminance
    } else {
        DEFAULT_PEAK_NITS
    }
}

/// The analyser's crush line, in display codes (0..1), for this peak.
pub fn crush_threshold(peak: f32) -> f32 {
    (HDR_CRUSH_NITS / peak).powf(1.0 / HDR_CODE_GAMMA)
}

/// IEEE 754 half → f32 (the duplication format is R16G16B16A16_FLOAT).
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1f;
    let frac = (h & 0x3ff) as f32;
    match exp {
        0 => sign * frac * (1.0 / 16_777_216.0), // subnormal: frac × 2^-24
        0x1f if frac == 0.0 => sign * f32::INFINITY,
        0x1f => f32::NAN,
        e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e as i32 - 15),
    }
}

/// One scRGB channel → an 8-bit display code relative to `peak` nits.
/// Negative values (colours outside BT.709) clamp to 0; light past the
/// panel's peak clamps to 255, which the analyser counts as clipped.
pub fn code(c: f32, peak: f32) -> u8 {
    if !c.is_finite() || c <= 0.0 {
        return 0;
    }
    let rel = (c * SCRGB_NITS / peak).min(1.0);
    (rel.powf(1.0 / HDR_CODE_GAMMA) * 255.0).round() as u8
}

/// A mapped R16G16B16A16_FLOAT frame (`pitch` in bytes) → packed RGB codes
/// for the analyser. The frame is never kept.
pub fn to_display_codes(
    data: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
    peak: f32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        let row = &data[y * pitch..];
        for x in 0..width {
            let px = &row[x * 8..x * 8 + 6];
            for c in 0..3 {
                let h = u16::from_le_bytes([px[c * 2], px[c * 2 + 1]]);
                out.push(code(f16_to_f32(h), peak));
            }
        }
    }
    out
}

/// An HDR look × panel → adjustments: vibrance only, plus advice.
pub fn realize_hdr(look: &LookTargets, caps: &PanelCaps) -> Adjustments {
    let mut a = Adjustments::default();
    let sat = look.saturation.clamp(0.0, 1.0);
    // OLED panels in HDR are wider still; the same boost reads stronger.
    let max = match caps.kind {
        PanelKind::Oled => HDR_MAX_VIBRANCE_BOOST - 2,
        _ => HDR_MAX_VIBRANCE_BOOST,
    };
    a.vibrance = 50 + (sat * max as f32).round() as i32;
    a.notes.push(NOTE_HDR_ONLY.into());
    if look.shadow > 0.0 {
        a.notes.push(ADVICE_SHADOWS.into());
    }
    if look.highlight > 0.0 {
        a.notes.push(ADVICE_HIGHLIGHTS.into());
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn half(v: f32) -> u16 {
        // Test-only encoder for normal, positive values.
        if v == 0.0 {
            return 0;
        }
        let e = v.log2().floor() as i32;
        let frac = ((v / 2f32.powi(e) - 1.0) * 1024.0).round() as u16;
        (((e + 15) as u16) << 10) | frac
    }

    #[test]
    fn halves_decode() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        assert!((f16_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-12);
        assert!((f16_to_f32(half(12.5)) - 12.5).abs() < 0.01);
    }

    #[test]
    fn codes_are_relative_to_the_panel_peak() {
        let peak = 1000.0;
        assert_eq!(code(0.0, peak), 0);
        assert_eq!(code(-0.3, peak), 0, "out-of-gamut negatives clamp");
        assert_eq!(code(f32::NAN, peak), 0);
        // Exactly the peak, and anything past it, is full scale: clipped.
        assert_eq!(code(peak / SCRGB_NITS, peak), 255);
        assert_eq!(code(40.0, peak), 255);
        // Mid grey-ish: 100 nits on a 1000-nit panel.
        let mid = code(100.0 / SCRGB_NITS, peak);
        assert!((90..=105).contains(&mid), "{mid}");
    }

    #[test]
    fn saturation_does_not_depend_on_brightness() {
        // (max − min) / max on power-law codes is scale-free, so the SDR
        // saturation rules carry over.
        let sat = |r: f32, g: f32| {
            let (r, g) = (code(r, 1000.0) as f32, code(g, 1000.0) as f32);
            (r - g) / r
        };
        let dim = sat(0.5, 0.1);
        let bright = sat(5.0, 1.0);
        assert!((dim - bright).abs() < 0.02, "{dim} vs {bright}");
    }

    #[test]
    fn crush_line_is_a_light_level() {
        let t = crush_threshold(1000.0);
        // A pixel at the line encodes to the line.
        let at = code(HDR_CRUSH_NITS / SCRGB_NITS, 1000.0) as f32 / 255.0;
        assert!((at - t).abs() < 0.005);
        // Brighter panel, lower line in codes.
        assert!(crush_threshold(1500.0) < t);
        assert!(t < super::super::analyse::LUMA_CRUSH);
    }

    #[test]
    fn peak_falls_back_when_unknown() {
        assert_eq!(peak_nits(0.0), DEFAULT_PEAK_NITS);
        assert_eq!(peak_nits(250.0), DEFAULT_PEAK_NITS);
        assert_eq!(peak_nits(f32::NAN), DEFAULT_PEAK_NITS);
        assert_eq!(peak_nits(1300.0), 1300.0);
    }

    #[test]
    fn frames_convert_with_pitch() {
        // 2×1 frame, pitch padded to 24 bytes.
        let mut data = vec![0u8; 24];
        for (i, v) in [1.0f32, 0.0, 0.0, 1.0, 50.0, 50.0, 50.0, 1.0].iter().enumerate() {
            data[i * 2..i * 2 + 2].copy_from_slice(&half(*v).to_le_bytes());
        }
        let out = to_display_codes(&data, 2, 1, 24, 1000.0);
        assert_eq!(out.len(), 6);
        assert!(out[0] > 0 && out[1] == 0 && out[2] == 0);
        assert_eq!(&out[3..], &[255, 255, 255]);
    }

    #[test]
    fn hdr_realisation_is_vibrance_and_advice_only() {
        let caps = PanelCaps { kind: PanelKind::Oled, black_equalizer_max: None };
        let full = LookTargets { shadow: 1.0, saturation: 1.0, highlight: 1.0 };
        let a = realize_hdr(&full, &caps);
        assert_eq!((a.gamma, a.shadow_lift, a.black_equalizer), (1.0, 0, None));
        assert_eq!(a.vibrance, 50 + HDR_MAX_VIBRANCE_BOOST - 2);
        assert_eq!(a.ddc_writes(), 0);
        assert!(a.notes.iter().any(|n| n == ADVICE_SHADOWS));
        assert!(a.notes.iter().any(|n| n == ADVICE_HIGHLIGHTS));

        let lcd = PanelCaps { kind: PanelKind::Ips, black_equalizer_max: Some(20) };
        let none = realize_hdr(&LookTargets::default(), &lcd);
        assert_eq!(none.vibrance, 50);
        assert_eq!(none.notes, vec![NOTE_HDR_ONLY.to_string()]);
        // Even a verified black equalizer is not used in HDR.
        assert_eq!(realize_hdr(&full, &lcd).black_equalizer, None);
    }
}
