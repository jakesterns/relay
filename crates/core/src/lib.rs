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
pub mod backup;
pub mod config;
pub mod footprint;
pub mod hardware;
pub mod hotkeys;
pub mod ipc;
pub mod profiles;
pub mod service;
pub mod types;
pub mod winloop;

pub use config::Paths;
pub use types::*;
