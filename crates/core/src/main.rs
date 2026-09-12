//! `relay-core` binary.
//!
//! ```text
//! relay-core run                 start the always-on service (headless by default)
//! relay-core status [--json]     human summary, or the raw state as JSON
//! relay-core restore             ask a running service to restore everything
//! relay-core shutdown            stop a running service (restores first)
//! relay-core autostart [on|off]  show or set start-at-login (HKCU Run key only)
//! relay-core --data-dir DIR      override the data root (any subcommand)
//! relay-core --verbose           debug-level logging
//! ```

use anyhow::Result;
use relay_core::config::{mutex_name, Paths};
use relay_core::instance::InstanceLock;
use relay_core::service::{Backends, Service};
use relay_core::{autostart, logging};

struct Args {
    cmd: String,
    arg: Option<String>,
    paths: Paths,
    json: bool,
    verbose: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut out = Args {
        cmd: String::new(),
        arg: None,
        paths: Paths::default_for_user()?,
        json: false,
        verbose: false,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => {
                let dir = args.next().ok_or_else(|| anyhow::anyhow!("--data-dir needs a path"))?;
                out.paths = Paths::at(dir);
            }
            "--json" => out.json = true,
            "--verbose" | "-v" => out.verbose = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => anyhow::bail!("unknown flag `{other}`\n{USAGE}"),
            other if out.cmd.is_empty() => out.cmd = other.to_string(),
            other if out.arg.is_none() => out.arg = Some(other.to_string()),
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
            client_command(&args.cmd, args.arg.as_deref(), args.json)
        }
        // Direct (no running service needed): the VM runbook drives these
        // from an elevated prompt. The livereg write gate applies.
        #[cfg(windows)]
        "apo" => {
            logging::init_console(args.verbose);
            match args.arg.as_deref() {
                None | Some("status") => {
                    let s = relay_core::audio_apo::apo_status();
                    println!(
                        "endpoint: {}\ninstalled: {}\nparams section: {}",
                        s.endpoint.as_deref().unwrap_or("none"),
                        s.installed,
                        if s.running { "reachable" } else { "not reachable" },
                    );
                }
                Some("install") => {
                    let ep = relay_core::audio_apo::install_live(&args.paths.apo_backup_dir())?;
                    println!("registered on {ep}; restart audiosrv to pick it up");
                }
                Some("uninstall") => {
                    let ep = relay_core::audio_apo::uninstall_live(&args.paths.apo_backup_dir())?;
                    println!("restored {ep} to its pre-install state");
                }
                Some(other) => {
                    anyhow::bail!("apo takes `status`, `install` or `uninstall`, not `{other}`")
                }
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
                            "windows build: {} (frame-server camera {})",
                            s.windows_build.map_or("unknown".into(), |b| b.to_string()),
                            if s.camera_supported { "supported" } else { "needs 22H2+" },
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
                    relay_core::vdevice::install_camera_live(&args.paths)?;
                    println!("Relay Camera media source registered");
                }
                Some("uninstall") => {
                    relay_core::vdevice::uninstall_camera_live(&args.paths)?;
                    println!("Relay Camera media source removed; installed.json cleared");
                }
                Some(other) => anyhow::bail!(
                    "vdevice takes `status`, `dry-run`, `consent-camera`, `install` or `uninstall`, not `{other}`"
                ),
            }
            Ok(())
        }
        other => anyhow::bail!("unknown command `{other}`\n{USAGE}"),
    }
}

fn run(args: Args) -> Result<()> {
    let Some(_lock) = InstanceLock::acquire(&mutex_name())? else {
        println!("relay-core is already running");
        return Ok(());
    };
    args.paths.ensure()?;
    logging::init_service(&args.paths.log_file(), args.verbose)?;
    hide_own_console();
    Service::run(args.paths, Backends::from_env())
}

/// Headless by default: when this process was given a fresh console (started
/// from Explorer or the Run key rather than a terminal) hide that window.
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
  apo [status|install|uninstall]  endpoint-APO registration (install/uninstall
             are VM / installer only: they refuse without
             RELAY_APO_ALLOW_LIVE_WRITE=1 and an elevated prompt)
  vdevice [status|dry-run|consent-camera|install|uninstall]  virtual-camera
             registration (install/uninstall refuse without
             RELAY_VDEVICE_ALLOW_LIVE_WRITE=1 and an elevated prompt)
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
                let code = arg
                    .map(str::to_string)
                    .or_else(|| std::env::var("RELAY_CODE").ok())
                    .ok_or_else(|| anyhow::anyhow!("usage: relay-core share-start <code>"))?;
                Method::StartShare {
                    request: Box::new(ShareRequest {
                        peer: std::env::var("RELAY_PEER").ok(),
                        code,
                        bitrate_mbps: std::env::var("RELAY_BITRATE_MBPS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(60),
                        fps: 60,
                        size: None,
                        audio: std::env::var("RELAY_NO_AUDIO").is_err(),
                        audio_pid: None,
                        mic: false,
                        cursor: true,
                        preset: None,
                        record: std::env::var("RELAY_RECORD").is_ok(),
                        replay_secs: std::env::var("RELAY_REPLAY_SECS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0),
                        record_dir: std::env::var("RELAY_RECORD_DIR").ok(),
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
