//! B16 measurement: how much audio waits between packet arrival and the
//! endpoint. Plays *silence* on the default render endpoint for a few seconds,
//! so it is `#[ignore]`d and run by hand:
//!
//! ```text
//! cargo test -p relay-capture --test playback_depth -- --ignored --nocapture --test-threads=1
//! ```
#![cfg(windows)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use relay_capture::playback::{self, PlaybackStats};

/// Feed 10 ms Opus packets of silence in real time for `secs`, sampling the
/// playback stats every 100 ms after a 1 s settle. Returns (mean, max) ms.
fn measure(secs: u64) -> (f64, f64, Arc<PlaybackStats>) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (_mic_tx, mic_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let stats = Arc::new(PlaybackStats::default());
    let stats2 = stats.clone();
    // S37: a third track and the faders; neither is fed here, and unity is
    // the default, so the depth measurement is unchanged.
    let (_rest_tx, rest_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
    let faders = relay_capture::mixer::Faders::shared();
    use relay_capture::mixer::Track;
    let player = std::thread::spawn(move || {
        playback::run(
            vec![(rx, Track::App), (mic_rx, Track::Mic), (rest_rx, Track::Rest)],
            stop_rx,
            relay_capture::devices::DeviceSlot::shared(None),
            stats2,
            faders,
            None,
        )
    });

    let mut enc =
        opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
    let silence = vec![0f32; 480 * 2];
    let start = Instant::now();
    let mut samples = Vec::new();
    let mut sent = 0u64;
    while start.elapsed() < Duration::from_secs(secs) {
        // Pace against the wall clock, not by sleeping 10 ms a time.
        while sent * 10 <= start.elapsed().as_millis() as u64 {
            let pkt = enc.encode_vec_float(&silence, 4000).unwrap();
            tx.blocking_send(pkt).unwrap();
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(2));
        if start.elapsed() > Duration::from_secs(1)
            && samples.len() as u128 * 100 < start.elapsed().as_millis() - 1000
        {
            samples.push(stats.buffered_ms());
        }
    }
    let stop_sent = Instant::now();
    stop_tx.send(()).unwrap();
    player.join().unwrap().unwrap();
    println!("playback thread stopped in {:.1} ms", stop_sent.elapsed().as_secs_f64() * 1e3);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let max = samples.iter().cloned().fold(0.0, f64::max);
    (mean, max, stats)
}

#[test]
#[ignore = "plays silence on the default endpoint"]
fn legacy_playback_holds_about_a_second() {
    std::env::set_var("RELAY_AUDIO_LEGACY", "1");
    let (mean, max, stats) = measure(5);
    std::env::remove_var("RELAY_AUDIO_LEGACY");
    println!("LEGACY  mean {mean:.1} ms  max {max:.1} ms  {}", stats.json());
    assert!(mean > 500.0, "the pre-S33 path should show its full buffer, got {mean:.1} ms");
}

#[test]
#[ignore = "plays silence on the default endpoint"]
fn playback_holds_well_under_a_tenth_of_a_second() {
    let (mean, max, stats) = measure(10);
    println!("CURRENT mean {mean:.1} ms  max {max:.1} ms  {}", stats.json());
    assert!(mean < 80.0, "mean {mean:.1} ms");
    assert_eq!(stats.underruns.load(std::sync::atomic::Ordering::Relaxed), 0);
}
