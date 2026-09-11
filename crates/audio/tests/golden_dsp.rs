//! Golden-response tests: every stage measured through its real `process`
//! path against closed-form expectations, within 0.1 dB.

use relay_audio::dsp::biquad::{BandParams, Biquad, Coeffs, EqCascade, FilterKind};
use relay_audio::dsp::limiter::{Limiter, LimiterParams};
use relay_audio::dsp::{Chain, ChainParams};

const FS: u32 = 48_000;

/// Steady-state gain (dB) of `process` for a pure tone: warm up, then
/// compare RMS out/in over whole cycles.
fn measure_gain_db(mut process: impl FnMut(&mut [f32]), freq: f64, fs: f64) -> f64 {
    let warm = (fs * 0.25) as usize;
    let cycles = ((freq * 0.5).max(20.0)).round(); // ≥ 0.5 s of signal, whole cycles
    let n = ((cycles / freq) * fs).round() as usize;
    let mut buf: Vec<f32> = (0..warm + n)
        .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / fs).sin() as f32)
        .collect();
    let in_rms = rms(&buf[warm..]);
    process(&mut buf);
    20.0 * (rms(&buf[warm..]) / in_rms).log10()
}

fn rms(b: &[f32]) -> f64 {
    (b.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / b.len() as f64).sqrt()
}

/// The frequencies each response is checked at (kept away from Nyquist,
/// where a finite sine measurement gets bin-leakage noisy).
const PROBE_HZ: &[f64] =
    &[40.0, 80.0, 150.0, 300.0, 600.0, 1200.0, 2500.0, 5000.0, 10_000.0, 16_000.0];

fn assert_matches_closed_form(kind: FilterKind, f0: f64, gain: f64, q: f64) {
    let c = Coeffs::design(kind, FS as f64, f0, gain, q).unwrap();
    for &f in PROBE_HZ {
        let mut bq = Biquad::new(c);
        let measured = measure_gain_db(|b| bq.process(b), f, FS as f64);
        let expected = c.magnitude_db(f, FS as f64);
        assert!(
            (measured - expected).abs() < 0.1,
            "{kind:?} f0={f0} at {f} Hz: measured {measured:.4} dB, closed form {expected:.4} dB"
        );
    }
}

#[test]
fn biquad_process_matches_closed_form_magnitude() {
    assert_matches_closed_form(FilterKind::Peaking, 1000.0, 6.0, 1.0);
    assert_matches_closed_form(FilterKind::Peaking, 3200.0, -8.0, 4.0);
    assert_matches_closed_form(FilterKind::LowShelf, 200.0, 4.5, 0.707);
    assert_matches_closed_form(FilterKind::HighShelf, 8000.0, -5.0, 0.707);
    assert_matches_closed_form(FilterKind::LowPass, 2000.0, 0.0, 0.707);
    assert_matches_closed_form(FilterKind::HighPass, 150.0, 0.0, 0.707);
}

#[test]
fn biquad_analytic_landmarks() {
    let fs = FS as f64;
    // RBJ peaking: gain at the centre frequency is exactly gain_db.
    let c = Coeffs::design(FilterKind::Peaking, fs, 1000.0, 6.0, 1.0).unwrap();
    assert!((c.magnitude_db(1000.0, fs) - 6.0).abs() < 1e-9);
    // Butterworth low-pass: −3.01 dB at the corner.
    let c = Coeffs::design(FilterKind::LowPass, fs, 2000.0, 0.0, std::f64::consts::FRAC_1_SQRT_2)
        .unwrap();
    assert!((c.magnitude_db(2000.0, fs) + 3.0103).abs() < 0.01);
    // Low shelf: full gain at DC, unity at Nyquist (and mirrored for high).
    let c = Coeffs::design(FilterKind::LowShelf, fs, 200.0, 4.5, 0.707).unwrap();
    assert!((c.magnitude_db(0.0001, fs) - 4.5).abs() < 0.01);
    assert!(c.magnitude_db(23_999.0, fs).abs() < 0.01);
    let c = Coeffs::design(FilterKind::HighShelf, fs, 8000.0, -5.0, 0.707).unwrap();
    assert!(c.magnitude_db(0.0001, fs).abs() < 0.01);
    assert!((c.magnitude_db(23_999.0, fs) + 5.0).abs() < 0.01);
}

#[test]
fn cascade_equals_sum_of_stages() {
    let bands = vec![
        BandParams::peaking(120.0, 5.0, 1.2),
        BandParams::peaking(1000.0, -4.0, 2.0),
        BandParams {
            kind: FilterKind::HighShelf,
            freq_hz: 9000.0,
            gain_db: 3.0,
            q: 0.707,
            enabled: true,
        },
    ];
    let mut cascade = EqCascade::design(&bands, FS).unwrap();
    for &f in PROBE_HZ {
        let measured = measure_gain_db(|b| cascade.process(b), f, FS as f64);
        // Expected: the dB sum of each band's closed-form response.
        let expected: f64 = bands
            .iter()
            .map(|b| {
                Coeffs::design(b.kind, FS as f64, b.freq_hz as f64, b.gain_db as f64, b.q as f64)
                    .unwrap()
                    .magnitude_db(f, FS as f64)
            })
            .sum();
        assert!(
            (measured - expected).abs() < 0.1,
            "cascade at {f} Hz: measured {measured:.4}, sum of stages {expected:.4}"
        );
    }
}

#[test]
fn disabled_band_is_identity() {
    let bands = vec![
        BandParams::peaking(500.0, 12.0, 1.0),
        BandParams { enabled: false, ..BandParams::peaking(2000.0, 12.0, 1.0) },
    ];
    let mut cascade = EqCascade::design(&bands, FS).unwrap();
    let measured = measure_gain_db(|b| cascade.process(b), 2000.0, FS as f64);
    let only_first = Coeffs::design(FilterKind::Peaking, FS as f64, 500.0, 12.0, 1.0)
        .unwrap()
        .magnitude_db(2000.0, FS as f64);
    assert!((measured - only_first).abs() < 0.1);
}

// ---------------------------------------------------------------- limiter

#[test]
fn limiter_crossover_recombines_flat_when_idle() {
    // Threshold way above the signal: the limiter must be magnitude-transparent.
    let p = LimiterParams::new(250.0, 40.0);
    for &f in PROBE_HZ {
        let mut lim = Limiter::prepare(&p, FS).unwrap();
        let g = measure_gain_db(|b| lim.process(b), f, FS as f64);
        assert!(g.abs() < 0.1, "idle limiter not flat at {f} Hz: {g:.4} dB");
    }
}

#[test]
fn limiter_holds_the_ceiling_with_lookahead() {
    // 30 Hz at 0 dBFS through a 250 Hz split, threshold −12 dB: the low band
    // is the whole signal (high-band leakage at 30 Hz is ~−73 dB for LR4).
    let p = LimiterParams::new(250.0, -12.0);
    let mut lim = Limiter::prepare(&p, FS).unwrap();
    let n = FS as usize; // 1 s
    let mut buf: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * 30.0 * i as f64 / FS as f64).sin() as f32)
        .collect();
    lim.process(&mut buf);
    // Skip the first 100 ms (crossover + envelope settle).
    let peak = buf[(FS as usize) / 10..].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    let ceiling_db = 20.0 * (peak as f64).log10();
    assert!(
        ceiling_db <= -12.0 + 0.3,
        "limiter let a peak through at {ceiling_db:.2} dBFS (threshold −12)"
    );
    // And it is limiting, not muting: the tone should sit near the ceiling.
    assert!(ceiling_db >= -13.5, "over-limited: peak {ceiling_db:.2} dBFS");
}

#[test]
fn limiter_release_follows_the_configured_exponential() {
    let release_ms = 80.0f32;
    let p = LimiterParams { release_ms, ..LimiterParams::new(250.0, -12.0) };
    let release_ms = release_ms as f64;
    let mut lim = Limiter::prepare(&p, FS).unwrap();
    // Drive hard for 0.5 s so the gain settles well below 1.
    let mut loud: Vec<f32> = (0..FS as usize / 2)
        .map(|i| (2.0 * std::f64::consts::PI * 30.0 * i as f64 / FS as f64).sin() as f32)
        .collect();
    lim.process(&mut loud);
    let g0 = lim.current_gain();
    assert!(g0 < 0.3, "gain should be well reduced while driven, got {g0}");

    // Silence: the gain must recover toward 1 exponentially with tau ≈ release.
    let mut last = g0;
    let mut t63 = None;
    let target = g0 + (1.0 - g0) * (1.0 - (-1.0f64).exp()); // 63.2 % of the way
    for i in 0..FS as usize {
        let mut s = [0.0f32];
        lim.process(&mut s);
        let g = lim.current_gain();
        assert!(g >= last - 1e-9, "release must be monotonic (sample {i})");
        last = g;
        if t63.is_none() && g >= target {
            t63 = Some(i);
        }
    }
    assert!((last - 1.0).abs() < 1e-3, "gain did not recover to 1: {last}");
    let t63 = t63.expect("gain never crossed the 63 % point") as f64;
    // The look-ahead window (1 ms) delays the start of release slightly.
    let measured_ms = t63 * 1000.0 / FS as f64;
    assert!(
        (measured_ms - release_ms).abs() < release_ms * 0.2 + 2.0,
        "release t63 = {measured_ms:.1} ms, configured {release_ms} ms"
    );
}

#[test]
fn limiter_leaves_the_high_band_alone() {
    // 2 kHz at 0 dBFS is far above a 250 Hz split: even with a low threshold
    // the tone passes unlimited.
    let p = LimiterParams::new(250.0, -20.0);
    let mut lim = Limiter::prepare(&p, FS).unwrap();
    let g = measure_gain_db(|b| lim.process(b), 2000.0, FS as f64);
    assert!(g.abs() < 0.1, "high band was touched: {g:.3} dB");
}

// ---------------------------------------------------------------- chain

#[test]
fn bypass_is_bit_exact() {
    let params = ChainParams {
        bands: vec![BandParams::peaking(1000.0, 6.0, 1.0)],
        limiter: Some(LimiterParams::new(120.0, -10.0)),
        hrtf: true,
    };
    let mut chain = Chain::new(params);
    chain.prepare(48_000, 512).unwrap();
    chain.set_bypass(true);
    let input: Vec<f32> =
        (0..1024).map(|i| ((i * 2_654_435_761u64 as usize) as f32).sin()).collect();
    let mut output = vec![0.0f32; 1024];
    chain.process(&input, &mut output);
    for (a, b) in input.iter().zip(&output) {
        assert_eq!(a.to_bits(), b.to_bits(), "bypass must be a plain copy");
    }
}

#[test]
fn chain_response_equals_its_stages() {
    // EQ + idle limiter: the chain's response must equal the EQ's closed form.
    let bands = vec![BandParams::peaking(400.0, 5.0, 1.0), BandParams::peaking(4000.0, -6.0, 2.0)];
    let params = ChainParams {
        bands: bands.clone(),
        limiter: Some(LimiterParams::new(100.0, 40.0)),
        hrtf: false,
    };
    let mut chain = Chain::new(params);
    chain.prepare(FS, 4096).unwrap();
    for &f in PROBE_HZ {
        let measured = measure_gain_db(
            |b| {
                // Interleave the mono probe as identical L/R, process, take L.
                let mut inter = vec![0.0f32; b.len() * 2];
                for (i, &s) in b.iter().enumerate() {
                    inter[2 * i] = s;
                    inter[2 * i + 1] = s;
                }
                let mut out = vec![0.0f32; inter.len()];
                for (ic, oc) in inter.chunks(2 * 4096).zip(out.chunks_mut(2 * 4096)) {
                    chain.process(ic, oc);
                }
                for i in 0..b.len() {
                    b[i] = out[2 * i];
                }
            },
            f,
            FS as f64,
        );
        let expected: f64 = bands
            .iter()
            .map(|b| {
                Coeffs::design(b.kind, FS as f64, b.freq_hz as f64, b.gain_db as f64, b.q as f64)
                    .unwrap()
                    .magnitude_db(f, FS as f64)
            })
            .sum();
        assert!(
            (measured - expected).abs() < 0.1,
            "chain at {f} Hz: measured {measured:.4}, expected {expected:.4}"
        );
    }
}

#[test]
fn denormals_are_flushed_from_all_state() {
    let params = ChainParams {
        bands: vec![BandParams::peaking(60.0, 10.0, 0.5), BandParams::peaking(8000.0, -10.0, 4.0)],
        limiter: Some(LimiterParams::new(120.0, -10.0)),
        hrtf: false,
    };
    let mut chain = Chain::new(params);
    chain.prepare(FS, 480).unwrap();
    // Excite, then feed 10 s of digital silence.
    let mut buf = vec![0.0f32; 960];
    buf[0] = 1.0;
    buf[1] = -1.0;
    let mut out = vec![0.0f32; 960];
    chain.process(&buf, &mut out);
    let silence = vec![0.0f32; 960];
    for _ in 0..1000 {
        chain.process(&silence, &mut out);
        assert!(chain.state_is_denormal_free(), "subnormal survived in filter state");
        assert!(out.iter().all(|s| s.is_finite()));
    }
}

#[test]
fn rejects_more_than_16_bands() {
    let params = ChainParams {
        bands: (0..17).map(|i| BandParams::peaking(100.0 + i as f32 * 100.0, 1.0, 1.0)).collect(),
        limiter: None,
        hrtf: false,
    };
    let mut chain = Chain::new(params);
    assert!(chain.prepare(FS, 256).is_err());
}
