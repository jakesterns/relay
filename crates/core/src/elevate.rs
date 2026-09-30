//! The elevated install helper: the one place Relay writes `HKLM`.
//!
//! The core runs unelevated by design — it manages one user's profiles and
//! has no business holding an administrator token all day. But both opt-in
//! components need exactly one elevated write each:
//!
//! - the endpoint APO, because audiodg runs as LOCAL SERVICE and cannot see
//!   per-user classes (M3b decision), and
//! - the virtual camera, because the Frame Server runs as LOCAL SERVICE and
//!   demonstrably never loads a per-user registration (S5, measured — see
//!   `docs/dev/vcam-live.md`).
//!
//! S22 added a third: the inbound Windows Firewall rule for
//! `relay-share.exe`. Firewall policy is machine-wide, so it needs the same
//! token — and it needs the same honesty, because the installer is per-user
//! and unelevated and must never reach for an administrator token behind the
//! user's back.
//!
//! So `relay-elevate.exe` exists: a tiny GUI-subsystem binary the core
//! launches through `ShellExecuteExW`'s `runas` verb, which is what raises
//! the UAC prompt. It is not a service, it is not resident, and it has no
//! command surface of its own — it reads one request file, does what that
//! request names, writes one result file and exits.
//!
//! # What keeps this honest
//!
//! 1. **A closed op set.** [`ElevatedOp`] has six variants and no free-form
//!    "run this key" escape hatch. A request that does not deserialise into
//!    one of them is refused before anything is touched.
//! 2. **The helper re-derives the work; the request never carries it.** No
//!    registry path, value or DLL path crosses the request boundary. The
//!    camera's keys come from `relay_vdevice::reg::plan_camera_install`, the
//!    APO's from `relay_apo::fxstore::plan_install`, and both DLL paths come
//!    from the helper's *own* directory — so a tampered request cannot point
//!    the registration at somebody else's binary.
//! 3. **Scope vetting before every write.** [`vet_com_keys`] refuses any COM
//!    key that is not at or under our own CLSID, and [`vet_endpoint_guid`]
//!    refuses an endpoint id that is not a GUID. The uninstall paths read
//!    their key lists off disk (`installed.json`, `apo-backup\*.json`), which
//!    is exactly where these checks matter.
//! 4. **The live-write gates stay in force.** `RELAY_APO_ALLOW_LIVE_WRITE`
//!    and `RELAY_VDEVICE_ALLOW_LIVE_WRITE` still guard every write in
//!    `relay_apo::livereg` and `relay_vdevice::livereg`. The helper is the
//!    sanctioned installer context those gates were written for, so it arms
//!    one of them around one vetted call and disarms it immediately after —
//!    it never exports them to children and never leaves them set. Nothing
//!    else in the product arms them, so an accidental call anywhere else
//!    still fails loudly.
//! 5. **Elevation is the user's, every time.** The core cannot suppress the
//!    prompt, and a declined prompt is reported as [`Declined`] — a distinct
//!    outcome, not an error — because "nothing happened" needs saying plainly.
//!
//! [`Declined`]: LaunchError::Declined

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Paths;

/// Name of the helper binary, beside the core in the install directory.
pub const HELPER_EXE: &str = "relay-elevate.exe";

/// Request-format version. The helper refuses anything else, so a stale
/// request left by an older build can never be replayed against a new one.
/// v2 (S42): the two APO ops carry the target endpoint GUID.
pub const REQUEST_VERSION: u32 = 2;

/// How long a request stays actionable. Long enough for a slow UAC prompt,
/// short enough that a file left behind by a crash is dead on arrival.
pub const MAX_REQUEST_AGE_SECS: u64 = 300;

/// The complete set of things the elevated helper will do. There is no
/// "other" variant on purpose: this enum *is* the allow-list.
///
/// The APO ops name their render endpoint by GUID and nothing else (S42);
/// the helper vets that it is a GUID *and* an active render endpoint before
/// touching anything, and derives every registry path itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElevatedOp {
    /// Register the APO on one render endpoint (the default when `None`),
    /// backup first.
    InstallApo {
        #[serde(default)]
        endpoint: Option<String>,
    },
    /// Restore one endpoint's FX property store from its install backup.
    /// `None` restores *every* recorded endpoint — the uninstaller's case.
    UninstallApo {
        #[serde(default)]
        endpoint: Option<String>,
    },
    /// Register the camera media source's COM class.
    InstallCamera,
    /// Delete exactly the camera keys recorded in `installed.json`.
    UninstallCamera,
    /// Add the inbound Windows Firewall Allow rule for `relay-share.exe`,
    /// after backing up and removing any Block rule that would override it.
    AllowFirewall,
    /// Remove the firewall rule Relay added.
    RemoveFirewall,
}

impl ElevatedOp {
    /// Plain-English name, used in the UI and in every report line.
    pub fn label(&self) -> &'static str {
        match self {
            ElevatedOp::InstallApo { .. } => "Install the endpoint audio processor",
            ElevatedOp::UninstallApo { .. } => "Restore the endpoint audio chain",
            ElevatedOp::InstallCamera => "Register the virtual camera",
            ElevatedOp::UninstallCamera => "Unregister the virtual camera",
            ElevatedOp::AllowFirewall => "Allow Relay through Windows Firewall",
            ElevatedOp::RemoveFirewall => "Remove Relay's Windows Firewall rule",
        }
    }

    /// CLI/IPC spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            ElevatedOp::InstallApo { .. } => "install-apo",
            ElevatedOp::UninstallApo { .. } => "uninstall-apo",
            ElevatedOp::InstallCamera => "install-camera",
            ElevatedOp::UninstallCamera => "uninstall-camera",
            ElevatedOp::AllowFirewall => "allow-firewall",
            ElevatedOp::RemoveFirewall => "remove-firewall",
        }
    }

    /// The endpoint an APO op names, if any.
    pub fn endpoint(&self) -> Option<&str> {
        match self {
            ElevatedOp::InstallApo { endpoint } | ElevatedOp::UninstallApo { endpoint } => {
                endpoint.as_deref()
            }
            _ => None,
        }
    }

    /// Parse the CLI spelling. APO ops come back targeting the default
    /// endpoint (install) or every recorded one (uninstall); see
    /// [`ElevatedOp::with_endpoint`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "install-apo" => Some(ElevatedOp::InstallApo { endpoint: None }),
            "uninstall-apo" => Some(ElevatedOp::UninstallApo { endpoint: None }),
            "install-camera" => Some(ElevatedOp::InstallCamera),
            "uninstall-camera" => Some(ElevatedOp::UninstallCamera),
            "allow-firewall" => Some(ElevatedOp::AllowFirewall),
            "remove-firewall" => Some(ElevatedOp::RemoveFirewall),
            _ => None,
        }
    }

    /// Point an APO op at one endpoint. No-op for the other ops.
    pub fn with_endpoint(self, ep: Option<String>) -> Self {
        match self {
            ElevatedOp::InstallApo { .. } => ElevatedOp::InstallApo { endpoint: ep },
            ElevatedOp::UninstallApo { .. } => ElevatedOp::UninstallApo { endpoint: ep },
            other => other,
        }
    }
}

/// What the core asks the helper to do. Note what is *not* here: no registry
/// paths, no value data, no DLL path. The helper derives all of that itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    /// Random per-request id; also the file name stem.
    pub nonce: String,
    /// Unix seconds. Requests older than [`MAX_REQUEST_AGE_SECS`] are refused.
    pub created_at: u64,
    /// The data root the core is using, so `--data-dir` and `RELAY_INSTANCE`
    /// test roots reach the same `installed.json` the UI is showing.
    pub data_dir: PathBuf,
    /// Ops to run, in order. Each is independent; a failure does not abort
    /// the rest, because a half-done install still has to be reported.
    pub ops: Vec<ElevatedOp>,
}

/// What one op did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum OpOutcome {
    /// It ran and the machine changed.
    Done { detail: String },
    /// Nothing to do — already in the requested state.
    Skipped { reason: String },
    /// Vetting rejected it. Nothing was touched.
    Refused { reason: String },
    /// It ran and failed.
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpResult {
    pub op: ElevatedOp,
    #[serde(flatten)]
    pub outcome: OpOutcome,
}

impl OpResult {
    pub fn line(&self) -> String {
        let note = match &self.outcome {
            OpOutcome::Done { detail } => format!("done — {detail}"),
            OpOutcome::Skipped { reason } => format!("nothing to do — {reason}"),
            OpOutcome::Refused { reason } => format!("REFUSED: {reason}"),
            OpOutcome::Failed { error } => format!("FAILED: {error}"),
        };
        format!("{}: {note}", self.op.label())
    }
}

/// What the helper writes back. The core never trusts the helper's word for
/// the machine state — it re-probes afterwards — but this is what it shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub nonce: String,
    /// The helper ran elevated (it refuses everything otherwise).
    pub elevated: bool,
    pub results: Vec<OpResult>,
}

impl Response {
    /// Every op either ran or had nothing to do.
    pub fn ok(&self) -> bool {
        self.elevated
            && self
                .results
                .iter()
                .all(|r| matches!(r.outcome, OpOutcome::Done { .. } | OpOutcome::Skipped { .. }))
    }

    /// Did anything on this machine actually change?
    pub fn changed_anything(&self) -> bool {
        self.results.iter().any(|r| matches!(r.outcome, OpOutcome::Done { .. }))
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self.results.iter().map(OpResult::line).collect();
        if !self.elevated {
            out.push(
                "The helper did not receive administrator rights — nothing was changed.".into(),
            );
        }
        out
    }
}

/// Why a launch did not produce a response.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// The user closed or declined the UAC prompt. **Nothing ran.** This is
    /// a normal answer, not a failure, and the wording above says so.
    #[error("nothing was changed — you declined the Windows permission prompt")]
    Declined,
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

// ---------------------------------------------------------------------------
// Vetting. Pure, so the rules are tested without a registry or a token.
// ---------------------------------------------------------------------------

/// The registry root every COM key Relay creates must sit at or under.
pub fn clsid_root(clsid: &str) -> String {
    format!(r"SOFTWARE\Classes\CLSID\{clsid}")
}

/// Refuse any COM key outside our own CLSID subtree.
///
/// This is the check that makes the *uninstall* paths safe: their key lists
/// come off disk (`installed.json`, `apo-backup\<endpoint>.json`), and a file
/// naming `SOFTWARE\Classes\CLSID\{someone else}` would otherwise be a
/// delete-anything primitive behind one UAC prompt.
pub fn vet_com_keys(keys: &[String], clsid: &str) -> Result<(), String> {
    let root = clsid_root(clsid).to_ascii_lowercase();
    for key in keys {
        let k = key.trim().to_ascii_lowercase();
        let in_scope = k == root || k.starts_with(&format!(r"{root}\"));
        if !in_scope || k.contains("..") {
            return Err(format!(r"{key} is outside HKLM\{}", clsid_root(clsid)));
        }
    }
    Ok(())
}

/// Refuse an endpoint id that is not a `{8-4-4-4-12}` GUID, so nothing can
/// steer the FX-store writes out of the MMDevices subtree by path tricks.
pub fn vet_endpoint_guid(guid: &str) -> Result<(), String> {
    let inner = guid.strip_prefix('{').and_then(|s| s.strip_suffix('}')).unwrap_or("");
    let groups: Vec<&str> = inner.split('-').collect();
    let shape_ok = groups.len() == 5
        && [8usize, 4, 4, 4, 12].iter().zip(&groups).all(|(n, g)| g.len() == *n)
        && inner.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if shape_ok {
        Ok(())
    } else {
        Err(format!("{guid} is not an endpoint GUID"))
    }
}

/// Refuse an APO install target that is not an active render endpoint.
/// `devices` is a read-only MMDevice enumeration of *active* endpoints, so
/// an unplugged/disabled output and a recording device both fail here — the
/// helper never writes an FX store for something Windows is not rendering to.
pub fn vet_apo_target(guid: &str, devices: &crate::share::AudioDevices) -> Result<(), String> {
    vet_endpoint_guid(guid)?;
    let is = |list: &[crate::share::AudioDevice]| {
        list.iter().any(|d| crate::hardware::fx_guid_of(&d.id).eq_ignore_ascii_case(guid))
    };
    if is(&devices.render) {
        Ok(())
    } else if is(&devices.capture) {
        Err(format!("{guid} is a recording device, not an output"))
    } else {
        Err(format!("{guid} is not an active output on this PC"))
    }
}

impl Request {
    /// Everything about a request that can be judged without touching the
    /// machine: format, freshness, and that it asks for something at all.
    pub fn vet(&self, now: u64) -> Result<(), String> {
        if self.version != REQUEST_VERSION {
            return Err(format!(
                "request version {} is not {REQUEST_VERSION} — refusing a request from another build",
                self.version
            ));
        }
        if self.ops.is_empty() {
            return Err("the request asks for nothing".into());
        }
        for op in &self.ops {
            if let Some(ep) = op.endpoint() {
                vet_endpoint_guid(ep)?;
            }
        }
        if self.nonce.is_empty()
            || !self.nonce.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err("malformed request id".into());
        }
        let age = now.saturating_sub(self.created_at);
        if self.created_at > now.saturating_add(60) || age > MAX_REQUEST_AGE_SECS {
            return Err(format!(
                "the request is {age}s old — stale requests are refused (limit {MAX_REQUEST_AGE_SECS}s)"
            ));
        }
        Ok(())
    }
}

/// Where request and result files live: under the data root, so a run with
/// `--data-dir` stays in its own sandbox and the uninstaller's data step
/// already covers them.
pub fn elevate_dir(paths: &Paths) -> PathBuf {
    paths.data_dir().join("elevate")
}

fn request_path(paths: &Paths, nonce: &str) -> PathBuf {
    elevate_dir(paths).join(format!("{nonce}.request.json"))
}

fn response_path(paths: &Paths, nonce: &str) -> PathBuf {
    elevate_dir(paths).join(format!("{nonce}.result.json"))
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A request id with no dependency on a RNG crate: time plus the pid plus the
/// address of a stack local is plenty for "two requests never collide".
fn make_nonce() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let local = 0u8;
    format!("{:x}-{:x}-{:x}", t, std::process::id(), &local as *const u8 as usize)
}

// ---------------------------------------------------------------------------
// The dry run the user reads *before* the prompt.
// ---------------------------------------------------------------------------

/// Exactly what one op will change, in the same shape the uninstall card
/// already renders — because for the two uninstall ops it *is* that listing,
/// filtered to the one step and rendered by [`crate::uninstall::Plan::lines`].
///
/// Read-only: the APO listing reads the live FX store, the camera listing is
/// a pure planner call, and neither writes anything.
#[cfg(windows)]
pub fn plan_lines(paths: &Paths, op: &ElevatedOp) -> Vec<String> {
    let mut lines = match op {
        ElevatedOp::InstallApo { endpoint } => {
            crate::audio_apo::install_dry_run(&paths.apo_backup_dir(), endpoint.as_deref())
        }
        ElevatedOp::InstallCamera => crate::vdevice::camera_dry_run(),
        ElevatedOp::UninstallApo { endpoint } => {
            let mut l = uninstall_step_lines(paths, crate::uninstall::StepKind::RestoreApo);
            if let Some(ep) = endpoint {
                l.retain(|line| line.contains(ep.as_str()));
                if l.is_empty() {
                    l.push(format!("{ep}: no install backup — nothing to restore"));
                }
            }
            l
        }
        ElevatedOp::UninstallCamera => {
            uninstall_step_lines(paths, crate::uninstall::StepKind::RemoveVcam)
        }
        ElevatedOp::AllowFirewall => match crate::firewall::share_program() {
            Ok(program) => crate::firewall::install_dry_run(&program),
            Err(e) => vec![format!("could not locate relay-share.exe: {e:#}")],
        },
        ElevatedOp::RemoveFirewall => {
            uninstall_step_lines(paths, crate::uninstall::StepKind::RemoveFirewallRule)
        }
    };
    lines.push(String::new());
    lines.push(
        "Windows will ask for permission before any of this happens. Decline and nothing on this PC changes."
            .into(),
    );
    lines
}

#[cfg(not(windows))]
pub fn plan_lines(_paths: &Paths, _op: &ElevatedOp) -> Vec<String> {
    vec!["Elevated installs are Windows-only.".into()]
}

/// The uninstall plan, narrowed to one step kind and rendered by the
/// uninstaller's own renderer. One source of truth for both cards.
#[cfg(windows)]
fn uninstall_step_lines(paths: &Paths, kind: crate::uninstall::StepKind) -> Vec<String> {
    let full = crate::uninstall::plan(paths, true);
    let steps: Vec<_> = full.steps.into_iter().filter(|s| s.kind == kind).collect();
    let narrowed = crate::uninstall::Plan { steps, keep_data: true };
    // `lines()` appends the data-folder sentence, which belongs to the whole
    // uninstall rather than to one component step.
    let mut lines = narrowed.lines();
    lines.truncate(lines.len().saturating_sub(2));
    lines
}

// ---------------------------------------------------------------------------
// Launching (core side) and executing (helper side).
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub use imp::{execute_request, helper_path, run, run_request_file};

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::{anyhow, Context, Result};
    use tracing::{info, warn};

    /// The helper beside the running binary. Absent in a `cargo build` tree
    /// that has not built it, which is a clear error rather than a silent
    /// fallback — there is no unelevated way to do this work.
    pub fn helper_path() -> Result<PathBuf> {
        let me = std::env::current_exe().context("locating the running binary")?;
        crate::launcher::sibling_of(&me, HELPER_EXE)
            .filter(|p| p.exists())
            .with_context(|| format!("{HELPER_EXE} not found next to {}", me.display()))
    }

    /// Ask for elevation and run `ops`. Blocks until the helper exits.
    ///
    /// Returns [`LaunchError::Declined`] — and leaves the machine exactly as
    /// it was — when the user dismisses the UAC prompt.
    pub fn run(paths: &Paths, ops: &[ElevatedOp]) -> Result<Response, LaunchError> {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::WaitForSingleObject;
        use windows::Win32::UI::Shell::{
            ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        if ops.is_empty() {
            return Err(anyhow!("no operations requested").into());
        }
        let helper = helper_path().map_err(LaunchError::Other)?;
        let nonce = make_nonce();
        let dir = elevate_dir(paths);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))
            .map_err(LaunchError::Other)?;

        let request = Request {
            version: REQUEST_VERSION,
            nonce: nonce.clone(),
            created_at: now_secs(),
            data_dir: paths.root().to_path_buf(),
            ops: ops.to_vec(),
        };
        let req_path = request_path(paths, &nonce);
        let res_path = response_path(paths, &nonce);
        let _ = std::fs::remove_file(&res_path);
        std::fs::write(
            &req_path,
            serde_json::to_vec_pretty(&request).map_err(|e| LaunchError::Other(e.into()))?,
        )
        .with_context(|| format!("writing {}", req_path.display()))
        .map_err(LaunchError::Other)?;

        let params = format!("--request \"{}\"", req_path.display());
        let file = HSTRING::from(helper.as_os_str());
        let params_w = HSTRING::from(params);
        let verb = HSTRING::from("runas");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params_w.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };

        info!(ops = ?ops, "asking for elevation");
        // SAFETY: `info` is fully initialised and every HSTRING outlives the
        // call. This is the UAC prompt; it cannot be suppressed from here.
        let launched = unsafe { ShellExecuteExW(&mut info) };
        if let Err(e) = launched {
            let _ = std::fs::remove_file(&req_path);
            return Err(if e.code().0 as u32 & 0xFFFF == ERROR_CANCELLED.0 {
                info!("UAC prompt declined; nothing was changed");
                LaunchError::Declined
            } else {
                LaunchError::Other(anyhow::Error::from(e).context("starting the elevated helper"))
            });
        }

        if !info.hProcess.is_invalid() {
            // 10 minutes: a UAC prompt can sit on screen for a long time, but
            // a helper that never exits must not wedge the core for ever.
            const TEN_MINUTES_MS: u32 = 10 * 60 * 1000;
            // SAFETY: the handle came from ShellExecuteExW with NOCLOSEPROCESS
            // and is closed exactly once, here.
            unsafe {
                let w = WaitForSingleObject(info.hProcess, TEN_MINUTES_MS);
                if w != WAIT_OBJECT_0 {
                    warn!("the elevated helper did not exit within 10 minutes");
                }
                let _ = CloseHandle(info.hProcess);
            }
        }

        let response = std::fs::read(&res_path)
            .map_err(|e| anyhow::Error::from(e).context("the elevated helper wrote no result"))
            .and_then(|b| Ok(serde_json::from_slice::<Response>(&b)?))
            .map_err(LaunchError::Other);
        let _ = std::fs::remove_file(&req_path);
        let _ = std::fs::remove_file(&res_path);
        let response = response?;
        if response.nonce != nonce {
            return Err(LaunchError::Other(anyhow!("the helper answered a different request")));
        }
        for r in &response.results {
            info!(op = r.op.as_str(), "{}", r.line());
        }
        Ok(response)
    }

    /// Helper side: read the request named on the command line, run it, write
    /// the result next to it. Never propagates an error to the caller without
    /// having written a result the core can show.
    pub fn run_request_file(path: &Path) -> Result<Response> {
        let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let request: Request = serde_json::from_slice(&raw)
            .with_context(|| format!("{} is not a Relay elevation request", path.display()))?;
        let response = execute_request(&request, path);
        let paths = Paths::at(&request.data_dir);
        let out = response_path(&paths, &request.nonce);
        if let Some(dir) = out.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&out, serde_json::to_vec_pretty(&response)?)
            .with_context(|| format!("writing {}", out.display()))?;
        Ok(response)
    }

    /// Run a vetted request. Every refusal happens before a write.
    pub fn execute_request(request: &Request, from: &Path) -> Response {
        let paths = Paths::at(&request.data_dir);
        let elevated = crate::processes::is_elevated();
        let mut response = Response {
            version: REQUEST_VERSION,
            nonce: request.nonce.clone(),
            elevated,
            results: Vec::new(),
        };

        // One refusal reason for the whole request: format, freshness, the
        // file's own location, and the token we are running with.
        let gate = request
            .vet(now_secs())
            .err()
            .or_else(|| vet_request_location(request, from).err())
            .or_else(|| {
                (!elevated)
                    .then(|| "the helper is not running elevated — it refuses to try".to_string())
            });
        if let Some(reason) = gate {
            warn!(%reason, "refusing the elevation request");
            response.results = request
                .ops
                .iter()
                .map(|op| OpResult {
                    op: op.clone(),
                    outcome: OpOutcome::Refused { reason: reason.clone() },
                })
                .collect();
            return response;
        }

        for op in &request.ops {
            let outcome = run_op(&paths, op);
            match &outcome {
                OpOutcome::Refused { reason } => warn!(op = op.as_str(), %reason, "refused"),
                OpOutcome::Failed { error } => warn!(op = op.as_str(), %error, "failed"),
                _ => info!(op = op.as_str(), "ran"),
            }
            response.results.push(OpResult { op: op.clone(), outcome });
        }
        response
    }

    /// The request file has to be the one the core wrote, in the elevate
    /// directory of the data root it names. Stops a request dropped anywhere
    /// else on disk from being fed to the helper by hand.
    fn vet_request_location(request: &Request, from: &Path) -> Result<(), String> {
        let expected = request_path(&Paths::at(&request.data_dir), &request.nonce);
        let same = std::fs::canonicalize(from)
            .ok()
            .zip(std::fs::canonicalize(&expected).ok())
            .map(|(a, b)| a == b)
            .unwrap_or(false);
        if same {
            Ok(())
        } else {
            Err(format!("the request is not at {}", expected.display()))
        }
    }

    /// Arm one live-write gate for exactly one call, then disarm it.
    ///
    /// The gates exist so nothing writes HKLM by accident; the helper is the
    /// one sanctioned installer context, and even here the window is a single
    /// vetted call rather than the process lifetime.
    fn armed<T>(var: &str, f: impl FnOnce() -> T) -> T {
        // SAFETY: the helper is single-threaded — it runs one request, in
        // order, with no background work — so no other thread can observe a
        // torn environment.
        unsafe { std::env::set_var(var, "1") };
        let out = f();
        // SAFETY: as above.
        unsafe { std::env::remove_var(var) };
        out
    }

    const APO_GATE: &str = "RELAY_APO_ALLOW_LIVE_WRITE";
    const VCAM_GATE: &str = "RELAY_VDEVICE_ALLOW_LIVE_WRITE";
    const FIREWALL_GATE: &str = crate::firewall::LIVE_WRITE_GATE;

    fn run_op(paths: &Paths, op: &ElevatedOp) -> OpOutcome {
        match op {
            ElevatedOp::InstallApo { endpoint } => install_apo(paths, endpoint.as_deref()),
            ElevatedOp::UninstallApo { endpoint: Some(ep) } => uninstall_apo(paths, ep),
            ElevatedOp::UninstallApo { endpoint: None } => uninstall_all_apo(paths),
            ElevatedOp::InstallCamera => install_camera(paths),
            ElevatedOp::UninstallCamera => uninstall_camera(paths),
            ElevatedOp::AllowFirewall => allow_firewall(paths),
            ElevatedOp::RemoveFirewall => remove_firewall(paths),
        }
    }

    /// Add the inbound Allow rule. Like every other op here, the helper
    /// re-derives the target from its *own* directory — the request carries
    /// no path — so a tampered request cannot point a firewall rule at
    /// somebody else's binary.
    fn allow_firewall(paths: &Paths) -> OpOutcome {
        let program = match crate::firewall::share_program() {
            Ok(p) => p,
            Err(e) => return OpOutcome::Failed { error: format!("{e:#}") },
        };
        if !program.exists() {
            return OpOutcome::Refused { reason: format!("{} is not on disk", program.display()) };
        }
        let before = crate::firewall::status(&program);
        if before.verdict == crate::firewall::Verdict::Allowed && before.blocking_rules == 0 {
            return OpOutcome::Skipped { reason: "Relay is already allowed through".into() };
        }
        match armed(FIREWALL_GATE, || crate::firewall::install_live(paths, &program)) {
            Ok(after) => OpOutcome::Done {
                detail: format!(
                    "{} allowed on private and domain networks{}",
                    program.display(),
                    if before.blocking_rules > 0 {
                        format!(" ({} blocking rule(s) removed)", before.blocking_rules)
                    } else {
                        String::new()
                    }
                ) + if after.verdict.can_share() {
                    ""
                } else {
                    " — but still not reachable"
                },
            },
            Err(e) => OpOutcome::Failed { error: format!("{e:#}") },
        }
    }

    fn remove_firewall(paths: &Paths) -> OpOutcome {
        let recorded = crate::firewall::load_record(&paths.firewall_file()).ok().flatten();
        let program = crate::firewall::share_program().ok();
        let present =
            program.as_ref().map(|p| crate::firewall::status(p).rule_present).unwrap_or(false);
        if recorded.is_none() && !present {
            return OpOutcome::Skipped { reason: "Relay added no firewall rule".into() };
        }
        match armed(FIREWALL_GATE, || crate::firewall::uninstall_live(paths)) {
            Ok(n) => OpOutcome::Done { detail: format!("{n} rule(s) removed") },
            Err(e) => OpOutcome::Failed { error: format!("{e:#}") },
        }
    }

    fn install_apo(paths: &Paths, endpoint: Option<&str>) -> OpOutcome {
        // Resolve the target (named, else the default output), then vet it
        // against a read-only enumeration of active endpoints before anything
        // else happens: a GUID, an output, and one Windows is rendering to.
        let endpoint = match endpoint {
            Some(e) => e.to_owned(),
            None => match relay_audio::sessions::default_render_endpoint_guid() {
                Ok(g) => g,
                Err(_) => return OpOutcome::Failed { error: "no default render endpoint".into() },
            },
        };
        let devices = crate::hardware::probe_win::list_audio_devices();
        if let Err(reason) = vet_apo_target(&endpoint, &devices) {
            return OpOutcome::Refused { reason };
        }
        if crate::audio_apo::fx_has_relay(&endpoint) {
            return OpOutcome::Skipped {
                reason: format!("the APO is already registered on {endpoint}"),
            };
        }
        // `install_live` re-derives the plan and the DLL path from this
        // process' own directory, writes the backup first, and only then
        // calls the gated live registry.
        match armed(APO_GATE, || {
            crate::audio_apo::install_live(&paths.apo_backup_dir(), Some(&endpoint))
        }) {
            Ok(ep) => OpOutcome::Done { detail: format!("registered on {ep}") },
            Err(e) => OpOutcome::Failed { error: format!("{e:#}") },
        }
    }

    /// Vet what one backup file will drive before handing it to the registry.
    fn vet_backup(dir: &Path, endpoint: &str) -> Result<(), OpOutcome> {
        let backup_file = crate::audio_apo::backup_file(dir, endpoint);
        match std::fs::read(&backup_file).map_err(|e| e.to_string()).and_then(|b| {
            serde_json::from_slice::<relay_apo::fxstore::Backup>(&b).map_err(|e| e.to_string())
        }) {
            Ok(backup) => {
                if !backup.endpoint_guid.eq_ignore_ascii_case(endpoint) {
                    return Err(OpOutcome::Refused {
                        reason: format!("the backup for {endpoint} names another endpoint"),
                    });
                }
                vet_endpoint_guid(&backup.endpoint_guid)
                    .and_then(|()| vet_com_keys(&backup.com_keys, relay_apo::ids::APO_CLSID))
                    .map_err(|reason| OpOutcome::Refused { reason })
            }
            Err(error) => Err(OpOutcome::Failed { error: format!("reading the backup: {error}") }),
        }
    }

    fn uninstall_apo(paths: &Paths, endpoint: &str) -> OpOutcome {
        let dir = paths.apo_backup_dir();
        if let Err(reason) = vet_endpoint_guid(endpoint) {
            return OpOutcome::Refused { reason };
        }
        // No "active" check here: an unplugged output must still be
        // restorable. The backup is the authority, and it is vetted.
        if !crate::audio_apo::backup_file(&dir, endpoint).exists() {
            return OpOutcome::Skipped { reason: format!("no install backup for {endpoint}") };
        }
        if let Err(outcome) = vet_backup(&dir, endpoint) {
            return outcome;
        }
        match armed(APO_GATE, || crate::audio_apo::uninstall_live(&dir, Some(endpoint))) {
            Ok(ep) => OpOutcome::Done { detail: format!("{ep} restored to its pre-install state") },
            Err(e) => OpOutcome::Failed { error: format!("{e:#}") },
        }
    }

    fn uninstall_all_apo(paths: &Paths) -> OpOutcome {
        let dir = paths.apo_backup_dir();
        let recorded = crate::audio_apo::recorded_endpoints(&dir);
        if recorded.is_empty() {
            return OpOutcome::Skipped { reason: "no install backup for any output".into() };
        }
        for ep in &recorded {
            if let Err(outcome) = vet_backup(&dir, ep) {
                return outcome;
            }
        }
        match armed(APO_GATE, || crate::audio_apo::uninstall_all_live(&dir)) {
            Ok(eps) => OpOutcome::Done {
                detail: format!("{} restored to the pre-install state", eps.join(", ")),
            },
            Err(e) => OpOutcome::Failed { error: format!("{e:#}") },
        }
    }

    fn install_camera(paths: &Paths) -> OpOutcome {
        // Vet the keys the planner will actually create, against our own
        // CLSID, using this process' own DLL — nothing from the request.
        let planned: Vec<String> = crate::vdevice::camera_plan_keys();
        if let Err(reason) = vet_com_keys(&planned, relay_vdevice::reg::VCAM_CLSID) {
            return OpOutcome::Refused { reason };
        }
        match armed(VCAM_GATE, || crate::vdevice::install_camera_live(paths)) {
            Ok(()) => OpOutcome::Done { detail: "\"Relay Camera\" registered".into() },
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("already recorded") {
                    OpOutcome::Skipped { reason: "the camera is already registered".into() }
                } else {
                    OpOutcome::Failed { error: msg }
                }
            }
        }
    }

    fn uninstall_camera(paths: &Paths) -> OpOutcome {
        let recorded = crate::vdevice::recorded_camera_keys(paths);
        if recorded.is_empty() {
            return OpOutcome::Skipped { reason: "no camera registration is recorded".into() };
        }
        if let Err(reason) = vet_com_keys(&recorded, relay_vdevice::reg::VCAM_CLSID) {
            return OpOutcome::Refused { reason };
        }
        match armed(VCAM_GATE, || crate::vdevice::uninstall_camera_live(paths)) {
            Ok(()) => OpOutcome::Done { detail: format!("{} keys removed", recorded.len()) },
            Err(e) => OpOutcome::Failed { error: format!("{e:#}") },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_names_round_trip() {
        for op in [
            ElevatedOp::InstallApo { endpoint: None },
            ElevatedOp::UninstallApo { endpoint: None },
            ElevatedOp::InstallCamera,
            ElevatedOp::UninstallCamera,
            ElevatedOp::AllowFirewall,
            ElevatedOp::RemoveFirewall,
        ] {
            assert_eq!(ElevatedOp::parse(op.as_str()), Some(op));
        }
        assert_eq!(ElevatedOp::parse("delete-everything"), None);
    }

    /// The op set is the allow-list: anything else fails to deserialise, so
    /// a hand-written request cannot name work the helper does not implement.
    #[test]
    fn an_unknown_op_cannot_be_deserialised() {
        let raw = r#"{"version":1,"nonce":"a","created_at":0,"data_dir":"C:\\x",
                      "ops":["run_this_command"]}"#;
        assert!(serde_json::from_str::<Request>(raw).is_err());
    }

    #[test]
    fn com_keys_outside_our_clsid_are_refused() {
        let ours = "{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}";
        assert!(vet_com_keys(&[clsid_root(ours)], ours).is_ok());
        assert!(vet_com_keys(&[format!(r"{}\InprocServer32", clsid_root(ours))], ours).is_ok());
        // Case differences are fine; the registry is case-insensitive.
        assert!(vet_com_keys(&[clsid_root(ours).to_lowercase()], ours).is_ok());

        for bad in [
            r"SOFTWARE\Classes\CLSID\{00000000-0000-0000-0000-000000000000}",
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
            r"SYSTEM\CurrentControlSet\Services",
            &format!(r"{}\..\{{other}}", clsid_root(ours)),
            // A prefix match must not be enough: a sibling key whose name
            // merely starts with ours is still somebody else's.
            &format!("{}-evil", clsid_root(ours)),
        ] {
            assert!(vet_com_keys(&[bad.to_string()], ours).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn endpoint_ids_must_be_guids() {
        assert!(vet_endpoint_guid("{f8ae226b-a4e3-45ab-97fc-3977dad232d1}").is_ok());
        for bad in
            ["", "{}", "..", r"{f8ae226b}\..\..\SYSTEM", "f8ae226b-a4e3-45ab-97fc-3977dad232d1"]
        {
            assert!(vet_endpoint_guid(bad).is_err(), "{bad} should be refused");
        }
    }

    const OUT: &str = "{f8ae226b-a4e3-45ab-97fc-3977dad232d1}";
    const MIC: &str = "{0a1b2c3d-a4e3-45ab-97fc-3977dad232d1}";

    fn devices() -> crate::share::AudioDevices {
        use crate::share::AudioDevice;
        crate::share::AudioDevices {
            render: vec![AudioDevice {
                id: format!("{{0.0.0.00000000}}.{OUT}"),
                name: "Headphones".into(),
                is_default: true,
            }],
            capture: vec![AudioDevice {
                id: format!("{{0.0.1.00000000}}.{MIC}"),
                name: "Mic".into(),
                is_default: true,
            }],
        }
    }

    #[test]
    fn apo_target_must_be_an_active_render_endpoint() {
        let d = devices();
        assert!(vet_apo_target(OUT, &d).is_ok());
        assert!(vet_apo_target(&OUT.to_uppercase(), &d).is_ok());
        // Bad GUID: refused before the enumeration is even consulted.
        assert!(vet_apo_target(r"{f8ae226b}\..\..\SYSTEM", &d)
            .unwrap_err()
            .contains("not an endpoint GUID"));
        // A recording endpoint is not an output.
        assert!(vet_apo_target(MIC, &d).unwrap_err().contains("recording device"));
        // Well-formed but not in the active list: unplugged, disabled or made up.
        let inactive = "{99999999-a4e3-45ab-97fc-3977dad232d1}";
        assert!(vet_apo_target(inactive, &d).unwrap_err().contains("not an active output"));
        assert!(vet_apo_target(OUT, &crate::share::AudioDevices::default()).is_err());
    }

    #[test]
    fn apo_ops_carry_only_an_endpoint_guid_on_the_wire() {
        let op = ElevatedOp::InstallApo { endpoint: Some(OUT.into()) };
        let v = serde_json::to_value(&op).unwrap();
        assert_eq!(v, serde_json::json!({ "install_apo": { "endpoint": OUT } }));
        assert_eq!(serde_json::from_value::<ElevatedOp>(v).unwrap(), op);
        // Endpoint omitted = default output (install) / every backup (uninstall).
        let u: ElevatedOp = serde_json::from_str(r#"{"uninstall_apo":{}}"#).unwrap();
        assert_eq!(u, ElevatedOp::UninstallApo { endpoint: None });
        // Unit ops keep their plain spelling.
        assert_eq!(serde_json::to_value(ElevatedOp::InstallCamera).unwrap(), "install_camera");
        // No smuggled registry path or DLL: unknown fields are just ignored
        // data — the helper never reads them — and the op is still only a GUID.
        let smuggled: ElevatedOp = serde_json::from_str(
            r#"{"install_apo":{"endpoint":null,"dll":"C:\\evil.dll","key":"SYSTEM"}}"#,
        )
        .unwrap();
        assert_eq!(smuggled, ElevatedOp::InstallApo { endpoint: None });
        assert_eq!(ElevatedOp::parse("install-apo").unwrap().with_endpoint(Some(OUT.into())), op);
        assert_eq!(
            ElevatedOp::InstallCamera.with_endpoint(Some(OUT.into())),
            ElevatedOp::InstallCamera
        );
    }

    #[test]
    fn a_request_naming_a_non_guid_endpoint_is_refused() {
        let bad =
            req(vec![ElevatedOp::UninstallApo { endpoint: Some(r"..\..\SYSTEM".into()) }], 1_000);
        assert!(bad.vet(1_000).unwrap_err().contains("not an endpoint GUID"));
        let good = req(vec![ElevatedOp::InstallApo { endpoint: Some(OUT.into()) }], 1_000);
        assert!(good.vet(1_000).is_ok());
        // A v1 request (pre-S42 shape) is refused on version alone.
        let mut v1 = good;
        v1.version = 1;
        assert!(v1.vet(1_000).is_err());
    }

    fn req(ops: Vec<ElevatedOp>, created_at: u64) -> Request {
        Request {
            version: REQUEST_VERSION,
            nonce: "abc-123".into(),
            created_at,
            data_dir: PathBuf::from(r"C:\tmp\relay"),
            ops,
        }
    }

    #[test]
    fn a_fresh_well_formed_request_passes() {
        assert!(req(vec![ElevatedOp::InstallApo { endpoint: None }], 1_000).vet(1_010).is_ok());
    }

    #[test]
    fn stale_future_empty_and_wrong_version_requests_are_refused() {
        // Stale: a request file left behind by a crash is dead on arrival.
        assert!(req(vec![ElevatedOp::InstallApo { endpoint: None }], 1_000)
            .vet(1_000 + MAX_REQUEST_AGE_SECS + 1)
            .is_err());
        // Dated in the future beyond clock slop.
        assert!(req(vec![ElevatedOp::InstallApo { endpoint: None }], 5_000).vet(1_000).is_err());
        // Asks for nothing.
        assert!(req(vec![], 1_000).vet(1_000).is_err());
        // Another build's format.
        let mut r = req(vec![ElevatedOp::InstallApo { endpoint: None }], 1_000);
        r.version = REQUEST_VERSION + 1;
        assert!(r.vet(1_000).is_err());
        // Path characters in the id, which is also a file name.
        let mut r = req(vec![ElevatedOp::InstallApo { endpoint: None }], 1_000);
        r.nonce = r"..\..\evil".into();
        assert!(r.vet(1_000).is_err());
    }

    #[test]
    fn a_response_reads_as_plain_english() {
        let r = Response {
            version: REQUEST_VERSION,
            nonce: "n".into(),
            elevated: true,
            results: vec![
                OpResult {
                    op: ElevatedOp::InstallApo { endpoint: None },
                    outcome: OpOutcome::Done { detail: "registered on {guid}".into() },
                },
                OpResult {
                    op: ElevatedOp::InstallCamera,
                    outcome: OpOutcome::Skipped { reason: "already registered".into() },
                },
            ],
        };
        assert!(r.ok() && r.changed_anything());
        assert!(r.lines()[0].contains("Install the endpoint audio processor: done"));
        assert!(r.lines()[1].contains("nothing to do"));

        let refused = Response {
            results: vec![OpResult {
                op: ElevatedOp::InstallApo { endpoint: None },
                outcome: OpOutcome::Refused { reason: "stale".into() },
            }],
            ..r.clone()
        };
        assert!(!refused.ok() && !refused.changed_anything());
    }

    #[test]
    fn a_response_from_an_unelevated_helper_is_never_ok() {
        let r = Response {
            version: REQUEST_VERSION,
            nonce: "n".into(),
            elevated: false,
            results: vec![],
        };
        assert!(!r.ok());
        assert!(r.lines().iter().any(|l| l.contains("nothing was changed")));
    }

    #[test]
    fn requests_and_results_live_under_the_data_root() {
        let paths = Paths::at(r"C:\tmp\relay");
        assert!(request_path(&paths, "n").starts_with(paths.data_dir()));
        assert!(response_path(&paths, "n").starts_with(paths.data_dir()));
        // …which is what makes the uninstaller's data step cover them.
        assert!(paths.data_paths().iter().any(|p| elevate_dir(&paths).starts_with(p)));
    }
}
