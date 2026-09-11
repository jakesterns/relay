//! `relay-preview` — offline A/B renderer, spawned on demand by the core.
//!
//! The always-on core must not link the FFT machinery (10 MB budget), so
//! this binary does the rendering and exits. Protocol:
//!
//! ```text
//! relay-preview --params <ChainParams JSON> --out-dir <dir> [--wav <source>]
//! ```
//!
//! Writes `original.wav` (the source clip, or a synthesized demo without
//! `--wav`) and `processed.wav` into `--out-dir`, then prints one JSON line:
//! `{"original": ..., "processed": ..., "sample_rate": ..., "hrtf_applied": ...}`.
//! Errors go to stderr with a non-zero exit.

use std::path::PathBuf;
use std::process::ExitCode;

use relay_audio::params::ChainParams;

fn run() -> Result<(), String> {
    let mut params: Option<ChainParams> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut wav: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--params" => {
                let json = value("--params")?;
                params =
                    Some(serde_json::from_str(&json).map_err(|e| format!("bad --params: {e}"))?);
            }
            "--out-dir" => out_dir = Some(PathBuf::from(value("--out-dir")?)),
            "--wav" => wav = Some(PathBuf::from(value("--wav")?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let params = params.ok_or("missing --params")?;
    let out_dir = out_dir.ok_or("missing --out-dir")?;

    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    let original = out_dir.join("original.wav");
    let processed = out_dir.join("processed.wav");
    match &wav {
        Some(src) => {
            std::fs::copy(src, &original).map_err(|e| format!("copying {}: {e}", src.display()))?;
        }
        None => relay_audio::offline::synthesize_demo(&original).map_err(|e| e.to_string())?,
    }
    let report =
        relay_audio::offline::render(&params, &original, &processed).map_err(|e| e.to_string())?;

    println!(
        "{}",
        serde_json::json!({
            "original": original.display().to_string(),
            "processed": processed.display().to_string(),
            "sample_rate": report.sample_rate,
            "hrtf_applied": report.hrtf_applied,
        })
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("relay-preview: {e}");
            ExitCode::FAILURE
        }
    }
}
