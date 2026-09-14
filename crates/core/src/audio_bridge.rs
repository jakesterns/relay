//! Glue between the core's profile types and `relay-audio`.
//!
//! Two jobs:
//! - Translate a profile's [`AudioSettings`] into a [`relay_audio::ChainParams`]
//!   (the shape the DSP, the offline renderer and later the APO consume).
//! - Render the offline A/B listening test: the profile's chain applied to a
//!   WAV — the user's own clip or a synthesized demo — into `previews/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use relay_audio::params::{BandParams, ChainParams, FilterKind, LimiterParams, MAX_BANDS};

use crate::types::AudioSettings;

/// How many of the cascade's bands the headset correction may claim. The rest
/// are the user's, so a measured curve can never crowd out the tuning someone
/// dialled in by ear.
pub const CORRECTION_BUDGET: usize = 8;

/// Profile audio → DSP parameters, with no headset correction.
pub fn chain_params(audio: &AudioSettings) -> ChainParams {
    chain_params_with(audio, None)
}

/// Profile audio → DSP parameters.
///
/// When the profile opts in and the connected headset has an imported curve,
/// the correction is fitted to filters and placed **first** in the cascade:
/// correction makes the headset neutral, and the profile's own bands are
/// taste applied on top of a neutral headset. The other order would have the
/// correction partly undo the user's own EQ.
///
/// The profile's bands are peaking filters; the fitted correction also uses
/// shelves, which is why it cannot be expressed as profile bands.
pub fn chain_params_with(audio: &AudioSettings, correction: Option<&[(f32, f32)]>) -> ChainParams {
    let mut bands: Vec<BandParams> = Vec::new();

    if audio.headset_correction {
        if let Some(curve) = correction.filter(|c| c.len() >= 2) {
            // Never take more than what is left after the user's own bands.
            let budget = CORRECTION_BUDGET.min(MAX_BANDS.saturating_sub(audio.bands.len()));
            bands.extend(relay_audio::fit::fit_curve(curve, budget).bands);
        }
    }

    bands.extend(audio.bands.iter().map(|b| BandParams {
        kind: FilterKind::Peaking,
        freq_hz: b.freq_hz,
        gain_db: b.gain_db,
        q: b.q,
        enabled: true,
    }));
    bands.truncate(MAX_BANDS);

    ChainParams {
        bands,
        limiter: audio.limiter.as_ref().map(|l| LimiterParams::new(l.below_hz, l.threshold_db)),
        hrtf: audio.hrtf,
    }
}

/// True when the profile configures any audio processing at all — the gate
/// for the exclusive-mode watcher (no processing ⇒ nothing is bypassed).
pub fn wants_processing(audio: &AudioSettings) -> bool {
    !audio.bands.is_empty() || audio.hrtf || audio.limiter.is_some()
}

/// Result of an A/B render, IPC-friendly.
pub struct Preview {
    pub original: PathBuf,
    pub processed: PathBuf,
    pub sample_rate: u32,
    pub hrtf_applied: bool,
}

/// The `relay-preview` child binary: next to the running exe in production,
/// one directory up when running from `target/…/deps` (unit tests).
pub fn preview_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current exe")?;
    let dir = exe.parent().context("exe has no parent")?;
    let name = if cfg!(windows) { "relay-preview.exe" } else { "relay-preview" };
    for cand in [dir.join(name), dir.join("..").join(name)] {
        if cand.exists() {
            return Ok(cand);
        }
    }
    bail!("preview renderer not found next to {} (build relay-audio)", dir.display())
}

/// Render the A/B pair into `previews_dir` by spawning `relay-preview` (the
/// DSP stays out of the always-on core's memory budget; the child renders
/// and exits). With `wav = None` the child synthesizes a demo clip. Both
/// output files land in `previews_dir`, the one directory the UI may read.
pub fn render_preview(
    audio: &AudioSettings,
    wav: Option<&Path>,
    previews_dir: &Path,
    correction: Option<&[(f32, f32)]>,
) -> Result<Preview> {
    let bin = preview_binary()?;
    // The same chain the APO would run, headset correction included -- an A/B
    // test that left it out would not be the sound the profile produces.
    let params = serde_json::to_string(&chain_params_with(audio, correction))?;
    let mut cmd = Command::new(&bin);
    cmd.arg("--params").arg(params).arg("--out-dir").arg(previews_dir);
    if let Some(src) = wav {
        cmd.arg("--wav").arg(src);
    }
    let out =
        cmd.stdin(Stdio::null()).output().with_context(|| format!("running {}", bin.display()))?;
    if !out.status.success() {
        bail!("relay-preview failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let reply: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing relay-preview output")?;
    let path = |k: &str| -> Result<PathBuf> {
        Ok(PathBuf::from(
            reply[k].as_str().with_context(|| format!("missing {k} in relay-preview output"))?,
        ))
    };
    Ok(Preview {
        original: path("original")?,
        processed: path("processed")?,
        sample_rate: reply["sample_rate"].as_u64().unwrap_or(0) as u32,
        hrtf_applied: reply["hrtf_applied"].as_bool().unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EqBand, Limiter};

    #[test]
    fn settings_translate_to_chain_params() {
        let audio = AudioSettings {
            bands: vec![EqBand { freq_hz: 3000.0, gain_db: 4.5, q: 1.0 }],
            hrtf: true,
            limiter: Some(Limiter { below_hz: 120.0, threshold_db: -10.0 }),
            apply_to_share: false,
            headset_correction: false,
        };
        let p = chain_params(&audio);
        assert_eq!(p.bands.len(), 1);
        assert_eq!(p.bands[0].freq_hz, 3000.0);
        assert!(p.bands[0].enabled);
        assert!(p.hrtf);
        assert_eq!(p.limiter.unwrap().below_hz, 120.0);
        assert!(wants_processing(&audio));
        assert!(!wants_processing(&AudioSettings::default()));
    }

    /// A correction asking for a broad +6 dB lift at 1 kHz.
    fn curve() -> Vec<(f32, f32)> {
        (0..200)
            .map(|i| {
                let hz = 20.0 * (1000.0f32).powf(i as f32 / 199.0);
                let x = (hz / 1000.0).ln() / std::f32::consts::LN_2;
                (hz, 6.0 * (-x * x * 2.0).exp())
            })
            .collect()
    }

    fn with_correction(on: bool, bands: Vec<EqBand>) -> AudioSettings {
        AudioSettings { bands, headset_correction: on, ..AudioSettings::default() }
    }

    #[test]
    fn the_correction_is_ignored_unless_the_profile_asks_for_it() {
        let off = chain_params_with(&with_correction(false, vec![]), Some(&curve()));
        assert!(off.bands.is_empty(), "opted out, so no correction: {:?}", off.bands);

        let on = chain_params_with(&with_correction(true, vec![]), Some(&curve()));
        assert!(!on.bands.is_empty(), "opted in, so the curve is fitted");
    }

    #[test]
    fn a_missing_or_degenerate_curve_is_not_an_error() {
        let s = with_correction(true, vec![]);
        assert!(chain_params_with(&s, None).bands.is_empty());
        // One point cannot describe a curve; treat it as no correction.
        assert!(chain_params_with(&s, Some(&[(1000.0, 5.0)])).bands.is_empty());
    }

    #[test]
    fn correction_comes_first_so_taste_sits_on_a_neutral_headset() {
        let taste = EqBand { freq_hz: 3000.0, gain_db: 4.5, q: 1.0 };
        let p = chain_params_with(&with_correction(true, vec![taste]), Some(&curve()));
        let last = p.bands.last().expect("bands");
        assert_eq!(last.freq_hz, 3000.0, "the user's band is last: {:?}", p.bands);
        assert_eq!(last.gain_db, 4.5);
        assert!(p.bands.len() > 1, "and the correction is ahead of it");
    }

    #[test]
    fn the_users_own_bands_are_never_crowded_out() {
        // Fill the cascade with user bands, leaving no room for correction.
        let taste: Vec<EqBand> = (0..MAX_BANDS)
            .map(|i| EqBand { freq_hz: 100.0 * (i + 1) as f32, gain_db: 1.0, q: 1.0 })
            .collect();
        let p = chain_params_with(&with_correction(true, taste.clone()), Some(&curve()));
        assert_eq!(p.bands.len(), MAX_BANDS);
        for (got, want) in p.bands.iter().zip(&taste) {
            assert_eq!(got.freq_hz, want.freq_hz, "every user band survived: {:?}", p.bands);
        }
    }

    #[test]
    fn the_correction_never_exceeds_its_budget() {
        let p = chain_params_with(&with_correction(true, vec![]), Some(&curve()));
        assert!(p.bands.len() <= CORRECTION_BUDGET, "{} bands", p.bands.len());
    }

    #[test]
    fn preview_renders_the_demo_when_no_wav_is_given() {
        if preview_binary().is_err() {
            eprintln!("skipped: relay-preview not built (run a workspace build first)");
            return;
        }
        let dir = std::env::temp_dir().join("relay-core-preview-test");
        std::fs::create_dir_all(&dir).unwrap();
        let audio = AudioSettings {
            bands: vec![EqBand { freq_hz: 3000.0, gain_db: 4.0, q: 1.0 }],
            hrtf: true,
            limiter: Some(Limiter { below_hz: 150.0, threshold_db: -12.0 }),
            apply_to_share: false,
            headset_correction: false,
        };
        let p = render_preview(&audio, None, &dir, None).unwrap();
        assert!(p.original.exists());
        assert!(p.processed.exists());
        assert_eq!(p.sample_rate, 48_000);
        assert!(p.hrtf_applied);
        std::fs::remove_dir_all(&dir).ok();
    }
}
