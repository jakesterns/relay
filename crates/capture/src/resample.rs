//! Getting whatever the endpoint gives us into what Opus takes.
//!
//! Opus on the share path is fixed at 48 kHz stereo — the receiver, the MP4
//! muxer and the virtual mic all assume it. WASAPI endpoints are not: a USB
//! interface commonly runs 44.1 kHz, an audiophile DAC 96 or 192 kHz, and
//! nearly every headset microphone is mono. Before S7 each of those hit a
//! `bail!` and the share simply refused to start.
//!
//! So this module is the one seam that adapts: fold to stereo, then convert
//! the rate. Both steps are pure and state-carrying across blocks (the
//! interpolator keeps four input frames of history and a fractional read
//! position), so 10 ms WASAPI packets stitch together without a click at every
//! boundary.
//!
//! This is *not* the APO path. CLAUDE.md's "never resample" rule is about the
//! endpoint chain, where Relay must match the hardware rate exactly. Here we
//! are encoding for the network, where 48 kHz is the destination format and
//! conversion is unavoidable.

/// Everything Opus needs from a WASAPI endpoint, in one place.
pub const OPUS_RATE: u32 = 48_000;
pub const OPUS_CHANNELS: u16 = 2;

/// Rates we will convert from. The low end is below any real endpoint; the
/// high end is 768 kHz, the top of the WASAPI/USB Audio 2 range. Outside this
/// the endpoint is reporting nonsense and we say so rather than allocating a
/// resampler for it.
pub const MIN_RATE: u32 = 4_000;
pub const MAX_RATE: u32 = 768_000;

/// Why an endpoint format cannot be used, with the numbers that make it
/// actionable. `Display` is what the user sees in the share's error line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    NoChannels,
    Rate { hz: u32 },
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoChannels => write!(
                f,
                "the audio endpoint reports 0 channels, which is not a format Relay can read. \
                 Open Sound settings > the device > Properties > Advanced and pick a normal \
                 format (for example \"2 channel, 24 bit, 48000 Hz\"), then start the share again"
            ),
            Self::Rate { hz } => write!(
                f,
                "the audio endpoint reports {hz} Hz, outside the {MIN_RATE}–{MAX_RATE} Hz range \
                 Relay can convert from. Open Sound settings > the device > Properties > Advanced \
                 and pick a standard rate (44100, 48000, 96000 or 192000 Hz), then start the \
                 share again"
            ),
        }
    }
}

impl std::error::Error for FormatError {}

/// Fold `channels` interleaved f32 channels down to stereo, in place of a new
/// buffer. Mono is duplicated; stereo passes through; anything wider is
/// downmixed with the ITU-R BS.775 coefficients over the WASAPI channel order
/// (FL FR FC LFE BL BR SL SR ...), LFE dropped. The sum can exceed unity on
/// hot 5.1 content, so the result is clamped — Opus float input above 1.0
/// clips harder than we would.
pub fn fold_to_stereo(samples: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels as usize;
    match channels {
        0 => Vec::new(),
        1 => {
            let mut out = Vec::with_capacity(samples.len() * 2);
            for s in samples {
                out.push(*s);
                out.push(*s);
            }
            out
        }
        2 => samples.to_vec(),
        _ => {
            const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;
            const SURROUND: f32 = std::f32::consts::FRAC_1_SQRT_2;
            let frames = samples.len() / ch;
            let mut out = Vec::with_capacity(frames * 2);
            for f in samples.chunks_exact(ch) {
                let mut l = f[0];
                let mut r = f[1];
                if ch > 2 {
                    l += CENTRE * f[2];
                    r += CENTRE * f[2];
                }
                // index 3 is LFE — deliberately dropped.
                for (i, s) in f.iter().enumerate().skip(4) {
                    // Remaining pairs are (left, right): BL BR, then SL SR.
                    if i % 2 == 0 {
                        l += SURROUND * s;
                    } else {
                        r += SURROUND * s;
                    }
                }
                out.push(l.clamp(-1.0, 1.0));
                out.push(r.clamp(-1.0, 1.0));
            }
            out
        }
    }
}

/// One direct-form-II transposed biquad section.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// RBJ low-pass at `fc` for a stream running at `rate`.
    fn low_pass(rate: f32, fc: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * (fc / rate);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cos) / 2.0) / a0,
            b1: (1.0 - cos) / a0,
            b2: ((1.0 - cos) / 2.0) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn step(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Stereo f32 at any rate in → stereo f32 at 48 kHz out.
///
/// Catmull-Rom interpolation over a fractional read position. Downsampling
/// first runs three cascaded low-pass sections (6th-order Butterworth) at
/// 20 kHz so 96/192 kHz material does not alias into the passband; upsampling
/// needs no filter, because interpolation adds no images below the input's own
/// Nyquist.
pub struct ToOpus48 {
    src_rate: u32,
    step: f64,
    /// Read position, in input frames, into `tail ++ incoming`.
    phase: f64,
    /// Last four input frames, interleaved stereo, prepended to the next block.
    tail: Vec<f32>,
    lp: Option<[[Biquad; 3]; 2]>,
}

/// Four frames of history is what Catmull-Rom needs (x[i-1]..x[i+2]) plus one
/// so the read position never falls below index 1 at the start of a block.
const HISTORY_FRAMES: usize = 4;

/// Anti-alias corner, comfortably under the 24 kHz output Nyquist and above
/// anything a headset reproduces.
const ANTI_ALIAS_HZ: f32 = 20_000.0;

impl ToOpus48 {
    /// `src_rate` must already have been checked with [`check_format`].
    pub fn new(src_rate: u32) -> Self {
        let lp = (src_rate > OPUS_RATE).then(|| {
            let section = |q: f32| Biquad::low_pass(src_rate as f32, ANTI_ALIAS_HZ, q);
            // Three sections at the 6th-order Butterworth Q values: -36 dB one
            // octave past the corner, which puts a 40 kHz tone from a 192 kHz
            // endpoint well below audibility after decimation.
            // Middle section's Q is exactly 1/sqrt(2) for 6th-order Butterworth.
            let bank =
                || [section(0.5176), section(std::f32::consts::FRAC_1_SQRT_2), section(1.9319)];
            [bank(), bank()]
        });
        Self {
            src_rate,
            step: src_rate as f64 / OPUS_RATE as f64,
            phase: 1.0,
            tail: vec![0.0; HISTORY_FRAMES * 2],
            lp,
        }
    }

    pub fn src_rate(&self) -> u32 {
        self.src_rate
    }

    /// True when this converter is a no-op (endpoint already at 48 kHz).
    pub fn is_passthrough(&self) -> bool {
        self.src_rate == OPUS_RATE
    }

    /// Convert one block of interleaved stereo. Returns interleaved stereo at
    /// 48 kHz; block boundaries are continuous across calls.
    pub fn push(&mut self, stereo: &[f32]) -> Vec<f32> {
        if self.is_passthrough() || stereo.is_empty() {
            return stereo.to_vec();
        }
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(stereo);
        if let Some(lp) = self.lp.as_mut() {
            // Filter only the fresh frames; the tail was filtered last call.
            for frame in buf[HISTORY_FRAMES * 2..].chunks_exact_mut(2) {
                for (c, sections) in lp.iter_mut().enumerate() {
                    let mut x = frame[c];
                    for s in sections.iter_mut() {
                        x = s.step(x);
                    }
                    frame[c] = x;
                }
            }
        }

        let frames = buf.len() / 2;
        // Need indices floor(phase)-1 ..= floor(phase)+2 in range.
        let last_start = frames as f64 - 3.0;
        let mut out =
            Vec::with_capacity(((frames as f64 - self.phase) / self.step) as usize * 2 + 4);
        while self.phase <= last_start {
            let i = self.phase.floor() as usize;
            let t = (self.phase - i as f64) as f32;
            for c in 0..2 {
                out.push(catmull_rom(
                    buf[(i - 1) * 2 + c],
                    buf[i * 2 + c],
                    buf[(i + 1) * 2 + c],
                    buf[(i + 2) * 2 + c],
                    t,
                ));
            }
            self.phase += self.step;
        }

        // Keep the last four frames; the frame formerly at `frames - 4`
        // becomes index 0, so the read position shifts by that much and stays
        // above 1.0 for the next block.
        let keep = HISTORY_FRAMES.min(frames);
        let drop_frames = frames - keep;
        self.tail = buf[drop_frames * 2..].to_vec();
        if self.tail.len() < HISTORY_FRAMES * 2 {
            // Pathologically short first block: pad on the left with silence.
            let mut padded = vec![0.0; HISTORY_FRAMES * 2 - self.tail.len()];
            padded.extend_from_slice(&self.tail);
            self.phase += (padded.len() - self.tail.len()) as f64 / 2.0;
            self.tail = padded;
        }
        self.phase -= drop_frames as f64;
        out
    }
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    ((a * t + b) * t + c) * t + p1
}

/// Reject only what cannot be converted at all, and say why in a sentence the
/// user can act on.
pub fn check_format(rate: u32, channels: u16) -> Result<(), FormatError> {
    if channels == 0 {
        return Err(FormatError::NoChannels);
    }
    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
        return Err(FormatError::Rate { hz: rate });
    }
    Ok(())
}

/// One-line note for the log and the instrument strip when the endpoint is not
/// already 48 kHz stereo — so a support question about audio quality has the
/// conversion visible rather than silent.
pub fn conversion_note(rate: u32, channels: u16) -> Option<String> {
    if rate == OPUS_RATE && channels == OPUS_CHANNELS {
        return None;
    }
    let ch = match channels {
        1 => "mono → stereo".to_string(),
        2 => String::new(),
        n => format!("{n} channels → stereo"),
    };
    let hz = if rate == OPUS_RATE { String::new() } else { format!("{rate} Hz → 48000 Hz") };
    let parts: Vec<&str> =
        [ch.as_str(), hz.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
    Some(format!("endpoint audio converted for the share: {}", parts.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_duplicated_and_stereo_passes_through() {
        assert_eq!(fold_to_stereo(&[0.5, -0.25], 1), vec![0.5, 0.5, -0.25, -0.25]);
        assert_eq!(fold_to_stereo(&[0.5, -0.25], 2), vec![0.5, -0.25]);
        assert!(fold_to_stereo(&[0.5], 0).is_empty());
    }

    #[test]
    fn five_one_downmix_drops_lfe_and_never_clips() {
        // FL FR FC LFE BL BR, LFE deliberately enormous.
        let frame = [0.4, 0.2, 0.4, 9.0, 0.2, 0.1];
        let out = fold_to_stereo(&frame, 6);
        assert_eq!(out.len(), 2);
        let k = std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - (0.4 + k * 0.4 + k * 0.2)).abs() < 1e-5, "{out:?}");
        assert!((out[1] - (0.2 + k * 0.4 + k * 0.1)).abs() < 1e-5, "{out:?}");

        // Hot content clamps rather than handing Opus values above 1.0.
        let hot = fold_to_stereo(&[1.0, 1.0, 1.0, 0.0, 1.0, 1.0], 6);
        assert_eq!(hot, vec![1.0, 1.0]);
    }

    #[test]
    fn forty_eight_k_is_a_pure_passthrough() {
        let mut r = ToOpus48::new(48_000);
        assert!(r.is_passthrough());
        let block: Vec<f32> = (0..960).map(|i| i as f32 * 0.001).collect();
        assert_eq!(r.push(&block), block);
    }

    /// Feed a fixed number of input frames in many small blocks and check the
    /// output frame count tracks the ratio — this is what stops audio drifting
    /// away from video over a long share.
    #[test]
    fn output_length_tracks_the_rate_ratio_across_blocks() {
        for rate in [8_000u32, 16_000, 22_050, 44_100, 88_200, 96_000, 192_000] {
            let mut r = ToOpus48::new(rate);
            let block_frames = (rate as usize / 100).max(1); // 10 ms
            let blocks = 100; // one second
            let mut produced = 0usize;
            for b in 0..blocks {
                let block: Vec<f32> = (0..block_frames * 2)
                    .map(|i| ((b * block_frames * 2 + i) as f32 * 0.01).sin())
                    .collect();
                produced += r.push(&block).len() / 2;
            }
            let expected = (block_frames * blocks) as f64 * 48_000.0 / rate as f64;
            let drift = (produced as f64 - expected).abs();
            assert!(
                drift < 8.0,
                "{rate} Hz: produced {produced} frames, expected ~{expected:.0} (drift {drift:.1})"
            );
        }
    }

    /// A 1 kHz sine resampled from 44.1 kHz must come out as a 1 kHz sine, not
    /// a staircase: check the peak is preserved and successive samples stay on
    /// a smooth curve (no boundary discontinuity every block).
    #[test]
    fn a_sine_survives_44k1_to_48k() {
        let rate = 44_100u32;
        let mut r = ToOpus48::new(rate);
        let mut out = Vec::new();
        let mut n = 0usize;
        for _ in 0..50 {
            let block: Vec<f32> = (0..441 * 2)
                .map(|i| {
                    let frame = n + i / 2;
                    (2.0 * std::f32::consts::PI * 1000.0 * frame as f32 / rate as f32).sin()
                })
                .collect();
            n += 441;
            out.extend(r.push(&block));
        }
        // Skip the priming transient, then compare against the ideal 48 kHz sine.
        let left: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        let start = 480;
        let mut worst = 0.0f32;
        for (i, s) in left.iter().enumerate().skip(start) {
            // The converter's output frame i corresponds to input time
            // (i * rate / 48000) + a fixed priming offset; compare phase-free
            // by checking the envelope instead.
            let _ = i;
            worst = worst.max(s.abs());
        }
        assert!((0.97..=1.01).contains(&worst), "peak {worst} — sine amplitude not preserved");

        // Smoothness: at 1 kHz into 48 kHz no adjacent pair may jump more than
        // one period's worth of slope. A block-boundary click shows up here.
        let max_step = 2.0 * std::f32::consts::PI * 1000.0 / 48_000.0 * 1.5;
        for w in left.windows(2).skip(start) {
            assert!(
                (w[1] - w[0]).abs() <= max_step,
                "discontinuity {:.4} > {max_step:.4} between {:?}",
                (w[1] - w[0]).abs(),
                w
            );
        }
    }

    #[test]
    fn downsampling_filters_content_above_the_new_nyquist() {
        // 40 kHz tone at 192 kHz in: must not come back as an alias at 48 kHz.
        let rate = 192_000u32;
        let mut r = ToOpus48::new(rate);
        let mut out = Vec::new();
        let mut n = 0usize;
        for _ in 0..40 {
            let block: Vec<f32> = (0..1920 * 2)
                .map(|i| {
                    let frame = n + i / 2;
                    (2.0 * std::f32::consts::PI * 40_000.0 * frame as f32 / rate as f32).sin()
                })
                .collect();
            n += 1920;
            out.extend(r.push(&block));
        }
        let tail: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).skip(4_800).collect();
        let peak = tail.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak < 0.05, "40 kHz aliased through at amplitude {peak}");
    }

    #[test]
    fn short_and_empty_blocks_do_not_panic() {
        let mut r = ToOpus48::new(44_100);
        assert!(r.push(&[]).is_empty());
        for _ in 0..10 {
            r.push(&[0.1, 0.1]);
        }
        r.push(&[0.0; 8]);
    }

    #[test]
    fn format_check_names_the_actual_number() {
        assert_eq!(check_format(0, 2), Err(FormatError::Rate { hz: 0 }));
        assert_eq!(check_format(48_000, 0), Err(FormatError::NoChannels));
        assert!(check_format(44_100, 1).is_ok());
        assert!(check_format(192_000, 8).is_ok());
        let msg = FormatError::Rate { hz: 1_000_000 }.to_string();
        assert!(msg.contains("1000000 Hz"), "{msg}");
        assert!(msg.contains("Sound settings"), "{msg}");
    }

    /// The dev PC's endpoints are all 48 kHz stereo, so the conversion path
    /// cannot be exercised live here without changing a Windows endpoint's
    /// default format — which Relay is not allowed to touch. This is the
    /// closest honest substitute: drive the real Opus encoder with what a
    /// 44.1 kHz *mono* microphone would produce, packet for packet, and decode
    /// it back. It proves the converter emits exactly what `OpusStream`
    /// promises (480-frame stereo blocks at 48 kHz) rather than something the
    /// encoder merely tolerates.
    #[cfg(windows)]
    #[test]
    fn mono_44k1_drives_the_real_opus_encoder() {
        let (rate, channels) = (44_100u32, 1u16);
        let mut convert = ToOpus48::new(rate);
        let mut enc =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        enc.set_bitrate(opus::Bitrate::Bits(160_000)).unwrap();
        let mut dec = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();

        let frame_samples = 480 * 2;
        let mut pending: Vec<f32> = Vec::new();
        let mut packets = 0usize;
        let mut decoded_peak = 0.0f32;
        let mut n = 0usize;
        // One second of 440 Hz, in the 10 ms blocks WASAPI delivers.
        for _ in 0..100 {
            let block: Vec<f32> = (0..441)
                .map(|i| {
                    let t = (n + i) as f32 / rate as f32;
                    0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                })
                .collect();
            n += 441;
            let stereo = fold_to_stereo(&block, channels);
            assert_eq!(stereo.len(), block.len() * 2, "mono must fold to stereo");
            pending.extend_from_slice(&convert.push(&stereo));
            while pending.len() >= frame_samples {
                let frame: Vec<f32> = pending.drain(..frame_samples).collect();
                let data = enc.encode_vec_float(&frame, 1500).expect("encode");
                let mut out = vec![0.0f32; frame_samples];
                let frames = dec.decode_float(&data, &mut out, false).expect("decode");
                assert_eq!(frames, 480, "10 ms at 48 kHz");
                decoded_peak = decoded_peak.max(out.iter().fold(0.0f32, |a, s| a.max(s.abs())));
                packets += 1;
            }
        }
        // 44.1 kHz in for one second is ~100 packets of 10 ms out; the tail
        // sits in `pending`.
        assert!((98..=100).contains(&packets), "{packets} packets from one second");
        assert!((0.3..=0.7).contains(&decoded_peak), "decoded peak {decoded_peak}");
    }

    #[test]
    fn conversion_note_only_appears_when_something_is_converted() {
        assert!(conversion_note(48_000, 2).is_none());
        assert_eq!(
            conversion_note(48_000, 1).unwrap(),
            "endpoint audio converted for the share: mono → stereo"
        );
        assert_eq!(
            conversion_note(44_100, 2).unwrap(),
            "endpoint audio converted for the share: 44100 Hz → 48000 Hz"
        );
        assert_eq!(
            conversion_note(96_000, 6).unwrap(),
            "endpoint audio converted for the share: 6 channels → stereo, 96000 Hz → 48000 Hz"
        );
    }
}
