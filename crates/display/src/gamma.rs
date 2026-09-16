//! Gamma-ramp maths and the GDI per-monitor ramp read/write path.
//!
//! The ramp is the vendor-neutral way to shape gamma, contrast and shadow
//! lift: 256 × RGB 16-bit lookup entries fed to `SetDeviceGammaRamp` on the
//! monitor's own DC (`CreateDC("\\.\DISPLAY1")`), so only that monitor
//! changes. The *original* ramp is captured raw and restored raw — if the
//! user runs f.lux / Night Light, whatever curve they had comes back exactly.
//!
//! Windows validates ramps (a roughly monotonic curve not too far from
//! identity, unless `GdiIcmGammaRange` widens it); the parameter ranges below
//! are chosen to stay inside the default envelope.

use serde::{Deserialize, Serialize};

/// One channel is 256 entries; a ramp is R, G, B.
pub const RAMP_SIZE: usize = 256;

/// A raw ramp as GDI sees it. Boxed in snapshots (1536 u16s).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ramp {
    pub r: Vec<u16>,
    pub g: Vec<u16>,
    pub b: Vec<u16>,
}

impl Ramp {
    pub fn identity() -> Self {
        let ch: Vec<u16> = (0..RAMP_SIZE).map(|i| (i as u32 * 65535 / 255) as u16).collect();
        Self { r: ch.clone(), g: ch.clone(), b: ch }
    }

    pub fn is_well_formed(&self) -> bool {
        self.r.len() == RAMP_SIZE && self.g.len() == RAMP_SIZE && self.b.len() == RAMP_SIZE
    }
}

/// Ramp-shaping parameters in profile units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RampParams {
    /// Display gamma multiplier; 1.0 = unchanged. Clamped to 0.5..=2.0.
    pub gamma: f32,
    /// -100..=100, 0 = unchanged. Scales the curve around mid-grey; clamped to
    /// ±50 % effect so the ramp stays inside the Windows validation envelope.
    pub contrast: i32,
    /// 0..=100, 0 = unchanged. Raises near-black up to +25 % with a cubic
    /// falloff, leaving highlights alone.
    pub shadow_lift: i32,
}

impl RampParams {
    pub fn neutral() -> Self {
        Self { gamma: 1.0, contrast: 0, shadow_lift: 0 }
    }

    pub fn is_neutral(&self) -> bool {
        (self.gamma - 1.0).abs() < 1e-3 && self.contrast == 0 && self.shadow_lift == 0
    }
}

/// Build the ramp for the given parameters. Pure; unit-tested.
pub fn build_ramp(p: &RampParams) -> Ramp {
    let gamma = p.gamma.clamp(0.5, 2.0);
    let contrast = 1.0 + (p.contrast.clamp(-100, 100) as f32 / 100.0) * 0.5;
    let lift = p.shadow_lift.clamp(0, 100) as f32 / 100.0 * 0.25;

    let ch: Vec<u16> = (0..RAMP_SIZE)
        .map(|i| {
            let x = i as f32 / (RAMP_SIZE - 1) as f32;
            let mut y = x.powf(1.0 / gamma);
            y = 0.5 + (y - 0.5) * contrast;
            let y_c = y.clamp(0.0, 1.0);
            let y_lifted = y_c + lift * (1.0 - y_c).powi(3);
            (y_lifted.clamp(0.0, 1.0) * 65535.0).round() as u16
        })
        .collect();
    Ramp { r: ch.clone(), g: ch.clone(), b: ch }
}

/// Read / write the ramp of one monitor's GDI device (`\\.\DISPLAY1`).
#[cfg(windows)]
pub mod io {
    use super::{Ramp, RAMP_SIZE};
    use anyhow::{bail, Context, Result};
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{CreateDCW, DeleteDC, HDC};
    use windows::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

    struct MonitorDc(HDC);

    impl MonitorDc {
        fn open(gdi_name: &str) -> Result<Self> {
            let wide: Vec<u16> = gdi_name.encode_utf16().chain(std::iter::once(0)).collect();
            // The device name goes in the *driver* argument for a display DC
            // (`CreateDC("\\.\DISPLAY1", NULL, …)`) — the (name, name) form
            // yields a DC that rejects gamma calls.
            // SAFETY: NUL-terminated wide string; the DC is deleted in Drop.
            let dc = unsafe { CreateDCW(PCWSTR(wide.as_ptr()), None, None, None) };
            if dc.is_invalid() {
                bail!("CreateDC failed for {gdi_name}");
            }
            Ok(Self(dc))
        }
    }

    impl Drop for MonitorDc {
        fn drop(&mut self) {
            // SAFETY: DC created by us above.
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }

    pub fn get_ramp(gdi_name: &str) -> Result<Ramp> {
        let dc = MonitorDc::open(gdi_name)?;
        let mut buf = [[0u16; RAMP_SIZE]; 3];
        // SAFETY: buf is exactly the WORD[3][256] the API contract requires.
        let ok = unsafe { GetDeviceGammaRamp(dc.0, buf.as_mut_ptr() as *mut _) };
        if !ok.as_bool() {
            bail!("GetDeviceGammaRamp failed for {gdi_name}");
        }
        Ok(Ramp { r: buf[0].to_vec(), g: buf[1].to_vec(), b: buf[2].to_vec() })
    }

    pub fn set_ramp(gdi_name: &str, ramp: &Ramp) -> Result<()> {
        if !ramp.is_well_formed() {
            bail!("malformed ramp");
        }
        let dc = MonitorDc::open(gdi_name)?;
        let mut buf = [[0u16; RAMP_SIZE]; 3];
        buf[0].copy_from_slice(&ramp.r);
        buf[1].copy_from_slice(&ramp.g);
        buf[2].copy_from_slice(&ramp.b);
        // SAFETY: as above.
        let ok = unsafe { SetDeviceGammaRamp(dc.0, buf.as_ptr() as *const _) };
        if !ok.as_bool() {
            bail!("SetDeviceGammaRamp rejected the ramp for {gdi_name}");
        }
        Ok(())
    }

    /// Restore convenience used by crash recovery when no raw ramp was saved.
    pub fn set_identity(gdi_name: &str) -> Result<()> {
        set_ramp(gdi_name, &Ramp::identity()).context("setting identity ramp")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_params_build_the_identity_ramp() {
        let ramp = build_ramp(&RampParams::neutral());
        assert_eq!(ramp, Ramp::identity());
        assert!(RampParams::neutral().is_neutral());
    }

    #[test]
    fn ramps_are_monotonic_and_full_range_for_all_corner_params() {
        for gamma in [0.5, 0.8, 1.0, 1.4, 2.0, 99.0, -1.0] {
            for contrast in [-100, -50, 0, 50, 100, 9999] {
                for lift in [0, 25, 100, 9999] {
                    let p = RampParams { gamma, contrast, shadow_lift: lift };
                    let ramp = build_ramp(&p);
                    assert!(ramp.is_well_formed());
                    for w in ramp.r.windows(2) {
                        assert!(w[1] >= w[0], "non-monotonic at {p:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn shadow_lift_raises_blacks_and_leaves_whites() {
        let lifted = build_ramp(&RampParams { gamma: 1.0, contrast: 0, shadow_lift: 100 });
        let id = Ramp::identity();
        assert!(lifted.r[0] > id.r[0] + 10_000, "black point must rise");
        assert_eq!(lifted.r[255], id.r[255], "white point must not move");
        assert!(lifted.r[250] - id.r[250] < 100, "highlights nearly untouched");
    }

    #[test]
    fn contrast_steepens_around_mid_grey() {
        let hi = build_ramp(&RampParams { gamma: 1.0, contrast: 100, shadow_lift: 0 });
        let id = Ramp::identity();
        assert!(hi.r[64] < id.r[64], "shadows crushed");
        assert!(hi.r[192] > id.r[192], "highlights pushed");
        // Mid-grey is the pivot.
        let mid = hi.r[128] as i32 - id.r[128] as i32;
        assert!(mid.abs() < 300, "pivot moved by {mid}");
    }

    #[test]
    fn gamma_above_one_brightens_midtones() {
        let bright = build_ramp(&RampParams { gamma: 1.4, contrast: 0, shadow_lift: 0 });
        assert!(bright.r[128] > Ramp::identity().r[128]);
    }

    /// The UI draws its Display A/B through a TypeScript port of `build_ramp`
    /// (`ui/src/lib/honest.ts`). Its test asserts these same samples, so a
    /// change to the curve here fails there too instead of quietly making the
    /// preview wrong.
    #[test]
    fn ramp_golden_samples_shared_with_the_ui() {
        let ramp = build_ramp(&RampParams { gamma: 1.2, contrast: 30, shadow_lift: 40 });
        let got: Vec<u16> = [0, 32, 64, 128, 192, 255].iter().map(|&i| ramp.r[i]).collect();
        let want = [6554u16, 12782, 21262, 38032, 54609, 65535];
        for (g, w) in got.iter().zip(want) {
            assert!((*g as i32 - w as i32).abs() <= 1, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn ramp_serde_round_trip() {
        let ramp = build_ramp(&RampParams { gamma: 1.2, contrast: 10, shadow_lift: 5 });
        let json = serde_json::to_string(&ramp).unwrap();
        let back: Ramp = serde_json::from_str(&json).unwrap();
        assert_eq!(ramp, back);
    }
}
