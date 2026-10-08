//! `relay-core` binary.
//!
//! ```text
//! relay-core run                 start the always-on service (headless by default)
//! relay-core status [--json]     human summary, or the raw state as JSON
//! relay-core restore             ask a running service to restore everything
//! relay-core shutdown            stop a running service (restores first)
//! relay-core autostart [on|off]  show or set start-at-login (HKCU Run key only)
//! relay-core uninstall [--dry-run] [--delete-data]   remove Relay from this PC
//! relay-core --data-dir DIR      override the data root (any subcommand)
//! relay-core --verbose           debug-level logging
//! relay-core --version           print the version and exit
//! ```
//!
//! This stays a console-subsystem binary. Building it for the GUI subsystem
//! would remove the brief console flash when the Run key or the installer
//! starts the service, but PowerShell does not capture output from a
//! GUI-subsystem process — `relay-core status` would print nothing into a
//! pipe, and the scripts and tests that read it would silently see empty
//! output. `hide_own_console` covers the flash instead; see the M7 plan's
//! Deferred for the two-binary split that would fix it properly.

use anyhow::Result;
use relay_core::config::{mutex_name, Paths};
use relay_core::instance::InstanceLock;
use relay_core::service::{Backends, Service};
use relay_core::{autostart, logging};

struct Args {
    cmd: String,
    arg: Option<String>,
    /// Third positional word: `elevate run <op>` is the only user of it.
    arg2: Option<String>,
    paths: Paths,
    json: bool,
    verbose: bool,
    /// `uninstall`: list what would be touched and exit.
    dry_run: bool,
    /// `uninstall`: no prompts, no console output beyond the report (the NSIS
    /// uninstaller drives it this way).
    silent: bool,
    /// `uninstall`: keep `%LOCALAPPDATA%\Relay`. Defaults to keeping it —
    /// deleting a user's profiles needs an explicit `--delete-data`.
    keep_data: bool,
    /// `uninstall`: only the HKLM component steps (the elevated phase).
    components_only: bool,
    /// `shutdown`: keep `active-stream.json`, so the next start resumes the
    /// share or receive. The installer stops the core this way: an update
    /// must not end what the user left running.
    keep_stream: bool,
    /// `apo` / `elevate`: target render endpoint GUID (S42). Default output
    /// when absent.
    endpoint: Option<String>,
    /// `apo uninstall`: restore every recorded endpoint.
    all: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut out = Args {
        cmd: String::new(),
        arg: None,
        arg2: None,
        paths: Paths::default_for_user()?,
        json: false,
        verbose: false,
        dry_run: false,
        silent: false,
        keep_data: true,
        components_only: false,
        keep_stream: false,
        endpoint: None,
        all: false,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => {
                let dir = args.next().ok_or_else(|| anyhow::anyhow!("--data-dir needs a path"))?;
                out.paths = Paths::at(dir);
            }
            "--json" => out.json = true,
            "--verbose" | "-v" => out.verbose = true,
            "--dry-run" => out.dry_run = true,
            "--silent" => out.silent = true,
            "--keep-data" => out.keep_data = true,
            "--delete-data" => out.keep_data = false,
            "--components-only" => out.components_only = true,
            "--keep-stream" => out.keep_stream = true,
            "--endpoint" => {
                let ep = args.next().ok_or_else(|| anyhow::anyhow!("--endpoint needs a GUID"))?;
                relay_core::elevate::vet_endpoint_guid(&ep).map_err(anyhow::Error::msg)?;
                out.endpoint = Some(ep);
            }
            "--all" => out.all = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("relay-core {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other if other.starts_with('-') => anyhow::bail!("unknown flag `{other}`\n{USAGE}"),
            other if out.cmd.is_empty() => out.cmd = other.to_string(),
            other if out.arg.is_none() => out.arg = Some(other.to_string()),
            other if out.arg2.is_none() => out.arg2 = Some(other.to_string()),
            other => anyhow::bail!("unexpected argument `{other}`\n{USAGE}"),
        }
    }
    if out.cmd.is_empty() {
        out.cmd = "run".into();
    }
    Ok(out)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    match args.cmd.as_str() {
        "run" => run(args),
        "autostart" => {
            logging::init_console(args.verbose);
            match args.arg.as_deref() {
                None => {
                    println!("{}", if autostart::is_enabled()? { "on" } else { "off" });
                }
                Some("on") => autostart::set(true)?,
                Some("off") => autostart::set(false)?,
                Some(other) => anyhow::bail!("autostart takes `on` or `off`, not `{other}`"),
            }
            Ok(())
        }
        #[cfg(windows)]
        "status" | "restore" | "shutdown" | "share-start" | "share-stop" => {
            logging::init_console(args.verbose);
            let path = relay_core::resilience::path_in(&args.paths);
            let kept = (args.cmd == "shutdown" && args.keep_stream)
                .then(|| std::fs::read(&path).ok())
                .flatten();
            let result = client_command(&args.cmd, args.arg.as_deref(), args.json);
            if let Some(bytes) = kept {
                // A clean shutdown clears the record on its way out; put it
                // back once the core is gone.
                for _ in 0..50 {
                    if !path.exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                std::fs::write(&path, bytes)?;
            }
            result
        }
        // Direct (no running service needed): the VM runbook drives these
        // from an elevated prompt. The livereg write gate applies.
        #[cfg(windows)]
        "apo" => {
            logging::init_console(args.verbose);
            let dir = args.paths.apo_backup_dir();
            let ep = args.endpoint.as_deref();
            match args.arg.as_deref() {
                None | Some("status") => {
                    let s = relay_core::audio_apo::apo_status(&dir);
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&s)?);
                        return Ok(());
                    }
                    for e in s
                        .endpoints
                        .iter()
                        .filter(|e| ep.is_none_or(|w| w.eq_ignore_ascii_case(&e.endpoint)))
                    {
                        println!(
                            "{}{} {}\n  installed: {}  backup: {}  params section: {}",
                            e.endpoint,
                            if e.is_default { " (default)" } else { "" },
                            e.name,
                            e.installed,
                            if e.backed_up { "yes" } else { "no" },
                            if e.running { "reachable" } else { "not reachable" },
                        );
                    }
                    println!(
                        "audio-engine registration (HKLM\\{}): {}",
                        relay_apo::ids::audio_engine_key(relay_apo::ids::APO_CLSID),
                        match s.audio_engine {
                            relay_core::audio_apo::AudioEngineRegistration::Registered => "present",
                            relay_core::audio_apo::AudioEngineRegistration::Mismatch =>
                                "present, values differ from this build",
                            relay_core::audio_apo::AudioEngineRegistration::Missing =>
                                "missing (audiodg will not load the APO)",
                        }
                    );
                }
                Some("install") => {
                    let ep = relay_core::audio_apo::install_live(&dir, ep)?;
                    println!("registered on {ep}; restart audiosrv to pick it up");
                }
                Some("uninstall") if args.all => {
                    let eps = relay_core::audio_apo::uninstall_all_live(&dir)?;
                    println!("restored {} endpoint(s) to their pre-install state", eps.len());
                }
                Some("uninstall") => {
                    let ep = relay_core::audio_apo::uninstall_live(&dir, ep)?;
                    println!("restored {ep} to its pre-install state");
                }
                Some(other) => {
                    anyhow::bail!("apo takes `status`, `install` or `uninstall`, not `{other}`")
                }
            }
            Ok(())
        }
        // Read-only by default. `allow` / `remove` change firewall policy and
        // so are gated exactly like `apo` and `vdevice`: the elevated helper
        // is the sanctioned caller, and these exist for the VM runbook.
        #[cfg(windows)]
        "firewall" => {
            logging::init_console(args.verbose);
            use relay_core::firewall;
            let program = firewall::share_program()?;
            match args.arg.as_deref() {
                None | Some("status") => {
                    let s = firewall::status(&program);
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&s)?);
                    } else {
                        println!("program:  {}", s.program);
                        println!("verdict:  {}", s.verdict.summary());
                        println!("our rule: {}", if s.rule_present { "present" } else { "absent" });
                        println!("blocking: {} rule(s)", s.blocking_rules);
                        println!("stale:    {} rule(s) for another copy of the exe", s.stale_rules);
                        println!(
                            "policy:   active profiles {:#x}, firewall {}, inbound {}",
                            s.policy.active_profiles,
                            if s.policy.enabled { "on" } else { "off" },
                            if s.policy.default_inbound_block { "blocked" } else { "allowed" },
                        );
                        if s.unknown {
                            println!(
                                "(firewall state could not be read; values above are defaults)"
                            );
                        }
                    }
                }
                Some("dry-run") => {
                    for line in firewall::install_dry_run(&program) {
                        println!("{line}");
                    }
                }
                Some("allow") => {
                    let s = firewall::install_live(&args.paths, &program)?;
                    println!("{}", s.verdict.summary());
                }
                Some("remove") => {
                    let n = firewall::uninstall_live(&args.paths)?;
                    println!("removed {n} rule(s) named \"{}\"", firewall::RULE_NAME);
                }
                Some(other) => anyhow::bail!(
                    "firewall takes `status`, `dry-run`, `allow` or `remove`, not `{other}`"
                ),
            }
            Ok(())
        }
        // Direct as well: registration wants an elevated prompt, and the
        // consent screen may not exist yet on a fresh machine. Same gates.
        #[cfg(windows)]
        "vdevice" => {
            logging::init_console(args.verbose);
            match args.arg.as_deref() {
                None | Some("status") => {
                    let s = relay_core::vdevice::status(&args.paths)?;
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&s)?);
                    } else {
                        println!(
                            "windows build: {} (camera path: {})",
                            s.windows_build.map_or("unknown".into(), |b| b.to_string()),
                            match s.camera_path {
                                Some(relay_core::vdevice::CameraPath::FrameServer) => {
                                    "frame server, HKLM"
                                }
                                Some(relay_core::vdevice::CameraPath::DirectShow) => {
                                    "DirectShow filter, per user"
                                }
                                None => "none",
                            },
                        );
                        println!("camera registered: {}", s.camera_registered);
                        println!(
                            "consent: {}",
                            s.consent.as_ref().map_or("not decided".into(), |c| format!(
                                "camera {} / microphone {} ({})",
                                c.camera, c.microphone, c.decided_at
                            )),
                        );
                        println!(
                            "obs virtualcam: {}",
                            s.obs_virtualcam.as_deref().unwrap_or("not detected"),
                        );
                        if s.mic_targets.is_empty() {
                            println!("mic route targets: none (install VB-Cable for the interim virtual mic)");
                        } else {
                            for t in &s.mic_targets {
                                println!("mic route target: {} [{:?}]", t.name, t.kind);
                            }
                        }
                        println!("elevated: {}", s.elevated);
                    }
                }
                Some("dry-run") => {
                    for line in relay_core::vdevice::camera_dry_run() {
                        println!("{line}");
                    }
                }
                Some("consent-camera") => {
                    // CLI convenience for the runbook; the UI screen is the
                    // real flow. Grants the camera opt-in only.
                    let c = relay_core::vdevice::set_consent(&args.paths, false, true, false)?;
                    println!("recorded: camera {} / microphone {}", c.camera, c.microphone);
                }
                Some("install") => {
                    relay_core::vdevice::install_vcam(&args.paths)?;
                    println!("Relay Camera media source registered");
                }
                Some("uninstall") => {
                    relay_core::vdevice::uninstall_vcam(&args.paths)?;
                    println!("Relay Camera media source removed; installed.json cleared");
                }
                Some(other) => anyhow::bail!(
                    "vdevice takes `status`, `dry-run`, `consent-camera`, `install` or `uninstall`, not `{other}`"
                ),
            }
            Ok(())
        }
        // The elevated helper, driven from a shell. `plan` is read-only and
        // prints the same listing the Settings card shows before the prompt;
        // `run` raises the UAC prompt and reports what came back.
        #[cfg(windows)]
        "elevate" => {
            logging::init_console(args.verbose);
            use relay_core::elevate::{self, ElevatedOp};
            let (sub, op) = match (args.arg.as_deref(), args.arg2.as_deref()) {
                (Some("plan"), Some(op)) | (Some("run"), Some(op)) => {
                    (args.arg.clone().unwrap(), op)
                }
                _ => anyhow::bail!(
                    "elevate takes `plan <op>` or `run <op>`, where <op> is one of                      install-apo, uninstall-apo, install-camera, uninstall-camera"
                ),
            };
            let Some(op) = ElevatedOp::parse(op).map(|o| o.with_endpoint(args.endpoint.clone()))
            else {
                anyhow::bail!(
                    "`{op}` is not one of install-apo, uninstall-apo, install-camera,                      uninstall-camera"
                )
            };
            if sub == "plan" {
                for line in elevate::plan_lines(&args.paths, &op) {
                    println!("{line}");
                }
                return Ok(());
            }
            match elevate::run(&args.paths, &[op]) {
                Ok(response) => {
                    for line in response.lines() {
                        println!("{line}");
                    }
                    if !response.ok() {
                        std::process::exit(1);
                    }
                }
                Err(elevate::LaunchError::Declined) => {
                    println!(
                        "Nothing on this PC was changed. You declined the Windows permission prompt."
                    );
                }
                Err(elevate::LaunchError::Other(e)) => return Err(e),
            }
            Ok(())
        }
        // The uninstaller's engine. `--dry-run` is read-only and is what the
        // Settings "what we installed" card shows, so the listing the user
        // sees before uninstalling is generated by the same code that does it.
        "uninstall" => {
            logging::init_console(args.verbose);
            uninstall(args)
        }
        other => anyhow::bail!("unknown command `{other}`\n{USAGE}"),
    }
}

/// `relay-core uninstall [--dry-run] [--keep-data|--delete-data] [--silent]`
///
/// Order and gating live in `uninstall.rs`; this is presentation plus the one
/// UAC round. Exit code is non-zero only when a step actually failed —
/// "needs elevation" after a declined prompt is reported, not fatal, so a
/// user who says no to UAC still gets the rest of Relay removed.
fn uninstall(args: Args) -> Result<()> {
    use relay_core::uninstall as un;

    let plan = un::plan(&args.paths, args.keep_data);
    if args.dry_run {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            for line in plan.lines() {
                println!("{line}");
            }
        }
        return Ok(());
    }

    // `--components-only` runs just the component steps, for a runbook that
    // is already in an elevated shell. It must not recurse, and it must not
    // delete the data root (the per-user phase owns that, so a declined UAC
    // prompt cannot lose the user's profiles).
    if args.components_only {
        let report = un::execute(&args.paths, &plan);
        print_report(&report, args.json, args.silent)?;
        return finish(&report);
    }

    let mut report = un::execute(&args.paths, &plan);
    // Anything left needing HKLM: one prompt, then re-probe so the report
    // reflects what the elevated child actually managed.
    if report.steps.iter().any(|s| matches!(s.outcome, un::Outcome::NeedsElevation)) {
        if !args.silent {
            println!("Two components were registered machine-wide; Windows will ask for permission to remove them.");
        }
        match un::finish_elevated(&args.paths) {
            Ok(response) => {
                if !args.silent {
                    for line in response.lines() {
                        println!("{line}");
                    }
                }
                let rest = un::plan(&args.paths, args.keep_data);
                let redo = un::execute(&args.paths, &rest);
                merge_elevated(&mut report, redo);
            }
            Err(e) => {
                eprintln!("elevated phase did not run: {e}");
                eprintln!(
                    "run `relay-core uninstall` from an administrator prompt to finish removing them."
                );
            }
        }
    }
    print_report(&report, args.json, args.silent)?;
    finish(&report)
}

/// Replace each pending-elevation step with what the elevated child reported
/// for the same target.
fn merge_elevated(report: &mut relay_core::uninstall::Report, redo: relay_core::uninstall::Report) {
    use relay_core::uninstall::Outcome;
    for step in &mut report.steps {
        if !matches!(step.outcome, Outcome::NeedsElevation) {
            continue;
        }
        // The elevated pass re-probed, so a step that is now gone succeeded.
        match redo.steps.iter().find(|s| s.kind == step.kind && s.target == step.target) {
            Some(s) => step.outcome = s.outcome.clone(),
            None => step.outcome = Outcome::Done,
        }
    }
}

fn print_report(report: &relay_core::uninstall::Report, json: bool, silent: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else if !silent {
        for line in report.lines() {
            println!("{line}");
        }
    }
    Ok(())
}

fn finish(report: &relay_core::uninstall::Report) -> Result<()> {
    use relay_core::uninstall::Outcome;
    let failed: Vec<&str> = report
        .steps
        .iter()
        .filter_map(|s| match &s.outcome {
            Outcome::Failed { error } => Some(error.as_str()),
            _ => None,
        })
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} uninstall step(s) failed: {}", failed.len(), failed.join("; "))
    }
}

fn run(args: Args) -> Result<()> {
    let Some(_lock) = InstanceLock::acquire(&mutex_name())? else {
        println!("relay-core is already running");
        return Ok(());
    };
    args.paths.ensure()?;
    logging::init_service(&args.paths.log_file(), args.verbose)?;
    // S44: the audio-protection switch is a deliberate, persistent machine
    // setting, so a restart keeps it — but never silently: say so in the log
    // (Settings and the uninstall listing say so on screen).
    let dg = relay_core::audiodg::record_file(&args.paths.apo_backup_dir());
    if let Ok(Some(rec)) = relay_core::audiodg::load_record(&dg) {
        tracing::info!(
            prior = ?rec.prior,
            "Windows audio protection is off at the user's request (DisableProtectedAudioDG); \
             turning it back on in Settings or uninstalling restores the recorded state"
        );
    }
    // S38: a panic leaves a record the next start can name, not just a
    // stderr line nobody was watching.
    relay_core::crash::install_panic_hook(relay_core::crash::dir(&args.paths), "relay-core");
    hide_own_console();
    Service::run(args.paths, Backends::from_env())
}

/// Headless by default: when this process was given a fresh console (started
/// from Explorer, the Run key or the installer rather than a terminal) hide
/// that window. `GetConsoleProcessList` returning 1 means the console is ours
/// alone, so hiding it cannot take a shell's window with it.
#[cfg(windows)]
fn hide_own_console() {
    use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    let mut pids = [0u32; 2];
    // SAFETY: plain queries on our own console.
    unsafe {
        if GetConsoleProcessList(&mut pids) == 1 {
            let hwnd = GetConsoleWindow();
            if !hwnd.is_invalid() {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

#[cfg(not(windows))]
fn hide_own_console() {}

const USAGE: &str = "\
relay-core [--data-dir DIR] [--verbose] [run|status [--json]|restore|shutdown|autostart [on|off]]

  run        start the always-on service (default; headless)
  status     print a summary of the running service (--json for the raw state)
  restore    restore original audio/display state now
  shutdown   stop the running service (it restores first)
  autostart  show, enable or disable start-at-login (HKCU Run key only)
  share-start <code>  spawn the share engine (RELAY_PEER, RELAY_BITRATE_MBPS optional)
  share-stop          stop the running share engine
  apo [status|install|uninstall] [--endpoint <guid>] [--all]
             endpoint-APO registration per output (default output when no
             --endpoint; `uninstall --all` restores every recorded output).
             install/uninstall are VM / installer only: they refuse without
             RELAY_APO_ALLOW_LIVE_WRITE=1 and an elevated prompt)
  vdevice [status|dry-run|consent-camera|install|uninstall]  virtual-camera
             registration (install/uninstall refuse without
             RELAY_VDEVICE_ALLOW_LIVE_WRITE=1; the Windows 11 media source also
             needs an elevated prompt, the Windows 10 filter is per user)
  firewall [status|dry-run|allow|remove]  the inbound rule for relay-share.exe
             (the only Relay binary that listens). `status` and `dry-run` are
             read-only; `allow`/`remove` refuse without
             RELAY_FIREWALL_ALLOW_LIVE_WRITE=1 and an elevated prompt
  elevate [plan|run] <op>   the elevated install helper. <op> is one of
             install-apo, uninstall-apo, install-camera, uninstall-camera,
             allow-firewall, remove-firewall, allow-audio-effects,
             disallow-audio-effects.
             `plan` prints what would change and touches nothing; `run` raises
             one UAC prompt and runs relay-elevate.exe. Declining changes
             nothing.
  uninstall  remove everything Relay put on this PC, in order (stop UI, stop
             core — which restores audio/display, restore the endpoint FX
             store, unregister the camera, drop the Run key, then the files)
             --dry-run       list it all and change nothing (--json for the plan)
             --delete-data   also delete %LOCALAPPDATA%\\Relay (kept by default)
             --silent        no console output except failures
";

#[cfg(windows)]
fn client_command(cmd: &str, arg: Option<&str>, json: bool) -> Result<()> {
    use relay_core::ipc::{client::Client, Method, Reply};
    use relay_core::share::ShareRequest;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let mut c = Client::connect().await?;
        let method = match cmd {
            "status" => Method::Status,
            "restore" => Method::RestoreAll,
            "share-start" => {
                // `relay-core share-start <code>`; peer from RELAY_PEER, else mDNS.
                // RELAY_PEER_ID shares to a remembered receiver with no code.
                let peer_id = std::env::var("RELAY_PEER_ID").ok();
                let code =
                    match arg.map(str::to_string).or_else(|| std::env::var("RELAY_CODE").ok()) {
                        Some(c) => c,
                        None if peer_id.is_some() => String::new(),
                        None => anyhow::bail!("usage: relay-core share-start <code>"),
                    };
                Method::StartShare {
                    request: Box::new(ShareRequest {
                        peer: std::env::var("RELAY_PEER").ok(),
                        code,
                        peer_id,
                        trusted: None,
                        bitrate_mbps: std::env::var("RELAY_BITRATE_MBPS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(60),
                        fps: std::env::var("RELAY_FPS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(60),
                        // S32 matrix: RELAY_SIZE=WxH, the encode size.
                        size: std::env::var("RELAY_SIZE").ok().and_then(|s| {
                            let (w, h) = s.split_once('x')?;
                            Some((w.parse().ok()?, h.parse().ok()?))
                        }),
                        audio: std::env::var("RELAY_NO_AUDIO").is_err(),
                        // S37/S19 two-PC automation: the same choices the
                        // Share screen makes, from the environment.
                        audio_pid: std::env::var("RELAY_AUDIO_PID")
                            .ok()
                            .and_then(|s| s.parse().ok()),
                        mic: std::env::var("RELAY_MIC").is_ok(),
                        rest: std::env::var("RELAY_REST").is_ok(),
                        vcam: false,
                        // The service fills it from the saved setting.
                        ndi: false,
                        mic_device: None,
                        output_device: None,
                        cursor: true,
                        preset: None,
                        record: std::env::var("RELAY_RECORD").is_ok(),
                        replay_secs: std::env::var("RELAY_REPLAY_SECS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0),
                        record_dir: std::env::var("RELAY_RECORD_DIR").ok(),
                        container: match std::env::var("RELAY_CONTAINER").as_deref() {
                            Ok("mkv") => relay_core::share::RecordingContainer::Mkv,
                            _ => relay_core::share::RecordingContainer::Mp4,
                        },
                        // No window to show it in when started from the CLI.
                        preview_fps: 0,
                    }),
                }
            }
            "share-stop" => Method::StopShare,
            _ => Method::Shutdown,
        };
        match c.call(method).await? {
            Reply::Status { state } if json => {
                println!("{}", serde_json::to_string_pretty(&*state)?)
            }
            Reply::Status { state } => {
                let autostart = autostart::is_enabled().unwrap_or(false);
                print!("{}", relay_core::status::summary(&state, autostart));
            }
            Reply::Error { message } => anyhow::bail!("{message}"),
            Reply::Ok => println!("ok"),
            other => println!("{}", serde_json::to_string(&other)?),
        }
        Ok(())
    })
}
