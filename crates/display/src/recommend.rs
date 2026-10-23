//! Suggested display values per panel kind and GPU vendor (S52).
//!
//! Pure and advisory: every suggestion carries a one-line reason, the UI
//! shows it beside the control with a "Use" button, and nothing here is ever
//! applied without that click. The table is deliberately short and
//! conservative — the established advice for a PC monitor, not a taste.

use serde::{Deserialize, Serialize};

use crate::learn::derive::PanelKind;
use crate::mode::{self, DisplayMode};

/// MCCS 0x14 (select colour preset) value for 6500 K.
pub const PRESET_6500K: u16 = 0x05;
/// MCCS 0x14 value for sRGB.
pub const PRESET_SRGB: u16 = 0x01;

/// Which colour API drives the monitor's GPU output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Nvidia,
    Amd,
    #[default]
    Other,
}

/// What the recommender knows about one monitor.
#[derive(Debug, Clone, Default)]
pub struct Input<'a> {
    /// The library's free-text panel field ("Nano IPS", "QD-OLED",
    /// "Mini-LED (VA)"); empty when the user has not said.
    pub panel_label: &'a str,
    /// Display name and stable id, for the model-table guess.
    pub name: &'a str,
    pub id: &'a str,
    /// Modes the driver lists, and the EDID native resolution.
    pub modes: &'a [DisplayMode],
    pub native: Option<(u32, u32)>,
    pub hdr_supported: bool,
    /// EDID bits per primary (8, 10, 12), when the panel reports it.
    pub edid_bpc: Option<u8>,
    pub vendor: Vendor,
    /// MCCS 0x14 values the monitor advertises.
    pub color_presets: &'a [u16],
}

/// A suggested value. Untagged so the UI reads it as the control's own type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Mode(DisplayMode),
    Flag(bool),
    Number(i64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    /// The profile field it is for: `mode`, `hdr`, `nvidia_color.format`,
    /// `nvidia_color.range`, `nvidia_color.bpc`, `color_preset`,
    /// `shadow_lift`, `vibrance`.
    pub field: String,
    pub value: Value,
    pub reason: String,
}

/// The panel kind used for advice, and whether it is Mini-LED (an LCD whose
/// local dimming already handles its blacks).
pub fn panel_of(input: &Input) -> (PanelKind, bool) {
    let label = input.panel_label.to_ascii_lowercase();
    let mini_led = label.contains("mini");
    let kind = match PanelKind::from_label(input.panel_label) {
        PanelKind::Unknown => {
            PanelKind::guess_from_model(input.name, input.id).unwrap_or(PanelKind::Unknown)
        }
        k => k,
    };
    (kind, mini_led)
}

fn rec(field: &str, value: Value, reason: &str) -> Recommendation {
    Recommendation { field: field.into(), value, reason: reason.into() }
}

/// Every suggestion that applies to this monitor, in the order the UI lists
/// its controls.
pub fn recommend(input: &Input) -> Vec<Recommendation> {
    let mut out = Vec::new();
    let (kind, mini_led) = panel_of(input);

    if let Some(m) = mode::recommended(input.modes, input.native) {
        out.push(rec(
            "mode",
            Value::Mode(m),
            "Native resolution at the panel's highest refresh: the sharpest picture and the least motion blur.",
        ));
    }
    if input.hdr_supported {
        out.push(rec(
            "hdr",
            Value::Flag(false),
            "Off unless the game has its own HDR mode you use: Windows HDR makes SDR games look washed out.",
        ));
    }
    if input.vendor == Vendor::Nvidia {
        out.push(rec(
            "nvidia_color.format",
            Value::Text("rgb".into()),
            "RGB is full 4:4:4 colour; YCbCr is meant for TVs and softens coloured text.",
        ));
        out.push(rec(
            "nvidia_color.range",
            Value::Text("full".into()),
            "A PC monitor expects full range; limited range turns blacks grey.",
        ));
        let bpc = match input.edid_bpc {
            Some(b) if b >= 10 => 10,
            _ => 8,
        };
        out.push(rec(
            "nvidia_color.bpc",
            Value::Number(bpc),
            if bpc >= 10 {
                "The panel takes 10-bit: smoother skies and smoke, no banding."
            } else {
                "8-bit is what this panel reports; more only adds dithering."
            },
        ));
    }
    if input.color_presets.contains(&PRESET_6500K) {
        out.push(rec(
            "color_preset",
            Value::Number(PRESET_6500K as i64),
            "6500 K is the standard white point games are mastered for.",
        ));
    } else if input.color_presets.contains(&PRESET_SRGB) {
        out.push(rec(
            "color_preset",
            Value::Number(PRESET_SRGB as i64),
            "sRGB is the colour space games are mastered in.",
        ));
    }

    let (lift, lift_why) = match (kind, mini_led) {
        (_, true) => (0, "Local dimming already handles this panel's blacks; a lift only adds bloom."),
        (PanelKind::Oled, _) => (0, "OLED black is true black; lifting shadows turns it grey."),
        (PanelKind::Va, _) => (10, "VA panels crush detail just above black; a small lift brings it back."),
        (PanelKind::Tn, _) => (5, "TN panels lose some shadow detail; a light lift recovers it."),
        (PanelKind::Ips, _) => (0, "IPS shows near-black detail as it is; a lift only adds glow."),
        (PanelKind::Unknown, _) => (0, "Set the panel type on the monitor for shadow advice; none until then."),
    };
    out.push(rec("shadow_lift", Value::Number(lift), lift_why));

    if input.vendor != Vendor::Other {
        let (vib, why) = match kind {
            PanelKind::Oled => (50, "OLED colour is already vivid; extra vibrance oversaturates skin and skies."),
            PanelKind::Unknown => (50, "Neutral until the panel type is known."),
            _ => (60, "A little vibrance helps enemies stand out on an sRGB LCD without clipping colour."),
        };
        out.push(rec("vibrance", Value::Number(vib), why));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes() -> Vec<DisplayMode> {
        vec![
            DisplayMode { width: 3840, height: 2160, refresh_hz: 240 },
            DisplayMode { width: 3840, height: 2160, refresh_hz: 60 },
            DisplayMode { width: 1920, height: 1080, refresh_hz: 480 },
        ]
    }

    fn field<'a>(r: &'a [Recommendation], f: &str) -> Option<&'a Recommendation> {
        r.iter().find(|x| x.field == f)
    }

    #[test]
    fn lg_oled_on_nvidia_gets_native_full_rgb_10_bit_and_no_lift() {
        let m = modes();
        let r = recommend(&Input {
            panel_label: "",
            name: "LG ULTRAGEAR+",
            id: "mon:GSM5C7C:402NTCZ9E219",
            modes: &m,
            native: Some((3840, 2160)),
            hdr_supported: true,
            edid_bpc: Some(10),
            vendor: Vendor::Nvidia,
            color_presets: &[0x05, 0x08, 0x0B],
        });
        assert_eq!(field(&r, "mode").unwrap().value, Value::Mode(m[0]));
        assert_eq!(field(&r, "hdr").unwrap().value, Value::Flag(false));
        assert_eq!(field(&r, "nvidia_color.format").unwrap().value, Value::Text("rgb".into()));
        assert_eq!(field(&r, "nvidia_color.range").unwrap().value, Value::Text("full".into()));
        assert_eq!(field(&r, "nvidia_color.bpc").unwrap().value, Value::Number(10));
        assert_eq!(field(&r, "color_preset").unwrap().value, Value::Number(5));
        // The model table knows GSM5C7C is OLED even with no label.
        assert_eq!(field(&r, "shadow_lift").unwrap().value, Value::Number(0));
        assert_eq!(field(&r, "vibrance").unwrap().value, Value::Number(50));
        assert!(r.iter().all(|x| !x.reason.is_empty()));
    }

    #[test]
    fn va_lifts_shadows_mini_led_does_not_and_amd_gets_no_nvidia_rows() {
        let base = Input { panel_label: "VA", vendor: Vendor::Amd, ..Default::default() };
        let r = recommend(&base);
        assert_eq!(field(&r, "shadow_lift").unwrap().value, Value::Number(10));
        assert!(field(&r, "nvidia_color.range").is_none());
        assert!(field(&r, "hdr").is_none(), "no HDR advice without HDR");
        assert!(field(&r, "mode").is_none(), "no modes listed, no mode advice");
        let mini = recommend(&Input { panel_label: "Mini-LED (VA)", ..base });
        assert_eq!(field(&mini, "shadow_lift").unwrap().value, Value::Number(0));
        assert_eq!(field(&mini, "vibrance").unwrap().value, Value::Number(60));
    }

    #[test]
    fn no_vendor_api_means_no_vibrance_advice() {
        let r = recommend(&Input { panel_label: "IPS", ..Default::default() });
        assert!(field(&r, "vibrance").is_none());
        assert!(field(&r, "nvidia_color.bpc").is_none());
    }

    #[test]
    fn values_serialise_as_the_controls_own_types() {
        let v = serde_json::to_value(Value::Mode(modes()[0])).unwrap();
        assert_eq!(v["refresh_hz"], 240);
        assert_eq!(serde_json::to_value(Value::Flag(false)).unwrap(), serde_json::json!(false));
        assert_eq!(serde_json::to_value(Value::Text("rgb".into())).unwrap(), serde_json::json!("rgb"));
    }
}
