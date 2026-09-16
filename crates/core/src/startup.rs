//! Getting the core running when the user opens the window.
//!
//! Relay is an installed desktop app, so opening it has to reach live state on
//! its own. Autostart is off until the user asks for it, which means the first
//! launch after an install — and every launch after a reboot for anyone who
//! left autostart off — finds no core. Before this, the window said so and
//! offered a command to type, which is exactly the thing an installed app
//! exists to avoid.
//!
//! So the shell starts the core itself, through the same `relay-svc.exe` the
//! installer and the Run key use: a GUI-subsystem launcher that spawns the
//! core with no console window and exits.
//!
//! The failures are what this module is really about. "It did not start" is
//! useless on its own, so each one is reported as something the person
//! reading it can actually do.

use std::path::Path;
use std::time::Duration;

/// How long to wait for the core's pipe after launching it. Generous: a cold
/// first start also probes audio endpoints and monitors before it serves IPC,
/// and on a slow disk that is seconds, not milliseconds. Waiting too long
/// only delays an error message; waiting too little produces a false alarm
/// while the core is coming up fine, which is worse.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Gap between pipe-connect attempts while waiting.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// What `ensure_running` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// A core was already up; nothing was launched.
    Already,
    /// The launcher ran and the core answered.
    Launched,
}

/// Why the core could not be started, in the shape the window shows it.
///
/// Each variant exists because it needs a *different* sentence — there is no
/// catch-all "start failed", because that is the message this whole module is
/// here to stop showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// `relay-svc.exe` is not beside the running binary. A broken or partial
    /// install; nothing the user can fix by retrying.
    LauncherMissing { dir: String },
    /// Windows refused to create the process. Security software is the common
    /// cause; the OS message is worth passing through.
    SpawnRefused { reason: String },
    /// The launcher ran, but no core answered within [`START_TIMEOUT`].
    NoResponse { log: String },
}

impl StartError {
    /// The sentence the user sees. Plain English, no command names, and it
    /// always ends with the thing to do next.
    pub fn message(&self) -> String {
        match self {
            Self::LauncherMissing { dir } => format!(
                "Part of Relay is missing from its installation folder ({dir}), so the \
                 background service could not be started. Reinstalling Relay will replace it."
            ),
            Self::SpawnRefused { reason } => format!(
                "Windows would not start Relay's background service: {reason}. \
                 This is usually security software blocking it — allow Relay in your \
                 antivirus or security settings, then try again."
            ),
            Self::NoResponse { log } => format!(
                "Relay's background service was started but did not finish starting up. \
                 Restarting your PC usually clears this. The details are in {log}."
            ),
        }
    }
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for StartError {}

/// Is `relay-svc.exe` where it should be, beside `exe`?
///
/// Split out from the launching so the "broken install" message can be
/// produced and tested without a filesystem full of executables.
pub fn launcher_beside(exe: &Path) -> Result<std::path::PathBuf, StartError> {
    crate::launcher::sibling_of(exe, crate::launcher::LAUNCHER_EXE)
        .filter(|p| p.exists())
        .ok_or_else(|| StartError::LauncherMissing {
            dir: exe.parent().map(|d| d.display().to_string()).unwrap_or_default(),
        })
}

/// Connect if a core is up, otherwise launch one and wait for it.
///
/// Safe to call concurrently and repeatedly: the core holds a single-instance
/// mutex, so a second launcher exits without starting a second service, and
/// the first branch here means the common case costs one pipe connect.
#[cfg(windows)]
pub async fn ensure_running() -> Result<Started, StartError> {
    use crate::ipc::client::Client;

    if Client::connect().await.is_ok() {
        return Ok(Started::Already);
    }

    let exe = std::env::current_exe().map_err(|e| StartError::SpawnRefused {
        reason: format!("Relay could not locate its own program file ({e})"),
    })?;
    let launcher = launcher_beside(&exe)?;

    // `relay-svc run` with no data-dir: the service resolves the same default
    // root the window does, so the two always agree about where profiles live.
    std::process::Command::new(&launcher)
        .arg("run")
        .spawn()
        .map_err(|e| StartError::SpawnRefused { reason: e.to_string() })?;

    let deadline = std::time::Instant::now() + START_TIMEOUT;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if Client::connect().await.is_ok() {
            return Ok(Started::Launched);
        }
        if std::time::Instant::now() >= deadline {
            return Err(StartError::NoResponse { log: log_location() });
        }
    }
}

#[cfg(not(windows))]
pub async fn ensure_running() -> Result<Started, StartError> {
    Err(StartError::SpawnRefused { reason: "Relay's core only runs on Windows".into() })
}

/// Where to point someone at for the details. A folder, not a command.
fn log_location() -> String {
    crate::config::Paths::default_for_user()
        .map(|p| p.log_dir().display().to_string())
        .unwrap_or_else(|_| "Relay's logs folder".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the session: nothing the user reads may be a thing
    /// to type into a terminal.
    #[test]
    fn no_message_names_a_command() {
        let messages = [
            StartError::LauncherMissing { dir: r"C:\x".into() }.message(),
            StartError::SpawnRefused { reason: "Access is denied".into() }.message(),
            StartError::NoResponse { log: r"C:\x\logs".into() }.message(),
        ];
        for m in messages {
            for banned in ["relay-core", "relay-svc", "cargo", "powershell", "cmd.exe", "--"] {
                assert!(!m.contains(banned), "{m:?} names {banned:?}");
            }
        }
    }

    #[test]
    fn every_message_says_what_to_do_next() {
        assert!(StartError::LauncherMissing { dir: "d".into() }.message().contains("Reinstall"));
        assert!(StartError::SpawnRefused { reason: "r".into() }.message().contains("try again"));
        assert!(StartError::NoResponse { log: "l".into() }.message().contains("Restarting"));
    }

    #[test]
    fn spawn_refused_passes_the_os_reason_through() {
        let m =
            StartError::SpawnRefused { reason: "Access is denied. (os error 5)".into() }.message();
        assert!(m.contains("Access is denied"));
    }

    #[test]
    fn a_missing_launcher_names_the_folder_it_looked_in() {
        let exe = Path::new(r"C:\nonexistent-relay\relay-ui.exe");
        let err = launcher_beside(exe).unwrap_err();
        assert_eq!(err, StartError::LauncherMissing { dir: r"C:\nonexistent-relay".into() });
        assert!(err.message().contains(r"C:\nonexistent-relay"));
    }

    #[test]
    fn a_present_launcher_resolves_to_its_path() {
        let dir = std::env::temp_dir().join(format!("relay-startup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join(crate::launcher::LAUNCHER_EXE);
        std::fs::write(&launcher, b"").unwrap();

        assert_eq!(launcher_beside(&dir.join("relay-ui.exe")).unwrap(), launcher);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
