//! Digital vibrance and hue via NvAPI.
//!
//! `nvapi64.dll` is loaded dynamically per operation and unloaded again —
//! no link-time dependency, nothing resident between applies (the always-on
//! core has a 10 MB budget), and a non-NVIDIA machine simply returns `None`
//! from [`NvApi::load`], falling back to the gamma-ramp path.
//!
//! The display-handle entry points used here are the long-stable ones every
//! vibrance tool uses (`EnumNvidiaDisplayHandle` / `GetDVCInfo` /
//! `SetDVCLevel` / hue). Function ids are the community-documented
//! `nvapi_QueryInterface` constants; struct versions follow the NvAPI
//! `MAKE_NVAPI_VERSION(struct, 1)` convention.

#[cfg(windows)]
mod ffi;
#[cfg(windows)]
pub use ffi::{Dvc, NvApi, NvDisplay};

// ---------------------------------------------------------------------------
// Pure unit mapping: profile vibrance percent ⇄ raw DVC level.
// ---------------------------------------------------------------------------

/// Profile vibrance is 0..=100 with 50 = neutral. NvAPI's DVC range is
/// `[min..max]` where `min` is neutral (0 on every known GPU); values below
/// neutral are not a DVC concept, so 0..50 clamps to neutral.
pub fn vibrance_percent_to_dvc(percent: i32, min: i32, max: i32) -> i32 {
    let p = percent.clamp(0, 100);
    if p <= 50 || max <= min {
        return min;
    }
    min + ((p - 50) as f64 / 50.0 * (max - min) as f64).round() as i32
}

/// Inverse of [`vibrance_percent_to_dvc`] for reporting.
pub fn dvc_to_vibrance_percent(dvc: i32, min: i32, max: i32) -> i32 {
    if max <= min {
        return 50;
    }
    let frac = (dvc.clamp(min, max) - min) as f64 / (max - min) as f64;
    50 + (frac * 50.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vibrance_mapping_round_trips_on_the_standard_range() {
        // Standard NVIDIA range 0..63, neutral 0.
        assert_eq!(vibrance_percent_to_dvc(50, 0, 63), 0);
        assert_eq!(vibrance_percent_to_dvc(0, 0, 63), 0, "below neutral clamps");
        assert_eq!(vibrance_percent_to_dvc(100, 0, 63), 63);
        assert_eq!(vibrance_percent_to_dvc(75, 0, 63), 32);
        assert_eq!(dvc_to_vibrance_percent(0, 0, 63), 50);
        assert_eq!(dvc_to_vibrance_percent(63, 0, 63), 100);
        for p in [50, 60, 75, 90, 100] {
            let dvc = vibrance_percent_to_dvc(p, 0, 63);
            assert!((dvc_to_vibrance_percent(dvc, 0, 63) - p).abs() <= 1, "p={p}");
        }
    }

    #[test]
    fn degenerate_ranges_are_neutral() {
        assert_eq!(vibrance_percent_to_dvc(80, 0, 0), 0);
        assert_eq!(dvc_to_vibrance_percent(5, 3, 3), 50);
    }
}
