//! Bundled VB-CABLE: the virtual microphone, installed by Relay so the
//! receiving PC needs no second download (owner, 2026-10-12: "everything
//! should be bundled into this software by default").
//!
//! **Pending VB-Audio's permission.** VB-CABLE is donationware by VB-Audio
//! Software; Relay may only redistribute it with their written agreement
//! (request drafted 2026-10-21). Until then [`MANIFEST`] is empty, no build
//! carries the payload, and every path here reports "not bundled in this
//! build" and does nothing. The plumbing is complete so the day permission
//! arrives the work is: drop the unmodified pack into the bundle, pin its
//! files in [`MANIFEST`], and run the first live install on PC2
//! (`docs/dev/vbcable-bundle.md`).
//!
//! # The rules
//!
//! - **Opt-in only.** Nothing installs without the recorded
//!   `Consent::vbcable` ("Install the virtual microphone (VB-CABLE by
//!   VB-Audio)"), off by default.
//! - **Already there = use it.** A machine that has VB-CABLE gets nothing
//!   installed and nothing recorded; the receive route simply uses it.
//! - **Pinned bytes only.** The helper runs only files named in [`MANIFEST`],
//!   whose SHA-256 it checks on bytes it holds in memory, then writes those
//!   bytes into an administrators-only staging folder under Program Files and
//!   runs them from there. The payload beside Relay sits in the user's
//!   `%LOCALAPPDATA%`, so running it in place would let anything running as
//!   the user swap the file between the check and the elevated launch.
//! - **Record, then apply.** `installed.json` names the component *before*
//!   the installer runs, and loses it again if the device never appears.
//! - **Remove only what Relay added.** The uninstaller removes VB-CABLE only
//!   when that record says Relay installed it; a VB-CABLE the user installed
//!   themselves is never touched.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where the pack sits inside the install directory.
pub const PAYLOAD_DIR: &str = r"drivers\vbcable";
/// VB-Audio's setup program inside the pack.
pub const SETUP_EXE: &str = "VBCABLE_Setup_x64.exe";
/// Silent install / uninstall. VB-Audio documents `-i` / `-u` with `-h`
/// for "hidden"; **to be verified on the first live run (PC2)** — keep the
/// command lines here and nowhere else.
pub const INSTALL_ARGS: &[&str] = &["-i", "-h"];
pub const UNINSTALL_ARGS: &[&str] = &["-u", "-h"];
/// Live gate on actually launching the setup program. Only the elevated
/// helper (and the elevated uninstaller) arms it, around one call.
pub const LIVE_RUN_GATE: &str = "RELAY_VBCABLE_ALLOW_LIVE_RUN";
/// Credit line and link the consent row shows.
pub const CREDIT: &str = "VB-CABLE by VB-Audio Software — donationware";
pub const CREDIT_URL: &str = "https://vb-cable.com";
/// How long the setup program may run before it is stopped.
pub const SETUP_TIMEOUT_SECS: u64 = 180;
/// How long to wait for the new device to appear afterwards.
pub const APPEAR_TIMEOUT_SECS: u64 = 15;

/// One pinned file of the pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadFile {
    /// Plain file name (no directories).
    pub name: &'static str,
    /// Lower-case hex SHA-256.
    pub sha256: &'static str,
}

/// The pinned pack. **Empty until VB-Audio grants permission**: an empty
/// manifest means "not bundled", whatever files happen to be on disk, so an
/// unpinned binary is never run.
pub const MANIFEST: &[PayloadFile] = &[];

/// What the payload directory holds, judged against a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Payload {
    /// This build carries no (pinned) VB-CABLE.
    NotBundled,
    /// Every pinned file is present with its pinned hash.
    Ready,
    /// A pinned file is missing (a partly copied install).
    Incomplete { missing: String },
    /// A pinned file's bytes differ from the pin. Never run.
    Mismatch { file: String },
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A manifest name must be a plain file name: nothing that could climb out
/// of the payload or staging folder.
pub fn vet_name(name: &str) -> Result<(), String> {
    let bad = name.is_empty()
        || name.contains(['\\', '/', ':'])
        || name == "."
        || name.contains("..")
        || name.trim() != name;
    if bad {
        Err(format!("{name:?} is not a plain file name"))
    } else {
        Ok(())
    }
}

/// Read every pinned file into memory and check it. `Ok` carries the
/// verified bytes, in manifest order; `Err` the state that refuses them.
pub fn read_verified(
    dir: &Path,
    manifest: &[PayloadFile],
) -> std::result::Result<Vec<(String, Vec<u8>)>, Payload> {
    if manifest.is_empty() || !manifest.iter().any(|f| f.name == SETUP_EXE) || !dir.is_dir() {
        return Err(Payload::NotBundled);
    }
    let mut out = Vec::with_capacity(manifest.len());
    for f in manifest {
        if vet_name(f.name).is_err() {
            return Err(Payload::Mismatch { file: f.name.into() });
        }
        let Ok(bytes) = std::fs::read(dir.join(f.name)) else {
            return Err(Payload::Incomplete { missing: f.name.into() });
        };
        if !sha256_hex(&bytes).eq_ignore_ascii_case(f.sha256) {
            return Err(Payload::Mismatch { file: f.name.into() });
        }
        out.push((f.name.to_string(), bytes));
    }
    Ok(out)
}

/// The payload's state, for status and the dry run.
pub fn check_payload(dir: &Path, manifest: &[PayloadFile]) -> Payload {
    match read_verified(dir, manifest) {
        Ok(_) => Payload::Ready,
        Err(p) => p,
    }
}

/// The pack's folder beside a binary (the install directory).
pub fn payload_dir_beside(exe_dir: &Path) -> PathBuf {
    exe_dir.join(PAYLOAD_DIR)
}

/// The administrators-only folder the verified bytes run from:
/// `%ProgramFiles%\Relay\drivers\vbcable` (the native one, never the x86
/// redirect). Program Files is writable by administrators only, which is
/// what closes the check-then-run window.
pub fn staging_dir() -> PathBuf {
    std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("Relay")
        .join(PAYLOAD_DIR)
}

/// Write verified bytes into `staging` (emptied first) and return the setup
/// program's path there.
pub fn stage(files: &[(String, Vec<u8>)], staging: &Path) -> Result<PathBuf> {
    if staging.exists() {
        std::fs::remove_dir_all(staging)
            .with_context(|| format!("clearing {}", staging.display()))?;
    }
    std::fs::create_dir_all(staging).with_context(|| format!("creating {}", staging.display()))?;
    for (name, bytes) in files {
        vet_name(name).map_err(anyhow::Error::msg)?;
        std::fs::write(staging.join(name), bytes)
            .with_context(|| format!("staging {name} into {}", staging.display()))?;
    }
    Ok(staging.join(SETUP_EXE))
}

/// What an install request should do. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallDecision {
    /// Run the pinned setup.
    Install,
    /// VB-CABLE is already on this PC: use it, install nothing.
    AlreadyPresent,
    /// No (pinned) pack in this build.
    NotBundled,
    /// The user has not opted in.
    NoConsent,
    /// The pack is damaged or altered.
    Refused(String),
}

pub fn decide_install(consent: bool, present: bool, payload: &Payload) -> InstallDecision {
    if !consent {
        return InstallDecision::NoConsent;
    }
    if present {
        return InstallDecision::AlreadyPresent;
    }
    match payload {
        Payload::NotBundled => InstallDecision::NotBundled,
        Payload::Ready => InstallDecision::Install,
        Payload::Incomplete { missing } => InstallDecision::Refused(format!(
            "the bundled VB-CABLE is incomplete ({missing} is missing)"
        )),
        Payload::Mismatch { file } => InstallDecision::Refused(format!(
            "{file} does not match the pinned VB-CABLE package — refusing to run it"
        )),
    }
}

/// Should an uninstall remove VB-CABLE? Only when Relay's record says Relay
/// installed it. Pure.
pub fn decide_uninstall(recorded: bool) -> bool {
    recorded
}

/// The `installed.json` record for a Relay-made install.
pub fn component(setup: &Path) -> relay_vdevice::installed::Component {
    relay_vdevice::installed::Component {
        id: relay_vdevice::installed::VBCABLE_DRIVER.into(),
        installed_at: relay_vdevice::installed::iso_now(),
        dll_path: setup.display().to_string(),
        hklm_keys: Vec::new(),
        hkcu_keys: Vec::new(),
        hkcu_created_parents: Vec::new(),
    }
}

/// What the Settings card shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The pack this build carries.
    pub payload: Payload,
    /// A VB-CABLE input endpoint exists right now.
    pub present: bool,
    /// Relay installed it (and the uninstaller will remove it).
    pub installed_by_relay: bool,
    /// The recorded opt-in.
    pub consent: bool,
    pub credit: String,
    pub credit_url: String,
}

/// The dry-run lines for the consent / elevation panel.
pub fn plan_lines(payload: &Payload, present: bool, staging: &Path) -> Vec<String> {
    if present {
        return vec!["VB-CABLE is already installed — Relay uses it and installs nothing.".into()];
    }
    match payload {
        Payload::NotBundled => {
            vec!["VB-CABLE is not bundled in this build — nothing would be installed.".into()]
        }
        Payload::Incomplete { .. } | Payload::Mismatch { .. } => {
            vec![format!(
                "Refused: {}",
                match decide_install(true, false, payload) {
                    InstallDecision::Refused(r) => r,
                    _ => String::new(),
                }
            )]
        }
        Payload::Ready => {
            let mut l: Vec<String> = MANIFEST
                .iter()
                .map(|f| format!("copy {} → {}", f.name, staging.display()))
                .collect();
            l.push(format!(
                "run {} {} (VB-Audio's own silent installer; adds \"CABLE Input\" / \"CABLE Output\")",
                staging.join(SETUP_EXE).display(),
                INSTALL_ARGS.join(" ")
            ));
            l.push("record: %LOCALAPPDATA%\\Relay\\installed.json (vbcable-driver), so the uninstaller removes it".into());
            l
        }
    }
}

/// What a live install or uninstall did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveOutcome {
    Done(String),
    Skipped(String),
    Refused(String),
    Failed(String),
}

/// The machine-facing parts, injected so the flows are tested without a
/// driver, a token or a UAC prompt.
pub trait Machine {
    /// A VB-CABLE input endpoint exists.
    fn present(&self) -> bool;
    /// Run `exe` with `args`; the exit code.
    fn run(&mut self, exe: &Path, args: &[&str]) -> Result<i32>;
    /// Wait up to `secs` for [`Machine::present`] to become `want`.
    fn wait_present(&self, want: bool, secs: u64) -> bool {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if self.present() == want {
                return true;
            }
            if std::time::Instant::now() >= end {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

/// Install flow (helper side). `installed_file` is `installed.json`.
pub fn install_with(
    installed_file: &Path,
    payload_dir: &Path,
    staging: &Path,
    manifest: &[PayloadFile],
    m: &mut dyn Machine,
) -> LiveOutcome {
    use relay_vdevice::installed;
    let mut file = match installed::load(installed_file) {
        Ok(f) => f,
        Err(e) => return LiveOutcome::Failed(format!("reading installed.json: {e}")),
    };
    let consent = file.consent.as_ref().is_some_and(|c| c.vbcable);
    let present = m.present();
    let verified = read_verified(payload_dir, manifest);
    let payload = match &verified {
        Ok(_) => Payload::Ready,
        Err(p) => p.clone(),
    };
    match decide_install(consent, present, &payload) {
        InstallDecision::NoConsent => {
            return LiveOutcome::Refused(
                "no recorded opt-in for the virtual microphone — opt in first".into(),
            )
        }
        InstallDecision::AlreadyPresent => {
            return LiveOutcome::Skipped(
                "VB-CABLE is already installed — Relay uses it and installs nothing".into(),
            )
        }
        InstallDecision::NotBundled => {
            return LiveOutcome::Skipped("VB-CABLE is not bundled in this build".into())
        }
        InstallDecision::Refused(r) => return LiveOutcome::Refused(r),
        InstallDecision::Install => {}
    }
    let Ok(files) = verified else { unreachable!("Install implies verified bytes") };
    let setup = match stage(&files, staging) {
        Ok(p) => p,
        Err(e) => return LiveOutcome::Failed(format!("{e:#}")),
    };
    // Record first: a setup that dies half-way still leaves the uninstaller
    // knowing Relay started it.
    file.record(component(&setup));
    if let Err(e) = installed::save(installed_file, &file) {
        let _ = std::fs::remove_dir_all(staging);
        return LiveOutcome::Failed(format!("writing installed.json: {e}"));
    }
    let code = m.run(&setup, INSTALL_ARGS);
    let appeared = m.wait_present(true, APPEAR_TIMEOUT_SECS);
    if appeared {
        return LiveOutcome::Done(match code {
            Ok(0) => "VB-CABLE installed (\"CABLE Input\" / \"CABLE Output\")".into(),
            Ok(c) => format!("VB-CABLE installed (setup exit code {c})"),
            Err(e) => format!("VB-CABLE installed ({e:#} while waiting for setup)"),
        });
    }
    // Nothing appeared: forget the record so the uninstaller never claims a
    // device Relay did not add, and drop the staged copy.
    file.remove(relay_vdevice::installed::VBCABLE_DRIVER);
    let _ = installed::save(installed_file, &file);
    let _ = std::fs::remove_dir_all(staging);
    LiveOutcome::Failed(match code {
        Ok(c) => format!("VB-CABLE setup exited with {c} and no CABLE device appeared"),
        Err(e) => format!("{e:#}"),
    })
}

/// Uninstall flow: only what Relay's record says Relay installed.
pub fn uninstall_with(
    installed_file: &Path,
    payload_dir: &Path,
    staging: &Path,
    manifest: &[PayloadFile],
    m: &mut dyn Machine,
) -> LiveOutcome {
    use relay_vdevice::installed;
    let mut file = match installed::load(installed_file) {
        Ok(f) => f,
        Err(e) => return LiveOutcome::Failed(format!("reading installed.json: {e}")),
    };
    let recorded = file.component(installed::VBCABLE_DRIVER).is_some();
    if !decide_uninstall(recorded) {
        return LiveOutcome::Skipped(
            "Relay did not install VB-CABLE — it is left exactly as it is".into(),
        );
    }
    // Run the uninstaller from verified bytes: the staged copy when it still
    // checks out, otherwise a fresh stage from the bundled pack.
    let files = read_verified(staging, manifest).or_else(|_| read_verified(payload_dir, manifest));
    let Ok(files) = files else {
        return LiveOutcome::Failed(
            "VB-CABLE's installer files are missing or altered — remove \"VB-Audio Virtual Cable\" from Windows Settings → Apps".into(),
        );
    };
    let setup = match stage(&files, staging) {
        Ok(p) => p,
        Err(e) => return LiveOutcome::Failed(format!("{e:#}")),
    };
    match m.run(&setup, UNINSTALL_ARGS) {
        Ok(0) => {}
        Ok(c) => return LiveOutcome::Failed(format!("VB-CABLE uninstall exited with {c}")),
        Err(e) => return LiveOutcome::Failed(format!("{e:#}")),
    }
    file.remove(installed::VBCABLE_DRIVER);
    if let Err(e) = installed::save(installed_file, &file) {
        return LiveOutcome::Failed(format!("writing installed.json: {e}"));
    }
    let _ = std::fs::remove_dir_all(staging);
    let gone = m.wait_present(false, APPEAR_TIMEOUT_SECS);
    LiveOutcome::Done(if gone {
        "VB-CABLE removed".into()
    } else {
        "VB-CABLE uninstalled — Windows finishes removing the device after a restart".into()
    })
}

/// The real machine: MMDevice enumeration and a gated, time-limited launch.
#[cfg(windows)]
pub struct LiveMachine;

#[cfg(windows)]
impl Machine for LiveMachine {
    fn present(&self) -> bool {
        vbcable_present()
    }

    fn run(&mut self, exe: &Path, args: &[&str]) -> Result<i32> {
        if std::env::var(LIVE_RUN_GATE).as_deref() != Ok("1") {
            anyhow::bail!("launching VB-CABLE's setup is disabled outside the elevated helper ({LIVE_RUN_GATE})");
        }
        let mut child = std::process::Command::new(exe)
            .args(args)
            .current_dir(exe.parent().unwrap_or(Path::new(".")))
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
        let end = std::time::Instant::now() + std::time::Duration::from_secs(SETUP_TIMEOUT_SECS);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status.code().unwrap_or(-1));
            }
            if std::time::Instant::now() >= end {
                let _ = child.kill();
                anyhow::bail!("VB-CABLE setup did not finish within {SETUP_TIMEOUT_SECS} s");
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

/// A VB-CABLE input endpoint exists (COM initialised on a scratch thread,
/// so any caller can ask).
#[cfg(windows)]
pub fn vbcable_present() -> bool {
    std::thread::spawn(|| {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
        // SAFETY: balanced COM init on this short-lived thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let r = relay_vdevice::detect::mic_targets()
                .map(|t| t.iter().any(|t| t.kind == relay_vdevice::detect::MicTargetKind::VbCable))
                .unwrap_or(false);
            CoUninitialize();
            r
        }
    })
    .join()
    .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn vbcable_present() -> bool {
    false
}

/// The pack beside the running binary.
pub fn payload_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .map(|d| payload_dir_beside(&d))
        .unwrap_or_else(|| PathBuf::from(PAYLOAD_DIR))
}

/// Status for the Settings card. Read-only.
pub fn status(paths: &crate::config::Paths) -> Status {
    let file = relay_vdevice::installed::load(&paths.installed_file()).unwrap_or_default();
    Status {
        payload: check_payload(&payload_dir(), MANIFEST),
        present: vbcable_present(),
        installed_by_relay: file.component(relay_vdevice::installed::VBCABLE_DRIVER).is_some(),
        consent: file.consent.as_ref().is_some_and(|c| c.vbcable),
        credit: CREDIT.into(),
        credit_url: CREDIT_URL.into(),
    }
}

/// Record (or withdraw) the opt-in. Installs nothing by itself.
pub fn set_consent(paths: &crate::config::Paths, on: bool) -> Result<()> {
    use relay_vdevice::installed;
    let path = paths.installed_file();
    let mut file = installed::load(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut c = file.consent.clone().unwrap_or(installed::Consent {
        decided_at: installed::iso_now(),
        apo: false,
        camera: false,
        microphone: false,
        vbcable: false,
    });
    c.vbcable = on;
    // The VB-CABLE route is a microphone: opting in to install it is opting
    // in to the mic route too. Opting out of the install leaves the route
    // choice alone (an already-installed VB-CABLE can still be used).
    if on {
        c.microphone = true;
    }
    c.decided_at = installed::iso_now();
    file.consent = Some(c);
    installed::save(&path, &file).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_vdevice::installed::{self, Consent, InstalledFile, VBCABLE_DRIVER};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("relay-vbc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A pack of two files on disk and a manifest that pins them.
    fn pack(dir: &Path) -> Vec<PayloadFile> {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(SETUP_EXE), b"setup bytes").unwrap();
        std::fs::write(dir.join("vbMmeCable64_win10.inf"), b"inf bytes").unwrap();
        vec![
            PayloadFile { name: SETUP_EXE, sha256: leak(sha256_hex(b"setup bytes")) },
            PayloadFile { name: "vbMmeCable64_win10.inf", sha256: leak(sha256_hex(b"inf bytes")) },
        ]
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn this_build_bundles_nothing_until_permission() {
        assert!(MANIFEST.is_empty(), "pin the pack only once VB-Audio has agreed in writing");
        let d = tmp("none");
        // Even with files on disk, an empty manifest means "not bundled".
        std::fs::write(d.join(SETUP_EXE), b"anything").unwrap();
        assert_eq!(check_payload(&d, MANIFEST), Payload::NotBundled);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn payload_verification_good_missing_mismatched() {
        let d = tmp("pay");
        let pdir = d.join("pack");
        let m = pack(&pdir);
        assert_eq!(check_payload(&pdir, &m), Payload::Ready);
        // Absent folder: not bundled.
        assert_eq!(check_payload(&d.join("nope"), &m), Payload::NotBundled);
        // A pinned file missing: incomplete.
        std::fs::remove_file(pdir.join("vbMmeCable64_win10.inf")).unwrap();
        assert_eq!(
            check_payload(&pdir, &m),
            Payload::Incomplete { missing: "vbMmeCable64_win10.inf".into() }
        );
        // Altered bytes: mismatch, never Ready.
        std::fs::write(pdir.join("vbMmeCable64_win10.inf"), b"inf bytes").unwrap();
        std::fs::write(pdir.join(SETUP_EXE), b"evil").unwrap();
        assert_eq!(check_payload(&pdir, &m), Payload::Mismatch { file: SETUP_EXE.into() });
        // A manifest without the setup program is not a pack.
        assert_eq!(check_payload(&pdir, &m[1..]), Payload::NotBundled);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn manifest_names_cannot_escape() {
        for bad in ["..\\x.exe", "a/b", "C:x", "", " x", "..", "sub\\f"] {
            assert!(vet_name(bad).is_err(), "{bad:?}");
        }
        assert!(vet_name(SETUP_EXE).is_ok());
        let d = tmp("esc");
        let m = [
            PayloadFile { name: SETUP_EXE, sha256: "00" },
            PayloadFile { name: "..\\..\\evil.dll", sha256: "00" },
        ];
        std::fs::write(d.join(SETUP_EXE), b"x").unwrap();
        // The setup hash is wrong first, so pin it to reach the bad name.
        let m2 = [PayloadFile { name: SETUP_EXE, sha256: leak(sha256_hex(b"x")) }, m[1]];
        assert!(matches!(check_payload(&d, &m2), Payload::Mismatch { .. }));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn install_decision_table() {
        use InstallDecision as D;
        let ready = Payload::Ready;
        assert_eq!(decide_install(false, false, &ready), D::NoConsent);
        assert_eq!(decide_install(false, true, &ready), D::NoConsent);
        assert_eq!(decide_install(true, true, &ready), D::AlreadyPresent);
        assert_eq!(decide_install(true, true, &Payload::NotBundled), D::AlreadyPresent);
        assert_eq!(decide_install(true, false, &Payload::NotBundled), D::NotBundled);
        assert_eq!(decide_install(true, false, &ready), D::Install);
        assert!(matches!(
            decide_install(true, false, &Payload::Mismatch { file: "x".into() }),
            D::Refused(_)
        ));
        assert!(matches!(
            decide_install(true, false, &Payload::Incomplete { missing: "x".into() }),
            D::Refused(_)
        ));
        assert!(decide_uninstall(true));
        assert!(!decide_uninstall(false));
    }

    /// A fake machine: records launches, flips presence as told.
    struct Fake {
        present: std::cell::Cell<bool>,
        after_run: Option<bool>,
        code: i32,
        runs: Vec<(PathBuf, Vec<String>)>,
    }
    impl Fake {
        fn new(present: bool, after_run: Option<bool>, code: i32) -> Self {
            Self { present: present.into(), after_run, code, runs: vec![] }
        }
    }
    impl Machine for Fake {
        fn present(&self) -> bool {
            self.present.get()
        }
        fn run(&mut self, exe: &Path, args: &[&str]) -> Result<i32> {
            // The launched file is the verified, staged copy.
            assert!(exe.exists(), "launching a file that is not there");
            self.runs.push((exe.into(), args.iter().map(|s| s.to_string()).collect()));
            if let Some(p) = self.after_run {
                self.present.set(p);
            }
            Ok(self.code)
        }
        fn wait_present(&self, want: bool, _secs: u64) -> bool {
            self.present() == want
        }
    }

    fn consent_file(path: &Path, vbcable: bool) {
        let f = InstalledFile {
            consent: Some(Consent {
                decided_at: installed::iso_now(),
                apo: false,
                camera: false,
                microphone: vbcable,
                vbcable,
            }),
            ..Default::default()
        };
        installed::save(path, &f).unwrap();
    }

    #[test]
    fn install_runs_staged_verified_bytes_and_records_first() {
        let d = tmp("inst");
        let (pdir, stg, inst) = (d.join("pack"), d.join("stage"), d.join("installed.json"));
        let m = pack(&pdir);

        // No consent: refused, nothing run, nothing recorded.
        consent_file(&inst, false);
        let mut f = Fake::new(false, Some(true), 0);
        assert!(matches!(install_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Refused(_)));
        assert!(f.runs.is_empty());

        // Already present: skipped, nothing recorded (so never uninstalled).
        consent_file(&inst, true);
        let mut f = Fake::new(true, None, 0);
        assert!(matches!(install_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Skipped(_)));
        assert!(f.runs.is_empty());
        assert!(installed::load(&inst).unwrap().component(VBCABLE_DRIVER).is_none());

        // Not bundled: skipped.
        let mut f = Fake::new(false, Some(true), 0);
        assert!(matches!(
            install_with(&inst, &d.join("missing"), &stg, &m, &mut f),
            LiveOutcome::Skipped(_)
        ));
        assert!(f.runs.is_empty());

        // Good: the staged copy runs with the silent install line.
        let mut f = Fake::new(false, Some(true), 0);
        let out = install_with(&inst, &pdir, &stg, &m, &mut f);
        assert!(matches!(out, LiveOutcome::Done(_)), "{out:?}");
        assert_eq!(f.runs.len(), 1);
        assert_eq!(f.runs[0].0, stg.join(SETUP_EXE));
        assert_eq!(f.runs[0].1, vec!["-i".to_string(), "-h".to_string()]);
        assert_eq!(std::fs::read(stg.join(SETUP_EXE)).unwrap(), b"setup bytes");
        assert!(installed::load(&inst).unwrap().component(VBCABLE_DRIVER).is_some());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn install_refuses_altered_bytes_and_forgets_a_failed_setup() {
        let d = tmp("bad");
        let (pdir, stg, inst) = (d.join("pack"), d.join("stage"), d.join("installed.json"));
        let m = pack(&pdir);
        consent_file(&inst, true);
        std::fs::write(pdir.join(SETUP_EXE), b"tampered").unwrap();
        let mut f = Fake::new(false, Some(true), 0);
        assert!(matches!(install_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Refused(_)));
        assert!(f.runs.is_empty(), "an altered pack is never run");
        assert!(!stg.exists());

        // Setup runs but no device appears: the record is rolled back.
        std::fs::write(pdir.join(SETUP_EXE), b"setup bytes").unwrap();
        let mut f = Fake::new(false, Some(false), 1);
        assert!(matches!(install_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Failed(_)));
        assert_eq!(f.runs.len(), 1);
        assert!(installed::load(&inst).unwrap().component(VBCABLE_DRIVER).is_none());
        assert!(!stg.exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn uninstall_removes_only_what_relay_installed() {
        let d = tmp("un");
        let (pdir, stg, inst) = (d.join("pack"), d.join("stage"), d.join("installed.json"));
        let m = pack(&pdir);
        consent_file(&inst, true);

        // The user's own VB-CABLE (no record): left alone.
        let mut f = Fake::new(true, Some(false), 0);
        assert!(matches!(uninstall_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Skipped(_)));
        assert!(f.runs.is_empty());

        // Relay's install: removed with the silent uninstall line.
        let mut f = Fake::new(false, Some(true), 0);
        assert!(matches!(install_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Done(_)));
        let mut f = Fake::new(true, Some(false), 0);
        let out = uninstall_with(&inst, &pdir, &stg, &m, &mut f);
        assert!(matches!(out, LiveOutcome::Done(_)), "{out:?}");
        assert_eq!(f.runs[0].1, vec!["-u".to_string(), "-h".to_string()]);
        assert!(installed::load(&inst).unwrap().component(VBCABLE_DRIVER).is_none());
        assert!(!stg.exists());

        // A failed uninstall keeps the record for a retry.
        let mut f = Fake::new(false, Some(true), 0);
        install_with(&inst, &pdir, &stg, &m, &mut f);
        let mut f = Fake::new(true, None, 3);
        assert!(matches!(uninstall_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Failed(_)));
        assert!(installed::load(&inst).unwrap().component(VBCABLE_DRIVER).is_some());

        // Staged copy altered and pack gone: refuse to run anything.
        std::fs::write(stg.join(SETUP_EXE), b"swapped").unwrap();
        std::fs::remove_dir_all(&pdir).unwrap();
        let mut f = Fake::new(true, Some(false), 0);
        assert!(matches!(uninstall_with(&inst, &pdir, &stg, &m, &mut f), LiveOutcome::Failed(_)));
        assert!(f.runs.is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn plan_lines_say_what_happens() {
        let s = Path::new(r"C:\Program Files\Relay\drivers\vbcable");
        assert!(plan_lines(&Payload::NotBundled, false, s)[0].contains("not bundled"));
        assert!(plan_lines(&Payload::Ready, true, s)[0].contains("already installed"));
        assert!(
            plan_lines(&Payload::Mismatch { file: "x".into() }, false, s)[0].starts_with("Refused")
        );
    }

    #[test]
    fn consent_toggle_keeps_other_answers() {
        let d = tmp("consent");
        let paths = crate::config::Paths::at(&d);
        crate::vdevice::set_consent(&paths, true, true, false).unwrap();
        set_consent(&paths, true).unwrap();
        let c = installed::load(&paths.installed_file()).unwrap().consent.unwrap();
        assert!(c.vbcable && c.microphone && c.apo && c.camera);
        set_consent(&paths, false).unwrap();
        let c = installed::load(&paths.installed_file()).unwrap().consent.unwrap();
        assert!(!c.vbcable && c.microphone && c.apo && c.camera);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn staging_is_under_program_files() {
        let s = staging_dir();
        assert!(s.ends_with(Path::new("Relay").join(PAYLOAD_DIR)), "{}", s.display());
        assert!(!s.to_string_lossy().to_ascii_lowercase().contains("appdata"));
    }
}
