//! Where the core keeps its files and how instances are named.
//!
//! Everything lives under one per-user root (`%LOCALAPPDATA%\Relay`); nothing
//! is written anywhere else on the machine.
//!
//! ```text
//! <root>/data/profiles.json          user profiles
//! <root>/data/original-state.json    pre-apply snapshot (crash-restore path)
//! <root>/logs/core.log[.1..3]        rotating service log
//! <root>/installed.json              virtual-device consent + registrations
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
/// Base of the window's own single-instance mutex. Separate from the core's:
/// the window and the core come and go independently.
const UI_MUTEX_BASE: &str = r"Local\RelayUi";

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

/// The window's mutex name, suffixed the same way as [`mutex_name`].
pub fn ui_mutex_name() -> String {
    match instance_suffix() {
        Some(s) => format!("{UI_MUTEX_BASE}-{s}"),
        None => UI_MUTEX_BASE.to_string(),
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
        for d in [self.data_dir(), self.log_dir(), self.previews_dir()] {
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

    /// Rendered A/B listening-test WAVs (`original.wav` / `processed.wav`).
    pub fn previews_dir(&self) -> PathBuf {
        self.root.join("previews")
    }

    /// Prior FX property stores, one `<endpoint>.json` per endpoint, written
    /// *before* any APO install touches the registry (M3b plan).
    pub fn apo_backup_dir(&self) -> PathBuf {
        self.root.join("apo-backup")
    }

    /// The virtual-device record: the user's two consent decisions plus
    /// every registered component, so opt-out removes exactly what opt-in
    /// added (M5 plan).
    pub fn installed_file(&self) -> PathBuf {
        self.root.join("installed.json")
    }

    /// The record of the Windows Firewall rule Relay added for
    /// `relay-share.exe`, plus any Block rules the install removed. Written
    /// before the rule goes in; the uninstaller plans from it rather than
    /// guessing, so a leftover rule cannot survive a clean-VM diff.
    pub fn firewall_file(&self) -> PathBuf {
        self.root.join("firewall.json")
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

    /// App preferences that are not about any one game — currently what
    /// closing the window means.
    pub fn settings_file(&self) -> PathBuf {
        self.data_dir().join("settings.json")
    }

    /// The window's last size, position and maximised state. Written and read
    /// by `relay-ui` only; the core never touches it.
    pub fn window_file(&self) -> PathBuf {
        self.data_dir().join("window.json")
    }

    /// Downloaded headphone measurements, one CSV per model. Cached so a
    /// model is fetched once ever; deleting this only costs a re-download.
    pub fn curves_dir(&self) -> PathBuf {
        self.data_dir().join("curves")
    }

    /// Everything under the root that is *user data* rather than program
    /// files, deepest-independent so each can be removed on its own.
    ///
    /// This list exists because Tauri's per-user NSIS installer puts Relay's
    /// binaries in `%LOCALAPPDATA%\Relay` — the same folder as the data root.
    /// "Delete my profiles and settings" therefore removes these named paths
    /// and leaves the folder itself to the uninstaller, instead of
    /// recursively deleting a directory that contains the running exe.
    pub fn data_paths(&self) -> Vec<PathBuf> {
        vec![
            self.data_dir(),
            self.log_dir(),
            self.previews_dir(),
            self.apo_backup_dir(),
            self.installed_file(),
            self.firewall_file(),
        ]
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
    fn data_paths_cover_every_file_the_core_writes() {
        let p = Paths::at(r"C:\tmp\relay");
        let data = p.data_paths();
        // Every path the core writes has to be reachable from this list, or
        // "delete my data" leaves something behind.
        for written in [
            p.profiles_file(),
            p.hardware_file(),
            p.backup_file(),
            p.presets_file(),
            p.settings_file(),
            p.window_file(),
            p.log_file(),
            p.installed_file(),
        ] {
            assert!(
                data.iter().any(|d| written == *d || written.starts_with(d)),
                "{} is not covered by data_paths()",
                written.display()
            );
        }
        // And none of them is the root itself: the installer owns that folder.
        assert!(data.iter().all(|d| d != p.root()), "data_paths must not name the root");
    }

    #[test]
    fn names_are_stable_without_instance_suffix() {
        // Other tests may set RELAY_INSTANCE; only assert the prefix.
        assert!(pipe_name().starts_with(PIPE_BASE));
        assert!(mutex_name().starts_with(MUTEX_BASE));
        assert!(ui_mutex_name().starts_with(UI_MUTEX_BASE));
        assert_ne!(mutex_name(), ui_mutex_name());
    }
}
