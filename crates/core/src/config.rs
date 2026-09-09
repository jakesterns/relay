//! Where the core keeps its files. Everything lives under the per-user local
//! app-data directory; nothing is written anywhere else on the machine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;

/// Name of the named pipe the core listens on.
pub const PIPE_NAME: &str = r"\\.\pipe\relay-core";

#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
}

impl Paths {
    /// `%LOCALAPPDATA%\Relay\data` on Windows.
    pub fn default_for_user() -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "Relay")
            .context("could not determine a per-user data directory")?;
        Ok(Self { data_dir: dirs.data_local_dir().to_path_buf() })
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { data_dir: dir.into() }
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))
    }

    pub fn profiles_file(&self) -> PathBuf {
        self.data_dir.join("profiles.json")
    }

    /// Snapshot of the machine's original state. Presence of this file with
    /// `applied = true` on start means we did not get to restore last time.
    pub fn backup_file(&self) -> PathBuf {
        self.data_dir.join("original-state.json")
    }

    pub fn dir(&self) -> &Path {
        &self.data_dir
    }
}
