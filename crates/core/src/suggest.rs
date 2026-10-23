//! The profile offer: "Call of Duty is open — make a profile for it?"
//!
//! Relay never detected games; a profile had to be made by hand from the
//! running-process list, and nothing said so (owner's first live setup,
//! 2026-10-22). The core already watches which window is in front, so when a
//! full-screen app comes forward and no profile names its exe, it is offered
//! in `CoreState::suggestion`. Nothing is created: the UI asks, and only the
//! user's click makes a profile.
//!
//! What it looks at is what the foreground watcher already had: the window's
//! rectangle, its monitor's rectangle, and the exe name. No new hook, nothing
//! opened in the game's process.
//!
//! The decision is pure ([`decide`]) so it is tested without a desktop. The
//! declined list lives in `offers.json` beside the other data files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A full-screen app with no profile, offered to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub exe: String,
    pub title: String,
}

/// A screen rectangle, edges in pixels (right/bottom exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// The window covers its whole monitor: borderless or exclusive full screen,
/// which is how games run. A maximised window stops at the taskbar and so
/// does not count — a browser or an editor is not offered.
pub fn covers(window: Rect, monitor: Rect) -> bool {
    monitor.right > monitor.left
        && monitor.bottom > monitor.top
        && window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// Never offered, and focusing one keeps the current offer: Relay's own
/// windows (the user Alt-Tabs to Relay to act on the offer) and the Windows
/// shell surfaces that come forward on their own — the desktop, Start,
/// search, the lock screen, the task switcher.
const KEEP_OFFER: &[&str] = &[
    "relay-ui.exe",
    "relay-core.exe",
    "relay-share.exe",
    "relay-svc.exe",
    "relay-elevate.exe",
    "relay-preview.exe",
    "explorer.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "searchapp.exe",
    "lockapp.exe",
    "textinputhost.exe",
    "applicationframehost.exe",
    "dwm.exe",
];

fn is_shell_or_relay(exe: &str) -> bool {
    KEEP_OFFER.iter().any(|k| k.eq_ignore_ascii_case(exe))
}

/// What the foreground window is, as far as the offer cares.
#[derive(Debug, Clone, Copy)]
pub struct Seen<'a> {
    pub exe: &'a str,
    pub title: &'a str,
    pub full_screen: bool,
}

/// The offer after the foreground changed to `seen`.
///
/// - Relay's own windows and the shell keep the current offer (that is how
///   the user gets from the game to Relay without losing it), unless a
///   profile now exists for it or it was declined meanwhile.
/// - A full-screen app with no profile, not declined, becomes the offer.
/// - Anything else — an app with a profile, a windowed app — clears it.
pub fn decide(
    current: Option<&Suggestion>,
    seen: Seen<'_>,
    has_profile: impl Fn(&str) -> bool,
    declined: &[String],
) -> Option<Suggestion> {
    let refused = |exe: &str| declined.iter().any(|d| d.eq_ignore_ascii_case(exe));
    if seen.exe.is_empty() || is_shell_or_relay(seen.exe) {
        return current.filter(|s| !has_profile(&s.exe) && !refused(&s.exe)).cloned();
    }
    if seen.full_screen && !has_profile(seen.exe) && !refused(seen.exe) {
        let title = seen.title.trim();
        return Some(Suggestion { exe: seen.exe.to_string(), title: title.to_string() });
    }
    None
}

/// What a profile made from an offer is called: the window title when it
/// reads like a name, otherwise the exe without `.exe`.
pub fn profile_name(s: &Suggestion) -> String {
    let t = s.title.trim();
    if !t.is_empty() && t.len() <= 60 {
        return t.to_string();
    }
    let stem = s.exe.strip_suffix(".exe").or_else(|| s.exe.strip_suffix(".EXE")).unwrap_or(&s.exe);
    stem.to_string()
}

/// The exes the user said "Not for this game" to, persisted.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Declined {
    #[serde(default)]
    pub exes: Vec<String>,
    #[serde(skip)]
    path: PathBuf,
}

impl Declined {
    /// Load `offers.json` from the data folder; missing or unreadable is an
    /// empty list (an offer reappearing is harmless).
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("offers.json");
        let mut d: Declined = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        d.path = path;
        d
    }

    /// Add one exe (case-insensitive, once) and save.
    pub fn add(&mut self, exe: &str) -> Result<()> {
        let exe = exe.trim();
        if exe.is_empty() {
            return Ok(());
        }
        if !self.exes.iter().any(|e| e.eq_ignore_ascii_case(exe)) {
            self.exes.push(exe.to_string());
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        crate::profiles::write_atomic(&self.path, &bytes)
            .with_context(|| format!("saving {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };

    fn seen<'a>(exe: &'a str, full_screen: bool) -> Seen<'a> {
        Seen { exe, title: "Call of Duty®", full_screen }
    }
    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn full_screen_means_the_whole_monitor_not_maximised() {
        assert!(covers(MON, MON));
        // Exclusive-mode games often report a rect a pixel past the edges.
        assert!(covers(Rect { left: -1, top: -1, right: 2561, bottom: 1441 }, MON));
        // Maximised: stops at the taskbar.
        assert!(!covers(Rect { left: 0, top: 0, right: 2560, bottom: 1392 }, MON));
        // A second monitor to the right: the window must cover that one.
        let right = Rect { left: 2560, top: 0, right: 4480, bottom: 1080 };
        assert!(covers(Rect { left: 2560, top: 0, right: 4480, bottom: 1080 }, right));
        assert!(!covers(MON, right));
        // A degenerate monitor never counts.
        assert!(!covers(MON, Rect { left: 0, top: 0, right: 0, bottom: 0 }));
    }

    #[test]
    fn a_full_screen_app_with_no_profile_is_offered() {
        let s = decide(None, seen("cod.exe", true), none, &[]).unwrap();
        assert_eq!(s, Suggestion { exe: "cod.exe".into(), title: "Call of Duty®".into() });
    }

    #[test]
    fn windowed_apps_and_apps_with_a_profile_are_not() {
        assert_eq!(decide(None, seen("chrome.exe", false), none, &[]), None);
        let has = |e: &str| e.eq_ignore_ascii_case("cod.exe");
        assert_eq!(decide(None, seen("COD.EXE", true), has, &[]), None);
    }

    #[test]
    fn a_declined_game_is_never_offered_again() {
        let declined = vec!["CoD.exe".to_string()];
        assert_eq!(decide(None, seen("cod.exe", true), none, &declined), None);
    }

    #[test]
    fn alt_tab_to_relay_keeps_the_offer_until_acted_on() {
        let offer = decide(None, seen("cod.exe", true), none, &[]);
        let kept = decide(offer.as_ref(), seen("relay-ui.exe", false), none, &[]);
        assert_eq!(kept, offer);
        // Through the task switcher and the desktop too.
        let kept = decide(kept.as_ref(), seen("explorer.exe", false), none, &[]);
        assert_eq!(kept, offer);
        // The user made the profile: gone, even while Relay is in front.
        let has = |e: &str| e == "cod.exe";
        assert_eq!(decide(kept.as_ref(), seen("relay-ui.exe", false), has, &[]), None);
        // Or declined it.
        assert_eq!(
            decide(offer.as_ref(), seen("relay-ui.exe", false), none, &["cod.exe".into()]),
            None
        );
    }

    #[test]
    fn relay_and_the_shell_are_never_offered_themselves() {
        assert_eq!(decide(None, seen("relay-ui.exe", true), none, &[]), None);
        assert_eq!(decide(None, seen("explorer.exe", true), none, &[]), None);
        assert_eq!(decide(None, seen("", true), none, &[]), None);
    }

    #[test]
    fn another_app_coming_forward_moves_or_clears_the_offer() {
        let offer = decide(None, seen("cod.exe", true), none, &[]);
        assert_eq!(decide(offer.as_ref(), seen("discord.exe", false), none, &[]), None);
        let next = decide(offer.as_ref(), seen("eldenring.exe", true), none, &[]).unwrap();
        assert_eq!(next.exe, "eldenring.exe");
    }

    #[test]
    fn the_profile_is_named_after_the_window_or_the_exe() {
        let s = Suggestion { exe: "cod.exe".into(), title: " Call of Duty® ".into() };
        assert_eq!(profile_name(&s), "Call of Duty®");
        let s = Suggestion { exe: "cod.exe".into(), title: String::new() };
        assert_eq!(profile_name(&s), "cod");
    }

    #[test]
    fn the_declined_list_persists_once_per_exe() {
        let dir = std::env::temp_dir().join(format!("relay-offers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = Declined::load(&dir);
        assert!(d.exes.is_empty());
        d.add("cod.exe").unwrap();
        d.add("COD.exe").unwrap();
        let again = Declined::load(&dir);
        assert_eq!(again.exes, vec!["cod.exe".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }
}
