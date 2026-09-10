//! Receiver-side presentation: MF HEVC hardware decode → D3D11 swapchain
//! window, plus WASAPI audio playback. Placeholder until the renderer lands;
//! `recv --headless` exercises the transport without it.

use std::sync::Arc;

use anyhow::{bail, Result};
use tokio::sync::mpsc;

use crate::transport::receiver::{AccessUnit, RecvStats};

pub async fn run(
    _aus: mpsc::Receiver<AccessUnit>,
    _opus: mpsc::Receiver<Vec<u8>>,
    _stats: Arc<RecvStats>,
    _closed: mpsc::Receiver<()>,
    _pc: impl webrtc::peer_connection::PeerConnection,
) -> Result<()> {
    bail!("renderer not implemented yet — use `recv --headless`")
}
