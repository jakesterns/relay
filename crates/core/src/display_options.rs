//! S52: what one monitor offers for the profile's display controls, read on
//! demand when the UI opens a profile's Display section.
//!
//! Read-only: the driver's mode list and current mode, Windows' HDR state,
//! the NVIDIA output colour, the colour presets the panel listed, and the
//! recommendations for this panel × GPU. Nothing here writes, and nothing is
//! read at idle — the core only answers when asked.

use serde::{Deserialize, Serialize};

use relay_display::learn::derive::PanelKind;
use relay_display::mode::{DisplayMode, HdrState};
use relay_display::nvcolor::{NvColorRaw, NvColorSettings};
use relay_display::recommend::{self, Recommendation, Vendor};

use crate::hardware::{Monitor, MonitorProbe};
use crate::types::MonitorId;

/// The live, read-only state [`assemble`] turns into options.
#[derive(Debug, Clone, Default)]
pub struct LiveOptions {
    pub modes: Vec<DisplayMode>,
    pub current_mode: Option<DisplayMode>,
    pub hdr: Option<HdrState>,
    pub nv_color: Option<NvColorRaw>,
    pub vendor: Vendor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayOptions {
    pub monitor: MonitorId,
    pub name: String,
    /// Modes the driver lists, largest and fastest first.
    #[serde(default)]
    pub modes: Vec<DisplayMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_mode: Option<DisplayMode>,
    /// Native resolution: the user's override, else the EDID's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<(u32, u32)>,
    /// Windows' HDR state (`None` = unknown). `hdr_supported` folds in the
    /// user's override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr: Option<HdrState>,
    pub hdr_supported: bool,
    /// The NVIDIA output colour now, when NvAPI drives this display and has
    /// the colour control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvidia_color: Option<NvColorSettings>,
    pub vendor: Vendor,
    /// Colour-preset (VCP 0x14) values the panel listed; empty = no control.
    #[serde(default)]
    pub color_presets: Vec<u16>,
    /// The panel kind recommendations used, and whether it was the user's
    /// own label (false = guessed from the model, or unknown).
    pub panel_kind: PanelKind,
    pub panel_from_user: bool,
    #[serde(default)]
    pub recommendations: Vec<Recommendation>,
}

/// Pure: probe + library + live reads → options and recommendations.
pub fn assemble(target: &MonitorProbe, lib: Option<&Monitor>, live: LiveOptions) -> DisplayOptions {
    let panel_label = lib.map(|m| m.panel.as_str()).unwrap_or("");
    let native = lib
        .and_then(|m| m.mode_override)
        .map(|m| (m.width, m.height))
        .or(target.native);
    let hdr_supported = lib
        .and_then(|m| m.hdr_override)
        .unwrap_or_else(|| live.hdr.is_some_and(|h| h.supported));
    let color_presets: Vec<u16> = target
        .color_presets
        .clone()
        .or_else(|| lib.and_then(|m| m.color_presets.clone()))
        .unwrap_or_default();
    let edid_bpc = target.color.as_ref().and_then(|c| c.bit_depth);
    // The mode the user says is best leads the list even if the driver's
    // enumeration ranks another first.
    let mut modes = live.modes.clone();
    if let Some(o) = lib.and_then(|m| m.mode_override) {
        if let Some(i) = modes.iter().position(|m| *m == o) {
            let m = modes.remove(i);
            modes.insert(0, m);
        }
    }
    let input = recommend::Input {
        panel_label,
        name: &target.name,
        id: &target.id.0,
        modes: &live.modes,
        native,
        hdr_supported,
        edid_bpc,
        vendor: live.vendor,
        color_presets: &color_presets,
    };
    let (panel_kind, _) = recommend::panel_of(&input);
    let mut recommendations = recommend::recommend(&input);
    if let Some(o) = lib.and_then(|m| m.mode_override) {
        if live.modes.contains(&o) {
            if let Some(r) = recommendations.iter_mut().find(|r| r.field == "mode") {
                r.value = recommend::Value::Mode(o);
                r.reason = "The mode you set as this monitor's best.".into();
            }
        }
    }
    DisplayOptions {
        monitor: target.id.clone(),
        name: lib.map(|m| m.name.clone()).unwrap_or_else(|| target.name.clone()),
        modes,
        current_mode: live.current_mode,
        native,
        hdr: live.hdr,
        hdr_supported,
        nvidia_color: live.nv_color.map(|raw| raw.reading()),
        vendor: live.vendor,
        color_presets,
        panel_kind,
        panel_from_user: PanelKind::from_label(panel_label) != PanelKind::Unknown,
        recommendations,
    }
}

/// The live reads for one display. Loads NvAPI / ADL only for this call.
#[cfg(windows)]
pub fn read_live(gdi_name: &str) -> LiveOptions {
    use relay_display::{amd, mode, nvapi};
    let nv = nvapi::NvApi::load().and_then(|api| {
        let d = api.display_by_gdi_name(gdi_name)?;
        Some(api.get_color(&d).ok())
    });
    let vendor = match nv {
        Some(_) => Vendor::Nvidia,
        None if amd::Adl::load().and_then(|a| a.display_by_gdi_name(gdi_name)).is_some() => {
            Vendor::Amd
        }
        None => Vendor::Other,
    };
    LiveOptions {
        modes: mode::io::list_modes(gdi_name),
        current_mode: mode::io::current_mode(gdi_name).ok(),
        hdr: mode::io::hdr_state(gdi_name),
        nv_color: nv.flatten(),
        vendor,
    }
}

#[cfg(not(windows))]
pub fn read_live(_gdi_name: &str) -> LiveOptions {
    LiveOptions::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lg() -> MonitorProbe {
        MonitorProbe {
            id: MonitorId("mon:GSM5C7C:402NTCZ9E219".into()),
            name: "LG ULTRAGEAR+".into(),
            native: Some((3840, 2160)),
            refresh_hz: Some(240.0),
            primary: true,
            hmonitor: 1,
            gdi_name: r"\\.\DISPLAY1".into(),
            ddc: Some(vec![0x10, 0x12, 0x14]),
            color_presets: Some(vec![0x05, 0x08, 0x0B]),
            color: None,
        }
    }

    fn m(w: u32, h: u32, hz: u32) -> DisplayMode {
        DisplayMode { width: w, height: h, refresh_hz: hz }
    }

    /// As read live from the dev PC on 2026-10-22.
    fn live() -> LiveOptions {
        LiveOptions {
            modes: vec![m(3840, 2160, 240), m(3840, 2160, 60), m(2560, 1440, 240)],
            current_mode: Some(m(2560, 1440, 240)),
            hdr: Some(HdrState { supported: true, enabled: true }),
            nv_color: Some(NvColorRaw { format: 0, colorimetry: 8, range: 0, bpc: 2, policy: 0, depth: 0 }),
            vendor: Vendor::Nvidia,
        }
    }

    #[test]
    fn the_dev_pc_lg_gets_its_live_state_and_oled_advice() {
        let o = assemble(&lg(), None, live());
        assert_eq!(o.current_mode, Some(m(2560, 1440, 240)));
        assert!(o.hdr_supported);
        assert_eq!(o.nvidia_color.unwrap().bpc, Some(8));
        assert_eq!(o.color_presets, vec![0x05, 0x08, 0x0B]);
        assert_eq!(o.panel_kind, PanelKind::Oled, "guessed from the model table");
        assert!(!o.panel_from_user);
        let mode = o.recommendations.iter().find(|r| r.field == "mode").unwrap();
        assert_eq!(mode.value, recommend::Value::Mode(m(3840, 2160, 240)));
    }

    #[test]
    fn library_overrides_steer_native_hdr_and_the_label() {
        let lib = Monitor {
            id: lg().id,
            name: "Desk LG".into(),
            panel: "WOLED".into(),
            ddcci: None,
            color: None,
            color_presets: None,
            mode_override: Some(m(2560, 1440, 240)),
            hdr_override: Some(false),
        };
        let o = assemble(&lg(), Some(&lib), live());
        assert_eq!(o.name, "Desk LG");
        assert_eq!(o.native, Some((2560, 1440)));
        assert!(!o.hdr_supported, "the user's override wins over Windows");
        assert!(o.recommendations.iter().all(|r| r.field != "hdr"));
        assert!(o.panel_from_user);
        assert_eq!(o.modes[0], m(2560, 1440, 240), "the user's best mode leads");
        let mode = o.recommendations.iter().find(|r| r.field == "mode").unwrap();
        assert_eq!(mode.value, recommend::Value::Mode(m(2560, 1440, 240)));
    }

    #[test]
    fn a_non_nvidia_display_has_no_output_colour() {
        let o = assemble(&lg(), None, LiveOptions { nv_color: None, vendor: Vendor::Amd, ..live() });
        assert!(o.nvidia_color.is_none());
        assert!(o.recommendations.iter().all(|r| !r.field.starts_with("nvidia_color")));
    }
}
