//! `relay-share apo-check`: the automated proof that the endpoint APO is in
//! the playing graph and that its EQ reaches the speakers' signal.
//!
//! It plays a quiet three-tone stimulus on the default render endpoint
//! (shared mode, nothing about the endpoint changes), loopback-captures the
//! same endpoint, and measures each tone's level in two phases:
//!
//! 1. **bypass** - the APO's shared-memory bypass word set;
//! 2. **eq** - a known test cascade written to the section and un-bypassed.
//!
//! The section's prior bypass word and parameters are put back afterwards,
//! whatever happened. The verdict needs both: the section must be reachable
//! while the stimulus plays (an APO instance is alive in the graph), and the
//! eq phase must move the low tone up and the high tone down by the test
//! cascade's amounts within a tolerance. Windows takes the loopback copy
//! after the mode effects (MFX), where Relay sits, so the change is visible
//! there; a zero delta with a reachable section means "loaded but not
//! processing", the S44d failure this command exists to catch.
//!
//! Level is fixed at -30 dBFS per tone (a measured automated run; the owner
//! allows tones for those, never for listening checks).

use serde::Serialize;

/// Stimulus tones (Hz): one well inside each test band and one between.
pub const TONES_HZ: [f64; 3] = [100.0, 1000.0, 6000.0];
/// Per-tone amplitude: -30 dBFS.
pub const TONE_AMP: f64 = 0.031_622_776;
/// The test cascade: +9 dB low peak, -8 dB high peak.
pub const LOW_BOOST_DB: f32 = 9.0;
pub const HIGH_CUT_DB: f32 = -8.0;
/// Pass when each move is at least this fraction of the designed amount.
pub const MIN_FRACTION: f64 = 0.6;
/// And the middle tone moves by at most this much.
pub const MID_TOLERANCE_DB: f64 = 2.0;

/// The cascade the eq phase writes.
pub fn test_params() -> relay_audio::params::ChainParams {
    use relay_audio::params::{BandParams, ChainParams};
    ChainParams {
        bands: vec![
            BandParams::peaking(TONES_HZ[0] as f32, LOW_BOOST_DB, 0.7),
            BandParams::peaking(TONES_HZ[2] as f32, HIGH_CUT_DB, 0.8),
        ],
        ..Default::default()
    }
}

/// Level of `freq` in one channel of interleaved audio, dBFS (Goertzel,
/// Hann-windowed so neighbouring tones do not leak in).
pub fn tone_db(samples: &[f32], channels: usize, rate: u32, freq: f64) -> f64 {
    let ch = channels.max(1);
    let n = samples.len() / ch;
    if n < 16 {
        return f64::NEG_INFINITY;
    }
    let w = 2.0 * std::f64::consts::PI * freq / rate as f64;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    let mut wsum = 0.0f64;
    for i in 0..n {
        let win = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
        wsum += win;
        let x = samples[i * ch] as f64 * win;
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    let amp = 2.0 * power.max(0.0).sqrt() / wsum;
    20.0 * amp.max(1e-12).log10()
}

/// Levels of every stimulus tone.
pub fn levels(samples: &[f32], channels: usize, rate: u32) -> [f64; 3] {
    TONES_HZ.map(|f| tone_db(samples, channels, rate, f))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub endpoint: String,
    /// The params section opened while the stimulus played.
    pub section_reachable: bool,
    pub bypass_db: [f64; 3],
    pub eq_db: [f64; 3],
    /// eq - bypass, per tone.
    pub delta_db: [f64; 3],
    pub pass: bool,
    pub verdict: String,
}

/// Turn two phases' levels into the verdict.
pub fn judge(endpoint: String, reachable: bool, bypass: [f64; 3], eq: [f64; 3]) -> Report {
    let delta = [eq[0] - bypass[0], eq[1] - bypass[1], eq[2] - bypass[2]];
    let audible = bypass.iter().all(|d| *d > -60.0);
    let low_ok = delta[0] >= LOW_BOOST_DB as f64 * MIN_FRACTION;
    let high_ok = delta[2] <= HIGH_CUT_DB as f64 * MIN_FRACTION;
    let mid_ok = delta[1].abs() <= MID_TOLERANCE_DB;
    let (pass, verdict) = if !reachable {
        (false, "no APO instance alive while playing: Relay is not in the playing graph".into())
    } else if !audible {
        (false, "stimulus not seen on loopback: endpoint muted or not the default".into())
    } else if low_ok && high_ok && mid_ok {
        (true, "EQ applied in the playing graph".into())
    } else if delta.iter().all(|d| d.abs() < 1.0) {
        (false, "section alive but nothing changed: APO loaded, not processing".into())
    } else {
        (false, format!("EQ moved the wrong amount: {delta:.1?}"))
    };
    Report {
        endpoint,
        section_reachable: reachable,
        bypass_db: bypass,
        eq_db: eq,
        delta_db: delta,
        pass,
        verdict,
    }
}

/// Run the check on the default render endpoint. `phase_secs` per phase.
#[cfg(windows)]
pub fn run(phase_secs: f64) -> anyhow::Result<Report> {
    win::run(phase_secs)
}

/// What one observe run saw.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Observation {
    pub endpoint: String,
    pub section_reachable: bool,
    /// Bypass word at the end of the run, when the section was reachable.
    pub bypass: Option<bool>,
    pub levels_db: [f64; 3],
}

/// Observe mode (the end-to-end run through the service): play the stimulus
/// on a fresh stream and measure, touching nothing. The first `settle_secs`
/// are dropped, which covers the service's 1 s keep-alive tick.
#[cfg(windows)]
pub fn observe(secs: f64, settle_secs: f64) -> anyhow::Result<Observation> {
    win::observe(secs, settle_secs)
}

#[cfg(windows)]
mod win {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};
    use relay_audio::shm::{section_name, SharedParams};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    use super::*;
    use crate::audio::{AudioCapture, AudioSource};

    const RATE: u32 = 48_000;

    /// The endpoint GUID part of an MMDevice id (`{0.0.0.00000000}.{guid}`).
    fn endpoint_guid(id: &str) -> String {
        id.rsplit('.').next().unwrap_or(id).to_ascii_uppercase()
    }

    /// Play the stimulus until `stop`, on its own COM thread.
    fn player(stop: Arc<AtomicBool>, ready: std::sync::mpsc::Sender<Result<String>>) {
        // SAFETY: COM + WASAPI render on this thread; balanced on exit.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let r = (|| -> Result<()> {
                let en: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let idp = dev.GetId()?;
                let id = idp.to_string()?;
                CoTaskMemFree(Some(idp.0 as *const _));
                let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
                let fmt = crate::playback::render_format();
                client
                    .Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                        1_000_000,
                        0,
                        &fmt.Format,
                        None,
                    )
                    .context("render Initialize")?;
                let ev = CreateEventW(None, false, false, windows::core::PCWSTR::null())?;
                client.SetEventHandle(ev)?;
                let render: IAudioRenderClient = client.GetService()?;
                let size = client.GetBufferSize()?;
                client.Start()?;
                let _ = ready.send(Ok(id));
                let mut t: u64 = 0;
                while !stop.load(Ordering::Acquire) {
                    WaitForSingleObject(ev, 50);
                    let pad = client.GetCurrentPadding()?;
                    let room = size - pad;
                    if room == 0 {
                        continue;
                    }
                    let p = render.GetBuffer(room)? as *mut f32;
                    let out = std::slice::from_raw_parts_mut(p, room as usize * 2);
                    for f in 0..room as usize {
                        let ts = t as f64 / RATE as f64;
                        let v: f64 = TONES_HZ
                            .iter()
                            .map(|hz| TONE_AMP * (2.0 * std::f64::consts::PI * hz * ts).sin())
                            .sum();
                        out[2 * f] = v as f32;
                        out[2 * f + 1] = v as f32;
                        t += 1;
                    }
                    render.ReleaseBuffer(room, 0)?;
                }
                client.Stop()?;
                let _ = CloseHandle(ev);
                Ok(())
            })();
            if let Err(e) = r {
                let _ = ready.send(Err(e));
            }
            CoUninitialize();
        }
    }

    /// Capture `secs` of loopback, dropping the first quarter (filter and
    /// parameter settling), and return the tone levels.
    fn measure(cap: &AudioCapture, secs: f64) -> [f64; 3] {
        measure_after(cap, secs, secs / 4.0)
    }

    fn measure_after(cap: &AudioCapture, secs: f64, settle_secs: f64) -> [f64; 3] {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        let settle = Instant::now() + Duration::from_secs_f64(settle_secs);
        let mut buf = Vec::new();
        let (mut ch, mut rate) = (2usize, RATE);
        while Instant::now() < until {
            if let Some(b) = cap.next(Duration::from_millis(100)) {
                if Instant::now() >= settle {
                    ch = b.channels as usize;
                    rate = b.sample_rate;
                    buf.extend_from_slice(&b.samples);
                }
            }
        }
        levels(&buf, ch, rate)
    }

    pub fn observe(secs: f64, settle_secs: f64) -> Result<Observation> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let s2 = stop.clone();
        let play = std::thread::spawn(move || player(s2, tx));
        let id = rx.recv_timeout(Duration::from_secs(5)).context("player did not start")??;
        let endpoint = endpoint_guid(&id);
        let result = (|| -> Result<Observation> {
            let cap = AudioCapture::start(AudioSource::Desktop)?;
            let levels_db = measure_after(&cap, secs, settle_secs);
            let name =
                section_name(&endpoint, &std::env::var("RELAY_INSTANCE").unwrap_or_default());
            let shm = SharedParams::open(&name).ok();
            Ok(Observation {
                endpoint: endpoint.clone(),
                section_reachable: shm.is_some(),
                bypass: shm.as_ref().map(|s| s.block().bypass()),
                levels_db,
            })
        })();
        stop.store(true, Ordering::Release);
        let _ = play.join();
        result
    }

    pub fn run(phase_secs: f64) -> Result<Report> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let s2 = stop.clone();
        let play = std::thread::spawn(move || player(s2, tx));
        let id = rx.recv_timeout(Duration::from_secs(5)).context("player did not start")??;
        let endpoint = endpoint_guid(&id);
        let result = (|| -> Result<Report> {
            let cap = AudioCapture::start(AudioSource::Desktop)?;
            // The section exists only while an APO instance lives; with the
            // stimulus playing that is now, if Relay is in the graph at all.
            let name =
                section_name(&endpoint, &std::env::var("RELAY_INSTANCE").unwrap_or_default());
            let deadline = Instant::now() + Duration::from_secs(3);
            let shm = loop {
                match SharedParams::open(&name) {
                    Ok(s) => break Some(s),
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(100))
                    }
                    Err(_) => break None,
                }
            };
            let Some(shm) = shm else {
                let lv = measure(&cap, phase_secs);
                return Ok(judge(endpoint.clone(), false, lv, lv));
            };
            let block = shm.block();
            let prior_bypass = block.bypass();
            let prior = block.read_params().map(|(_, p)| p);
            block.set_bypass(true);
            shm.notify();
            let bypass = measure(&cap, phase_secs);
            block.write_params(&test_params());
            block.set_bypass(false);
            shm.notify();
            let eq = measure(&cap, phase_secs);
            // Put back exactly what was there.
            if let Some(p) = prior {
                block.write_params(&p);
            }
            block.set_bypass(prior_bypass);
            shm.notify();
            Ok(judge(endpoint.clone(), true, bypass, eq))
        })();
        stop.store(true, Ordering::Release);
        let _ = play.join();
        result
    }

    #[cfg(test)]
    #[test]
    fn endpoint_guid_is_the_last_dotted_part() {
        assert_eq!(
            endpoint_guid("{0.0.0.00000000}.{fe542c97-9221-4358-a079-dbd65b2fb258}"),
            "{FE542C97-9221-4358-A079-DBD65B2FB258}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stimulus(gains_db: [f64; 3], secs: f64) -> Vec<f32> {
        let rate = 48_000.0;
        let n = (secs * rate) as usize;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / rate;
            let s: f64 = TONES_HZ
                .iter()
                .zip(gains_db)
                .map(|(hz, g)| {
                    TONE_AMP * 10f64.powf(g / 20.0) * (2.0 * std::f64::consts::PI * hz * t).sin()
                })
                .sum();
            v.push(s as f32);
            v.push(s as f32);
        }
        v
    }

    #[test]
    fn goertzel_reads_each_tone_at_its_level() {
        let l = levels(&stimulus([0.0, -6.0, 3.0], 1.0), 2, 48_000);
        assert!((l[0] - -30.0).abs() < 0.3, "{l:?}");
        assert!((l[1] - -36.0).abs() < 0.3, "{l:?}");
        assert!((l[2] - -27.0).abs() < 0.3, "{l:?}");
    }

    #[test]
    fn the_test_cascade_passes_judge_when_run_through_the_real_chain() {
        let input = stimulus([0.0; 3], 1.0);
        let mut chain = relay_audio::dsp::Chain::new(test_params());
        chain.prepare(48_000, 480).unwrap();
        let mut out = vec![0.0; input.len()];
        for (i, o) in input.chunks(960).zip(out.chunks_mut(960)) {
            chain.process(i, o);
        }
        let r = judge("x".into(), true, levels(&input, 2, 48_000), levels(&out, 2, 48_000));
        assert!(r.pass, "{r:?}");
    }

    #[test]
    fn judge_names_each_failure() {
        let b = [-30.0, -30.0, -30.0];
        assert!(!judge("x".into(), false, b, b).verdict.is_empty());
        let r = judge("x".into(), true, b, b);
        assert!(!r.pass && r.verdict.contains("not processing"), "{r:?}");
        let r = judge("x".into(), true, [-90.0; 3], [-90.0; 3]);
        assert!(!r.pass && r.verdict.contains("not seen"), "{r:?}");
        let r = judge("x".into(), true, b, [-26.0, -30.0, -30.0]);
        assert!(!r.pass && r.verdict.contains("wrong amount"), "{r:?}");
        let r = judge("x".into(), true, b, [-21.5, -30.5, -35.5]);
        assert!(r.pass, "{r:?}");
    }
}
