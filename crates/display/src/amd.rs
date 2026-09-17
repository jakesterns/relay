//! Per-display colour on AMD: saturation and hue via the AMD Display Library.
//!
//! This is the AMD half of the `DisplayIo` GPU-colour seam; [`nvapi`] is the
//! other. Same contract: loaded dynamically per operation, nothing resident
//! between applies, and a machine without the driver simply returns `None`
//! from [`Adl::load`] so the caller can fall back to the (vendor-neutral)
//! gamma-ramp path.
//!
//! [`nvapi`]: crate::nvapi
//!
//! # Why ADL and not the ADLX vtables
//!
//! AMD ships two user-mode interfaces to the same driver feature — the
//! "Custom Color" block in Radeon Software:
//!
//! - **ADLX** (`amdadlx64.dll`), the modern one, is a COM-like C++ interface.
//!   Its C binding is vtable-based: you reach `IADLXDisplayServices` ⇒
//!   `IADLXDisplay` ⇒ custom colour by indexing function-pointer tables whose
//!   layout is fixed by the SDK headers of the version you compiled against.
//!   A wrong or shifted slot does not fail — it calls *a different method*.
//! - **ADL** (`atiadlxx.dll`), the long-standing one, is a flat C API whose
//!   entry points are resolved by name with `GetProcAddress`.
//!
//! We have no AMD-driven display on the development machine (see
//! `docs/plans/M2-display.md`), so a mis-ordered vtable slot could not be
//! caught by running it. With the flat API, a name that is missing or
//! renamed fails loudly at load time and we report "unavailable" instead of
//! writing something nobody asked for. That is the same rule the VCP quirks
//! table follows: never issue a command you cannot prove the meaning of.
//!
//! ADL is still shipped by current drivers (`atiadlxx.dll` 7.25.10.1590 on
//! this machine, alongside ADLX 1.4.0.121) and `ADL2_Display_Color_Set` is
//! the exact call Radeon Software's own slider makes. If ADLX becomes the
//! only transport, it replaces this module's *inside* — the seam above it
//! (`DisplayIo::gpu_*`) does not move.
//!
//! x86-64 only, which is all Relay targets: with one calling convention on
//! that ABI the `extern "C"` declarations below cannot be mismatched.

#[cfg(windows)]
mod ffi;
#[cfg(windows)]
pub use ffi::{Adl, AdlDisplay};

/// A driver-reported control range. ADL exposes `default` separately from
/// `min`, and neutral is `default` — unlike NvAPI's DVC, where neutral *is*
/// the minimum. That difference is the whole reason the mapping below is not
/// the NVIDIA one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdlRange {
    pub current: i32,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    pub step: i32,
}

// ---------------------------------------------------------------------------
// Pure unit mapping: profile units ⇄ ADL raw units.
//
// See the module docs in `nvapi` for the NVIDIA side. The two are *not* the
// same curve, because the two drivers do not expose the same control:
//
//   NvAPI DVC        range [min..max] (0..63), neutral == min
//   ADL saturation   range [min..max] (0..200), neutral == driver default (100)
//
// So NVIDIA can only add vibrance — the bottom half of the profile slider
// (0..50, "less colour than normal") has nowhere to go and clamps to
// neutral. AMD *can* desaturate, and the mapping below honours it: pinning
// the AMD half to neutral to match NVIDIA would throw away a control the
// hardware has, and the point of the vendor seam is that each GPU does the
// best it can with the same profile, not that both do the worst.
// ---------------------------------------------------------------------------

/// Round to the nearest representable value: inside `[min, max]` and on the
/// driver's `step` grid measured from `min`. ADL rejects off-grid values on
/// some controls, and silently snaps on others — neither is a good surprise.
fn snap(value: i32, r: &AdlRange) -> i32 {
    let step = r.step.max(1);
    let v = value.clamp(r.min, r.max);
    let snapped = r.min + ((v - r.min) as f64 / step as f64).round() as i32 * step;
    snapped.clamp(r.min, r.max)
}

/// Profile vibrance (0..=100, 50 = neutral) → raw ADL saturation.
///
/// Piecewise-linear with the knee at the driver's own default:
///
/// ```text
///   0 ──────────── 50 ──────────── 100      profile
///  min ─────── default ─────────── max      ADL
/// ```
///
/// Two straight segments rather than one line across `[min, max]`, because
/// the default is not necessarily the midpoint of the range, and 50 must mean
/// "leave the colour alone" exactly — a profile at 50 has to be a no-op or
/// every game would shift the desktop's colour a little.
pub fn vibrance_percent_to_saturation(percent: i32, r: &AdlRange) -> i32 {
    let p = percent.clamp(0, 100);
    if r.max <= r.min {
        return r.default;
    }
    let raw = if p >= 50 {
        r.default as f64 + (p - 50) as f64 / 50.0 * (r.max - r.default) as f64
    } else {
        r.default as f64 - (50 - p) as f64 / 50.0 * (r.default - r.min) as f64
    };
    snap(raw.round() as i32, r)
}

/// Inverse of [`vibrance_percent_to_saturation`], for reporting.
pub fn saturation_to_vibrance_percent(raw: i32, r: &AdlRange) -> i32 {
    if r.max <= r.min {
        return 50;
    }
    let raw = raw.clamp(r.min, r.max);
    let pct = if raw >= r.default {
        let span = (r.max - r.default) as f64;
        if span <= 0.0 {
            50.0
        } else {
            50.0 + (raw - r.default) as f64 / span * 50.0
        }
    } else {
        let span = (r.default - r.min) as f64;
        if span <= 0.0 {
            50.0
        } else {
            50.0 - (r.default - raw) as f64 / span * 50.0
        }
    };
    (pct.round() as i32).clamp(0, 100)
}

/// Profile hue (degrees, 0 = neutral, wraps) → raw ADL hue, plus whether the
/// driver's range forced a clamp.
///
/// Degrees stay degrees. NvAPI takes a full 0..359 rotation; ADL's hue is a
/// narrow signed trim (commonly ±30°). The profile angle is first folded into
/// `(-180, 180]` — +350° and −10° are the same rotation — and then *clamped*,
/// not rescaled. Rescaling would make "10°" mean one thing on NVIDIA and
/// another on AMD for the same profile; clamping keeps the unit honest and
/// tells the caller the request was trimmed, which surfaces in the UI's
/// "applied via" line instead of silently under-delivering.
pub fn hue_deg_to_adl(hue_deg: i32, r: &AdlRange) -> (i32, bool) {
    let wrapped = ((hue_deg % 360) + 540) % 360 - 180; // (-180, 180]
    let wrapped = if wrapped == -180 { 180 } else { wrapped };
    if r.max <= r.min {
        return (r.default, wrapped != 0);
    }
    let snapped = snap(wrapped, r);
    (snapped, snapped != wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The range Radeon Software reports for saturation on current drivers.
    fn saturation() -> AdlRange {
        AdlRange { current: 100, default: 100, min: 0, max: 200, step: 1 }
    }

    /// ADL hue is a narrow signed trim.
    fn hue() -> AdlRange {
        AdlRange { current: 0, default: 0, min: -30, max: 30, step: 1 }
    }

    #[test]
    fn neutral_vibrance_is_exactly_the_driver_default() {
        assert_eq!(vibrance_percent_to_saturation(50, &saturation()), 100);
        // And with an off-centre default, still exactly the default.
        let odd = AdlRange { current: 80, default: 80, min: 0, max: 200, step: 1 };
        assert_eq!(vibrance_percent_to_saturation(50, &odd), 80);
    }

    #[test]
    fn vibrance_is_piecewise_linear_through_the_default() {
        let r = saturation();
        assert_eq!(vibrance_percent_to_saturation(100, &r), 200);
        assert_eq!(vibrance_percent_to_saturation(75, &r), 150);
        assert_eq!(vibrance_percent_to_saturation(0, &r), 0);
        assert_eq!(vibrance_percent_to_saturation(25, &r), 50);
        // Off-centre default: each side scales to its own span.
        let odd = AdlRange { current: 80, default: 80, min: 0, max: 200, step: 1 };
        assert_eq!(vibrance_percent_to_saturation(75, &odd), 140, "half of 80..200");
        assert_eq!(vibrance_percent_to_saturation(25, &odd), 40, "half of 0..80");
    }

    #[test]
    fn amd_honours_desaturation_where_nvidia_clamps() {
        // The documented divergence, pinned so it cannot drift unnoticed.
        let r = saturation();
        assert_eq!(vibrance_percent_to_saturation(20, &r), 40, "AMD goes below neutral");
        assert_eq!(
            crate::nvapi::vibrance_percent_to_dvc(20, 0, 63),
            0,
            "NVIDIA has no sub-neutral DVC and stays neutral"
        );
    }

    #[test]
    fn vibrance_round_trips_within_one_percent() {
        let r = saturation();
        for p in [0, 10, 25, 40, 50, 60, 75, 90, 100] {
            let raw = vibrance_percent_to_saturation(p, &r);
            assert!(
                (saturation_to_vibrance_percent(raw, &r) - p).abs() <= 1,
                "p={p} raw={raw} back={}",
                saturation_to_vibrance_percent(raw, &r)
            );
        }
    }

    #[test]
    fn coarse_steps_snap_onto_the_driver_grid() {
        let r = AdlRange { current: 100, default: 100, min: 0, max: 200, step: 10 };
        assert_eq!(vibrance_percent_to_saturation(51, &r), 100, "nearest grid point");
        assert_eq!(vibrance_percent_to_saturation(52, &r), 100, "104 rounds down onto 100");
        assert_eq!(vibrance_percent_to_saturation(57, &r), 110);
        assert_eq!(vibrance_percent_to_saturation(100, &r), 200);
        // Never off the ends, whatever the grid.
        let ragged = AdlRange { current: 0, default: 0, min: -30, max: 30, step: 7 };
        for p in 0..=100 {
            let v = vibrance_percent_to_saturation(p, &ragged);
            assert!((-30..=30).contains(&v), "p={p} -> {v}");
        }
    }

    #[test]
    fn degenerate_ranges_report_neutral() {
        let dead = AdlRange { current: 5, default: 5, min: 5, max: 5, step: 1 };
        assert_eq!(vibrance_percent_to_saturation(90, &dead), 5);
        assert_eq!(saturation_to_vibrance_percent(5, &dead), 50);
        assert_eq!(hue_deg_to_adl(20, &dead), (5, true));
        assert_eq!(hue_deg_to_adl(0, &dead), (5, false));
    }

    #[test]
    fn hue_wraps_to_a_signed_trim_then_clamps() {
        let r = hue();
        assert_eq!(hue_deg_to_adl(0, &r), (0, false));
        assert_eq!(hue_deg_to_adl(15, &r), (15, false));
        assert_eq!(hue_deg_to_adl(350, &r), (-10, false), "350 deg == -10 deg");
        assert_eq!(hue_deg_to_adl(-10, &r), (-10, false));
        assert_eq!(hue_deg_to_adl(90, &r), (30, true), "clamped, and says so");
        assert_eq!(hue_deg_to_adl(270, &r), (-30, true));
        assert_eq!(hue_deg_to_adl(360, &r), (0, false), "a full turn is neutral");
    }

    #[test]
    fn hue_stays_in_range_for_every_angle() {
        let r = hue();
        for deg in -720..=720 {
            let (v, _) = hue_deg_to_adl(deg, &r);
            assert!((r.min..=r.max).contains(&v), "deg={deg} -> {v}");
        }
    }
}
