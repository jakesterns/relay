//! The headset-correction path end to end, on a real measurement.
//!
//! Import an actual oratory1990 results CSV, fit it, and check the resulting
//! cascade really does what the file asked for. Synthetic curves in the unit
//! tests prove the fitter's behaviour; this proves the numbers a user would
//! actually get.

use relay_audio::fit;
use relay_audio::params::FilterKind;
use relay_core::audio_bridge::{chain_params_with, CORRECTION_BUDGET};
use relay_core::hardware::autoeq::parse_curve;
use relay_core::types::{AudioSettings, EqBand};

/// Real oratory1990 result for the Sennheiser HD 560S.
const HD560S: &str = include_str!("fixtures/autoeq-hd560s.csv");

fn curve() -> Vec<(f32, f32)> {
    parse_curve(HD560S).expect("parse the real results CSV")
}

#[test]
fn the_real_measurement_fits_closely() {
    let c = curve();
    let f = fit::fit_curve(&c, CORRECTION_BUDGET);

    println!(
        "HD 560S: {} bands, max {:.2} dB, rms {:.2} dB",
        f.bands.len(),
        f.max_error_db,
        f.rms_error_db
    );
    for b in &f.bands {
        println!("  {:?} {:.0} Hz {:+.2} dB Q{:.2}", b.kind, b.freq_hz, b.gain_db, b.q);
    }
    assert!(!f.bands.is_empty(), "a real correction curve should need filters");
    assert!(f.bands.len() <= CORRECTION_BUDGET);
    // Measured on this fixture: 2.22 dB worst case, 0.78 dB RMS with eight
    // bands. The bounds are loose enough to survive a tweak to the fitter and
    // tight enough to catch it regressing to the shelf-stacking it used to do
    // (which gave 4.09 / 1.64 on the same curve).
    assert!(f.max_error_db < 3.0, "worst error {} dB over the curve", f.max_error_db);
    assert!(f.rms_error_db < 1.0, "rms error {} dB", f.rms_error_db);

    // Budget buys accuracy: more bands must not fit worse on average.
    let coarse = fit::fit_curve(&c, 4);
    assert!(f.rms_error_db <= coarse.rms_error_db + 1e-4);

    // And the shape is the one the method promises: at most one shelf per
    // end, the rest peaking, spread across the spectrum.
    let shelves = f
        .bands
        .iter()
        .filter(|b| matches!(b.kind, FilterKind::LowShelf | FilterKind::HighShelf))
        .count();
    assert!(shelves <= 2, "{:?}", f.bands);
}

#[test]
fn the_fitted_cascade_tracks_the_measurement_at_named_frequencies() {
    let c = curve();
    let f = fit::fit_curve(&c, CORRECTION_BUDGET);
    // Spot-check across the range rather than trusting the summary stats: a
    // fit can have a good RMS and still be wrong somewhere specific.
    for hz in [30.0, 60.0, 200.0, 800.0, 2000.0, 6000.0, 12000.0] {
        let want = sample(&c, hz);
        let got = fit::response_db(&f.bands, hz);
        assert!(
            (got - want).abs() < 2.5,
            "{hz} Hz: curve asks {want:+.2} dB, cascade gives {got:+.2} dB"
        );
    }
}

#[test]
fn correction_reaches_the_chain_the_apo_would_run() {
    let c = curve();
    let taste = EqBand { freq_hz: 3000.0, gain_db: 3.0, q: 1.0 };
    let audio =
        AudioSettings { bands: vec![taste], headset_correction: true, ..AudioSettings::default() };

    let with = chain_params_with(&audio, Some(&c));
    let without = chain_params_with(&audio, None);
    assert_eq!(without.bands.len(), 1, "no curve means only the user's band");
    assert!(with.bands.len() > 1, "the curve adds filters ahead of the user's band");
    // The user's band survives, last, unchanged.
    let last = with.bands.last().unwrap();
    assert_eq!((last.freq_hz, last.gain_db), (3000.0, 3.0));
}

/// Linear interpolation in log frequency, matching what the fitter does.
fn sample(curve: &[(f32, f32)], hz: f64) -> f64 {
    if hz <= curve[0].0 as f64 {
        return curve[0].1 as f64;
    }
    let last = curve[curve.len() - 1];
    if hz >= last.0 as f64 {
        return last.1 as f64;
    }
    let i = curve.partition_point(|&(f, _)| (f as f64) < hz);
    let (f1, d1) = (curve[i - 1].0 as f64, curve[i - 1].1 as f64);
    let (f2, d2) = (curve[i].0 as f64, curve[i].1 as f64);
    let t = (hz.ln() - f1.ln()) / (f2.ln() - f1.ln());
    d1 + t * (d2 - d1)
}
