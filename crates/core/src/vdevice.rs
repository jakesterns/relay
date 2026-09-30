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
use relay_vdevice::installed::{
    self, Consent, InstalledFile, CAMERA_DSHOW_FILTER, CAMERA_MEDIA_SOURCE,
};

/// How Relay Camera is provided (re-exported for the IPC mirror).
pub use relay_vdevice::reg::CameraPath;

/// What the consent screen, Settings card and `relay-core vdevice` show.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VdeviceStatus {
    /// Windows build number.
    pub windows_build: Option<u32>,
    /// Relay Camera can exist on this PC — through either path (S43: the
    /// DirectShow filter makes that true on Windows 10 too).
    pub camera_supported: bool,
    /// Which path this PC uses: "frame_server" (Windows 11 22H2+, HKLM,
    /// elevated) or "direct_show" (older, per-user, no elevation). None`n    /// off Windows.
    #[serde(default)]
    pub camera_path: Option<CameraPath>,
    /// The component for this PC's path is recorded (and expected) as
    /// registered.
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
    let path = detect::camera_path();
    Ok(VdeviceStatus {
        windows_build: detect::windows_build(),
        camera_supported: true,
        camera_path: Some(path),
        camera_registered: file.component(component_for(path)).is_some(),
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
        camera_path: None,
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
    #[cfg(windows)]
    let path = relay_vdevice::detect::camera_path();
    #[cfg(not(windows))]
    let path = CameraPath::FrameServer;
    camera_dry_run_for(path)
}

/// The dry-run listing for one path (pure apart from locating the DLL).
pub fn camera_dry_run_for(path: CameraPath) -> Vec<String> {
    let dll = camera_dll_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| format!("<install dir>\\{CAMERA_DLL} (not built yet)"));
    let (hive, plan) = match path {
        CameraPath::FrameServer => ("HKLM", relay_vdevice::reg::plan_camera_install(&dll)),
        CameraPath::DirectShow => ("HKCU", relay_vdevice::reg::plan_dshow_install(&dll)),
    };
    let mut lines: Vec<String> = plan.keys.iter().map(|k| format!(r"{hive}\{}", k.path)).collect();
    lines.push(format!("file: {dll} (stays in place; only registered)"));
    lines
}

/// The `installed.json` component id for a camera path.
pub fn component_for(path: CameraPath) -> &'static str {
    match path {
        CameraPath::FrameServer => CAMERA_MEDIA_SOURCE,
        CameraPath::DirectShow => CAMERA_DSHOW_FILTER,
    }
}

/// The HKCU keys `installed.json` says the DirectShow filter created,
/// deepest first and vetted; empty when nothing (valid) is recorded.
pub fn recorded_dshow_keys(paths: &Paths) -> Vec<String> {
    load(paths)
        .ok()
        .and_then(|f| f.component(CAMERA_DSHOW_FILTER).cloned())
        .and_then(|c| relay_vdevice::reg::plan_dshow_uninstall(&c).ok())
        .unwrap_or_default()
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

/// Register the DirectShow camera filter for this user (the Windows 10
/// path, S43). Consent-checked, vetted, record-then-apply like the media
/// source — but HKCU only, so no elevation and no helper. Live writes stay
/// behind `RELAY_VDEVICE_ALLOW_LIVE_WRITE=1`.
#[cfg(windows)]
pub fn install_dshow_live(paths: &Paths) -> Result<()> {
    let mut file = load(paths)?;
    if !file.consent.as_ref().is_some_and(|c| c.camera) {
        bail!("no recorded consent for the virtual camera — opt in first");
    }
    if file.component(CAMERA_DSHOW_FILTER).is_some() {
        bail!("the camera filter is already recorded as registered; uninstall first");
    }
    let dll = camera_dll_path()?;
    let plan = relay_vdevice::reg::plan_dshow_install(&dll.to_string_lossy());
    relay_vdevice::reg::vet_dshow_keys(&plan.record.hkcu_keys).map_err(anyhow::Error::msg)?;

    file.record(plan.record.clone());
    save(paths, &file)?;
    match relay_vdevice::livereg::apply_user(&plan.keys) {
        Ok(()) => {
            tracing::info!(dll = %dll.display(), "camera filter registered for this user");
            Ok(())
        }
        Err(e) => {
            if matches!(e, relay_vdevice::livereg::LiveRegError::WritesDisabled) {
                file.remove(CAMERA_DSHOW_FILTER);
                save(paths, &file)?;
            }
            Err(e).context("registering the camera filter (RELAY_VDEVICE_ALLOW_LIVE_WRITE gate)")
        }
    }
}

/// Remove the recorded per-user filter registration — exactly the recorded
/// keys, after vetting them against the three the filter may own.
#[cfg(windows)]
pub fn uninstall_dshow_live(paths: &Paths) -> Result<()> {
    let mut file = load(paths)?;
    let Some(record) = file.component(CAMERA_DSHOW_FILTER).cloned() else {
        bail!("no camera filter registration recorded; nothing to uninstall");
    };
    let keys = relay_vdevice::reg::plan_dshow_uninstall(&record).map_err(anyhow::Error::msg)?;
    relay_vdevice::livereg::remove_user(&keys)
        .context("removing the camera filter keys (RELAY_VDEVICE_ALLOW_LIVE_WRITE gate)")?;
    file.remove(CAMERA_DSHOW_FILTER);
    save(paths, &file)?;
    tracing::info!("camera filter unregistered");
    Ok(())
}

/// Install Relay Camera by this PC's path: the per-user filter directly on
/// Windows 10; the media source (HKLM — in practice via the elevated
/// helper) on Windows 11 22H2+.
#[cfg(windows)]
pub fn install_vcam(paths: &Paths) -> Result<()> {
    match relay_vdevice::detect::camera_path() {
        CameraPath::DirectShow => install_dshow_live(paths),
        CameraPath::FrameServer => install_camera_live(paths),
    }
}

/// Remove whatever camera registration is recorded, whichever path made it
/// (a PC upgraded from 10 to 11 may carry the filter record).
#[cfg(windows)]
pub fn uninstall_vcam(paths: &Paths) -> Result<()> {
    let file = load(paths)?;
    let (fs, ds) = (
        file.component(CAMERA_MEDIA_SOURCE).is_some(),
        file.component(CAMERA_DSHOW_FILTER).is_some(),
    );
    if !fs && !ds {
        bail!("no camera registration recorded; nothing to uninstall");
    }
    if ds {
        uninstall_dshow_live(paths)?;
    }
    if fs {
        uninstall_camera_live(paths)?;
    }
    Ok(())
}

/// What the service passes to `relay-share recv`: camera on when consented
/// and registered; mic route to the first VB-Cable (preferred) or
/// VoiceMeeter input when the microphone opt-in is on.
#[cfg(windows)]
pub fn receive_routing(paths: &Paths) -> (bool, Option<String>) {
    let Ok(file) = load(paths) else { return (false, None) };
    // relay-share picks the same path itself (frame server where the API
    // exists, the DirectShow ring otherwise); here it is only "registered".
    let camera = file.consent.as_ref().is_some_and(|c| c.camera)
        && file.component(component_for(relay_vdevice::detect::camera_path())).is_some();
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
    fn dshow_dry_run_is_hkcu_only() {
        let lines = camera_dry_run_for(CameraPath::DirectShow);
        let keys: Vec<_> = lines.iter().filter(|l| !l.starts_with("file:")).collect();
        assert_eq!(keys.len(), 3);
        assert!(keys.iter().all(|l| l.starts_with(r"HKCU\Software\Classes\CLSID\")), "{keys:?}");
        assert!(keys.iter().any(|l| l.contains(relay_vdevice::reg::VIDEO_INPUT_CATEGORY)));
        assert!(camera_dry_run_for(CameraPath::FrameServer)
            .iter()
            .filter(|l| !l.starts_with("file:"))
            .all(|l| l.starts_with(r"HKLM\")));
    }

    #[test]
    fn component_follows_the_path() {
        assert_eq!(component_for(CameraPath::FrameServer), CAMERA_MEDIA_SOURCE);
        assert_eq!(component_for(CameraPath::DirectShow), CAMERA_DSHOW_FILTER);
        assert_eq!(CameraPath::for_support(false), CameraPath::DirectShow);
        assert_eq!(CameraPath::for_support(true), CameraPath::FrameServer);
        assert_eq!(serde_json::to_string(&CameraPath::DirectShow).unwrap(), "\"direct_show\"");
    }

    #[cfg(windows)]
    #[test]
    fn dshow_install_refuses_without_consent_and_records_nothing() {
        let paths = temp_paths("dsgate");
        let err = install_dshow_live(&paths).unwrap_err();
        assert!(err.to_string().contains("consent"), "{err}");
        // Consent, but no DLL beside the test binary: refused before the
        // registry, nothing recorded, the gate never reached.
        set_consent(&paths, false, true, false).unwrap();
        let _ = install_dshow_live(&paths).unwrap_err();
        let file = installed::load(&paths.installed_file()).unwrap();
        assert!(file.components.is_empty());
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[cfg(windows)]
    #[test]
    fn dshow_uninstall_refuses_a_tampered_record_before_the_registry() {
        let paths = temp_paths("dstamper");
        let mut file = InstalledFile::default();
        let mut rec = relay_vdevice::reg::plan_dshow_install("x.dll").record;
        rec.hkcu_keys.push(r"Software\Microsoft\Windows\CurrentVersion\Run".into());
        file.record(rec);
        installed::save(&paths.installed_file(), &file).unwrap();
        let err = uninstall_dshow_live(&paths).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert!(recorded_dshow_keys(&paths).is_empty(), "a tampered record lists nothing");
        // The record stays, so nothing is silently forgotten.
        assert!(load(&paths).unwrap().component(CAMERA_DSHOW_FILTER).is_some());
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[cfg(windows)]
    #[test]
    fn dshow_uninstall_with_a_good_record_stops_at_the_gate() {
        // The gate env var is never set in tests: the vetted delete is
        // refused by livereg and the record is kept for a later retry.
        let paths = temp_paths("dsgood");
        let mut file = InstalledFile::default();
        file.record(relay_vdevice::reg::plan_dshow_install("x.dll").record);
        installed::save(&paths.installed_file(), &file).unwrap();
        assert_eq!(recorded_dshow_keys(&paths).len(), 3);
        let err = uninstall_vcam(&paths).unwrap_err();
        assert!(format!("{err:#}").contains("RELAY_VDEVICE_ALLOW_LIVE_WRITE"), "{err:#}");
        assert!(load(&paths).unwrap().component(CAMERA_DSHOW_FILTER).is_some());
        let _ = std::fs::remove_dir_all(paths.root());
    }

    #[test]
    fn dry_run_names_both_keys_and_the_dll() {
        let lines = camera_dry_run_for(CameraPath::FrameServer);
        assert!(lines.iter().any(|l| l.contains("InprocServer32")));
        assert!(lines.iter().any(|l| l.contains(relay_vdevice::reg::VCAM_CLSID)));
        assert!(lines.iter().any(|l| l.contains("relay_vdevice.dll")));
    }
}
