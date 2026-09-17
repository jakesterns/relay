//! Hardware-only video decode (the receiver side), HEVC or H.264 per the
//! share's negotiated codec. Media Foundation MFT bound to the D3D11 device
//! that owns the presentation window, so decoded NV12 stays on the GPU all the
//! way to the swapchain.

pub mod mf;

use crate::codec::VideoCodec;

/// (width, height) in luma samples from the SPS in an Annex B access unit,
/// to size the window and the decoder before the first frame decodes.
pub fn probe_dimensions(codec: VideoCodec, au: &[u8]) -> Option<(u32, u32)> {
    codec.dimensions(au)
}
