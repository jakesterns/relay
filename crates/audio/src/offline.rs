//! Offline rendering for the A/B listening test: run a WAV through the
//! chain at the file's own rate (never resampling) and write the result,
//! so profiles can be tuned by ear before the APO (M3b) ships.

use std::path::Path;

use crate::dsp::{Chain, ChainParams};

const BLOCK_FRAMES: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum OfflineError {
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: hound::Error,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: hound::Error,
    },
    #[error(transparent)]
    Prepare(#[from] crate::dsp::PrepareError),
    #[error("unsupported channel count {0} (mono or stereo only)")]
    Channels(u16),
}

/// What the render actually did — the UI shows this next to the A/B buttons.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OfflineReport {
    pub sample_rate: u32,
    pub frames: usize,
    /// False when the profile asked for HRTF but no bundled IR matches the
    /// file's sample rate (the render then runs without HRTF).
    pub hrtf_applied: bool,
}

/// Render `input` through `params` into `output` (16-bit stereo WAV at the
/// input's rate). Mono input is duplicated to both channels first.
pub fn render(
    params: &ChainParams,
    input: &Path,
    output: &Path,
) -> Result<OfflineReport, OfflineError> {
    let read_err = |source| OfflineError::Read { path: input.display().to_string(), source };
    let mut reader = hound::WavReader::open(input).map_err(read_err)?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.channels > 2 {
        return Err(OfflineError::Channels(spec.channels));
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => {
            reader.samples::<f32>().collect::<Result<_, _>>().map_err(read_err)?
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(read_err)?
        }
    };
    let interleaved: Vec<f32> =
        if spec.channels == 1 { samples.iter().flat_map(|&s| [s, s]).collect() } else { samples };

    // A profile can ask for HRTF at a rate we have no IR for; render the rest
    // of the chain rather than failing the listening test.
    let mut effective = params.clone();
    let mut hrtf_applied = params.hrtf;
    let mut chain = Chain::new(effective.clone());
    if let Err(crate::dsp::PrepareError::HrtfUnsupportedRate(_)) =
        chain.prepare(spec.sample_rate, BLOCK_FRAMES)
    {
        effective.hrtf = false;
        hrtf_applied = false;
        chain = Chain::new(effective);
        chain.prepare(spec.sample_rate, BLOCK_FRAMES)?;
    }

    let mut processed = vec![0.0f32; interleaved.len()];
    for (inb, outb) in
        interleaved.chunks(2 * BLOCK_FRAMES).zip(processed.chunks_mut(2 * BLOCK_FRAMES))
    {
        chain.process(inb, outb);
    }

    let out_spec = hound::WavSpec {
        channels: 2,
        sample_rate: spec.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let write_err = |source| OfflineError::Write { path: output.display().to_string(), source };
    let mut writer = hound::WavWriter::create(output, out_spec).map_err(write_err)?;
    for &s in &processed {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        writer.write_sample(v).map_err(write_err)?;
    }
    writer.finalize().map_err(write_err)?;

    Ok(OfflineReport { sample_rate: spec.sample_rate, frames: interleaved.len() / 2, hrtf_applied })
}

/// Write a synthetic 48 kHz stereo demo clip (~9 s) for A/B listening when
/// the user has no game capture at hand: footstep-like filtered taps panned
/// across the field, an "explosion" low thump for the limiter, and a short
/// pink-noise bed with a 1 kHz reference beep.
pub fn synthesize_demo(output: &Path) -> Result<(), OfflineError> {
    const RATE: u32 = 48_000;
    let fs = RATE as f32;
    let seconds = 9.0f32;
    let frames = (fs * seconds) as usize;
    let mut left = vec![0.0f32; frames];
    let mut right = vec![0.0f32; frames];

    // Deterministic tiny PRNG (xorshift) — no rand dependency.
    let mut state = 0x2545_F491u32;
    let mut noise = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state as f32 / u32::MAX as f32) * 2.0 - 1.0
    };

    // Pink-ish noise bed at -30 dBFS (one-pole lowpassed white noise).
    let mut lp = 0.0f32;
    for i in 0..frames {
        let w = noise();
        lp += 0.02 * (w - lp);
        let bed = lp * 0.15;
        left[i] += bed;
        right[i] += bed;
    }

    // Footsteps: 120 ms band-limited taps, walking left → right.
    let steps = 8;
    for s in 0..steps {
        let start = (fs * (0.6 + s as f32 * 0.55)) as usize;
        let pan = s as f32 / (steps - 1) as f32; // 0 = left, 1 = right
        let mut body = 0.0f32;
        for i in 0..(fs * 0.12) as usize {
            let t = i as f32 / fs;
            let env = (-t * 45.0).exp();
            body += 0.25 * (noise() - body); // ~2 kHz-ish rumble of a step
            let v = body * env * 0.9;
            if start + i < frames {
                left[start + i] += v * (1.0 - pan).sqrt();
                right[start + i] += v * pan.sqrt();
            }
        }
    }

    // Explosion at 5.5 s: 55 Hz decaying sine + burst, centred, hot on purpose
    // so the limiter has something to tame.
    let start = (fs * 5.5) as usize;
    for i in 0..(fs * 1.2) as usize {
        let t = i as f32 / fs;
        let env = (-t * 4.0).exp();
        let thump = (2.0 * std::f32::consts::PI * 55.0 * t).sin() * env * 0.95;
        let crackle = noise() * env * env * 0.25;
        if start + i < frames {
            left[start + i] += thump + crackle;
            right[start + i] += thump + crackle;
        }
    }

    // 1 kHz reference beep at 8 s, -20 dBFS.
    let start = (fs * 8.0) as usize;
    for i in 0..(fs * 0.4) as usize {
        let t = i as f32 / fs;
        let fade = (t * 50.0).min(1.0) * ((0.4 - t) * 50.0).clamp(0.0, 1.0);
        let v = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.1 * fade;
        if start + i < frames {
            left[start + i] += v;
            right[start + i] += v;
        }
    }

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let write_err = |source| OfflineError::Write { path: output.display().to_string(), source };
    let mut writer = hound::WavWriter::create(output, spec).map_err(write_err)?;
    for i in 0..frames {
        for v in [left[i], right[i]] {
            writer
                .write_sample((v.clamp(-1.0, 1.0) * 32767.0).round() as i16)
                .map_err(write_err)?;
        }
    }
    writer.finalize().map_err(write_err)
}
