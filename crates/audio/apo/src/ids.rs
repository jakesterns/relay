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

/// Root of the audio engine's APO registry (relative to HKLM). audiodg only
/// instantiates an APO that has a key here — the `RegisterAPO` /
/// `APO_REG_PROPERTIES` registration. Live finding 2026-09-30: with only
/// the COM class and the FxProperties values, the APO never loaded.
pub const AUDIO_ENGINE_APO_ROOT: &str = r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects";

/// The audio-engine registration key of one APO (relative to HKLM).
pub fn audio_engine_key(clsid: &str) -> String {
    format!(r"{AUDIO_ENGINE_APO_ROOT}\{clsid}")
}

/// Copyright string, as `GetRegistrationProperties` reports it and the
/// audio-engine registration records it.
pub const APO_COPYRIGHT: &str = "\u{a9} Relay";

/// `APO_REG_PROPERTIES.Flags` — `APO_FLAG_DEFAULT` (0x0e) =
/// `SAMPLESPERFRAME_MUST_MATCH` (2) | `FRAMESPERSECOND_MUST_MATCH` (4) |
/// `BITSPERSAMPLE_MUST_MATCH` (8). Each is true of the code: `negotiate`
/// accepts only float32 stereo on both sides and refuses a rate that differs
/// from the opposite side, and `CalcInput/OutputFrames` are 1:1. Not
/// `INPLACE` (1): `APOProcess` reads the input connection and writes the
/// output connection as separate buffers. Must equal what
/// `com::GetRegistrationProperties` returns (asserted in `tests/apo_com.rs`).
pub const APO_REG_FLAGS: u32 = 0x0000_000e;

/// `APO_REG_PROPERTIES` version fields (match `GetRegistrationProperties`).
pub const APO_MAJOR_VERSION: u32 = 1;
pub const APO_MINOR_VERSION: u32 = 0;

/// The one interface `GetRegistrationProperties` lists:
/// `IID_IAudioProcessingObject`, uppercase as `RegisterAPO` writes it for
/// the third-party APOs registered on the dev PC.
pub const IID_IAUDIO_PROCESSING_OBJECT: &str = "{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}";

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

/// `PKEY_FX_ModeEffectClsid` — legacy single-MFX slot (mode effect: runs
/// once per processing mode, after the mix). S42d: Relay's slot.
pub const PKEY_FX_MODE_EFFECT_CLSID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},6";
/// `PKEY_CompositeFX_ModeEffectClsid` — multi-APO MFX chain.
pub const PKEY_COMPOSITEFX_MODE_EFFECT_CLSID: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},14";
/// `PKEY_MFX_ProcessingModes_Supported_For_Streaming`.
pub const PKEY_MFX_MODES: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b},6";
/// `PKEY_AudioEndpoint_Disable_SysFx` — the endpoint's "Disable all
/// enhancements" switch. While it is non-zero no APO on the endpoint loads;
/// the install deletes it (as Equalizer APO's Configurator does) and the
/// backup puts it back.
pub const PKEY_DISABLE_SYSFX: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

/// Every FxProperties value name an install may create, change or delete.
/// The live writer refuses a plan whose diff names anything else.
pub const FX_WRITABLE_VALUES: &[&str] = &[
    PKEY_FX_MODE_EFFECT_CLSID,
    PKEY_COMPOSITEFX_MODE_EFFECT_CLSID,
    PKEY_MFX_MODES,
    PKEY_DISABLE_SYSFX,
];

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
        assert_eq!(
            audio_engine_key(APO_CLSID),
            r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{5A8E9C3B-1F6D-4B0A-9C41-7E2D83A6F0B4}"
        );
    }
}
