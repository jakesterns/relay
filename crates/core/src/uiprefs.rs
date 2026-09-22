//! Preferences about the app itself rather than about a game (`settings.json`).
//!
//! Today there is exactly one: what closing the window means. It lives in the
//! core rather than in the webview because the answer has to survive the
//! window it is about — the core is what keeps running, and the core is what
//! the tray hangs off, so the core is where the decision belongs.
//!
//! Forcing either behaviour would be wrong in both directions. Someone who
//! closes the window before launching a game wants Relay to stay up, because
//! that is the entire feature. Someone who closes the window expecting the app
//! to be gone should not find a process still holding their audio chain. So it
//! is a preference, defaulting to the one that matches what the app is for,
//! stated in plain words next to the toggle.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::profiles::write_atomic;

const FILE_VERSION: u32 = 1;

/// What happens to the core when the window is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CloseAction {
    /// Close the window, leave the core running. The default: profiles keep
    /// applying while you play, and the notification-area icon is how you get
    /// back or stop.
    #[default]
    KeepRunning,
    /// Close the window and stop Relay entirely, restoring everything on the
    /// way out.
    QuitRelay,
}

/// The whole settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPrefs {
    #[serde(default)]
    pub close_action: CloseAction,
    /// Bring a share back on its own after a crash, a dropped link or a
    /// reboot (S38). On by default — Jake's decision — because the person it
    /// is for is mid-stream with an audience and cannot rebuild it by hand.
    #[serde(default = "default_true")]
    pub resilience: bool,
    /// Say in the notification area that Relay is still running when the
    /// window closes (S38). On by default: a process that keeps going after
    /// its window has gone should say so, every time, until told not to.
    #[serde(default = "default_true")]
    pub close_notice: bool,
}

fn default_true() -> bool {
    true
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs { close_action: CloseAction::default(), resilience: true, close_notice: true }
    }
}

/// On-disk shape, versioned like the other stores so a later field can be
/// added without a migration scramble.
#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(flatten)]
    prefs: UiPrefs,
}

/// `settings.json`, loaded once at start and rewritten on every change.
#[derive(Debug, Clone)]
pub struct PrefsStore {
    path: PathBuf,
    prefs: UiPrefs,
}

impl PrefsStore {
    /// Read the file, or start from defaults.
    ///
    /// A missing file is the normal first-run case, not an error. A *corrupt*
    /// file is also not an error here: this is one enum controlling a window
    /// close, and refusing to start the core over it would be a far worse
    /// outcome than silently using the default.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let prefs = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<File>(&s).ok())
            .map(|f| f.prefs)
            .unwrap_or_default();
        Self { path, prefs }
    }

    pub fn get(&self) -> UiPrefs {
        self.prefs
    }

    /// Replace the preferences and write them out.
    pub fn set(&mut self, prefs: UiPrefs) -> Result<()> {
        self.prefs = prefs;
        self.save()
    }

    fn save(&self) -> Result<()> {
        let file = File { version: FILE_VERSION, prefs: self.prefs };
        let json = serde_json::to_vec_pretty(&file).context("serialising settings")?;
        write_atomic(&self.path, &json).with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("relay-uiprefs-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn the_default_keeps_the_core_running() {
        // The product decision, pinned: closing the window must not stop
        // Relay unless the user asked for that.
        assert_eq!(UiPrefs::default().close_action, CloseAction::KeepRunning);
    }

    #[test]
    fn a_missing_file_loads_as_defaults() {
        let p = temp("missing");
        let _ = std::fs::remove_file(&p);
        assert_eq!(PrefsStore::load(&p).get(), UiPrefs::default());
    }

    #[test]
    fn a_corrupt_file_loads_as_defaults_rather_than_failing() {
        let p = temp("corrupt");
        std::fs::write(&p, b"{ this is not json").unwrap();
        assert_eq!(PrefsStore::load(&p).get(), UiPrefs::default());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_saved_choice_survives_a_reload() {
        let p = temp("roundtrip");
        let mut s = PrefsStore::load(&p);
        s.set(UiPrefs { close_action: CloseAction::QuitRelay, ..Default::default() }).unwrap();

        let reloaded = PrefsStore::load(&p);
        assert_eq!(reloaded.get().close_action, CloseAction::QuitRelay);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_s38_switches_default_on_and_an_older_file_turns_them_on_too() {
        // Jake's decisions: resilience and the close notice are on unless
        // turned off. And the standing rule: a settings.json written before
        // these fields existed must not read as "off".
        assert!(UiPrefs::default().resilience);
        assert!(UiPrefs::default().close_notice);
        let p = temp("pre-s38");
        std::fs::write(&p, r#"{"version":1,"close_action":"quit_relay"}"#).unwrap();
        let prefs = PrefsStore::load(&p).get();
        assert_eq!(prefs.close_action, CloseAction::QuitRelay, "the old field survives");
        assert!(prefs.resilience);
        assert!(prefs.close_notice);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_file_is_versioned_and_uses_the_wire_spellings() {
        let p = temp("shape");
        let mut s = PrefsStore::load(&p);
        s.set(UiPrefs { close_action: CloseAction::QuitRelay, ..Default::default() }).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        // Must match the TypeScript mirror in ui/src/lib/ipc.ts.
        assert!(text.contains("\"close_action\": \"quit_relay\""), "{text}");
        let _ = std::fs::remove_file(&p);
    }
}
