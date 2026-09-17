//! Receiver presentation, stubbed: there is no decoder or window outside
//! Windows yet. The transport half of a receive (pairing, DTLS-SRTP,
//! depacketizing, stats) is portable and runs up to this call. A macOS port
//! decodes with VideoToolbox (`VTDecompressionSession`) and presents through a
//! `CAMetalLayer`; see `docs/dev/porting.md`.

use std::sync::Arc;

use anyhow::Result;
use relay_core::platform::{unsupported, Capability};
use tokio::sync::mpsc;

use crate::transport::receiver::{AccessUnit, RecvStats};

/// Receiver output options beyond the window itself.
#[derive(Debug, Default, Clone)]
pub struct RenderOpts {
    /// Start "Relay Camera" and mirror decoded frames into its ring.
    pub vcam: bool,
    /// Render decoded audio to this endpoint id instead of the default device.
    pub mic_route: Option<String>,
}

pub async fn run(
    _aus: mpsc::Receiver<AccessUnit>,
    _opus: mpsc::Receiver<Vec<u8>>,
    _mic: mpsc::Receiver<Vec<u8>>,
    _stats: Arc<RecvStats>,
    _closed: mpsc::Receiver<()>,
    _pc: impl webrtc::peer_connection::PeerConnection,
    _opts: RenderOpts,
) -> Result<()> {
    Err(unsupported(Capability::Share))
}
