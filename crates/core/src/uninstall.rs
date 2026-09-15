//! The uninstall plan — the product's promise, expressed as a list.
//!
//! Relay's claim is that after an uninstall a clean machine shows no
//! difference except the data folder the user chose to keep. That only holds
//! if removal is driven by a *record* of what was added rather than by
//! guesswork, and if the order is right:
//!
//! ```text
//! 1. stop the UI                       (so nothing rewrites state behind us)
//! 2. shut the core down                (it restores audio/display on the way out)
//! 3. restore the endpoint FX store     (from apo-backup\<endpoint>.json, byte-for-byte)
//! 4. delete the camera COM keys        (exactly the keys in installed.json)
//! 5. remove the Run key value          (HKCU, the one value)
//! 6. delete the program files          (NSIS owns this; listed for the diff)
//! 7. delete the data paths             (only if the user says so)
//! ```
//!
//! Step 7 deletes the paths [`Paths::data_paths`] names rather than the data
//! root, because Tauri's per-user NSIS installer puts Relay's binaries in
//! that same folder — `%LOCALAPPDATA%\Relay` is both the install directory
//! and the data root, so recursively deleting it would delete the running
//! executable out from under step 6.
//!
//! Steps 3 and 4 are the ones that touch HKLM, so they need elevation; the
//! plan says so per step and the executor reports what it had to skip rather
//! than failing the whole uninstall. Steps 1–2 and 5–7 are per-user and work
//! from a plain NSIS uninstaller.
//!
//! [`MachineState`] is the only part that reads the machine. Everything
//! below it — the plan, its ordering, its rendering — is a pure function of
//! that struct, which is what makes the promise testable without a VM.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Paths;

/// Which of Relay's parts a step belongs to. Ordering of the enum is the
/// order the steps have to run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Close the Tauri window / UI process.
    StopUi,
    /// `relay-core shutdown` — restores audio and display state first.
    StopCore,
    /// Restore one endpoint's FX property store from its install backup.
    RestoreApo,
    /// Delete the camera media source's COM registration.
    RemoveVcam,
    /// Remove `HKCU\...\Run\Relay`.
    RemoveRunKey,
    /// Delete the installed binaries (the NSIS uninstaller's own job).
    RemoveFiles,
    /// Delete `%LOCALAPPDATA%\Relay` (profiles, hardware library, logs).
    RemoveData,
}

impl StepKind {
    /// HKLM is involved, so a non-elevated uninstaller cannot do it.
    pub fn needs_elevation(self) -> bool {
        matches!(self, StepKind::RestoreApo | StepKind::RemoveVcam)
    }

    /// Plain-English name for the dry-run listing and the Settings card.
    pub fn label(self) -> &'static str {
        match self {
            StepKind::StopUi => "Close the Relay window",
            StepKind::StopCore => "Stop the core (restores your audio and display settings)",
            StepKind::RestoreApo => "Restore the endpoint audio chain",
            StepKind::RemoveVcam => "Unregister the virtual camera",
            StepKind::RemoveRunKey => "Remove the start-at-login entry",
            StepKind::RemoveFiles => "Delete the program files",
            StepKind::RemoveData => "Delete your profiles and settings",
        }
    }
}

/// One thing the uninstaller will do, and the exact target it does it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub kind: StepKind,
    /// The registry key, file path or endpoint this step acts on. One line,
    /// shown verbatim in the UI — no invented wording.
    pub target: String,
    /// This target is on the machine right now. Absent targets stay in the
    /// plan (as "nothing to do") so the listing is the same shape whether or
    /// not the user opted in.
    pub present: bool,
}

/// The whole uninstall, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// The user chose to keep `%LOCALAPPDATA%\Relay`.
    pub keep_data: bool,
}

impl Plan {
    /// Steps that will actually do something.
    pub fn active(&self) -> impl Iterator<Item = &Step> {
        self.steps.iter().filter(|s| s.present)
    }

    /// Any remaining work needs an elevated process.
    pub fn needs_elevation(&self) -> bool {
        self.active().any(|s| s.kind.needs_elevation())
    }

    /// The dry-run listing: `[x] label — target`, one line per step. This is
    /// what `relay-core uninstall --dry-run` prints and what the Settings
    /// "what we installed" card renders, so the two can never disagree.
    pub fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .steps
            .iter()
            .map(|s| {
                let mark = if s.present { "x" } else { " " };
                let elev =
                    if s.present && s.kind.needs_elevation() { " (needs admin)" } else { "" };
                format!("[{mark}] {} — {}{elev}", s.kind.label(), s.target)
            })
            .collect();
        out.push(String::new());
        out.push(
            if self.keep_data {
                "Your profiles and hardware library are kept in %LOCALAPPDATA%\\Relay."
            } else {
                "Everything above is removed. Nothing else on this PC was changed by Relay."
            }
            .to_string(),
        );
        out
    }
}

/// Everything the plan needs to know about the machine, read once and
/// read-only. Constructed by [`MachineState::probe`] in production and by
/// hand in tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineState {
    /// Relay's CLSID is in an endpoint's FX chain, or an install backup
    /// exists — either way the APO needs restoring. Carries the endpoint id.
    pub apo_endpoint: Option<String>,
    /// The install-time FX store backup, if it is on disk.
    pub apo_backup: Option<PathBuf>,
    /// Camera COM keys recorded in `installed.json`, deepest first.
    pub vcam_keys: Vec<String>,
    /// `HKCU\...\Run\Relay` exists.
    pub run_key: bool,
    /// Directory the running binaries live in.
    pub install_dir: Option<PathBuf>,
    /// The data paths that exist and can therefore be deleted. Named
    /// individually rather than as one root because under the per-user
    /// installer the root also holds the program files.
    pub data_targets: Vec<PathBuf>,
    /// A core is answering on the pipe.
    pub core_running: bool,
    /// A UI process is up.
    pub ui_running: bool,
}

/// Build the ordered plan. Pure: same state in, same plan out.
pub fn plan_from(state: &MachineState, keep_data: bool) -> Plan {
    let mut steps = Vec::new();

    steps.push(Step {
        kind: StepKind::StopUi,
        target: "relay-ui.exe".into(),
        present: state.ui_running,
    });
    steps.push(Step {
        kind: StepKind::StopCore,
        target: crate::config::pipe_name(),
        present: state.core_running,
    });

    // The APO step is present when there is a backup to restore from: that
    // file, not the registry, is what makes restoration exact. A registered
    // APO without a backup is listed too, because leaving it behind would
    // break the promise — the target says which case it is.
    let apo_target = match (&state.apo_endpoint, &state.apo_backup) {
        (Some(ep), Some(backup)) => format!("{ep} ← {}", backup.display()),
        (Some(ep), None) => format!("{ep} (no install backup found)"),
        (None, Some(backup)) => backup.display().to_string(),
        (None, None) => "not installed".into(),
    };
    steps.push(Step {
        kind: StepKind::RestoreApo,
        target: apo_target,
        present: state.apo_endpoint.is_some() || state.apo_backup.is_some(),
    });

    // One step per recorded key, so the listing is literally the diff the VM
    // test checks.
    if state.vcam_keys.is_empty() {
        steps.push(Step {
            kind: StepKind::RemoveVcam,
            target: "not installed".into(),
            present: false,
        });
    } else {
        for key in &state.vcam_keys {
            steps.push(Step {
                kind: StepKind::RemoveVcam,
                target: format!(r"HKLM\{key}"),
                present: true,
            });
        }
    }

    steps.push(Step {
        kind: StepKind::RemoveRunKey,
        target: format!(r"HKCU\{}\{}", crate::autostart::RUN_KEY, crate::autostart::VALUE_NAME),
        present: state.run_key,
    });
    steps.push(Step {
        kind: StepKind::RemoveFiles,
        target: state
            .install_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<install dir>".into()),
        present: state.install_dir.is_some(),
    });
    if state.data_targets.is_empty() {
        steps.push(Step {
            kind: StepKind::RemoveData,
            target: "nothing saved yet".into(),
            present: false,
        });
    } else {
        for path in &state.data_targets {
            steps.push(Step {
                kind: StepKind::RemoveData,
                target: path.display().to_string(),
                present: !keep_data,
            });
        }
    }

    Plan { steps, keep_data }
}

/// What actually happened to one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The step ran.
    Done,
    /// Nothing to do (target absent).
    Skipped,
    /// Needs an elevated process; left for the elevated phase.
    NeedsElevation,
    /// Owned by the NSIS uninstaller, not by us.
    DeferredToInstaller,
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub kind: StepKind,
    pub target: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub steps: Vec<StepReport>,
}

impl Report {
    /// Nothing failed and nothing is waiting on elevation.
    pub fn complete(&self) -> bool {
        !self
            .steps
            .iter()
            .any(|s| matches!(s.outcome, Outcome::Failed { .. } | Outcome::NeedsElevation))
    }

    pub fn lines(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|s| {
                let note = match &s.outcome {
                    Outcome::Done => "done".to_string(),
                    Outcome::Skipped => "nothing to do".to_string(),
                    Outcome::NeedsElevation => "needs admin — re-run elevated".to_string(),
                    Outcome::DeferredToInstaller => "left to the uninstaller".to_string(),
                    Outcome::Failed { error } => format!("FAILED: {error}"),
                };
                format!("{}: {} [{note}]", s.kind.label(), s.target)
            })
            .collect()
    }
}

#[cfg(windows)]
pub use imp::{execute, probe};

#[cfg(windows)]
mod imp {
    use super::*;
    use tracing::{info, warn};

    /// Read the machine, touching nothing.
    pub fn probe(paths: &Paths) -> MachineState {
        let apo = crate::audio_apo::apo_status();
        let apo_backup = apo
            .endpoint
            .as_ref()
            .map(|ep| paths.apo_backup_dir().join(format!("{ep}.json")))
            .filter(|p| p.exists())
            // An endpoint switch since install would hide the backup; fall
            // back to any backup in the folder so nothing is orphaned.
            .or_else(|| first_backup(&paths.apo_backup_dir()));

        let vcam_keys = relay_vdevice::installed::load(&paths.installed_file())
            .map(|f| {
                f.components
                    .iter()
                    .flat_map(relay_vdevice::reg::plan_camera_uninstall)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        MachineState {
            apo_endpoint: if apo.installed { apo.endpoint } else { None },
            apo_backup,
            vcam_keys,
            run_key: crate::autostart::is_enabled().unwrap_or(false),
            install_dir: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
            data_targets: paths.data_paths().into_iter().filter(|p| p.exists()).collect(),
            core_running: core_is_running(),
            ui_running: process_running("relay-ui.exe"),
        }
    }

    fn first_backup(dir: &Path) -> Option<PathBuf> {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "json"))
    }

    /// Is a core up? Its single-instance mutex is the authoritative answer
    /// and probing it costs one handle open — cheaper than a pipe connect,
    /// and it does not consume a pipe instance from a live service.
    fn core_is_running() -> bool {
        use windows::core::PCWSTR;
        use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_ACCESS_RIGHTS};
        let name: Vec<u16> =
            crate::config::mutex_name().encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `name` is NUL-terminated; the handle is closed by `Owned`.
        unsafe {
            match OpenMutexW(
                SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000),
                false,
                PCWSTR(name.as_ptr()),
            ) {
                Ok(h) => {
                    let _owned = windows::core::Owned::new(h);
                    true
                }
                Err(_) => false,
            }
        }
    }

    fn process_running(exe: &str) -> bool {
        crate::processes::list_all().iter().any(|p| p.eq_ignore_ascii_case(exe))
    }

    /// Run the plan. Per-user steps always run; HKLM steps run only when this
    /// process is elevated and the matching live-write gate is set (the NSIS
    /// uninstaller sets both). Every step is reported either way — a step we
    /// cannot do is never silently dropped.
    pub fn execute(paths: &Paths, plan: &Plan) -> Report {
        let mut report = Report::default();
        for step in &plan.steps {
            let outcome = if !step.present {
                Outcome::Skipped
            } else {
                match step.kind {
                    StepKind::StopUi => run_step(stop_ui()),
                    StepKind::StopCore => run_step(stop_core()),
                    StepKind::RestoreApo => elevated_step(|| {
                        crate::audio_apo::uninstall_live(&paths.apo_backup_dir()).map(|_| ())
                    }),
                    StepKind::RemoveVcam => elevated_step(|| {
                        // Idempotent across the per-key steps: the first one
                        // removes every recorded key and empties the record,
                        // the rest find nothing left and report skipped.
                        match crate::vdevice::uninstall_camera_live(paths) {
                            Err(e) if e.to_string().contains("nothing to uninstall") => Ok(()),
                            other => other,
                        }
                    }),
                    StepKind::RemoveRunKey => run_step(crate::autostart::set(false)),
                    // NSIS deletes its own install tree after this process
                    // exits; doing it here would delete the running exe.
                    StepKind::RemoveFiles => Outcome::DeferredToInstaller,
                    StepKind::RemoveData => run_step(remove_path(Path::new(&step.target))),
                }
            };
            match &outcome {
                Outcome::Failed { error } => {
                    warn!(kind = ?step.kind, target = %step.target, %error, "uninstall step failed")
                }
                Outcome::Done => info!(kind = ?step.kind, target = %step.target, "uninstall step"),
                _ => {}
            }
            report.steps.push(StepReport { kind: step.kind, target: step.target.clone(), outcome });
        }
        report
    }

    /// Delete one data path. The data root itself is never deleted here: the
    /// per-user installer puts Relay's binaries in that same folder, so NSIS
    /// removes it (non-recursively, after its own files) once it is empty.
    fn remove_path(path: &Path) -> Result<()> {
        let r =
            if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        match r {
            Ok(()) => {
                info!(path = %path.display(), "removed");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::Error::from(e).context(format!("removing {}", path.display()))),
        }
    }

    fn run_step(r: Result<()>) -> Outcome {
        match r {
            Ok(()) => Outcome::Done,
            Err(e) => Outcome::Failed { error: e.to_string() },
        }
    }

    fn elevated_step(f: impl FnOnce() -> Result<()>) -> Outcome {
        if !crate::processes::is_elevated() {
            return Outcome::NeedsElevation;
        }
        run_step(f())
    }

    /// Ask the core to shut down (it restores audio and display first) and
    /// give it a moment to go. Falls back to "done" if the pipe has already
    /// gone away — the goal is a stopped core, not a successful call.
    fn stop_core() -> Result<()> {
        use crate::ipc::{client::Client, Method};
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        rt.block_on(async {
            let Ok(mut c) = Client::connect().await else { return Ok(()) };
            let _ = c.call(Method::Shutdown).await;
            Ok(())
        })
    }

    fn stop_ui() -> Result<()> {
        crate::processes::terminate_by_name("relay-ui.exe")
    }
}

/// Finish the two HKLM steps in the elevated helper. One UAC prompt, and
/// only when there is something left that needs it.
///
/// This used to re-launch `relay-core uninstall --components-only` under
/// `runas`, which could not work: elevation starts the child from the user's
/// logon environment block, not the parent's, so the live-write gates the
/// parent set never reached it and both steps failed the gate check. The
/// helper needs no inherited environment — it arms each gate itself, around
/// one vetted call, and reports what it did.
#[cfg(windows)]
pub fn finish_elevated(paths: &Paths) -> Result<crate::elevate::Response> {
    use crate::elevate::{ElevatedOp, LaunchError};
    crate::elevate::run(paths, &[ElevatedOp::UninstallApo, ElevatedOp::UninstallCamera]).map_err(
        |e| match e {
            LaunchError::Declined => anyhow::anyhow!("{e}"),
            LaunchError::Other(e) => e,
        },
    )
}

#[cfg(not(windows))]
pub fn finish_elevated(_paths: &Paths) -> Result<crate::elevate::Response> {
    anyhow::bail!("elevation is Windows-only")
}

/// Registry name the NSIS bundle registers itself under, per-user. Tauri's
/// NSIS template keys Add/Remove Programs on the product name.
#[cfg(windows)]
const ARP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Relay";

/// Where the NSIS uninstaller lives, according to Add/Remove Programs. Falls
/// back to `uninstall.exe` beside this exe, which is where the per-user NSIS
/// template puts it.
#[cfg(windows)]
pub fn uninstaller_path() -> Option<PathBuf> {
    read_arp_value("UninstallString")
        .map(|s| PathBuf::from(s.trim_matches('"')))
        .filter(|p| p.exists())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("uninstall.exe")))
                .filter(|p| p.exists())
        })
}

#[cfg(windows)]
fn read_arp_value(name: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

    let key: Vec<u16> = ARP_KEY.encode_utf16().chain(std::iter::once(0)).collect();
    let val: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 1024];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: out-buffer and length are valid; RegGetValueW NUL-terminates
    // and never writes past `len`.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    };
    r.ok().ok()?;
    let chars = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..chars]))
}

/// Start the Windows uninstaller and return. The caller shuts the core down
/// afterwards so the uninstaller can replace the files it is holding.
#[cfg(windows)]
pub fn launch_uninstaller() -> Result<PathBuf> {
    use anyhow::Context;
    let path = uninstaller_path().context(
        "Relay's uninstaller was not found — this looks like a build run from the repo rather \
         than an installed copy. Use `relay-core uninstall` instead.",
    )?;
    std::process::Command::new(&path)
        .spawn()
        .with_context(|| format!("launching {}", path.display()))?;
    Ok(path)
}

#[cfg(not(windows))]
pub fn launch_uninstaller() -> Result<PathBuf> {
    anyhow::bail!("the Windows uninstaller is Windows-only")
}

#[cfg(not(windows))]
pub fn probe(paths: &Paths) -> MachineState {
    MachineState {
        data_targets: paths.data_paths().into_iter().filter(|p| p.exists()).collect(),
        ..Default::default()
    }
}

#[cfg(not(windows))]
pub fn execute(_paths: &Paths, _plan: &Plan) -> Report {
    Report::default()
}

/// The plan for this machine right now.
pub fn plan(paths: &Paths, keep_data: bool) -> Plan {
    plan_from(&probe(paths), keep_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine where the user opted into both components and autostart.
    fn everything_installed() -> MachineState {
        MachineState {
            apo_endpoint: Some("{0.0.0.00000000}.{abc}".into()),
            apo_backup: Some(PathBuf::from(r"C:\d\Relay\apo-backup\{abc}.json")),
            vcam_keys: vec![
                r"SOFTWARE\Classes\CLSID\{9B7E62D4}\InprocServer32".into(),
                r"SOFTWARE\Classes\CLSID\{9B7E62D4}".into(),
            ],
            run_key: true,
            install_dir: Some(PathBuf::from(r"C:\d\Relay")),
            // The per-user installer shares the folder with the data root, so
            // the data step names its paths rather than the root.
            data_targets: vec![
                PathBuf::from(r"C:\d\Relay\data"),
                PathBuf::from(r"C:\d\Relay\logs"),
                PathBuf::from(r"C:\d\Relay\installed.json"),
            ],
            core_running: true,
            ui_running: true,
        }
    }

    #[test]
    fn steps_run_in_the_promised_order() {
        let plan = plan_from(&everything_installed(), false);
        let kinds: Vec<StepKind> = plan.steps.iter().map(|s| s.kind).collect();
        // Sorted == as-generated: the enum order *is* the uninstall order, so
        // a future step inserted in the wrong place fails here.
        let mut sorted = kinds.clone();
        sorted.sort();
        assert_eq!(kinds, sorted, "steps must be ordered UI → core → HKLM → HKCU → files → data");
        assert_eq!(kinds.first(), Some(&StepKind::StopUi));
        assert_eq!(kinds.last(), Some(&StepKind::RemoveData));
        // The core is stopped (and so restores) before any component is
        // pulled out from under it.
        let core = kinds.iter().position(|k| *k == StepKind::StopCore).unwrap();
        let apo = kinds.iter().position(|k| *k == StepKind::RestoreApo).unwrap();
        assert!(core < apo);
    }

    #[test]
    fn every_recorded_vcam_key_gets_its_own_step() {
        let state = everything_installed();
        let plan = plan_from(&state, false);
        let targets: Vec<&str> = plan
            .steps
            .iter()
            .filter(|s| s.kind == StepKind::RemoveVcam)
            .map(|s| s.target.as_str())
            .collect();
        assert_eq!(targets.len(), state.vcam_keys.len());
        for key in &state.vcam_keys {
            assert!(targets.iter().any(|t| t.contains(key.as_str())), "missing {key}");
        }
        // Deepest first, as the record stores them: a parent key cannot be
        // deleted before its child on Windows.
        assert!(targets[0].contains("InprocServer32"));
    }

    #[test]
    fn fresh_machine_plan_has_the_same_shape_but_nothing_to_do() {
        let plan = plan_from(&MachineState::default(), false);
        assert!(plan.active().next().is_none(), "nothing is present on a fresh machine");
        assert!(!plan.needs_elevation(), "nothing to remove means no UAC prompt");
        // Every kind is still listed, so the Settings card reads the same
        // before and after opt-in.
        for kind in [
            StepKind::StopUi,
            StepKind::StopCore,
            StepKind::RestoreApo,
            StepKind::RemoveVcam,
            StepKind::RemoveRunKey,
            StepKind::RemoveFiles,
            StepKind::RemoveData,
        ] {
            assert!(plan.steps.iter().any(|s| s.kind == kind), "{kind:?} missing from the listing");
        }
    }

    #[test]
    fn keep_data_leaves_every_data_path_alone() {
        let state = everything_installed();
        let kept = plan_from(&state, true);
        let data: Vec<&Step> =
            kept.steps.iter().filter(|s| s.kind == StepKind::RemoveData).collect();
        assert_eq!(data.len(), state.data_targets.len());
        assert!(data.iter().all(|s| !s.present), "keep_data must not delete any data path");
        assert!(kept.lines().iter().any(|l| l.contains("are kept")));

        let deleted = plan_from(&state, false);
        let data: Vec<&Step> =
            deleted.steps.iter().filter(|s| s.kind == StepKind::RemoveData).collect();
        assert!(data.iter().all(|s| s.present));
        assert!(deleted.lines().iter().any(|l| l.contains("Nothing else on this PC")));
    }

    /// The regression this module exists to prevent: the per-user installer
    /// and the data root are the same folder, so no step may ever name the
    /// root as something to delete recursively — that would take the running
    /// executable with it.
    #[test]
    fn the_data_step_never_targets_the_install_directory_itself() {
        let state = everything_installed();
        let install = state.install_dir.clone().unwrap();
        for step in plan_from(&state, false).steps.iter().filter(|s| s.kind == StepKind::RemoveData)
        {
            assert_ne!(
                Path::new(&step.target),
                install.as_path(),
                "the data step must name paths under the install dir, never the dir itself"
            );
            assert!(Path::new(&step.target).starts_with(&install));
        }
    }

    #[test]
    fn only_the_hklm_steps_ask_for_elevation() {
        assert!(StepKind::RestoreApo.needs_elevation());
        assert!(StepKind::RemoveVcam.needs_elevation());
        for kind in [
            StepKind::StopUi,
            StepKind::StopCore,
            StepKind::RemoveRunKey,
            StepKind::RemoveFiles,
            StepKind::RemoveData,
        ] {
            assert!(!kind.needs_elevation(), "{kind:?} must work per-user");
        }
        // A machine with only per-user traces never prompts.
        let state = MachineState { run_key: true, ..Default::default() };
        assert!(!plan_from(&state, false).needs_elevation());
    }

    #[test]
    fn dry_run_lines_mark_present_targets_and_name_them_verbatim() {
        let state = everything_installed();
        let lines = plan_from(&state, false).lines();
        let joined = lines.join("\n");
        assert!(joined.contains(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Relay"));
        assert!(joined.contains(r"C:\d\Relay\apo-backup\{abc}.json"));
        assert!(joined.contains("needs admin"), "elevated steps are flagged to the user");
        assert!(lines.iter().filter(|l| l.starts_with("[x]")).count() >= 6);
    }

    #[test]
    fn report_is_incomplete_while_anything_waits_on_elevation() {
        let mut r = Report::default();
        r.steps.push(StepReport {
            kind: StepKind::RemoveRunKey,
            target: "x".into(),
            outcome: Outcome::Done,
        });
        assert!(r.complete());
        r.steps.push(StepReport {
            kind: StepKind::RestoreApo,
            target: "y".into(),
            outcome: Outcome::NeedsElevation,
        });
        assert!(!r.complete());
        assert!(r.lines().iter().any(|l| l.contains("needs admin")));
    }
}
