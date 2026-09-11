//! Where the core keeps its files and how instances are named.
//!
//! Everything lives under one per-user root (`%LOCALAPPDATA%\Relay`); nothing
//! is written anywhere else on the machine.
//!
//! ```text
//! <root>/data/profiles.json          user profiles
//! <root>/data/original-state.json    pre-apply snapshot (crash-restore path)
//! <root>/logs/core.log[.1..3]        rotating service log
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::BaseDirs;

/// Environment variable that namespaces the pipe and the single-instance
/// mutex so tests and the footprint gate can run next to a live service.
pub const INSTANCE_ENV: &str = "RELAY_INSTANCE";

/// Base of the named pipe the core listens on.
const PIPE_BASE: &str = r"\\.\pipe\relay-core";
/// Base of the single-instance mutex. The `Local` prefix scopes it to this session.
const MUTEX_BASE: &str = r"Local\RelayCore";

fn instance_suffix() -> Option<String> {
    std::env::var(INSTANCE_ENV).ok().filter(|s| !s.is_empty())
}

/// The pipe name, with `-<RELAY_INSTANCE>` appended when that variable is set.
pub fn pipe_name() -> String {
    match instance_suffix() {
        Some(s) => format!("{PIPE_BASE}-{s}"),
        None => PIPE_BASE.to_string(),
    }
}

/// The mutex name, with `-<RELAY_INSTANCE>` appended when that variable is set.
pub fn mutex_name() -> String {
    match instance_suffix() {
        Some(s) => format!("{MUTEX_BASE}-{s}"),
        None => MUTEX_BASE.to_string(),
    }
}

/// Maximum accepted size of one IPC line (request or reply). Anything larger
/// is rejected and the connection is closed.
pub const IPC_MAX_LINE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// `%LOCALAPPDATA%\Relay` on Windows.
    pub fn default_for_user() -> Result<Self> {
        let dirs = BaseDirs::new().context("could not determine a per-user data directory")?;
        Ok(Self { root: dirs.data_local_dir().join("Relay") })
    }

    /// Use an explicit root (the `--data-dir` flag). `data/` and `logs/` are
    /// created beneath it.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn ensure(&self) -> Result<()> {
        for d in [self.data_dir(), self.log_dir()] {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn log_file(&self) -> PathBuf {
        self.log_dir().join("core.log")
    }

    pub fn profiles_file(&self) -> PathBuf {
        self.data_dir().join("profiles.json")
    }

    /// The hardware library (headsets with curves, monitors, interfaces).
    pub fn hardware_file(&self) -> PathBuf {
        self.data_dir().join("hardware.json")
    }

    /// Snapshot of the machine's original state. Presence of this file with
    /// `applied = true` on start means we did not get to restore last time.
    pub fn backup_file(&self) -> PathBuf {
        self.data_dir().join("original-state.json")
    }

    /// Share presets and recording settings.
    pub fn presets_file(&self) -> PathBuf {
        self.data_dir().join("presets.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_root_nests_data_and_logs() {
        let p = Paths::at(r"C:\tmp\relay");
        assert_eq!(p.profiles_file(), PathBuf::from(r"C:\tmp\relay\data\profiles.json"));
        assert_eq!(p.log_file(), PathBuf::from(r"C:\tmp\relay\logs\core.log"));
    }

    #[test]
    fn names_are_stable_without_instance_suffix() {
        // Other tests may set RELAY_INSTANCE; only assert the prefix.
        assert!(pipe_name().starts_with(PIPE_BASE));
        assert!(mutex_name().starts_with(MUTEX_BASE));
    }
}
