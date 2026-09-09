//! Relay display.
//!
//! Planned layout:
//! - `gpu/` NvAPI (vibrance, gamma, contrast, hue, LUT) and ADLX equivalents.
//! - `ddc/` DDC/CI over `dxva2` (brightness, contrast, black equaliser, etc.),
//!   per monitor, with capability probing from the VCP string.
//! - `adapter` the `DisplayControl` impl for `relay_core::apply`: capture the
//!   current values, apply, restore. Only the monitor the game is on.
//!
//! Multi-monitor and restore-on-crash are listed as early risks in the brief;
//! the core's snapshot file is the recovery mechanism.

#![forbid(unsafe_code)]

pub const CRATE: &str = "relay-display";
