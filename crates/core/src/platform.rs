//! What this build of Relay can do on the platform it was compiled for.
//!
//! Relay's Windows backends are the only real ones today. Everywhere else
//! the portable half (profiles, backup, DSP parameters, the hardware library,
//! presets, the uninstall planner) builds and tests, and every OS-facing
//! operation goes through a stub that fails with [`Unsupported`] naming the
//! [`Capability`] — never a silent default that looks like success.
//!
//! The rule the stubs follow: a *read* that has an honest empty answer
//! ("no processes with windows", "autostart is off") may return it; an
//! *action* (install, apply, spawn, fetch) returns [`unsupported`]. Status
//! surfaces can call [`supported`] to say so up front.
//!
//! `crates/core/tests/platform_seam.rs` keeps every `windows` crate call
//! inside a `#[cfg(windows)]` module; `docs/dev/porting.md` maps each
//! capability to its macOS equivalent.

use std::fmt;

/// One OS-facing capability. Coarse on purpose: this is the unit a port
/// lands in, and the unit the user would be told is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    /// The core's IPC endpoint (named pipe `\\.\pipe\relay-core`).
    Ipc,
    /// Foreground-app watching and global hotkeys (the core's event loop).
    FocusWatch,
    /// The notification-area icon and menu.
    Tray,
    /// Start at login.
    Autostart,
    /// Starting the core with no console window, and opening the UI from it.
    Launcher,
    /// Enumerating and closing other processes.
    Processes,
    /// Connected headsets, monitors and GPUs, and their change notifications.
    HardwareProbe,
    /// GPU colour (vibrance, hue), gamma ramps and DDC/CI monitor controls.
    DisplayControl,
    /// The per-app audio chain (endpoint APO on Windows).
    AudioProcessing,
    /// Detecting exclusive-mode streams that bypass the audio chain.
    ExclusiveModeDetect,
    /// The receiver's virtual camera.
    VirtualCamera,
    /// The inbound firewall rule for the share engine.
    Firewall,
    /// The administrator-rights install helper.
    Elevation,
    /// Removing Relay from the machine.
    Uninstall,
    /// Fetching a headset measurement over HTTPS.
    CatalogFetch,
    /// Screen and audio capture, hardware encode and the share engine.
    Share,
}

impl Capability {
    pub const ALL: [Capability; 16] = [
        Capability::Ipc,
        Capability::FocusWatch,
        Capability::Tray,
        Capability::Autostart,
        Capability::Launcher,
        Capability::Processes,
        Capability::HardwareProbe,
        Capability::DisplayControl,
        Capability::AudioProcessing,
        Capability::ExclusiveModeDetect,
        Capability::VirtualCamera,
        Capability::Firewall,
        Capability::Elevation,
        Capability::Uninstall,
        Capability::CatalogFetch,
        Capability::Share,
    ];

    /// Plain-English name, for messages the user may read.
    pub fn describe(self) -> &'static str {
        match self {
            Capability::Ipc => "the connection between Relay's window and its core",
            Capability::FocusWatch => "following the focused app and global hotkeys",
            Capability::Tray => "the tray icon",
            Capability::Autostart => "starting Relay at login",
            Capability::Launcher => "starting Relay in the background",
            Capability::Processes => "listing running apps",
            Capability::HardwareProbe => "detecting headsets and monitors",
            Capability::DisplayControl => "display colour and monitor controls",
            Capability::AudioProcessing => "per-app audio processing",
            Capability::ExclusiveModeDetect => "exclusive-mode audio detection",
            Capability::VirtualCamera => "the virtual camera",
            Capability::Firewall => "the firewall rule for sharing",
            Capability::Elevation => "installs that need administrator rights",
            Capability::Uninstall => "uninstalling from inside Relay",
            Capability::CatalogFetch => "downloading headset measurements",
            Capability::Share => "screen sharing",
        }
    }
}

/// This build's OS, as Rust names it (`windows`, `macos`, `linux`).
pub const OS: &str = std::env::consts::OS;

/// Whether this build has a real backend for `cap`.
pub fn supported(cap: Capability) -> bool {
    let _ = cap;
    cfg!(windows)
}

/// The error every stub returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported {
    pub capability: Capability,
    pub os: &'static str,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is not supported on this platform ({}) yet",
            self.capability.describe(),
            self.os
        )
    }
}

impl std::error::Error for Unsupported {}

/// `Err(unsupported(cap))?` in a stub. Downcast to [`Unsupported`] to tell
/// "missing on this OS" apart from a real failure.
pub fn unsupported(capability: Capability) -> anyhow::Error {
    Unsupported { capability, os: OS }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_capability_is_listed_once_and_described() {
        let mut seen = std::collections::BTreeSet::new();
        for c in Capability::ALL {
            assert!(seen.insert(c), "{c:?} listed twice");
            assert!(!c.describe().is_empty());
        }
    }

    #[test]
    fn unsupported_names_the_capability_and_the_os() {
        let e = unsupported(Capability::VirtualCamera);
        let text = e.to_string();
        assert!(text.contains("the virtual camera"), "{text}");
        assert!(text.contains(OS), "{text}");
        assert!(text.contains("not supported"), "{text}");
        let u = e.downcast_ref::<Unsupported>().unwrap();
        assert_eq!(u.capability, Capability::VirtualCamera);
    }

    #[test]
    fn windows_builds_have_every_backend_and_others_have_none() {
        for c in Capability::ALL {
            assert_eq!(supported(c), cfg!(windows), "{c:?}");
        }
    }
}
