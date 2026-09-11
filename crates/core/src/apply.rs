//! Apply / restore with the backup-first invariant enforced in one place.
//!
//! The backends are traits so `audio/` and `display/` can plug in real
//! NvAPI/ADLX, DDC/CI and APO parameter writers later. The core only ever calls
//! them through [`Applier`], which guarantees:
//!
//! 1. `capture()` original state → write snapshot to disk → then `apply()`.
//! 2. `restore()` is idempotent and safe to call when nothing is applied.
//! 3. A failure part-way through apply triggers an immediate restore.
//!
//! Display applies carry a *target*: the monitor hosting the game window.
//! Only that monitor is captured and changed. When the same profile is
//! re-applied with a different target (the game moved monitors), the old
//! monitor is restored first, then the new one is captured and applied.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::backup::{AudioState, BackupFile, DisplayStateSnapshot, Snapshot};
use crate::hardware::MonitorProbe;
use crate::types::{
    AudioChainState, AudioSettings, DisplaySettings, DisplayState, DisplayVia, Profile,
};

pub trait AudioControl: Send + Sync {
    fn capture(&self) -> Result<AudioState>;
    fn apply(&self, settings: &AudioSettings) -> Result<AudioChainState>;
    fn restore(&self, original: &AudioState) -> Result<()>;
}

pub trait DisplayControl: Send + Sync {
    /// Read every value `apply` would touch on the target monitor. `None`
    /// target = no monitor known (headless test rig): capture nothing.
    fn capture(
        &self,
        target: Option<&MonitorProbe>,
        settings: &DisplaySettings,
    ) -> Result<DisplayStateSnapshot>;
    /// Change the target monitor only; report which paths carried it.
    fn apply(
        &self,
        target: Option<&MonitorProbe>,
        settings: &DisplaySettings,
    ) -> Result<DisplayVia>;
    /// Put back exactly what `capture` recorded. Must work after a crash or
    /// reboot, when the snapshot's volatile handles have gone stale.
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
    fn capture(
        &self,
        _: Option<&MonitorProbe>,
        _: &DisplaySettings,
    ) -> Result<DisplayStateSnapshot> {
        Ok(DisplayStateSnapshot::default())
    }
    fn apply(&self, _: Option<&MonitorProbe>, _: &DisplaySettings) -> Result<DisplayVia> {
        Ok(DisplayVia::default())
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
    fn capture(
        &self,
        target: Option<&MonitorProbe>,
        _: &DisplaySettings,
    ) -> Result<DisplayStateSnapshot> {
        match target {
            Some(t) => self.record(&format!("display.capture {}", t.id.0)),
            None => self.record("display.capture"),
        }
        Ok(DisplayStateSnapshot::default())
    }
    fn apply(&self, target: Option<&MonitorProbe>, _: &DisplaySettings) -> Result<DisplayVia> {
        match target {
            Some(t) => self.record(&format!("display.apply {}", t.id.0)),
            None => self.record("display.apply"),
        }
        Ok(DisplayVia::default())
    }
    fn restore(&self, _: &DisplayStateSnapshot) -> Result<()> {
        self.record("display.restore");
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub audio: AudioChainState,
    pub display: DisplayState,
    pub via: DisplayVia,
}

pub struct Applier {
    audio: Arc<dyn AudioControl>,
    display: Arc<dyn DisplayControl>,
    backup: BackupFile,
    current: Option<Snapshot>,
    /// Target of the current display apply, to detect monitor moves.
    current_target: Option<MonitorProbe>,
    /// `via` of the current apply, replayed on the same-profile fast path.
    current_via: DisplayVia,
}

impl Applier {
    pub fn new(
        audio: Arc<dyn AudioControl>,
        display: Arc<dyn DisplayControl>,
        backup: BackupFile,
    ) -> Self {
        Self {
            audio,
            display,
            backup,
            current: None,
            current_target: None,
            current_via: DisplayVia::default(),
        }
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

    pub fn apply(&mut self, profile: &Profile, target: Option<&MonitorProbe>) -> Result<Applied> {
        if let Some(cur) = &self.current {
            let same_target = match (&self.current_target, target) {
                (Some(a), Some(b)) => a.id == b.id,
                (None, None) => true,
                _ => false,
            };
            if cur.profile_id == Some(profile.id) && same_target {
                return Ok(Applied {
                    audio: AudioChainState::Active,
                    display: DisplayState::Applied,
                    via: self.current_via.clone(),
                });
            }
            // Switching game or monitor: restore first so the snapshot always
            // describes the true original state (of the new target).
            self.restore()?;
        }

        let audio_orig = self.audio.capture().context("capturing audio state")?;
        let display_orig =
            self.display.capture(target, &profile.display).context("capturing display state")?;
        let snap = Snapshot::new(profile.id, audio_orig, display_orig);
        self.backup.write(&snap).context("writing original-state snapshot")?;
        self.current = Some(snap);
        self.current_target = target.cloned();

        let result = (|| -> Result<Applied> {
            let audio = self.audio.apply(&profile.audio).context("applying audio")?;
            let (display, via) = if profile.display.follow_focus {
                let via =
                    self.display.apply(target, &profile.display).context("applying display")?;
                (DisplayState::Applied, via)
            } else {
                (DisplayState::Default, DisplayVia::default())
            };
            Ok(Applied { audio, display, via })
        })();

        match result {
            Ok(applied) => {
                self.current_via = applied.via.clone();
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
        self.current_target = None;
        self.current_via = DisplayVia::default();
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
    use crate::types::{GameMatch, MonitorId};
    use parking_lot::Mutex;
    use uuid::Uuid;

    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<String>>,
        fail_display_apply: bool,
    }

    impl Recorder {
        fn push(&self, s: impl Into<String>) {
            self.log.lock().push(s.into());
        }
        fn log(&self) -> Vec<String> {
            self.log.lock().clone()
        }
    }

    impl AudioControl for Recorder {
        fn capture(&self) -> Result<AudioState> {
            self.push("audio.capture");
            Ok(AudioState { bypass: true })
        }
        fn apply(&self, _: &AudioSettings) -> Result<AudioChainState> {
            self.push("audio.apply");
            Ok(AudioChainState::Active)
        }
        fn restore(&self, _: &AudioState) -> Result<()> {
            self.push("audio.restore");
            Ok(())
        }
    }

    impl DisplayControl for Recorder {
        fn capture(
            &self,
            target: Option<&MonitorProbe>,
            _: &DisplaySettings,
        ) -> Result<DisplayStateSnapshot> {
            self.push(format!(
                "display.capture {}",
                target.map(|t| t.id.0.as_str()).unwrap_or("-")
            ));
            Ok(DisplayStateSnapshot::default())
        }
        fn apply(&self, target: Option<&MonitorProbe>, _: &DisplaySettings) -> Result<DisplayVia> {
            self.push(format!("display.apply {}", target.map(|t| t.id.0.as_str()).unwrap_or("-")));
            if self.fail_display_apply {
                anyhow::bail!("nvapi says no")
            }
            Ok(DisplayVia { gamma: true, ..DisplayVia::default() })
        }
        fn restore(&self, _: &DisplayStateSnapshot) -> Result<()> {
            self.push("display.restore");
            Ok(())
        }
    }

    fn monitor(id: &str) -> MonitorProbe {
        MonitorProbe {
            id: MonitorId(id.into()),
            name: id.to_uppercase(),
            native: None,
            refresh_hz: None,
            primary: true,
            hmonitor: 1,
            gdi_name: r"\\.\DISPLAY1".into(),
            ddc: None,
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
        let m = monitor("mon:A");
        let applied = a.apply(&profile(), Some(&m)).unwrap();
        assert!(applied.via.gamma);
        let log = rec.log();
        let cap = log.iter().position(|s| s == "display.capture mon:A").unwrap();
        let app = log.iter().position(|s| s == "display.apply mon:A").unwrap();
        assert!(cap < app);
        assert!(BackupFile::at(dir.join("original-state.json")).pending().unwrap().is_some());
        a.restore().unwrap();
        assert!(BackupFile::at(dir.join("original-state.json")).pending().unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_apply_restores_immediately() {
        let (rec, mut a, dir) = setup(true);
        assert!(a.apply(&profile(), Some(&monitor("mon:A"))).is_err());
        let log = rec.log();
        assert!(log.contains(&"display.restore".to_string()));
        assert!(log.contains(&"audio.restore".to_string()));
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
    fn same_profile_same_monitor_is_a_no_op() {
        let (rec, mut a, dir) = setup(false);
        let m = monitor("mon:A");
        let p = profile();
        a.apply(&p, Some(&m)).unwrap();
        let before = rec.log().len();
        let applied = a.apply(&p, Some(&m)).unwrap();
        assert_eq!(rec.log().len(), before, "no backend calls on the fast path");
        assert!(applied.via.gamma, "via is replayed, not reset");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn moving_monitors_restores_old_then_applies_new() {
        let (rec, mut a, dir) = setup(false);
        let p = profile();
        a.apply(&p, Some(&monitor("mon:A"))).unwrap();
        rec.log.lock().clear();
        a.apply(&p, Some(&monitor("mon:B"))).unwrap();
        let log = rec.log();
        let restore = log.iter().position(|s| s == "display.restore").unwrap();
        let cap_b = log.iter().position(|s| s == "display.capture mon:B").unwrap();
        let app_b = log.iter().position(|s| s == "display.apply mon:B").unwrap();
        assert!(restore < cap_b && cap_b < app_b, "restore old → capture new → apply new: {log:?}");
        assert!(!log.contains(&"display.apply mon:A".to_string()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovers_pending_snapshot_on_start() {
        let (rec, mut a, dir) = setup(false);
        a.apply(&profile(), Some(&monitor("mon:A"))).unwrap();
        // Simulate a crash: drop the applier without restoring.
        drop(a);
        rec.log.lock().clear();
        let mut fresh =
            Applier::new(rec.clone(), rec.clone(), BackupFile::at(dir.join("original-state.json")));
        assert!(fresh.recover_on_start().unwrap());
        assert!(rec.log().contains(&"display.restore".to_string()));
        assert!(!fresh.recover_on_start().unwrap(), "second start finds nothing pending");
        let _ = std::fs::remove_dir_all(dir);
    }
}
