//! Uniformly-partitioned convolution (overlap-save with a frequency-domain
//! delay line), stereo → binaural.
//!
//! The bundled default set — "Relay Arena" — is SADIE II subject D1
//! (Neumann KU100), sources at ±30° / elevation 0°, one HRIR pair per
//! bundled sample rate (44.1 k, 48 k, 96 k). See `assets/hrtf/LICENSE`.
//! No resampling: preparing at any other rate is an error the caller
//! surfaces ("HRTF unavailable at this endpoint rate").
//!
//! Latency is exactly one partition ([`PARTITION`] frames): input is staged
//! into a partition-sized FIFO regardless of the caller's block size.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

use super::PrepareError;

/// Partition length in frames. 128 ⇒ 2.7 ms at 48 kHz.
pub const PARTITION: usize = 128;

const ARENA_44100_L: &[u8] = include_bytes!("../../assets/hrtf/arena-44100-left.wav");
const ARENA_44100_R: &[u8] = include_bytes!("../../assets/hrtf/arena-44100-right.wav");
const ARENA_48000_L: &[u8] = include_bytes!("../../assets/hrtf/arena-48000-left.wav");
const ARENA_48000_R: &[u8] = include_bytes!("../../assets/hrtf/arena-48000-right.wav");
const ARENA_96000_L: &[u8] = include_bytes!("../../assets/hrtf/arena-96000-left.wav");
const ARENA_96000_R: &[u8] = include_bytes!("../../assets/hrtf/arena-96000-right.wav");

/// Headroom so left + right sources summing at one ear cannot clip.
const ARENA_GAIN: f32 = 0.5;

pub struct Hrtf {
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    partitions: usize,
    /// Previous input partition per source, for overlap-save.
    in_prev: [Vec<f32>; 2],
    /// Input staging, one partition per source.
    in_fifo: [Vec<f32>; 2],
    fifo_fill: usize,
    /// One partition of rendered output per ear.
    out_fifo: [Vec<f32>; 2],
    out_read: usize,
    /// Frequency-domain delay line: per source, a ring of `partitions` spectra.
    fdl: [Vec<Vec<Complex<f32>>>; 2],
    fdl_pos: usize,
    /// IR spectra: `[source][ear][partition]`.
    ir: [[Vec<Vec<Complex<f32>>>; 2]; 2],
    // Scratch, all sized at prepare.
    fft_in: Vec<f32>,
    acc: [Vec<Complex<f32>>; 2],
    ifft_out: Vec<f32>,
    scratch_fwd: Vec<Complex<f32>>,
    scratch_inv: Vec<Complex<f32>>,
}

impl Hrtf {
    /// Prepare with the bundled "Relay Arena" IR set at `sample_rate`.
    pub fn prepare_arena(sample_rate: u32, max_block: usize) -> Result<Self, PrepareError> {
        let (l, r) = match sample_rate {
            44100 => (ARENA_44100_L, ARENA_44100_R),
            48000 => (ARENA_48000_L, ARENA_48000_R),
            96000 => (ARENA_96000_L, ARENA_96000_R),
            other => return Err(PrepareError::HrtfUnsupportedRate(other)),
        };
        let left = decode_hrir(l)?;
        let right = decode_hrir(r)?;
        Self::prepare_with_ir(max_block, left, right)
    }

    /// Prepare with caller-supplied HRIR pairs: `src_left[0]` is the left
    /// source's IR to the left ear, `src_left[1]` to the right ear, and the
    /// same layout for `src_right`. Public so tests can drive the engine
    /// with arbitrary IRs and compare against a naive reference.
    pub fn prepare_with_ir(
        _max_block: usize,
        src_left: [Vec<f32>; 2],
        src_right: [Vec<f32>; 2],
    ) -> Result<Self, PrepareError> {
        let ir_len = src_left
            .iter()
            .chain(src_right.iter())
            .map(|v| v.len())
            .max()
            .filter(|&n| n > 0)
            .ok_or(PrepareError::InvalidParam("empty HRIR"))?;
        let partitions = ir_len.div_ceil(PARTITION);
        let fft_len = 2 * PARTITION;
        let bins = PARTITION + 1;

        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_len);
        let ifft = planner.plan_fft_inverse(fft_len);
        let scratch_fwd = fft.make_scratch_vec();
        let scratch_inv = ifft.make_scratch_vec();

        // Partition each IR and take its spectrum: H_p = FFT([h_p, 0…]).
        let mut fft_in = vec![0.0f32; fft_len];
        let mut spectra = |h: &[f32]| -> Vec<Vec<Complex<f32>>> {
            let mut out = Vec::with_capacity(partitions);
            let mut scratch = fft.make_scratch_vec();
            for p in 0..partitions {
                fft_in.fill(0.0);
                let start = p * PARTITION;
                let end = (start + PARTITION).min(h.len());
                if start < h.len() {
                    fft_in[..end - start].copy_from_slice(&h[start..end]);
                }
                let mut spec = vec![Complex::default(); bins];
                fft.process_with_scratch(&mut fft_in, &mut spec, &mut scratch)
                    .expect("FFT length is consistent");
                out.push(spec);
            }
            out
        };
        let ir = [
            [spectra(&src_left[0]), spectra(&src_left[1])],
            [spectra(&src_right[0]), spectra(&src_right[1])],
        ];
        fft_in.fill(0.0);

        let empty_fdl = || vec![vec![Complex::default(); bins]; partitions];
        Ok(Self {
            fft,
            ifft,
            partitions,
            in_prev: [vec![0.0; PARTITION], vec![0.0; PARTITION]],
            in_fifo: [vec![0.0; PARTITION], vec![0.0; PARTITION]],
            fifo_fill: 0,
            out_fifo: [vec![0.0; PARTITION], vec![0.0; PARTITION]],
            out_read: 0,
            fdl: [empty_fdl(), empty_fdl()],
            fdl_pos: 0,
            ir,
            fft_in,
            acc: [vec![Complex::default(); bins], vec![Complex::default(); bins]],
            ifft_out: vec![0.0; fft_len],
            scratch_fwd,
            scratch_inv,
        })
    }

    /// Frames of delay from input to output: one partition.
    pub fn latency_frames(&self) -> usize {
        PARTITION
    }

    /// Render one block. All four slices have the same length; any length is
    /// accepted (internally staged to partition boundaries). Allocation-free.
    pub fn process(&mut self, l_in: &[f32], r_in: &[f32], l_out: &mut [f32], r_out: &mut [f32]) {
        debug_assert_eq!(l_in.len(), r_in.len());
        debug_assert_eq!(l_in.len(), l_out.len());
        debug_assert_eq!(l_in.len(), r_out.len());
        for i in 0..l_in.len() {
            self.in_fifo[0][self.fifo_fill] = l_in[i];
            self.in_fifo[1][self.fifo_fill] = r_in[i];
            self.fifo_fill += 1;
            l_out[i] = self.out_fifo[0][self.out_read];
            r_out[i] = self.out_fifo[1][self.out_read];
            self.out_read += 1;
            if self.fifo_fill == PARTITION {
                self.render_partition();
                self.fifo_fill = 0;
                self.out_read = 0;
            }
        }
    }

    fn render_partition(&mut self) {
        let cur = self.fdl_pos;
        // FFT the new input block of each source into the delay line:
        // X_k = FFT([x_{k-1}, x_k]).
        for s in 0..2 {
            self.fft_in[..PARTITION].copy_from_slice(&self.in_prev[s]);
            self.fft_in[PARTITION..].copy_from_slice(&self.in_fifo[s]);
            self.fft
                .process_with_scratch(
                    &mut self.fft_in,
                    &mut self.fdl[s][cur],
                    &mut self.scratch_fwd,
                )
                .expect("FFT length is consistent");
            self.in_prev[s].copy_from_slice(&self.in_fifo[s]);
        }
        // Y_ear = Σ_s Σ_p X_{k-p}[s] · H_p[s][ear]
        for acc in self.acc.iter_mut() {
            acc.fill(Complex::default());
        }
        for s in 0..2 {
            for p in 0..self.partitions {
                let slot = (cur + self.partitions - p) % self.partitions;
                let x = &self.fdl[s][slot];
                for ear in 0..2 {
                    let h = &self.ir[s][ear][p];
                    let acc = &mut self.acc[ear];
                    for i in 0..x.len() {
                        acc[i] += x[i] * h[i];
                    }
                }
            }
        }
        // Overlap-save: keep the last PARTITION samples of each IFFT.
        let norm = 1.0 / (2 * PARTITION) as f32;
        for ear in 0..2 {
            self.ifft
                .process_with_scratch(&mut self.acc[ear], &mut self.ifft_out, &mut self.scratch_inv)
                .expect("IFFT length is consistent");
            for i in 0..PARTITION {
                self.out_fifo[ear][i] = self.ifft_out[PARTITION + i] * norm;
            }
        }
        self.fdl_pos = (self.fdl_pos + 1) % self.partitions;
    }
}

/// Parse a stereo HRIR WAV (any bit depth hound supports) into per-ear `f32`
/// impulse responses, with the bundled headroom gain applied.
fn decode_hrir(bytes: &[u8]) -> Result<[Vec<f32>; 2], PrepareError> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))
        .map_err(|_| PrepareError::InvalidParam("bundled HRIR failed to parse"))?;
    let spec = reader.spec();
    if spec.channels != 2 {
        return Err(PrepareError::InvalidParam("bundled HRIR is not stereo"));
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    let frames = samples.len() / 2;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for f in 0..frames {
        left.push(samples[2 * f] * ARENA_GAIN);
        right.push(samples[2 * f + 1] * ARENA_GAIN);
    }
    Ok([left, right])
}
