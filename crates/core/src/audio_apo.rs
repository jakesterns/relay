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
//! S42: the APO is installed *per render endpoint* — any number of outputs
//! can carry it at once, each with its own backup
//! (`apo-backup\<endpoint>.json`) and its own parameter section
//! ([`params_section`]). At runtime the profile plus the active listening
//! device's correction go to the endpoint the game's audio plays on — the
//! default render endpoint — and restore bypasses both that endpoint and any
//! endpoint an earlier apply wrote to (the default can change mid-game).
//!
//! The connection is per-call: applies happen on focus changes, and an open
//! is one `OpenFileMappingW`. Holding a mapping in the always-on core would
//! buy microseconds and cost resident pages.

use std::path::{Path, PathBuf};

use anyhow::Result;
#[cfg(windows)]
use tracing::{debug, info};

use crate::apply::AudioControl;
use crate::backup::AudioState;
use crate::types::{AudioChainState, AudioSettings};

#[cfg(windows)]
use relay_audio::shm::SharedParams;

/// Backend for the installed endpoint APO.
#[derive(Debug, Default)]
pub struct ApoAudioControl {
    /// The endpoint the last `apply` un-bypassed, so `restore` bypasses it
    /// even when the default output has changed since.
    #[allow(dead_code)]
    applied: std::sync::Mutex<Option<String>>,
}

/// One render endpoint as the Settings APO card lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndpointApo {
    /// Endpoint GUID (`{...}`), the MMDevices key name.
    pub endpoint: String,
    /// Friendly name ("Speakers (Realtek)").
    pub name: String,
    /// The console default output.
    pub is_default: bool,
    /// Relay's CLSID is present in this endpoint's FX chain.
    pub installed: bool,
    /// An install backup for this endpoint is on disk.
    pub backed_up: bool,
    /// Its parameter section is reachable (the APO is running a stream).
    pub running: bool,
}

/// What the Settings opt-in card shows.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApoStatus {
    /// Relay's CLSID is present in the default render endpoint's FX chain.
    pub installed: bool,
    /// The default endpoint (`{guid}`), if there is one.
    pub endpoint: Option<String>,
    /// Default endpoint's parameter section reachable.
    pub running: bool,
    /// Every active render endpoint, plus any endpoint that has a backup but
    /// is no longer active (so an unplugged device can still be restored).
    #[serde(default)]
    pub endpoints: Vec<EndpointApo>,
}

// ---------------------------------------------------------------------------
// Pure helpers (tested without a registry).
// ---------------------------------------------------------------------------

/// The install backup of one endpoint.
pub fn backup_file(backup_dir: &Path, endpoint: &str) -> PathBuf {
    backup_dir.join(format!("{endpoint}.json"))
}

/// Every endpoint with a live install backup in `backup_dir`, sorted. Only
/// `<guid>.json` counts: `.json.restored` (kept after an uninstall) and
/// `.json.tmp` (a half-written backup) do not, and nor does a stem that is
/// not a GUID — a file named anything else cannot steer a restore.
pub fn recorded_endpoints(backup_dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(backup_dir) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .filter(|s| crate::elevate::vet_endpoint_guid(s).is_ok())
        .collect();
    out.sort();
    out
}

/// The backup to hand the registry restore. Every endpoint's backup records
/// the COM registration keys, and the restore deletes them — which would
/// unregister the class out from under every *other* endpoint still carrying
/// the APO. So while another endpoint remains installed, the COM keys stay.
pub fn restore_backup_for(
    backup: &relay_apo::fxstore::Backup,
    others_installed: bool,
) -> relay_apo::fxstore::Backup {
    let mut b = backup.clone();
    if others_installed {
        b.com_keys.clear();
    }
    b
}

/// The name of one endpoint's parameter section. Per endpoint by
/// construction: the APO instance on each endpoint creates its own.
pub fn params_section(endpoint: &str, instance: &str, local: bool) -> String {
    let name = relay_audio::shm::section_name(endpoint, instance);
    if local {
        // Test rig: section created in-session, not by audiodg.
        name.replacen("Global\\", "Local\\", 1)
    } else {
        name
    }
}

/// Does this FX chain text name Relay's CLSID?
fn names_relay(text: &str) -> bool {
    text.to_ascii_lowercase().contains(&relay_apo::ids::APO_CLSID.to_ascii_lowercase())
}

/// Merge the active render endpoints with the recorded backups into the
/// card's list. Pure: the probes are passed in.
pub fn merge_endpoints(
    active: &[(String, String, bool)],
    recorded: &[String],
    installed: impl Fn(&str) -> bool,
    running: impl Fn(&str) -> bool,
) -> Vec<EndpointApo> {
    let mut out: Vec<EndpointApo> = active
        .iter()
        .map(|(guid, name, is_default)| EndpointApo {
            endpoint: guid.clone(),
            name: name.clone(),
            is_default: *is_default,
            installed: installed(guid),
            backed_up: recorded.iter().any(|r| r.eq_ignore_ascii_case(guid)),
            running: running(guid),
        })
        .collect();
    for r in recorded {
        if !out.iter().any(|e| e.endpoint.eq_ignore_ascii_case(r)) {
            out.push(EndpointApo {
                endpoint: r.clone(),
                name: "Disconnected output".into(),
                is_default: false,
                installed: installed(r),
                backed_up: true,
                running: false,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Windows probes (read-only).
// ---------------------------------------------------------------------------

/// Active render endpoints as `(guid, name, is_default)`. Read-only.
#[cfg(windows)]
pub fn active_render_endpoints() -> Vec<(String, String, bool)> {
    crate::hardware::probe_win::list_audio_devices()
        .render
        .into_iter()
        .map(|d| (crate::hardware::fx_guid_of(&d.id), d.name, d.is_default))
        .filter(|(g, _, _)| !g.is_empty())
        .collect()
}

/// Read-only: does this endpoint's FX property store carry Relay's CLSID?
#[cfg(windows)]
pub fn fx_has_relay(guid: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ,
    };
    if crate::elevate::vet_endpoint_guid(guid).is_err() {
        return false;
    }
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
        if r.is_ok() && names_relay(&String::from_utf16_lossy(&buf[..(len as usize / 2)])) {
            return true;
        }
    }
    false
}

/// Read-only install probe over every render endpoint. Never writes.
#[cfg(windows)]
pub fn apo_status(backup_dir: &Path) -> ApoStatus {
    let active = active_render_endpoints();
    let recorded = recorded_endpoints(backup_dir);
    let endpoints =
        merge_endpoints(&active, &recorded, fx_has_relay, |g| open_endpoint(g).is_some());
    let default = endpoints.iter().find(|e| e.is_default);
    ApoStatus {
        installed: default.is_some_and(|e| e.installed),
        endpoint: default.map(|e| e.endpoint.clone()),
        running: default.is_some_and(|e| e.running),
        endpoints,
    }
}

#[cfg(not(windows))]
pub fn apo_status(_backup_dir: &Path) -> ApoStatus {
    ApoStatus { installed: false, endpoint: None, running: false, endpoints: Vec::new() }
}

/// The endpoint an op targets: the one named, or the default output.
#[cfg(windows)]
pub fn resolve_endpoint(endpoint: Option<&str>) -> Result<String> {
    use anyhow::Context;
    match endpoint {
        Some(e) => {
            crate::elevate::vet_endpoint_guid(e).map_err(anyhow::Error::msg)?;
            Ok(e.to_owned())
        }
        None => Ok(relay_audio::sessions::default_render_endpoint_guid()
            .context("no default render endpoint")?),
    }
}

/// Exactly what an install would write, read-only — the listing the user
/// reads *before* the UAC prompt. Reads the endpoint's live FX store and
/// plans against it, so the values named are the ones that would really
/// change on this machine rather than a generic description.
#[cfg(windows)]
pub fn install_dry_run(backup_dir: &Path, endpoint: Option<&str>) -> Vec<String> {
    let endpoint = match resolve_endpoint(endpoint) {
        Ok(e) => e,
        Err(e) => return vec![format!("No output to install on: {e:#}")],
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
        backup_file(backup_dir, &endpoint).display()
    ));
    lines.push(format!("file: {dll_text} (stays in place; only registered)"));
    lines
}

#[cfg(not(windows))]
pub fn install_dry_run(_backup_dir: &Path, _endpoint: Option<&str>) -> Vec<String> {
    vec!["The endpoint APO is Windows-only.".into()]
}

/// Register the APO on one render endpoint (the default when `None`).
/// Backup-then-apply, same contract as the `Applier`: the complete prior FX
/// property store lands in `<backup_dir>\<endpoint>.json` *before* the
/// registry changes.
///
/// Gated twice: `relay-apo`'s livereg refuses without
/// `RELAY_APO_ALLOW_LIVE_WRITE=1` (VM / installer only — this dev machine
/// never sets it), and this fn refuses to overwrite an existing backup (an
/// install over an install would lose the true original).
#[cfg(windows)]
pub fn install_live(backup_dir: &Path, endpoint: Option<&str>) -> Result<String> {
    use anyhow::{bail, Context};

    let endpoint = resolve_endpoint(endpoint)?;
    let backup_path = backup_file(backup_dir, &endpoint);
    if backup_path.exists() {
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
    let others = !recorded_endpoints(backup_dir).is_empty();

    std::fs::create_dir_all(backup_dir)?;
    let tmp = backup_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&plan.backup)?)?;
    std::fs::rename(&tmp, &backup_path).context("persisting the backup")?;

    if let Err(e) = relay_apo::livereg::LiveRegistry::apply_install(&plan) {
        // Roll the backup file back, or the next attempt refuses with "a
        // backup already exists — the APO looks installed" and the user is
        // stuck with a failure that reads like a success. Reconciling the
        // live tree to the backup first is what makes deleting it safe. The
        // COM keys stay when another endpoint still carries the APO.
        match relay_apo::livereg::LiveRegistry::restore(&restore_backup_for(&plan.backup, others)) {
            Ok(()) => {
                let _ = std::fs::remove_file(&backup_path);
                return Err(anyhow::Error::from(e)
                    .context("registering the APO; the endpoint was left exactly as it was"));
            }
            Err(restore_err) => {
                tracing::warn!(error = %restore_err, "could not roll back a failed APO install");
                return Err(anyhow::Error::from(e).context(format!(
                    "registering the APO, and the rollback also failed ({restore_err}); \
                     the prior state is kept in {}",
                    backup_path.display()
                )));
            }
        }
    }
    info!(%endpoint, "APO registered; endpoint streams pick it up on their next start");
    Ok(endpoint)
}

/// Restore one endpoint's FX property store from the backup taken at
/// install, byte-for-byte. Our COM registration is removed only with the
/// last installed endpoint. The backup file is kept with a `.restored`
/// suffix for the post-uninstall export diff.
#[cfg(windows)]
pub fn uninstall_live(backup_dir: &Path, endpoint: Option<&str>) -> Result<String> {
    use anyhow::Context;

    let endpoint = resolve_endpoint(endpoint)?;
    let backup_path = backup_file(backup_dir, &endpoint);
    let backup: relay_apo::fxstore::Backup = serde_json::from_slice(
        &std::fs::read(&backup_path)
            .with_context(|| format!("no backup for {endpoint}; nothing to uninstall"))?,
    )?;
    let others = recorded_endpoints(backup_dir).iter().any(|r| !r.eq_ignore_ascii_case(&endpoint));
    relay_apo::livereg::LiveRegistry::restore(&restore_backup_for(&backup, others))
        .context("restoring the FX property store (RELAY_APO_ALLOW_LIVE_WRITE gate)")?;
    let _ = std::fs::rename(&backup_path, backup_path.with_extension("json.restored"));
    info!(%endpoint, "FX property store restored to the pre-install state");
    Ok(endpoint)
}

/// Restore every endpoint with a recorded backup — the uninstaller's path.
/// Keeps going past a failure so one bad endpoint cannot strand the rest;
/// returns the restored endpoints, or the first error when any failed.
#[cfg(windows)]
pub fn uninstall_all_live(backup_dir: &Path) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let mut first_err = None;
    for ep in recorded_endpoints(backup_dir) {
        match uninstall_live(backup_dir, Some(&ep)) {
            Ok(e) => done.push(e),
            Err(e) => {
                tracing::warn!(endpoint = %ep, error = %e, "APO restore failed");
                first_err.get_or_insert(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(done),
    }
}

#[cfg(windows)]
fn open_endpoint(guid: &str) -> Option<SharedParams> {
    let instance = std::env::var("RELAY_INSTANCE").unwrap_or_default();
    let local = std::env::var("RELAY_APO_LOCAL_SECTION").is_ok();
    let name = params_section(guid, &instance, local);
    match SharedParams::open(&name) {
        Ok(s) => Some(s),
        Err(e) => {
            debug!(error = %e, section = %name, "APO parameter section not available");
            None
        }
    }
}

/// The endpoint the game's audio plays on: the default render endpoint.
#[cfg(windows)]
fn target_endpoint() -> Option<String> {
    match relay_audio::sessions::default_render_endpoint_guid() {
        Ok(g) => Some(g),
        Err(e) => {
            debug!(error = %e, "no default render endpoint for APO params");
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
        let bypass =
            target_endpoint().and_then(|g| open_endpoint(&g)).is_none_or(|s| s.block().bypass());
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
        let Some(guid) = target_endpoint() else { return Ok(AudioChainState::Bypass) };
        let Some(s) = open_endpoint(&guid) else {
            // APO not installed on this output / not running: nothing
            // applied, nothing to restore.
            return Ok(AudioChainState::Bypass);
        };
        s.block().write_params(&params);
        s.block().set_bypass(false);
        s.notify();
        if let Ok(mut a) = self.applied.lock() {
            *a = Some(guid.clone());
        }
        info!(endpoint = %guid, bands = params.bands.len(), hrtf = params.hrtf, "APO chain active");
        Ok(AudioChainState::Active)
    }

    fn restore(&self, _original: &AudioState) -> Result<()> {
        // Restore = bypass, regardless of what was captured, on the endpoint
        // the last apply wrote to and on the current default.
        let applied = self.applied.lock().ok().and_then(|mut a| a.take());
        let mut targets: Vec<String> = applied.into_iter().collect();
        if let Some(d) = target_endpoint() {
            if !targets.iter().any(|t| t.eq_ignore_ascii_case(&d)) {
                targets.push(d);
            }
        }
        for guid in targets {
            if let Some(s) = open_endpoint(&guid) {
                s.block().set_bypass(true);
                s.notify();
                info!(endpoint = %guid, "APO chain bypassed");
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "{11111111-2222-3333-4444-555555555555}";
    const B: &str = "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}";

    fn backup(ep: &str) -> relay_apo::fxstore::Backup {
        relay_apo::fxstore::plan_install(&relay_apo::fxstore::FxStore::empty(), ep, r"C:\x.dll")
            .backup
    }

    #[test]
    fn backups_are_named_per_endpoint() {
        let dir = Path::new(r"C:\d\Relay\apo-backup");
        assert_eq!(backup_file(dir, A), dir.join(format!("{A}.json")));
        assert_ne!(backup_file(dir, A), backup_file(dir, B));
    }

    #[test]
    fn recorded_endpoints_are_only_live_guid_backups() {
        let dir = std::env::temp_dir().join(format!("relay-s42-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            format!("{A}.json"),
            format!("{B}.json"),
            format!("{A}.json.restored"),
            format!("{B}.json.tmp"),
            "not-a-guid.json".into(),
            r"..json".into(),
        ] {
            std::fs::write(dir.join(f), b"{}").unwrap();
        }
        let mut want = vec![A.to_string(), B.to_string()];
        want.sort();
        assert_eq!(recorded_endpoints(&dir), want);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(recorded_endpoints(&dir).is_empty());
    }

    #[test]
    fn com_keys_survive_while_another_endpoint_is_installed() {
        let b = backup(A);
        assert!(!b.com_keys.is_empty());
        assert!(restore_backup_for(&b, true).com_keys.is_empty());
        assert_eq!(restore_backup_for(&b, true).store, b.store);
        assert_eq!(restore_backup_for(&b, false), b);
    }

    #[test]
    fn params_sections_are_per_endpoint() {
        assert_eq!(params_section(A, "", false), format!("Global\\Relay.APO.{A}"));
        assert_eq!(params_section(B, "", false), format!("Global\\Relay.APO.{B}"));
        assert_eq!(params_section(A, "t1", true), format!("Local\\Relay.APO.t1.{A}"));
        assert_ne!(params_section(A, "", false), params_section(B, "", false));
    }

    #[test]
    fn merge_lists_active_outputs_and_orphaned_backups() {
        let active = vec![(A.to_string(), "Headphones".to_string(), true)];
        let list = merge_endpoints(&active, &[A.into(), B.into()], |g| g == A, |_| false);
        assert_eq!(list.len(), 2);
        assert!(list[0].installed && list[0].backed_up && list[0].is_default);
        assert_eq!(list[1].endpoint, B);
        assert!(list[1].backed_up && !list[1].installed);
        assert_eq!(list[1].name, "Disconnected output");
    }

    #[test]
    fn status_wire_shape_keeps_old_fields_and_accepts_old_replies() {
        let s: ApoStatus =
            serde_json::from_str(r#"{"installed":false,"endpoint":null,"running":false}"#).unwrap();
        assert!(s.endpoints.is_empty());
        let v = serde_json::to_value(ApoStatus {
            installed: true,
            endpoint: Some(A.into()),
            running: false,
            endpoints: merge_endpoints(&[(A.into(), "X".into(), true)], &[], |_| true, |_| false),
        })
        .unwrap();
        assert_eq!(v["endpoints"][0]["endpoint"], A);
        assert_eq!(v["endpoints"][0]["backed_up"], false);
    }
}
