//! Hardware-only encoding. `convert` turns the captured BGRA texture into
//! NV12 on the GPU (and scales, e.g. 1440p→4K for benchmarks); `mf` drives the
//! vendor's HEVC or H.264 hardware MFT. There is deliberately no software path.

pub mod convert;
pub mod mf;

/// One encoded access unit (Annex B, HEVC or H.264 per the share's codec).
pub struct EncodedFrame {
    pub data: Vec<u8>,
    /// Sample time in QPC 100 ns ticks (carried through from capture).
    pub pts_100ns: i64,
    pub keyframe: bool,
}
