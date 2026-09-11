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
use relay_audio::params::{BandParams, ChainParams, FilterKind, LimiterParams};

use crate::types::AudioSettings;

/// Profile audio → DSP parameters. EQ bands are peaking filters; shelves and
/// passes come later with the hardware library's headset-correction curves.
pub fn chain_params(audio: &AudioSettings) -> ChainParams {
    ChainParams {
        bands: audio
            .bands
            .iter()
            .map(|b| BandParams {
                kind: FilterKind::Peaking,
                freq_hz: b.freq_hz,
                gain_db: b.gain_db,
                q: b.q,
                enabled: true,
            })
            .collect(),
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
) -> Result<Preview> {
    let bin = preview_binary()?;
    let params = serde_json::to_string(&chain_params(audio))?;
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
        };
        let p = render_preview(&audio, None, &dir).unwrap();
        assert!(p.original.exists());
        assert!(p.processed.exists());
        assert_eq!(p.sample_rate, 48_000);
        assert!(p.hrtf_applied);
        std::fs::remove_dir_all(&dir).ok();
    }
}
