//! Windows display mode (resolution × refresh) and HDR ("advanced colour")
//! for one display (S52).
//!
//! Both are per-display Windows state, vendor-neutral, and both are captured
//! before a profile changes them and put back on restore:
//!
//! - **Mode** goes through `ChangeDisplaySettingsExW` on that one GDI device,
//!   *without* `CDS_UPDATEREGISTRY`: the change is dynamic and never saved, so
//!   even a crash that skipped the restore is undone by a sign-out or reboot.
//!   Only modes the driver lists for the device are accepted
//!   ([`io::set_mode`] looks the requested mode up in the enumeration and
//!   refuses anything else, then asks Windows with `CDS_TEST` first).
//! - **HDR** goes through `DisplayConfigSetDeviceInfo`
//!   (`DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE`) on the target of
//!   the active path whose source is that GDI device. Offered only where
//!   Windows reports advanced colour as supported.
//!
//! A mode change blanks the screen for about a second on most panels, so it
//! is opt-in per profile and never part of a learned look.

use serde::{Deserialize, Serialize};

/// One display mode as the user picks it: pixels and whole-hertz refresh.
/// (`DEVMODEW::dmDisplayFrequency` is an integer; 239/240 and 59/60 are what
/// the driver lists, and both are offered as listed.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

impl DisplayMode {
    pub fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

impl std::fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}×{} @ {} Hz", self.width, self.height, self.refresh_hz)
    }
}

/// Sort the driver's list for a picker: largest resolution first, then
/// highest refresh, duplicates (the same mode at several bit depths or
/// scaling flags) removed.
pub fn normalize(mut modes: Vec<DisplayMode>) -> Vec<DisplayMode> {
    modes.sort_by(|a, b| {
        b.pixels().cmp(&a.pixels()).then(b.width.cmp(&a.width)).then(b.refresh_hz.cmp(&a.refresh_hz))
    });
    modes.dedup();
    modes
}

/// The mode to recommend: the panel's native resolution at the highest
/// refresh the driver lists for it. Falls back to the largest mode when the
/// native resolution is unknown or not listed.
pub fn recommended(modes: &[DisplayMode], native: Option<(u32, u32)>) -> Option<DisplayMode> {
    let at_native = native.and_then(|(w, h)| {
        modes.iter().filter(|m| m.width == w && m.height == h).max_by_key(|m| m.refresh_hz).copied()
    });
    at_native.or_else(|| {
        let best = modes.iter().max_by_key(|m| m.pixels())?;
        modes
            .iter()
            .filter(|m| m.width == best.width && m.height == best.height)
            .max_by_key(|m| m.refresh_hz)
            .copied()
    })
}

/// Windows' advanced-colour state for one display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HdrState {
    /// The display and link can do HDR (Windows would offer the toggle).
    pub supported: bool,
    /// HDR is on now.
    pub enabled: bool,
}

/// Decode `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO`'s bitfield: bit 0
/// advancedColorSupported, bit 1 advancedColorEnabled, bit 3
/// advancedColorForceDisabled (policy forbids it: treated as unsupported).
pub fn decode_advanced_color(value: u32) -> HdrState {
    let force_disabled = value & 0b1000 != 0;
    HdrState { supported: value & 0b1 != 0 && !force_disabled, enabled: value & 0b10 != 0 }
}

#[cfg(windows)]
pub mod io {
    //! The live calls. Reads are free to call any time; [`set_mode`] and
    //! [`set_hdr`] change the screen and are only reached through the core's
    //! capture-then-apply path.

    use super::{decode_advanced_color, normalize, DisplayMode, HdrState};
    use anyhow::{bail, Context, Result};
    use windows::core::PCWSTR;
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes,
        QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};
    use windows::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, EnumDisplaySettingsExW, CDS_TYPE, DEVMODEW,
        DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
        ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS, ENUM_DISPLAY_SETTINGS_MODE,
    };

    /// `ChangeDisplaySettingsEx` flag: validate only, change nothing.
    const CDS_TEST: u32 = 0x0000_0002;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn devmode() -> DEVMODEW {
        DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() }
    }

    fn mode_of(dm: &DEVMODEW) -> DisplayMode {
        DisplayMode { width: dm.dmPelsWidth, height: dm.dmPelsHeight, refresh_hz: dm.dmDisplayFrequency }
    }

    /// Every raw DEVMODE the driver lists for this device.
    fn raw_modes(gdi_name: &str) -> Vec<DEVMODEW> {
        let name = wide(gdi_name);
        let mut out = Vec::new();
        for i in 0..4096u32 {
            let mut dm = devmode();
            // SAFETY: dmSize is set; the call fills the struct or returns false
            // past the last mode.
            let ok = unsafe {
                EnumDisplaySettingsExW(
                    PCWSTR(name.as_ptr()),
                    ENUM_DISPLAY_SETTINGS_MODE(i),
                    &mut dm,
                    ENUM_DISPLAY_SETTINGS_FLAGS(0),
                )
            };
            if !ok.as_bool() {
                break;
            }
            out.push(dm);
        }
        out
    }

    /// The modes this display offers, sorted for a picker. Read-only.
    pub fn list_modes(gdi_name: &str) -> Vec<DisplayMode> {
        normalize(raw_modes(gdi_name).iter().map(mode_of).filter(|m| m.refresh_hz > 1).collect())
    }

    fn current_raw(gdi_name: &str) -> Result<DEVMODEW> {
        let name = wide(gdi_name);
        let mut dm = devmode();
        // SAFETY: as above.
        let ok = unsafe {
            EnumDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut dm,
                ENUM_DISPLAY_SETTINGS_FLAGS(0),
            )
        };
        if !ok.as_bool() {
            bail!("could not read the current mode of {gdi_name}");
        }
        Ok(dm)
    }

    /// The mode the display is in now. Read-only.
    pub fn current_mode(gdi_name: &str) -> Result<DisplayMode> {
        current_raw(gdi_name).map(|dm| mode_of(&dm))
    }

    /// Switch this one display to `mode`, which must be one the driver
    /// lists (same colour depth as now when the driver lists several).
    /// Dynamic only: never written to the registry.
    pub fn set_mode(gdi_name: &str, mode: DisplayMode) -> Result<()> {
        let current = current_raw(gdi_name)?;
        if mode_of(&current) == mode {
            return Ok(());
        }
        let listed = raw_modes(gdi_name);
        let pick = listed
            .iter()
            .filter(|dm| mode_of(dm) == mode)
            .max_by_key(|dm| dm.dmBitsPerPel == current.dmBitsPerPel)
            .with_context(|| format!("{mode} is not a mode {gdi_name} offers"))?;
        let mut dm = *pick;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
        let name = wide(gdi_name);
        for flags in [CDS_TEST, 0] {
            // SAFETY: a fully initialised DEVMODEW from the driver's own list.
            let rc = unsafe {
                ChangeDisplaySettingsExW(
                    PCWSTR(name.as_ptr()),
                    Some(&dm),
                    None,
                    CDS_TYPE(flags),
                    None,
                )
            };
            if rc != DISP_CHANGE_SUCCESSFUL {
                bail!(
                    "Windows refused {mode} on {gdi_name} ({}: {})",
                    if flags == CDS_TEST { "test" } else { "change" },
                    rc.0
                );
            }
        }
        Ok(())
    }

    /// (adapter, target id) of the active path whose source is this GDI
    /// device.
    fn target_of(gdi_name: &str) -> Option<(LUID, u32)> {
        active_paths().into_iter().find_map(|p| {
            let mut info = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: p.sourceInfo.adapterId,
                    id: p.sourceInfo.id,
                },
                ..Default::default()
            };
            // SAFETY: sized, typed request packet.
            if unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } != 0 {
                return None;
            }
            let end = info.viewGdiDeviceName.iter().position(|&c| c == 0).unwrap_or(32);
            let name = String::from_utf16_lossy(&info.viewGdiDeviceName[..end]);
            name.eq_ignore_ascii_case(gdi_name).then_some((p.targetInfo.adapterId, p.targetInfo.id))
        })
    }

    fn active_paths() -> Vec<DISPLAYCONFIG_PATH_INFO> {
        // SAFETY: sizes from GetDisplayConfigBufferSizes; retried because the
        // topology can change between the two calls.
        unsafe {
            for _ in 0..3 {
                let (mut n_paths, mut n_modes) = (0u32, 0u32);
                if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes)
                    != ERROR_SUCCESS
                {
                    return Vec::new();
                }
                let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
                let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
                if QueryDisplayConfig(
                    QDC_ONLY_ACTIVE_PATHS,
                    &mut n_paths,
                    paths.as_mut_ptr(),
                    &mut n_modes,
                    modes.as_mut_ptr(),
                    None,
                ) == ERROR_SUCCESS
                {
                    paths.truncate(n_paths as usize);
                    return paths;
                }
            }
            Vec::new()
        }
    }

    /// Windows' HDR state for this display; `None` when it is not on an
    /// active path or the query fails (older Windows). Read-only.
    pub fn hdr_state(gdi_name: &str) -> Option<HdrState> {
        let (adapter, id) = target_of(gdi_name)?;
        let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                adapterId: adapter,
                id,
            },
            ..Default::default()
        };
        // SAFETY: sized, typed request packet; the union is read as its u32.
        unsafe {
            if DisplayConfigGetDeviceInfo(&mut info.header) != 0 {
                return None;
            }
            Some(decode_advanced_color(info.Anonymous.value))
        }
    }

    /// Turn HDR on or off for this one display.
    pub fn set_hdr(gdi_name: &str, enabled: bool) -> Result<()> {
        let (adapter, id) =
            target_of(gdi_name).with_context(|| format!("{gdi_name} is not on an active path"))?;
        let mut packet = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                size: std::mem::size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32,
                adapterId: adapter,
                id,
            },
            ..Default::default()
        };
        packet.Anonymous.value = enabled as u32;
        // SAFETY: sized, typed set packet.
        let rc = unsafe { DisplayConfigSetDeviceInfo(&packet.header) };
        if rc != 0 {
            bail!("Windows refused HDR {} on {gdi_name} ({rc})", if enabled { "on" } else { "off" });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(width: u32, height: u32, refresh_hz: u32) -> DisplayMode {
        DisplayMode { width, height, refresh_hz }
    }

    #[test]
    fn normalize_sorts_largest_and_fastest_first_without_duplicates() {
        let got = normalize(vec![m(1920, 1080, 60), m(3840, 2160, 60), m(3840, 2160, 240), m(1920, 1080, 60)]);
        assert_eq!(got, vec![m(3840, 2160, 240), m(3840, 2160, 60), m(1920, 1080, 60)]);
    }

    #[test]
    fn recommends_the_fastest_native_mode() {
        let modes = vec![m(3840, 2160, 240), m(3840, 2160, 60), m(2560, 1440, 360), m(1920, 1080, 480)];
        assert_eq!(recommended(&modes, Some((3840, 2160))), Some(m(3840, 2160, 240)));
        // A faster mode at a lower resolution is not "native".
        assert_eq!(recommended(&modes, Some((2560, 1440))), Some(m(2560, 1440, 360)));
        // Unknown native: the largest resolution, fastest refresh.
        assert_eq!(recommended(&modes, None), Some(m(3840, 2160, 240)));
        // Native not listed: same fallback.
        assert_eq!(recommended(&modes, Some((5120, 2880))), Some(m(3840, 2160, 240)));
        assert_eq!(recommended(&[], None), None);
    }

    #[test]
    fn advanced_colour_bits_decode() {
        assert_eq!(decode_advanced_color(0), HdrState { supported: false, enabled: false });
        assert_eq!(decode_advanced_color(0b01), HdrState { supported: true, enabled: false });
        assert_eq!(decode_advanced_color(0b11), HdrState { supported: true, enabled: true });
        // Force-disabled by policy: not offered.
        assert_eq!(decode_advanced_color(0b1001), HdrState { supported: false, enabled: false });
        assert_eq!(m(2560, 1440, 165).to_string(), "2560×1440 @ 165 Hz");
    }
}
