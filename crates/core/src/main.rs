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
                        audio: std::env::var("RELAY_NO_AUDIO").is_err(),
                        audio_pid: None,
                        cursor: true,
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
