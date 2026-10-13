//! Pure frame and timing logic for NDI® output: pixel packing, audio
//! de-interleaving, frame-rate detection and timecodes. No runtime, no GPU,
//! so all of it is unit-tested.

/// 100 ns ticks per second: the unit of NDI timecodes and of Relay's pts.
pub const TICKS_PER_SEC: i64 = 10_000_000;

/// Pack a staged NV12 picture into the layout NDI's `NV12` FourCC wants: `h`
/// rows of Y, then `h / 2` rows of interleaved UV, both `w` bytes wide with no
/// padding. The staging copy is pitched and its height is the decoder's
/// aligned height (1088 for a 1080 stream), so the UV plane does not start
/// where NDI expects; this moves it. Returns the line stride (`w`), or `None`
/// when the source is too small for the size asked for.
pub fn pack_nv12(
    y: &[u8],
    uv: &[u8],
    pitch: usize,
    w: usize,
    h: usize,
    dst: &mut Vec<u8>,
) -> Option<usize> {
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || pitch < w {
        return None;
    }
    let uv_rows = h / 2;
    if y.len() < pitch * (h - 1) + w || uv.len() < pitch * (uv_rows - 1) + w {
        return None;
    }
    dst.clear();
    dst.reserve(w * h + w * uv_rows);
    for row in 0..h {
        dst.extend_from_slice(&y[row * pitch..row * pitch + w]);
    }
    for row in 0..uv_rows {
        dst.extend_from_slice(&uv[row * pitch..row * pitch + w]);
    }
    Some(w)
}

/// Interleaved frames (L R L R …) to planar (L L … R R …). `dst` is resized
/// to exactly `frames * channels`; returns the frame count.
pub fn deinterleave(interleaved: &[f32], channels: usize, dst: &mut Vec<f32>) -> usize {
    if channels == 0 {
        dst.clear();
        return 0;
    }
    let frames = interleaved.len() / channels;
    dst.clear();
    dst.resize(frames * channels, 0.0);
    for (i, frame) in interleaved.chunks_exact(channels).enumerate() {
        for (c, s) in frame.iter().enumerate() {
            dst[c * frames + i] = *s;
        }
    }
    frames
}

/// The frame rates NDI receivers (OBS, vMix, Studio Monitor) expect to see,
/// as exact fractions. A measured interval is snapped to the nearest one.
const RATES: &[(i32, i32)] = &[
    (24000, 1001),
    (24, 1),
    (25, 1),
    (30000, 1001),
    (30, 1),
    (50, 1),
    (60000, 1001),
    (60, 1),
    (90, 1),
    (100, 1),
    (120, 1),
    (144, 1),
    (165, 1),
    (240, 1),
];

/// Nearest standard rate to `fps`, or `fps` itself rounded when nothing is
/// within 1.5 %.
pub fn snap_rate(fps: f64) -> (i32, i32) {
    if !fps.is_finite() || fps <= 0.0 {
        return (60, 1);
    }
    let best = RATES
        .iter()
        .copied()
        .min_by(|a, b| {
            let da = (a.0 as f64 / a.1 as f64 - fps).abs();
            let db = (b.0 as f64 / b.1 as f64 - fps).abs();
            da.total_cmp(&db)
        })
        .expect("non-empty");
    if ((best.0 as f64 / best.1 as f64) - fps).abs() / fps <= 0.015 {
        best
    } else {
        ((fps.round() as i32).clamp(1, 1000), 1)
    }
}

/// The stream's frame rate, learned from the pts of decoded frames. Relay
/// does not tell the receiver the rate (a share runs at whatever the source
/// gives), so it is measured: a smoothed interval, snapped to a standard
/// rate, starting from the share default until there is evidence.
#[derive(Debug, Clone)]
pub struct FrameRate {
    last_pts: Option<i64>,
    /// Smoothed interval, 100 ns.
    avg: Option<f64>,
    samples: u32,
    rate: (i32, i32),
}

impl FrameRate {
    /// Frames needed before the measurement replaces the default.
    const SETTLE: u32 = 30;

    pub fn new(default_fps: u32) -> Self {
        Self { last_pts: None, avg: None, samples: 0, rate: snap_rate(default_fps as f64) }
    }

    pub fn rate(&self) -> (i32, i32) {
        self.rate
    }

    /// Feed one frame's pts; returns the current rate.
    pub fn push(&mut self, pts_100ns: i64) -> (i32, i32) {
        if let Some(last) = self.last_pts {
            let d = pts_100ns - last;
            // A pause (a switch, a stall) or a pts jump is not a frame
            // interval: ignore anything outside 4..1000 fps.
            if (10_000..=2_500_000).contains(&d) {
                let d = d as f64;
                self.avg = Some(match self.avg {
                    None => d,
                    Some(a) => a + (d - a) * 0.05,
                });
                self.samples = self.samples.saturating_add(1);
                if self.samples >= Self::SETTLE {
                    let fps = TICKS_PER_SEC as f64 / self.avg.expect("set above");
                    self.rate = snap_rate(fps);
                }
            }
        }
        self.last_pts = Some(pts_100ns);
        self.rate
    }
}

/// Timecodes for one NDI sender, video and audio on one clock.
///
/// NDI timecodes are 100 ns ticks; receivers use them to line audio up with
/// video. Both halves are stamped from the same local monotonic clock (`now`
/// is ticks since the output started), so they agree with each other the way
/// local playback does:
///
/// - video: the moment the frame was presented here;
/// - audio: the moment the buffer will be *heard* here (`now` plus what is
///   already queued in the endpoint), advanced by sample count so a steady
///   stream gets gapless, monotonic timecodes, and re-anchored only when that
///   count drifts more than [`AudioClock::REANCHOR`] from the clock (a stall
///   or a device change), never sample-by-sample.
#[derive(Debug, Clone, Default)]
pub struct AudioClock {
    anchor: Option<i64>,
    samples: u64,
    rate: u32,
    pub reanchors: u64,
}

impl AudioClock {
    /// 20 ms: below that, sample-count timing wins (it is exact); above,
    /// something happened and the clock wins.
    pub const REANCHOR: i64 = 200_000;

    pub fn new(rate: u32) -> Self {
        Self { rate, ..Self::default() }
    }

    /// Timecode for a buffer of `frames` written at `now` with `queued`
    /// frames already ahead of it in the endpoint.
    pub fn stamp(&mut self, now: i64, queued: u32, frames: usize) -> i64 {
        let rate = self.rate.max(1) as i64;
        let heard = now + queued as i64 * TICKS_PER_SEC / rate;
        let expected = self.anchor.map(|a| a + (self.samples as i64 * TICKS_PER_SEC) / rate);
        let tc = match expected {
            Some(e) if (e - heard).abs() <= Self::REANCHOR => e,
            Some(_) => {
                self.reanchors += 1;
                self.anchor = Some(heard);
                self.samples = 0;
                heard
            }
            None => {
                self.anchor = Some(heard);
                self.samples = 0;
                heard
            }
        };
        self.samples += frames as u64;
        tc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_drops_pitch_padding_and_aligned_rows() {
        // A 4x2 picture staged at pitch 8 in a texture 4 rows tall (aligned).
        let pitch = 8;
        let tex_h = 4;
        let mut y = vec![0xEEu8; pitch * tex_h];
        for row in 0..2 {
            for x in 0..4 {
                y[row * pitch + x] = (row * 10 + x) as u8;
            }
        }
        let mut uv = vec![0xEEu8; pitch * tex_h / 2];
        for (x, b) in uv.iter_mut().take(4).enumerate() {
            *b = 100 + x as u8;
        }
        let mut out = Vec::new();
        let stride = pack_nv12(&y, &uv, pitch, 4, 2, &mut out).unwrap();
        assert_eq!(stride, 4);
        assert_eq!(out, vec![0, 1, 2, 3, 10, 11, 12, 13, 100, 101, 102, 103]);
    }

    #[test]
    fn nv12_rejects_bad_geometry() {
        let mut out = Vec::new();
        assert!(pack_nv12(&[0; 16], &[0; 8], 4, 3, 2, &mut out).is_none(), "odd width");
        assert!(pack_nv12(&[0; 16], &[0; 8], 4, 4, 3, &mut out).is_none(), "odd height");
        assert!(pack_nv12(&[0; 16], &[0; 8], 2, 4, 2, &mut out).is_none(), "pitch < width");
        assert!(pack_nv12(&[0; 4], &[0; 8], 4, 4, 2, &mut out).is_none(), "short Y");
        assert!(pack_nv12(&[0; 8], &[0; 2], 4, 4, 2, &mut out).is_none(), "short UV");
        // The last row may be short of a full pitch (a tight staging map).
        assert!(pack_nv12(&[0; 8], &[0; 4], 4, 4, 2, &mut out).is_some());
    }

    #[test]
    fn nv12_full_hd_size() {
        let (w, h, pitch, tex_h) = (1920, 1080, 2048, 1088);
        let y = vec![1u8; pitch * tex_h];
        let uv = vec![2u8; pitch * tex_h / 2];
        let mut out = Vec::new();
        pack_nv12(&y, &uv, pitch, w, h, &mut out).unwrap();
        assert_eq!(out.len(), w * h * 3 / 2);
        assert_eq!(out[w * h - 1], 1);
        assert_eq!(out[w * h], 2, "UV starts straight after h rows of Y");
    }

    #[test]
    fn deinterleave_stereo() {
        let mut out = Vec::new();
        let n = deinterleave(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0], 2, &mut out);
        assert_eq!(n, 3);
        assert_eq!(out, vec![1.0, 2.0, 3.0, -1.0, -2.0, -3.0]);
        // A ragged tail (never produced by the mixer) is dropped, not misread.
        let n = deinterleave(&[1.0, 2.0, 3.0], 2, &mut out);
        assert_eq!((n, out.len()), (1, 2));
        assert_eq!(deinterleave(&[1.0], 0, &mut out), 0);
    }

    #[test]
    fn rates_snap_to_standard_fractions() {
        assert_eq!(snap_rate(60.0), (60, 1));
        assert_eq!(snap_rate(59.94), (60000, 1001));
        assert_eq!(snap_rate(60.5), (60, 1), "within 1.5 % of 60");
        assert_eq!(snap_rate(29.97), (30000, 1001));
        assert_eq!(snap_rate(143.8), (144, 1));
        assert_eq!(snap_rate(72.0), (72, 1), "nothing standard nearby");
        assert_eq!(snap_rate(0.0), (60, 1));
        assert_eq!(snap_rate(f64::NAN), (60, 1));
    }

    #[test]
    fn frame_rate_is_learned_from_pts() {
        let mut fr = FrameRate::new(60);
        assert_eq!(fr.rate(), (60, 1));
        let mut pts = 0i64;
        for _ in 0..100 {
            pts += 333_333; // 30 fps
            fr.push(pts);
        }
        assert_eq!(fr.rate(), (30, 1));
        // A two-second pause is not an interval and does not move the rate.
        pts += 20_000_000;
        assert_eq!(fr.push(pts), (30, 1));
    }

    #[test]
    fn frame_rate_waits_for_evidence() {
        let mut fr = FrameRate::new(60);
        for i in 1..10 {
            fr.push(i * 400_000); // 25 fps, but only a handful
        }
        assert_eq!(fr.rate(), (60, 1));
    }

    #[test]
    fn audio_timecodes_are_gapless_while_the_clock_agrees() {
        let mut c = AudioClock::new(48_000);
        // 10 ms buffers written every ~10 ms with 1 ms of scheduling jitter.
        let t0 = c.stamp(1_000_000, 480, 480);
        assert_eq!(t0, 1_000_000 + 100_000, "heard after the 10 ms already queued");
        let mut now = 1_000_000;
        for i in 1..=100i64 {
            now += 100_000 + if i % 2 == 0 { 10_000 } else { -10_000 };
            let tc = c.stamp(now, 480, 480);
            assert_eq!(tc, t0 + i * 100_000, "buffer {i}");
        }
        assert_eq!(c.reanchors, 0);
    }

    #[test]
    fn audio_reanchors_after_a_stall() {
        let mut c = AudioClock::new(48_000);
        let t0 = c.stamp(0, 0, 480);
        assert_eq!(t0, 0);
        assert_eq!(c.stamp(100_000, 0, 480), 100_000);
        // Half a second of nothing (a device change), then playback resumes.
        let tc = c.stamp(5_000_000, 0, 480);
        assert_eq!(tc, 5_000_000);
        assert_eq!(c.reanchors, 1);
        assert_eq!(c.stamp(5_100_000, 0, 480), 5_100_000);
    }
}
