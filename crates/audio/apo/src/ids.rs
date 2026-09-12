//! Fixed identifiers shared by the COM object, the registration engine and
//! the core. Everything here is a string because the fxstore engine works on
//! a registry *image* (names and bytes), not live COM types.

/// CLSID of the Relay endpoint APO. Generated once for this project; never
/// reuse or change it — uninstall matches on it.
pub const APO_CLSID: &str = "{5A8E9C3B-1F6D-4B0A-9C41-7E2D83A6F0B4}";

/// Friendly name shown by Windows' enhancements UI and in our consent card.
pub const APO_FRIENDLY_NAME: &str = "Relay Audio (per-game EQ)";

/// `AUDIO_SIGNALPROCESSINGMODE_DEFAULT` — the only processing mode we serve.
pub const MODE_DEFAULT: &str = "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}";

/// MMDevices endpoint root (under HKLM), forward part of every endpoint path.
pub const RENDER_ROOT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render";

/// The FX property store of one render endpoint (relative to HKLM).
pub fn fx_key(endpoint_guid: &str) -> String {
    format!(r"{RENDER_ROOT}\{endpoint_guid}\FxProperties")
}

/// COM registration key for the APO DLL (relative to HKLM; audiodg runs as
/// LOCAL SERVICE and cannot see per-user classes, so HKCU is *not* possible —
/// decision recorded in the M3b plan).
pub fn clsid_key(clsid: &str) -> String {
    format!(r"SOFTWARE\Classes\CLSID\{clsid}")
}

// Value names inside FxProperties. A property key serialises in the registry
// as "{fmtid},pid" (lowercase braces/hex, no space) — the shapes below match
// `reg export` output byte-for-byte.

/// `PKEY_FX_EndpointEffectClsid` — legacy single-EFX slot.
pub const PKEY_FX_ENDPOINT_EFFECT_CLSID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},7";
/// `PKEY_CompositeFX_EndpointEffectClsid` — Win10 1809+ multi-APO EFX chain.
pub const PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID: &str =
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},15";
/// `PKEY_EFX_ProcessingModes_Supported_For_Streaming`.
pub const PKEY_EFX_MODES: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b},7";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_render_as_expected() {
        assert_eq!(
            fx_key("{f8ae226b-a4e3-45ab-97fc-3977dad232d1}"),
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{f8ae226b-a4e3-45ab-97fc-3977dad232d1}\FxProperties"
        );
        assert!(clsid_key(APO_CLSID).ends_with(APO_CLSID));
    }
}
