//! Virtual-device management in the core: consent, camera registration and
//! the interim mic route — the glue between `relay-vdevice` and the UI/CLI.
//!
//! Contract (brief + M5 plan):
//! - Nothing registers before the recorded consent says so.
//! - `installed.json` is written *before* any registry key is created
//!   (record-then-apply, same order as every other backup in the core), so
//!   uninstall always knows exactly what to remove.
//! - Registration itself is double-gated in `relay_vdevice::livereg`
//!   (`RELAY_VDEVICE_ALLOW_LIVE_WRITE=1` + elevation via HKLM ACLs).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Paths;
use relay_vdevice::installed::{self, Consent, InstalledFile, CAMERA_MEDIA_SOURCE};

/// What the consent screen, Settings card and `relay-core vdevice` show.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VdeviceStatus {
    /// Windows build number, and whether it carries the frame-server API.
    pub windows_build: Option<u32>,
    pub camera_supported: bool,
    /// Camera media source recorded (and expected) as registered.
    pub camera_registered: bool,
    /// OBS VirtualCam filter DLL, when its CLSID is registered (fallback).
    pub obs_virtualcam: Option<String>,
    /// VB-Cable / VoiceMeeter render endpoints usable as the interim mic.
    pub mic_targets: Vec<relay_vdevice::detect::MicTarget>,
    /// The recorded consent decision; `None` = the first-run screen is due.
    pub consent: Option<Consent>,
    /// This process can write HKLM (an install attempt would not be
    /// rejected by ACLs). Display-only.
    pub elevated: bool,
}

fn load(paths: &Paths) -> Result<InstalledFile> {
    installed::load(&paths.installed_file())
        .with_context(|| format!("reading {}", paths.installed_file().display()))
}

fn save(paths: &Paths, file: &InstalledFile) -> Result<()> {
    installed::save(&paths.installed_file(), file)
        .with_context(|| format!("writing {}", paths.installed_file().display()))
}

#[cfg(windows)]
pub fn status(paths: &Paths) -> Result<VdeviceStatus> {
    use relay_vdevice::detect;
    let file = load(paths)?;
    Ok(VdeviceStatus {
        windows_build: detect::windows_build(),
        camera_supported: detect::frameserver_supported(),
        camera_registered: file.component(CAMERA_MEDIA_SOURCE).is_some(),
        obs_virtualcam: detect::obs_virtualcam(),
        mic_targets: detect::mic_targets().unwrap_or_default(),
        consent: file.consent,
        elevated: crate::processes::is_elevated(),
    })
}

#[cfg(not(windows))]
pub fn status(paths: &Paths) -> Result<VdeviceStatus> {
    let file = load(paths)?;
    Ok(VdeviceStatus {
        windows_build: None,
        camera_supported: false,
        camera_registered: file.component(CAMERA_MEDIA_SOURCE).is_some(),
        obs_virtualcam: None,
        mic_targets: Vec::new(),
        consent: file.consent,
        elevated: false,
    })
}

/// Record the first-run decision. Never installs anything by itself.
pub fn set_consent(paths: &Paths, apo: bool, camera: bool, microphone: bool) -> Result<Consent> {
    let mut file = load(paths)?;
    let consent = Consent { decided_at: installed::iso_now(), apo, camera, microphone };
    file.consent = Some(consent.clone());
    save(paths, &file)?;
    Ok(consent)
}

/// The dry-run listing the consent screen shows: exactly what an install
/// would create, before anything is created.
pub fn camera_dry_run() -> Vec<String> {
    let dll = camera_dll_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| format!("<install dir>\\{CAMERA_DLL} (not built yet)"));
    let plan = relay_vdevice::reg::plan_camera_install(&dll);
    let mut lines: Vec<String> = plan.keys.iter().map(|k| format!(r"HKLM\{}", k.path)).collect();
    lines.push(format!("file: {dll} (stays in place; only registered)"));
    lines
}

/// The registry keys a camera install would create, derived here rather
/// than taken from anyone's input — the elevated helper vets these against
/// our own CLSID before it arms the live-write gate.
pub fn camera_plan_keys() -> Vec<String> {
    let dll = camera_dll_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    relay_vdevice::reg::plan_camera_install(&dll).keys.into_iter().map(|k| k.path).collect()
}

/// The keys `installed.json` says we created, deepest first. Empty when
/// nothing is recorded, which is how the helper tells "nothing to do" from
/// "something to remove".
pub fn recorded_camera_keys(paths: &Paths) -> Vec<String> {
    load(paths)
        .ok()
        .and_then(|f| f.component(CAMERA_MEDIA_SOURCE).cloned())
        .map(|c| relay_vdevice::reg::plan_camera_uninstall(&c))
        .unwrap_or_default()
}

const CAMERA_DLL: &str = "relay_vdevice.dll";

fn camera_dll_path() -> Result<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(CAMERA_DLL)))
        .filter(|p| p.exists())
        .with_context(|| format!("{CAMERA_DLL} not found next to the running binary"))
}

/// Let the Frame Server read the camera DLL. It runs as LOCAL SERVICE
/// (S-1-5-19) and loads the media source from where Relay is installed --
/// `%LOCALAPPDATA%\Relay`, whose ACL admits only the user, SYSTEM and
/// Administrators. Without this `IMFVirtualCamera::Start` fails with
/// E_ACCESSDENIED and Relay Camera never appears (found in the 2026-09-29
/// live pass). Read + execute on this one file only; nothing else in the
/// folder is exposed. The installer does the same after every update, since
/// a replaced file inherits the folder ACL again.
#[cfg(windows)]
pub fn grant_frameserver_read(dll: &std::path::Path) {
    let sys = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| r"C:\Windows".into());
    let out = std::process::Command::new(sys.join("System32").join("icacls.exe"))
        .arg(dll)
        .args(["/grant", "*S-1-5-19:(RX)"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            tracing::info!(dll = %dll.display(), "Frame Server may read the camera DLL")
        }
        Ok(o) => {
            tracing::warn!(status = ?o.status, "could not grant the Frame Server read access; Relay Camera will not start")
        }
        Err(e) => tracing::warn!(error = %e, "could not run icacls; Relay Camera will not start"),
    }
}

/// Register the camera media source. Consent-checked, record-then-apply,
/// idempotent refusal when already recorded.
#[cfg(windows)]
pub fn install_camera_live(paths: &Paths) -> Result<()> {
    let mut file = load(paths)?;
    if !file.consent.as_ref().is_some_and(|c| c.camera) {
        bail!("no recorded consent for the virtual camera — opt in first");
    }
    if file.component(CAMERA_MEDIA_SOURCE).is_some() {
        bail!("the camera media source is already recorded as registered; uninstall first");
    }
    let dll = camera_dll_path()?;
    let plan = relay_vdevice::reg::plan_camera_install(&dll.to_string_lossy());

    // Record first: if the registry write dies half-way, uninstall still
    // knows every key the plan would have created.
    file.record(plan.record.clone());
    save(paths, &file)?;

    match relay_vdevice::livereg::apply(&plan.keys) {
        Ok(()) => {
            tracing::info!(dll = %dll.display(), "camera media source registered");
            grant_frameserver_read(&dll);
            Ok(())
        }
        Err(e) => {
            // Roll the record back only for the pure refusals; a half-applied
            // write keeps the record so uninstall can clean up.
            if matches!(e, relay_vdevice::livereg::LiveRegError::WritesDisabled) {
                file.remove(CAMERA_MEDIA_SOURCE);
                save(paths, &file)?;
            }
            Err(e).context("registering the camera media source (RELAY_VDEVICE_ALLOW_LIVE_WRITE gate + elevation)")
        }
    }
}

/// Remove the recorded registration; on success the component leaves
/// `installed.json` (DoD: after a full opt-out the list is empty).
#[cfg(windows)]
pub fn uninstall_camera_live(paths: &Paths) -> Result<()> {
    let mut file = load(paths)?;
    let Some(record) = file.component(CAMERA_MEDIA_SOURCE).cloned() else {
        bail!("no camera registration recorded; nothing to uninstall");
    };
    let keys = relay_vdevice::reg::plan_camera_uninstall(&record);
    relay_vdevice::livereg::remove(&keys).context(
        "removing the camera media source keys (RELAY_VDEVICE_ALLOW_LIVE_WRITE gate + elevation)",
    )?;
    file.remove(CAMERA_MEDIA_SOURCE);
    save(paths, &file)?;
    tracing::info!("camera media source unregistered");
    Ok(())
}

/// What the service passes to `relay-share recv`: camera on when consented
/// and registered; mic route to the first VB-Cable (preferred) or
/// VoiceMeeter input when the microphone opt-in is on.
#[cfg(windows)]
pub fn receive_routing(paths: &Paths) -> (bool, Option<String>) {
    let Ok(file) = load(paths) else { return (false, None) };
    let camera = file.consent.as_ref().is_some_and(|c| c.camera)
        && file.component(CAMERA_MEDIA_SOURCE).is_some()
        && relay_vdevice::detect::frameserver_supported();
    let mic = if file.consent.as_ref().is_some_and(|c| c.microphone) {
        let mut targets = relay_vdevice::detect::mic_targets().unwrap_or_default();
        targets.sort_by_key(|t| t.kind != relay_vdevice::detect::MicTargetKind::VbCable);
        targets.into_iter().next().map(|t| t.endpoint_id)
    } else {
        None
    };
    (camera, mic)
}

#[cfg(not(windows))]
pub fn receive_routing(_paths: &Paths) -> (bool, Option<String>) {
    (false, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(tag: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("relay-vdev-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Paths::at(dir)
    }

    #[test]
    fn consent_round_trip_and_status() {
        let paths = temp_paths("consent");
        let s = status(&paths).expect("status");
        assert!(s.consent.is_none(), "fresh install: consent not decided");
        assert!(!s.camera_registered);

        set_consent(&paths, false, true, false).expect("consent");
        let s = status(&paths).expect("status 2");
        let c = s.consent.expect("recorded");
        assert!(c.camera && !c.microphone);
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[cfg(windows)]
    #[test]
    fn install_refuses_without_consent_and_gate_never_reached() {
        let paths = temp_paths("gate");
        // No consent → refused before any planner/registry work.
        let err = install_camera_live(&paths).unwrap_err();
        assert!(err.to_string().contains("consent"), "{err}");

        // Consent but no DLL next to the test binary → refused before the
        // registry too, and nothing is recorded.
        set_consent(&paths, false, true, false).unwrap();
        let _ = install_camera_live(&paths).unwrap_err();
        let file = installed::load(&paths.installed_file()).unwrap();
        assert!(file.components.is_empty(), "no record without an applied install");
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[test]
    fn dry_run_names_both_keys_and_the_dll() {
        let lines = camera_dry_run();
        assert!(lines.iter().any(|l| l.contains("InprocServer32")));
        assert!(lines.iter().any(|l| l.contains(relay_vdevice::reg::VCAM_CLSID)));
        assert!(lines.iter().any(|l| l.contains("relay_vdevice.dll")));
    }
}
