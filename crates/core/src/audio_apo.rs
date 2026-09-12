//! The endpoint-APO [`AudioControl`] backend: parameters over shared memory.
//!
//! The APO (relay-apo, hosted by audiodg) creates a named section per
//! endpoint; this side opens it and steers the chain: `apply` writes the
//! profile's [`ChainParams`] and clears the bypass word, `restore` sets
//! bypass. If the section cannot be opened — APO not installed, or the audio
//! engine has not started a stream on that endpoint yet — apply degrades to
//! `Bypass` and restore succeeds silently: exactly what the Noop backend did,
//! so nothing regresses on machines without the APO.
//!
//! The connection is per-call: applies happen on focus changes, and an open
//! is one `OpenFileMappingW`. Holding a mapping in the always-on core would
//! buy microseconds and cost resident pages.

use anyhow::Result;
use tracing::{debug, info};

use crate::apply::AudioControl;
use crate::backup::AudioState;
use crate::types::{AudioChainState, AudioSettings};

#[cfg(windows)]
use relay_audio::shm::{section_name, SharedParams};

/// Backend for the installed endpoint APO.
#[derive(Debug, Default)]
pub struct ApoAudioControl;

/// What the Settings opt-in card shows.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApoStatus {
    /// Relay's CLSID is present in the default render endpoint's FX chain.
    pub installed: bool,
    /// The endpoint the status was read from (`{guid}`), if there is one.
    pub endpoint: Option<String>,
    /// Live parameter section reachable (APO actually running a stream).
    pub running: bool,
}

/// Read-only install probe: does the default render endpoint's FX property
/// store carry [`relay_apo::ids::APO_CLSID`]? Never writes anything.
#[cfg(windows)]
pub fn apo_status() -> ApoStatus {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ,
    };

    let endpoint = relay_audio::sessions::default_render_endpoint_guid().ok();
    let mut installed = false;
    if let Some(guid) = &endpoint {
        let key: Vec<u16> =
            relay_apo::ids::fx_key(guid).encode_utf16().chain(std::iter::once(0)).collect();
        for (value, flags) in [
            (relay_apo::ids::PKEY_COMPOSITEFX_ENDPOINT_EFFECT_CLSID, RRF_RT_REG_MULTI_SZ),
            (relay_apo::ids::PKEY_FX_ENDPOINT_EFFECT_CLSID, RRF_RT_REG_SZ),
        ] {
            let wval: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            let mut buf = [0u16; 2048];
            let mut len = (buf.len() * 2) as u32;
            // SAFETY: out-buffer and length are valid; RegGetValueW
            // NUL-terminates and never overruns `len`.
            let r = unsafe {
                RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    PCWSTR(key.as_ptr()),
                    PCWSTR(wval.as_ptr()),
                    flags,
                    None,
                    Some(buf.as_mut_ptr() as *mut _),
                    Some(&mut len),
                )
            };
            if r.is_ok() {
                let text = String::from_utf16_lossy(&buf[..(len as usize / 2)]);
                if text
                    .to_ascii_lowercase()
                    .contains(&relay_apo::ids::APO_CLSID.to_ascii_lowercase())
                {
                    installed = true;
                    break;
                }
            }
        }
    }
    let running = open_default_endpoint().is_some();
    ApoStatus { installed, endpoint, running }
}

#[cfg(not(windows))]
pub fn apo_status() -> ApoStatus {
    ApoStatus { installed: false, endpoint: None, running: false }
}

#[cfg(windows)]
fn open_default_endpoint() -> Option<SharedParams> {
    let guid = match relay_audio::sessions::default_render_endpoint_guid() {
        Ok(g) => g,
        Err(e) => {
            debug!(error = %e, "no default render endpoint for APO params");
            return None;
        }
    };
    let instance = std::env::var("RELAY_INSTANCE").unwrap_or_default();
    let mut name = section_name(&guid, &instance);
    if std::env::var("RELAY_APO_LOCAL_SECTION").is_ok() {
        // Test rig: section created in-session, not by audiodg.
        name = name.replacen("Global\\", "Local\\", 1);
    }
    match SharedParams::open(&name) {
        Ok(s) => Some(s),
        Err(e) => {
            debug!(error = %e, section = %name, "APO parameter section not available");
            None
        }
    }
}

#[cfg(windows)]
impl AudioControl for ApoAudioControl {
    fn capture(&self) -> Result<AudioState> {
        // The APO's resting state is bypass by construction (the block is
        // initialised that way), so the original state is always "bypass".
        // Reading the live word anyway keeps the snapshot honest.
        let bypass = open_default_endpoint().map_or(true, |s| s.block().bypass());
        Ok(AudioState { bypass })
    }

    fn apply(&self, settings: &AudioSettings) -> Result<AudioChainState> {
        let params = crate::audio_bridge::chain_params(settings);
        if params.bands.is_empty() && params.limiter.is_none() && !params.hrtf {
            return Ok(AudioChainState::Bypass);
        }
        let Some(s) = open_default_endpoint() else {
            // APO not installed / not running: nothing applied, nothing to
            // restore. The exclusive-mode watcher is unaffected.
            return Ok(AudioChainState::Bypass);
        };
        s.block().write_params(&params);
        s.block().set_bypass(false);
        s.notify();
        info!(bands = params.bands.len(), hrtf = params.hrtf, "APO chain active");
        Ok(AudioChainState::Active)
    }

    fn restore(&self, _original: &AudioState) -> Result<()> {
        // Restore = bypass, regardless of what was captured: the APO never
        // has profile state worth putting back (the brief's "original state"
        // for audio is silence-through-the-wire).
        if let Some(s) = open_default_endpoint() {
            s.block().set_bypass(true);
            s.notify();
            info!("APO chain bypassed");
        }
        Ok(())
    }
}

#[cfg(not(windows))]
impl AudioControl for ApoAudioControl {
    fn capture(&self) -> Result<AudioState> {
        Ok(AudioState { bypass: true })
    }
    fn apply(&self, _: &AudioSettings) -> Result<AudioChainState> {
        Ok(AudioChainState::Bypass)
    }
    fn restore(&self, _: &AudioState) -> Result<()> {
        Ok(())
    }
}
