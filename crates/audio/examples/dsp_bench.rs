//! Benchmark for the plan's Measurements table. Run in release:
//!
//! ```text
//! cargo run -p relay-audio --release --example dsp_bench
//! ```
//!
//! Full chain (16-band EQ + band-split limiter + HRTF) and bypass, at 48 k
//! and 96 k, 256-frame stereo blocks. Reports per-block time and % of one
//! core (time per block ÷ real-time duration of a block).

use std::time::Instant;

use relay_audio::dsp::biquad::BandParams;
use relay_audio::dsp::limiter::LimiterParams;
use relay_audio::dsp::{Chain, ChainParams};

const BLOCK: usize = 256;

fn bench(rate: u32, hrtf: bool, bypass: bool) -> (f64, f64) {
    let params = ChainParams {
        bands: (0..16)
            .map(|i| {
                BandParams::peaking(
                    60.0 + i as f32 * 800.0,
                    if i % 2 == 0 { 4.0 } else { -3.0 },
                    1.2,
                )
            })
            .collect(),
        limiter: Some(LimiterParams::new(120.0, -10.0)),
        hrtf,
    };
    let mut chain = Chain::new(params);
    chain.prepare(rate, BLOCK).expect("prepare");
    chain.set_bypass(bypass);

    let input: Vec<f32> = (0..BLOCK * 2).map(|i| ((i as f32) * 0.37).sin() * 0.5).collect();
    let mut output = vec![0.0f32; BLOCK * 2];

    for _ in 0..200 {
        chain.process(&input, &mut output);
    }
    let iters = 20_000u32;
    let start = Instant::now();
    for _ in 0..iters {
        chain.process(&input, &mut output);
    }
    let per_block = start.elapsed().as_secs_f64() / iters as f64;
    let block_seconds = BLOCK as f64 / rate as f64;
    (per_block * 1e6, 100.0 * per_block / block_seconds)
}

fn main() {
    println!("relay-audio dsp_bench — {BLOCK}-frame stereo blocks, release build");
    println!("{:<34}{:>12}{:>14}", "configuration", "µs/block", "% of one core");
    for rate in [48_000u32, 96_000] {
        for (label, hrtf, bypass) in [
            ("full chain (EQ+limiter+HRTF)", true, false),
            ("EQ + limiter only", false, false),
            ("bypass (plain copy)", true, true),
        ] {
            let (us, pct) = bench(rate, hrtf, bypass);
            println!("{:<34}{:>12.2}{:>13.3}%", format!("{rate} Hz  {label}"), us, pct);
        }
    }
}
