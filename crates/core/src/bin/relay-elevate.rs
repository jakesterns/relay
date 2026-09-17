//! `relay-elevate.exe` — the one Relay binary that runs elevated.
//!
//! It is deliberately almost empty. All it does is read the request file
//! named on its command line, hand it to [`relay_core::elevate`], and exit.
//! Everything that decides *whether* a request may run — the closed op set,
//! the freshness and location checks, the CLSID scope vetting, the live-write
//! gates — lives in that module, next to its tests.
//!
//! GUI subsystem on purpose: it is launched through `ShellExecuteExW`, which
//! gives a console-subsystem process a console window, and a black box
//! flashing up behind a UAC prompt is exactly the kind of thing that makes an
//! install feel untrustworthy. Its output is the result file the core reads
//! back, plus an appended line in `logs\elevate.log` for the runbook.
//!
//! Run by hand it refuses politely: an unelevated token is a refusal, and so
//! is a request file anywhere other than where the core writes it.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut request: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--request" => request = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                log_to(
                    None,
                    "relay-elevate --request <file>  (started by Relay; not a user command)",
                );
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                log_to(None, &format!("unknown argument {other}; nothing was done"));
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let Some(request) = request else {
        log_to(None, "no --request file given; nothing was done");
        return std::process::ExitCode::FAILURE;
    };

    #[cfg(windows)]
    {
        // The log belongs to the run's own data root, not to the installed
        // one: a `--data-dir` run (a test, the footprint gate, a second
        // instance) must not write into `%LOCALAPPDATA%\Relay`.
        let root = std::fs::read(&request)
            .ok()
            .and_then(|b| serde_json::from_slice::<relay_core::elevate::Request>(&b).ok())
            .map(|r| r.data_dir);
        match relay_core::elevate::run_request_file(&request) {
            Ok(response) => {
                for line in response.lines() {
                    log_to(root.as_deref(), &line);
                }
                if response.ok() {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                }
            }
            Err(e) => {
                // No result file could be written, so the core will report
                // "the elevated helper wrote no result". Leave a trace here.
                log_to(root.as_deref(), &format!("elevation request failed before it ran: {e:#}"));
                std::process::ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = request;
        let e = relay_core::platform::unsupported(relay_core::platform::Capability::Elevation);
        log_to(None, &e.to_string());
        std::process::ExitCode::FAILURE
    }
}

/// Append one line to `<data root>\logs\elevate.log`, best effort. The helper
/// has no console, so this is where a hand-run leaves its answer.
///
/// `root` is the data root the request named; without one (no request, or an
/// unreadable one) it falls back to the installed location, which is the only
/// place a helper with nothing to go on could sensibly write.
fn log_to(root: Option<&std::path::Path>, text: &str) {
    use std::io::Write;
    eprintln!("{text}");
    let paths = match root {
        Some(r) => relay_core::config::Paths::at(r),
        None => match relay_core::config::Paths::default_for_user() {
            Ok(p) => p,
            Err(_) => return,
        },
    };
    let dir = paths.log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("elevate.log"))
    {
        let _ = writeln!(f, "{text}");
    }
}
