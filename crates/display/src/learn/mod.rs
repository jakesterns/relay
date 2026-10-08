//! S47: learn a game's look from its own frames, per game × monitor.
//!
//! Pure: no OS calls, no capture. The sampler (`relay-share look`) hands
//! small frames to [`analyse::Analyser`] and prints only [`FrameReport`]s;
//! the core feeds those to a [`converge::Learner`] and turns a converged
//! [`derive::LookTargets`] into [`derive::Adjustments`] for one panel.
//!
//! Layout:
//! - [`analyse`] — per-frame statistics and frame classification.
//! - [`derive`] — aggregates → panel-neutral look → panel-aware adjustments.
//! - [`converge`] — readiness, convergence, rolling window, freeze rule.

pub mod analyse;
pub mod converge;
pub mod derive;

pub use analyse::{with_input_idle, Analyser, Frame, FrameClass, FrameReport, FrameStats, Order};
pub use converge::{Learner, Phase, Readiness, SAMPLE_FPS};
pub use derive::{realize, Adjustments, LookTargets, PanelCaps, PanelKind};

/// DXGI colour space of an output in HDR (PQ / BT.2020) mode:
/// `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020` = 12. A desktop duplicated in
/// that mode is not SDR display code values, so the sampler skips it and says
/// so instead of learning from the wrong space.
pub const DXGI_COLOR_SPACE_HDR_PQ: i32 = 12;

pub fn is_hdr_color_space(cs: i32) -> bool {
    cs == DXGI_COLOR_SPACE_HDR_PQ
}

#[cfg(test)]
mod tests;
