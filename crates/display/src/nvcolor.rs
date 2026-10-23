//! NVIDIA output colour (S52): colour format, dynamic range and bit depth —
//! what the NVIDIA Control Panel's "Change resolution → output colour"
//! section sets — as plain values, plus the pure planning between them and
//! NvAPI's raw `NV_COLOR_DATA`.
//!
//! The live calls are `nvapi::NvApi::{get_color, set_color}`. A restore puts
//! the captured raw state back verbatim, including the driver's colour
//! selection policy, so "let the driver choose" comes back as exactly that.

use serde::{Deserialize, Serialize};

/// `NV_COLOR_FORMAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorFormat {
    Rgb,
    Ycbcr444,
    Ycbcr422,
}

impl ColorFormat {
    pub const fn raw(self) -> u8 {
        match self {
            ColorFormat::Rgb => 0,
            ColorFormat::Ycbcr422 => 1,
            ColorFormat::Ycbcr444 => 2,
        }
    }
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(ColorFormat::Rgb),
            1 => Some(ColorFormat::Ycbcr422),
            2 => Some(ColorFormat::Ycbcr444),
            _ => None,
        }
    }
}

/// `NV_DYNAMIC_RANGE`: VESA is full range (0–255), CEA limited (16–235).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicRange {
    Full,
    Limited,
    Auto,
}

impl DynamicRange {
    pub const fn raw(self) -> u8 {
        match self {
            DynamicRange::Full => 0,
            DynamicRange::Limited => 1,
            DynamicRange::Auto => 0xFF,
        }
    }
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(DynamicRange::Full),
            1 => Some(DynamicRange::Limited),
            0xFF => Some(DynamicRange::Auto),
            _ => None,
        }
    }
}

/// `NV_BPC` ⇄ bits per component (6/8/10/12/16). 0 is "driver default".
pub const fn bpc_raw(bits: u8) -> Option<u32> {
    match bits {
        6 => Some(1),
        8 => Some(2),
        10 => Some(3),
        12 => Some(4),
        16 => Some(5),
        _ => None,
    }
}

pub const fn bpc_bits(raw: u32) -> Option<u8> {
    match raw {
        1 => Some(6),
        2 => Some(8),
        3 => Some(10),
        4 => Some(12),
        5 => Some(16),
        _ => None,
    }
}

/// `NV_COLOR_SELECTION_POLICY_USER`: the driver honours explicit values only
/// under this policy.
pub const POLICY_USER: u32 = 0;

/// What a profile asks for. `None` = leave that part as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NvColorSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<ColorFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<DynamicRange>,
    /// Bits per component: 6, 8, 10 or 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpc: Option<u8>,
}

impl NvColorSettings {
    pub fn is_inert(&self) -> bool {
        self.format.is_none() && self.range.is_none() && self.bpc.is_none()
    }
}

/// `NV_COLOR_DATA`'s data block, raw, as captured and restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NvColorRaw {
    pub format: u8,
    pub colorimetry: u8,
    pub range: u8,
    pub bpc: u32,
    pub policy: u32,
    pub depth: u32,
}

impl NvColorRaw {
    /// The profile-unit reading of this state, for the UI ("now: RGB, full,
    /// 10-bit").
    pub fn reading(&self) -> NvColorSettings {
        NvColorSettings {
            format: ColorFormat::from_raw(self.format),
            range: DynamicRange::from_raw(self.range),
            bpc: bpc_bits(self.bpc),
        }
    }
}

/// The raw state to write for `want` on top of `current`, or `None` when it
/// already matches (no write, nothing to restore). Fields the profile leaves
/// unset keep their current raw value. An unknown bit depth is refused as
/// `Err` naming the field, so it reaches "not on this hardware".
pub fn plan(
    current: &NvColorRaw,
    want: &NvColorSettings,
) -> Result<Option<NvColorRaw>, &'static str> {
    if want.is_inert() {
        return Ok(None);
    }
    let mut next = *current;
    if let Some(f) = want.format {
        next.format = f.raw();
    }
    if let Some(r) = want.range {
        next.range = r.raw();
    }
    if let Some(b) = want.bpc {
        next.bpc = bpc_raw(b).ok_or("nvidia_color.bpc")?;
    }
    if next == *current {
        return Ok(None);
    }
    next.policy = POLICY_USER;
    Ok(Some(next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NvColorRaw {
        // What an RTX on a DisplayPort HDR monitor reports by default.
        NvColorRaw { format: 0, colorimetry: 0, range: 0xFF, bpc: 3, policy: 1, depth: 0 }
    }

    #[test]
    fn raw_values_round_trip() {
        for f in [ColorFormat::Rgb, ColorFormat::Ycbcr444, ColorFormat::Ycbcr422] {
            assert_eq!(ColorFormat::from_raw(f.raw()), Some(f));
        }
        for r in [DynamicRange::Full, DynamicRange::Limited, DynamicRange::Auto] {
            assert_eq!(DynamicRange::from_raw(r.raw()), Some(r));
        }
        for b in [6u8, 8, 10, 12, 16] {
            assert_eq!(bpc_bits(bpc_raw(b).unwrap()), Some(b));
        }
        assert_eq!(bpc_raw(7), None);
    }

    #[test]
    fn plan_changes_only_what_the_profile_sets_and_takes_the_user_policy() {
        let want = NvColorSettings { range: Some(DynamicRange::Full), ..Default::default() };
        let next = plan(&now(), &want).unwrap().unwrap();
        assert_eq!(next.range, 0);
        assert_eq!((next.format, next.bpc, next.depth), (0, 3, 0), "rest untouched");
        assert_eq!(next.policy, POLICY_USER, "the driver only honours values under USER");
    }

    #[test]
    fn plan_is_a_no_op_when_nothing_changes() {
        assert_eq!(plan(&now(), &NvColorSettings::default()), Ok(None));
        let same = NvColorSettings { format: Some(ColorFormat::Rgb), bpc: Some(10), range: None };
        assert_eq!(plan(&now(), &same), Ok(None));
        assert_eq!(
            plan(&now(), &NvColorSettings { bpc: Some(9), ..Default::default() }),
            Err("nvidia_color.bpc")
        );
    }

    #[test]
    fn reading_names_the_current_state() {
        assert_eq!(
            now().reading(),
            NvColorSettings {
                format: Some(ColorFormat::Rgb),
                range: Some(DynamicRange::Auto),
                bpc: Some(10)
            }
        );
    }
}
