//! `relay-svc` — start the Relay core without a console window.
//!
//! A GUI-subsystem binary so Windows never allocates it a console, which is
//! the whole point: the Run key at login and the installer invoke this, and
//! the user sees nothing. It spawns `relay-core.exe` with `CREATE_NO_WINDOW`,
//! forwarding its arguments, and exits immediately.
//!
//! See `relay_core::launcher` for why the split is this way round rather than
//! the usual GUI-service-plus-CLI-shim.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // Default to `run`, so the Run key value can be just the exe path and the
    // installer does not have to remember the subcommand.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        args.push("run".into());
    }

    if let Err(e) = relay_core::launcher::spawn_core(&args) {
        // No console to print to, and no UI yet. The core's own log is not
        // reachable either (it never started), so this goes where a
        // GUI-subsystem failure can still be found after the fact.
        report(&format!("{e:#}"));
        std::process::exit(1);
    }
}

/// Record a launch failure somewhere a user or a support log can find it.
/// Best-effort by construction: if this fails too there is nowhere left to go.
#[cfg(windows)]
fn report(message: &str) {
    if let Ok(paths) = relay_core::config::Paths::default_for_user() {
        let _ = std::fs::create_dir_all(paths.log_dir());
        let line = format!("relay-svc could not start the core: {message}\r\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.log_dir().join("launcher.log"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    }
}

#[cfg(not(windows))]
fn report(message: &str) {
    eprintln!("relay-svc: {message}");
}
