//! Offline A/B rendering: synthesize the demo clip, run it through a
//! profile-shaped chain, and check the output file and report.

use relay_audio::dsp::biquad::BandParams;
use relay_audio::dsp::limiter::LimiterParams;
use relay_audio::dsp::ChainParams;
use relay_audio::offline::{render, synthesize_demo};

#[test]
fn demo_renders_through_the_full_chain() {
    let dir = std::env::temp_dir().join("relay-audio-offline-test");
    std::fs::create_dir_all(&dir).unwrap();
    let original = dir.join("original.wav");
    let processed = dir.join("processed.wav");

    synthesize_demo(&original).unwrap();

    let params = ChainParams {
        bands: vec![BandParams::peaking(3000.0, 4.0, 1.0), BandParams::peaking(120.0, -3.0, 0.8)],
        limiter: Some(LimiterParams::new(150.0, -12.0)),
        hrtf: true, // demo is 48 kHz, so the bundled IR applies
    };
    let report = render(&params, &original, &processed).unwrap();
    assert_eq!(report.sample_rate, 48_000);
    assert!(report.hrtf_applied);
    assert!(report.frames > 8 * 48_000);

    // The processed file must be a readable stereo WAV of the same length.
    let r = hound::WavReader::open(&processed).unwrap();
    assert_eq!(r.spec().channels, 2);
    assert_eq!(r.spec().sample_rate, 48_000);
    assert_eq!(r.duration() as usize, report.frames);

    // And the limiter must actually have tamed the explosion: peak of the
    // processed low content stays under the original's.
    let peak = |p: &std::path::Path| {
        hound::WavReader::open(p)
            .unwrap()
            .samples::<i16>()
            .map(|s| s.unwrap().unsigned_abs())
            .max()
            .unwrap()
    };
    assert!(peak(&processed) < peak(&original));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn hrtf_is_skipped_not_fatal_at_unbundled_rates() {
    let dir = std::env::temp_dir().join("relay-audio-offline-rate-test");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in32k.wav");
    let output = dir.join("out32k.wav");

    // A 32 kHz file: no bundled IR at that rate.
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 32_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&input, spec).unwrap();
    for i in 0..32_000 {
        w.write_sample(((i as f32 * 0.05).sin() * 8000.0) as i16).unwrap();
    }
    w.finalize().unwrap();

    let params = ChainParams { bands: vec![], limiter: None, hrtf: true };
    let report = render(&params, &input, &output).unwrap();
    assert!(!report.hrtf_applied);
    assert_eq!(report.sample_rate, 32_000);

    std::fs::remove_dir_all(&dir).ok();
}
