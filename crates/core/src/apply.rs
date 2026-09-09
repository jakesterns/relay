//! Apply / restore with the backup-first invariant enforced in one place.
//!
//! The backends are traits so `audio/` and `display/` can plug in real
//! NvAPI/ADLX, DDC/CI and APO parameter writers later. The core only ever calls
//! them through [`Applier`], which guarantees:
//!
//! 1. `capture()` original state → write snapshot to disk → then `apply()`.
//! 2. `restore()` is idempotent and safe to call when nothing is applied.
//! 3. A failure part-way through apply triggers an immediate restore.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::backup::{AudioState, BackupFile, DisplayStateSnapshot, Snapshot};
use crate::types::{AudioChainState, AudioSettings, DisplaySettings, DisplayState, Profile};

pub trait AudioControl: Send + Sync {
    fn capture(&self) -> Result<AudioState>;
    fn apply(&self, settings: &AudioSettings) -> Result<AudioChainState>;
    fn restore(&self, original: &AudioState) -> Result<()>;
}

pub trait DisplayControl: Send + Sync {
    fn capture(&self, settings: &DisplaySettings) -> Result<DisplayStateSnapshot>;
    fn apply(&self, settings: &DisplaySettings) -> Result<()>;
    fn restore(&self, original: &DisplayStateSnapshot) -> Result<()>;
}

/// Backend that does nothing. Used until the real crates land and in tests.
#[derive(Debug, Default)]
pub struct Noop;

impl AudioControl for Noop {
    fn capture(&self) -> Result<AudioState> {
        Ok(AudioState { bypass: true })
    }
    fn apply(&self, _: &AudioSettings) -> Result<AudioChainState> {
        Ok(AudioChainState::Bypass)
    }
    fn restore(&self, _: &AudioState) -> Result<()> {
        Ok(())
    }
}

impl DisplayControl for Noop {
    fn capture(&self, _: &DisplaySettings) -> Result<DisplayStateSnapshot> {
        Ok(DisplayStateSnapshot::default())
    }
    fn apply(&self, _: &DisplaySettings) -> Result<()> {
        Ok(())
    }
    fn restore(&self, _: &DisplayStateSnapshot) -> Result<()> {
        Ok(())
    }
}

/// Test harness backend: appends one line per call to a file so an external
/// process (the crash-restore integration test) can see what the core did.
/// Selected with `RELAY_RECORDING_BACKEND=<path>`; never used in production.
#[derive(Debug)]
pub struct FileRecorder {
    path: std::path::PathBuf,
}

impl FileRecorder {
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn record(&self, what: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{what}");
        }
    }
}

impl AudioControl for FileRecorder {
    fn capture(&self) -> Result<AudioState> {
        self.record("audio.capture");
        Ok(AudioState { bypass: true })
    }
    fn apply(&self, _: &AudioSettings) -> Result<AudioChainState> {
        self.record("audio.apply");
        Ok(AudioChainState::Active)
    }
    fn restore(&self, _: &AudioState) -> Result<()> {
        self.record("audio.restore");
        Ok(())
    }
}

impl DisplayControl for FileRecorder {
    fn capture(&self, _: &DisplaySettings) -> Result<DisplayStateSnapshot> {
        self.record("display.capture");
        Ok(DisplayStateSnapshot::default())
    }
    fn apply(&self, _: &DisplaySettings) -> Result<()> {
        self.record("display.apply");
        Ok(())
    }
    fn restore(&self, _: &DisplayStateSnapshot) -> Result<()> {
        self.record("display.restore");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub audio: AudioChainState,
    pub display: DisplayState,
}

pub struct Applier {
    audio: Arc<dyn AudioControl>,
    display: Arc<dyn DisplayControl>,
    backup: BackupFile,
    current: Option<Snapshot>,
}

impl Applier {
    pub fn new(
        audio: Arc<dyn AudioControl>,
        display: Arc<dyn DisplayControl>,
        backup: BackupFile,
    ) -> Self {
        Self { audio, display, backup, current: None }
    }

    pub fn is_applied(&self) -> bool {
        self.current.is_some()
    }

    /// Called once at start-up. If a previous run left a snapshot marked
    /// `applied`, put the machine back before anything else happens.
    pub fn recover_on_start(&mut self) -> Result<bool> {
        match self.backup.pending()? {
            Some(snap) => {
                warn!(
                    profile = ?snap.profile_id,
                    written_at = snap.written_at_unix,
                    "found un-restored state from a previous run; restoring"
                );
                self.restore_snapshot(&snap)?;
                self.backup.clear()?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn apply(&mut self, profile: &Profile) -> Result<Applied> {
        if let Some(cur) = &self.current {
            if cur.profile_id == Some(profile.id) {
                return Ok(Applied {
                    audio: AudioChainState::Active,
                    display: DisplayState::Applied,
                });
            }
            // Switching straight from one game to another: restore first so the
            // snapshot always describes the true original state.
            self.restore()?;
        }

        let audio_orig = self.audio.capture().context("capturing audio state")?;
        let display_orig =
            self.display.capture(&profile.display).context("capturing display state")?;
        let snap = Snapshot::new(profile.id, audio_orig, display_orig);
        self.backup.write(&snap).context("writing original-state snapshot")?;
        self.current = Some(snap);

        let result = (|| -> Result<Applied> {
            let audio = self.audio.apply(&profile.audio).context("applying audio")?;
            let display = if profile.display.follow_focus {
                self.display.apply(&profile.display).context("applying display")?;
                DisplayState::Applied
            } else {
                DisplayState::Default
            };
            Ok(Applied { audio, display })
        })();

        match result {
            Ok(applied) => {
                info!(profile = %profile.name, "profile applied");
                Ok(applied)
            }
            Err(e) => {
                warn!(error = %e, "apply failed part-way; restoring");
                let _ = self.restore();
                Err(e)
            }
        }
    }

    /// Put everything back. No-op when nothing is applied.
    pub fn restore(&mut self) -> Result<()> {
        let Some(snap) = self.current.take() else { return Ok(()) };
        let r = self.restore_snapshot(&snap);
        // Even if a backend failed, clear only on success so a retry / next
        // start can attempt again.
        if r.is_ok() {
            self.backup.clear()?;
            info!("original state restored");
        }
        r
    }

    fn restore_snapshot(&self, snap: &Snapshot) -> Result<()> {
        let mut first_err: Option<anyhow::Error> = None;
        if let Err(e) = self.display.restore(&snap.display) {
            warn!(error = %e, "display restore failed");
            first_err.get_or_insert(e);
        }
        if let Err(e) = self.audio.restore(&snap.audio) {
            warn!(error = %e, "audio restore failed");
            first_err.get_or_insert(e);
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GameMatch;
    use parking_lot::Mutex;
    use uuid::Uuid;

    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<&'static str>>,
        fail_display_apply: bool,
    }

    impl AudioControl for Recorder {
        fn capture(&self) -> Result<AudioState> {
            self.log.lock().push("audio.capture");
            Ok(AudioState { bypass: true })
        }
        fn apply(&self, _: &AudioSettings) -> Result<AudioChainState> {
            self.log.lock().push("audio.apply");
            Ok(AudioChainState::Active)
        }
        fn restore(&self, _: &AudioState) -> Result<()> {
            self.log.lock().push("audio.restore");
            Ok(())
        }
    }

    impl DisplayControl for Recorder {
        fn capture(&self, _: &DisplaySettings) -> Result<DisplayStateSnapshot> {
            self.log.lock().push("display.capture");
            Ok(DisplayStateSnapshot::default())
        }
        fn apply(&self, _: &DisplaySettings) -> Result<()> {
            self.log.lock().push("display.apply");
            if self.fail_display_apply {
                anyhow::bail!("nvapi says no")
            }
            Ok(())
        }
        fn restore(&self, _: &DisplayStateSnapshot) -> Result<()> {
            self.log.lock().push("display.restore");
            Ok(())
        }
    }

    fn setup(fail: bool) -> (Arc<Recorder>, Applier, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("relay-apply-{}", Uuid::new_v4()));
        let rec = Arc::new(Recorder { fail_display_apply: fail, ..Default::default() });
        let applier =
            Applier::new(rec.clone(), rec.clone(), BackupFile::at(dir.join("original-state.json")));
        (rec, applier, dir)
    }

    fn profile() -> Profile {
        let mut p = Profile::new("CoD", GameMatch::exe("cod.exe"));
        p.display.follow_focus = true;
        p
    }

    #[test]
    fn captures_and_persists_before_applying() {
        let (rec, mut a, dir) = setup(false);
        a.apply(&profile()).unwrap();
        let log = rec.log.lock().clone();
        let cap = log.iter().position(|s| *s == "display.capture").unwrap();
        let app = log.iter().position(|s| *s == "display.apply").unwrap();
        assert!(cap < app);
        assert!(BackupFile::at(dir.join("original-state.json")).pending().unwrap().is_some());
        a.restore().unwrap();
        assert!(BackupFile::at(dir.join("original-state.json")).pending().unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_apply_restores_immediately() {
        let (rec, mut a, dir) = setup(true);
        assert!(a.apply(&profile()).is_err());
        let log = rec.log.lock().clone();
        assert!(log.contains(&"display.restore"));
        assert!(log.contains(&"audio.restore"));
        assert!(!a.is_applied());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restore_is_idempotent() {
        let (_, mut a, dir) = setup(false);
        a.restore().unwrap();
        a.restore().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovers_pending_snapshot_on_start() {
        let (rec, mut a, dir) = setup(false);
        a.apply(&profile()).unwrap();
        // Simulate a crash: drop the applier without restoring.
        drop(a);
        rec.log.lock().clear();
        let mut fresh =
            Applier::new(rec.clone(), rec.clone(), BackupFile::at(dir.join("original-state.json")));
        assert!(fresh.recover_on_start().unwrap());
        assert!(rec.log.lock().contains(&"display.restore"));
        assert!(!fresh.recover_on_start().unwrap(), "second start finds nothing pending");
        let _ = std::fs::remove_dir_all(dir);
    }
}
