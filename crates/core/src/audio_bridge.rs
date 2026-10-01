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

/// How many cascade bands the learned / imported game layer may claim.
pub const GAME_LAYER_BUDGET: usize = relay_audio::learn::GAME_BUDGET;

/// The safety limiter a game layer brings when the profile has none: its
/// boosts are gain-compensated, but a transient in a lifted band can still
/// peak higher. Full band (the split sits above the audible range).
pub const GAME_LAYER_LIMITER_HZ: f32 = 18_000.0;
pub const GAME_LAYER_LIMITER_DB: f32 = -1.0;

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
///
/// S46: the game layer (learned or imported) sits between the two:
/// correction → game layer → the user's own bands. It does not depend on the
/// headset, so switching listening device swaps only the correction.
pub fn chain_params_with(audio: &AudioSettings, correction: Option<&[(f32, f32)]>) -> ChainParams {
    let mut bands: Vec<BandParams> = Vec::new();

    if audio.headset_correction {
        if let Some(curve) = correction.filter(|c| c.len() >= 2) {
            // Never take more than what is left after the user's own bands.
            let budget = CORRECTION_BUDGET.min(MAX_BANDS.saturating_sub(audio.bands.len()));
            bands.extend(relay_audio::fit::fit_curve(curve, budget).bands);
        }
    }

    let mut layer_boosts = false;
    if let Some(layer) = audio.game_eq.as_ref().filter(|l| l.curve.len() >= 2) {
        let left = MAX_BANDS.saturating_sub(audio.bands.len() + bands.len());
        let game = relay_audio::fit::fit_curve(&layer.curve, GAME_LAYER_BUDGET.min(left)).bands;
        // Judged on the curve, not the fitted filters: a shelf that shapes a
        // pure cut can carry a small positive band without boosting anything.
        layer_boosts = layer.curve.iter().any(|&(_, db)| db > 0.05);
        bands.extend(game);
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
        limiter: match &audio.limiter {
            Some(l) => Some(LimiterParams::new(l.below_hz, l.threshold_db)),
            None if layer_boosts => {
                Some(LimiterParams::new(GAME_LAYER_LIMITER_HZ, GAME_LAYER_LIMITER_DB))
            }
            None => None,
        },
        hrtf: audio.hrtf,
    }
}

/// The measured curve headset correction should use (S41).
///
/// The headset the profile names, else the ACTIVE listening device of the
/// default output (`connected.headset`, which [`crate::hardware::HardwareStore::connected`]
/// resolves from the per-output listening list). Speakers, or an output
/// whose listening device has not been picked, resolve to no headset and so
/// to no correction: correcting for headphones that are not on the user's
/// head would be wrong.
pub fn correction_curve<'a>(
    library: &'a crate::hardware::HardwareStore,
    profile_headset: Option<&crate::types::HeadsetId>,
    connected: &crate::hardware::ConnectedHardware,
) -> Option<&'a [(f32, f32)]> {
    let id = profile_headset.or(connected.headset.as_ref())?;
    library.headset(id)?.curve.as_deref()
}

/// True when the profile configures any audio processing at all — the gate
/// for the exclusive-mode watcher (no processing ⇒ nothing is bypassed).
pub fn wants_processing(audio: &AudioSettings) -> bool {
    !audio.bands.is_empty()
        || audio.hrtf
        || audio.limiter.is_some()
        || audio.game_eq.as_ref().is_some_and(|l| l.curve.len() >= 2)
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
            ..AudioSettings::default()
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
            ..AudioSettings::default()
        };
        let p = render_preview(&audio, None, &dir, None).unwrap();
        assert!(p.original.exists());
        assert!(p.processed.exists());
        assert_eq!(p.sample_rate, 48_000);
        assert!(p.hrtf_applied);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn layer(curve: &[(f32, f32)]) -> relay_audio::learn::GameEqLayer {
        relay_audio::learn::GameEqLayer::learned(curve.to_vec(), None)
    }

    /// S46: correction, then the game layer, then the user's bands.
    #[test]
    fn the_game_layer_stacks_between_correction_and_taste() {
        let taste = EqBand { freq_hz: 3000.0, gain_db: 4.5, q: 1.0 };
        let mut audio = with_correction(true, vec![taste]);
        let game = [(20.0, -4.0), (100.0, -4.0), (400.0, 0.0), (16000.0, 0.0)];
        audio.game_eq = Some(layer(&game));
        let without = chain_params_with(&with_correction(true, vec![]), Some(&curve()));
        let p = chain_params_with(&audio, Some(&curve()));
        let n = without.bands.len();
        assert_eq!(&p.bands[..n], &without.bands[..], "correction first, unchanged");
        assert_eq!(p.bands.last().unwrap().freq_hz, 3000.0, "the user's band is last");
        let g = &p.bands[n..p.bands.len() - 1];
        assert!(!g.is_empty() && g.len() <= GAME_LAYER_BUDGET, "{g:?}");
        assert!(relay_audio::fit::response_db(g, 50.0) < -2.0);
        assert!(p.bands.len() <= MAX_BANDS);
        let only = AudioSettings { game_eq: Some(layer(&game)), ..AudioSettings::default() };
        assert!(wants_processing(&only));
    }

    /// Owner rule: the learned layer is hardware-independent. A different
    /// headset (or none) changes only the correction ahead of it.
    #[test]
    fn the_game_layer_is_the_same_whatever_the_headset() {
        let game = [(20.0, -3.0), (250.0, 0.0), (3150.0, 2.0), (16000.0, 0.0)];
        let audio = AudioSettings { game_eq: Some(layer(&game)), ..with_correction(true, vec![]) };
        let a = chain_params_with(&audio, Some(&curve()));
        let flat = [(20.0, 1.5), (20000.0, 1.5)];
        let b = chain_params_with(&audio, Some(&flat));
        let c = chain_params_with(&audio, None);
        let tail = |p: &ChainParams, k: usize| p.bands[p.bands.len() - k..].to_vec();
        let k = c.bands.len();
        assert!(k > 0);
        assert_eq!(tail(&a, k), c.bands);
        assert_eq!(tail(&b, k), c.bands);
    }

    #[test]
    fn a_boosting_game_layer_brings_a_safety_limiter_only_when_none_is_set() {
        let boost = [(20.0, 0.0), (2000.0, 0.0), (3150.0, 3.0), (16000.0, 0.0)];
        let cut = [(20.0, -4.0), (200.0, -2.0), (400.0, 0.0), (16000.0, 0.0)];
        let mut audio = AudioSettings { game_eq: Some(layer(&boost)), ..AudioSettings::default() };
        let l = chain_params(&audio).limiter.expect("safety limiter");
        assert_eq!((l.below_hz, l.threshold_db), (GAME_LAYER_LIMITER_HZ, GAME_LAYER_LIMITER_DB));
        audio.game_eq = Some(layer(&cut));
        assert!(chain_params(&audio).limiter.is_none(), "cuts alone cannot overshoot");
        audio.game_eq = Some(layer(&boost));
        audio.limiter = Some(Limiter { below_hz: 120.0, threshold_db: -10.0 });
        assert_eq!(
            chain_params(&audio).limiter.unwrap().below_hz,
            120.0,
            "the user's limiter wins"
        );
    }

    #[test]
    fn learning_defaults_follow_the_owner_rules() {
        use relay_audio::learn::{GameEqFile, Goal};
        // No processing: off.
        assert!(!AudioSettings::default().learning_on());
        // Processing: on, but inactive until a goal is chosen.
        let mut a = with_correction(true, vec![EqBand { freq_hz: 1000.0, gain_db: 1.0, q: 1.0 }]);
        assert!(a.learning_on() && !a.learning_active());
        a.game_eq_goal = Some(Goal::Awareness);
        assert!(a.learning_active());
        // An imported layer turns the default off ("applied (imported)").
        let text = r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"g.exe"},"curve":[[20,0],[1000,1]]}"#;
        a.game_eq = Some(GameEqFile::parse(text).unwrap().to_layer());
        assert!(!a.learning_on());
        // ...unless the user asks to keep learning to fine-tune.
        a.learn_game_eq = Some(true);
        assert!(a.learning_active());
    }

    /// S41: one RODECaster output feeds IEMs, a headset and speakers. The
    /// correction follows whichever the user marked active; speakers get
    /// none.
    #[test]
    fn correction_follows_the_active_listening_device() {
        use crate::hardware::{
            EndpointInfo, HardwareStore, Headset, HeadsetKind, ListeningDevice, ProbeReport,
        };
        use crate::types::HeadsetId;

        let mut lib = HardwareStore::in_memory();
        for (id, gain) in [("blessing3", 3.0), ("hd560s", -2.0)] {
            lib.upsert_headset(Headset {
                id: HeadsetId(id.into()),
                name: id.into(),
                kind: HeadsetKind::Headphone,
                curve: Some(vec![(20.0, gain), (20000.0, gain)]),
                source: String::new(),
                endpoints: vec![],
            });
        }
        let report = ProbeReport {
            endpoints: vec![EndpointInfo {
                key: "ep:c:rode".into(),
                name: "RODECaster".into(),
                default: true,
                fx_guid: String::new(),
            }],
            monitors: vec![],
        };
        let iem = ListeningDevice::Headset { id: HeadsetId("blessing3".into()) };
        let hd = ListeningDevice::Headset { id: HeadsetId("hd560s".into()) };
        let l = lib.listening_entry("ep:c:rode");
        l.set_devices(vec![iem.clone(), hd.clone(), ListeningDevice::Speakers]);

        // Several entries and no pick: no correction rather than a guess.
        let hw = lib.connected(&report);
        assert_eq!(correction_curve(&lib, None, &hw), None);

        lib.listening_entry("ep:c:rode").set_active(&hd);
        let hw = lib.connected(&report);
        assert_eq!(correction_curve(&lib, None, &hw).unwrap()[0].1, -2.0);

        lib.listening_entry("ep:c:rode").set_active(&iem);
        let hw = lib.connected(&report);
        assert_eq!(correction_curve(&lib, None, &hw).unwrap()[0].1, 3.0);

        lib.listening_entry("ep:c:rode").set_active(&ListeningDevice::Speakers);
        let hw = lib.connected(&report);
        assert_eq!(correction_curve(&lib, None, &hw), None);

        // A profile that names its headset keeps that curve.
        let named = HeadsetId("hd560s".into());
        assert_eq!(correction_curve(&lib, Some(&named), &hw).unwrap()[0].1, -2.0);
    }
}
