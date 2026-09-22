//! Relay core — the always-resident service.
//!
//! Responsibilities (and nothing more, to keep the footprint tiny):
//! - watch which window has focus and match it to a profile
//! - snapshot original audio/display state to disk *before* changing anything
//! - apply the profile while the game has focus, restore on blur/exit/crash/reboot
//! - global hotkeys
//! - a small IPC surface for the Tauri shell
//!
//! Everything heavy (DSP engine, capture/encode, UI) lives in other crates and
//! other processes and is loaded on demand.

pub mod apply;
pub mod audio_apo;
pub mod audio_bridge;
pub mod autostart;
pub mod backup;
pub mod config;
pub mod display_backend;
pub mod display_sim;
pub mod elevate;
pub mod firewall;
pub mod footprint;
pub mod hardware;
pub mod hotkeys;
pub mod instance;
pub mod ipc;
pub mod launcher;
pub mod logging;
pub mod peers;
pub mod presets;
pub mod processes;
pub mod profiles;
pub mod service;
pub mod share;
pub mod startup;
pub mod status;
pub mod tray;
pub mod types;
pub mod uiprefs;
pub mod uninstall;
pub mod vdevice;
pub mod winloop;

pub use config::Paths;
pub use types::*;
