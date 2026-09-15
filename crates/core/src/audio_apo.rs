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

/// Exactly what an install would write, read-only — the listing the user
/// reads *before* the UAC prompt. Reads the endpoint's live FX store and
/// plans against it, so the values named are the ones that would really
/// change on this machine rather than a generic description.
#[cfg(windows)]
pub fn install_dry_run(backup_dir: &std::path::Path) -> Vec<String> {
    let Ok(endpoint) = relay_audio::sessions::default_render_endpoint_guid() else {
        return vec!["No default render endpoint — there is nothing to install on.".into()];
    };
    let dll = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("relay_apo.dll")))
        .filter(|p| p.exists());
    let dll_text = dll
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| r"<install dir>\relay_apo.dll (not built yet)".into());

    let mut lines = Vec::new();
    match relay_apo::livereg::LiveRegistry::read_fx_store(&endpoint) {
        Ok(current) => {
            let plan = relay_apo::fxstore::plan_install(&current, &endpoint, &dll_text);
            let fx_root = relay_apo::ids::fx_key(&endpoint);
            for (rel, name) in relay_apo::fxstore::diff(&plan.backup.store, &plan.new_store) {
                let key =
                    if rel.is_empty() { fx_root.clone() } else { format!(r"{fx_root}\{rel}") };
                lines.push(format!(r"HKLM\{key} :: {name}"));
            }
            for path in plan.com_keys.keys() {
                lines.push(format!(r"HKLM\{path}"));
            }
        }
        Err(e) => lines.push(format!("Could not read the endpoint's FX chain: {e}")),
    }
    lines.push(format!(
        "backup: {} (written before anything is changed)",
        backup_dir.join(format!("{endpoint}.json")).display()
    ));
    lines.push(format!("file: {dll_text} (stays in place; only registered)"));
    lines
}

#[cfg(not(windows))]
pub fn install_dry_run(_backup_dir: &std::path::Path) -> Vec<String> {
    vec!["The endpoint APO is Windows-only.".into()]
}

/// Register the APO on the default render endpoint. Backup-then-apply, same
/// contract as the `Applier`: the complete prior FX property store lands in
/// `<backup_dir>\<endpoint>.json` *before* the registry changes.
///
/// Gated twice: `relay-apo`'s livereg refuses without
/// `RELAY_APO_ALLOW_LIVE_WRITE=1` (VM / installer only — this dev machine
/// never sets it), and this fn refuses to overwrite an existing backup (an
/// install over an install would lose the true original).
#[cfg(windows)]
pub fn install_live(backup_dir: &std::path::Path) -> Result<String> {
    use anyhow::{bail, Context};

    let endpoint = relay_audio::sessions::default_render_endpoint_guid()
        .context("no default render endpoint")?;
    let backup_file = backup_dir.join(format!("{endpoint}.json"));
    if backup_file.exists() {
        bail!("a backup for {endpoint} already exists — the APO looks installed; uninstall first");
    }
    let dll = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("relay_apo.dll")))
        .filter(|p| p.exists())
        .context("relay_apo.dll not found next to the running binary")?;

    let current = relay_apo::livereg::LiveRegistry::read_fx_store(&endpoint)
        .with_context(|| format!("reading FX store of {endpoint}"))?;
    let plan = relay_apo::fxstore::plan_install(&current, &endpoint, &dll.to_string_lossy());

    std::fs::create_dir_all(backup_dir)?;
    let tmp = backup_file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&plan.backup)?)?;
    std::fs::rename(&tmp, &backup_file).context("persisting the backup")?;

    relay_apo::livereg::LiveRegistry::apply_install(&plan)
        .context("registering the APO (VM / installer only; RELAY_APO_ALLOW_LIVE_WRITE gate)")?;
    info!(%endpoint, "APO registered; endpoint streams pick it up on their next start");
    Ok(endpoint)
}

/// Restore the endpoint's FX property store from the backup taken at
/// install, byte-for-byte, and remove our COM registration. The backup file
/// is kept with a `.restored` suffix for the post-uninstall export diff.
#[cfg(windows)]
pub fn uninstall_live(backup_dir: &std::path::Path) -> Result<String> {
    use anyhow::Context;

    let endpoint = relay_audio::sessions::default_render_endpoint_guid()
        .context("no default render endpoint")?;
    let backup_file = backup_dir.join(format!("{endpoint}.json"));
    let backup: relay_apo::fxstore::Backup = serde_json::from_slice(
        &std::fs::read(&backup_file)
            .with_context(|| format!("no backup for {endpoint}; nothing to uninstall"))?,
    )?;
    relay_apo::livereg::LiveRegistry::restore(&backup)
        .context("restoring the FX property store (RELAY_APO_ALLOW_LIVE_WRITE gate)")?;
    let _ = std::fs::rename(&backup_file, backup_file.with_extension("json.restored"));
    info!(%endpoint, "FX property store restored to the pre-install state");
    Ok(endpoint)
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
        let bypass = open_default_endpoint().is_none_or(|s| s.block().bypass());
        Ok(AudioState { bypass })
    }

    fn apply(
        &self,
        settings: &AudioSettings,
        correction: Option<&[(f32, f32)]>,
    ) -> Result<AudioChainState> {
        let params = crate::audio_bridge::chain_params_with(settings, correction);
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
    fn apply(&self, _: &AudioSettings, _: Option<&[(f32, f32)]>) -> Result<AudioChainState> {
        Ok(AudioChainState::Bypass)
    }
    fn restore(&self, _: &AudioState) -> Result<()> {
        Ok(())
    }
}
