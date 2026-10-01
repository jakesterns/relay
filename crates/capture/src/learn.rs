//! `relay-share learn` — the S46 learner helper.
//!
//! Spawned by the core only while a profiled game with learning on has
//! focus; stopped on blur (`stop` on stdin, or stdin closing). It:
//! - captures **only the game's own audio** by process loopback of its PID
//!   (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, the same OS path
//!   Discord and OBS use; Discord and every other app are never in it);
//! - feeds it to [`relay_audio::learn::Analyzer`], which keeps aggregate
//!   statistics only — no samples are stored or sent anywhere;
//! - once a second reads the system-wide idle time (`GetLastInputInfo`: one
//!   timestamp, no hooks, no keys) so audio heard while nobody is playing
//!   does not count;
//! - folds the aggregates into the game's record, takes checkpoints, saves
//!   the record every [`SAVE_EVERY`] and on exit, and prints
//!   `{"event":"candidate"}` when a new curve is offered.

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use relay_audio::learn::{Analyzer, Goal, LearnRecord, Limits, Thresholds};
use relay_core::game_eq::{load_record_at, save_record_at};

use crate::audio::{AudioCapture, AudioSource};

/// How often the record is written while learning.
pub const SAVE_EVERY: Duration = Duration::from_secs(30);
/// How often aggregates are folded in and a checkpoint considered.
pub const FOLD_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
pub struct LearnArgs {
    pub pid: u32,
    pub exe: String,
    pub record: PathBuf,
    pub goal: Goal,
    pub version: Option<String>,
}

impl LearnArgs {
    pub fn parse(args: &[String]) -> Result<Self> {
        let mut pid = None;
        let mut exe = None;
        let mut record = None;
        let mut goal = Goal::default();
        let mut version = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().with_context(|| format!("{a} needs a value"));
            match a.as_str() {
                "--pid" => pid = Some(val()?.parse()?),
                "--exe" => exe = Some(val()?.clone()),
                "--record" => record = Some(PathBuf::from(val()?)),
                "--goal" => goal = serde_json::from_str(&format!("\"{}\"", val()?))?,
                "--version" => version = Some(val()?.clone()),
                other => anyhow::bail!("unknown learn option {other}"),
            }
        }
        Ok(Self {
            pid: pid.context("--pid is required")?,
            exe: exe.context("--exe is required")?,
            record: record.context("--record is required")?,
            goal,
            version,
        })
    }
}

/// Milliseconds since the last keyboard / mouse / pad input on this PC.
#[cfg(windows)]
pub fn input_idle_ms() -> u32 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: a plain query filling a struct we own; no handles involved.
    unsafe {
        if GetLastInputInfo(&mut info).as_bool() {
            GetTickCount().wrapping_sub(info.dwTime)
        } else {
            0
        }
    }
}

fn emit(v: serde_json::Value) {
    println!("{v}");
}

/// Run until stopped.
pub fn run(args: LearnArgs) -> Result<()> {
    let th = Thresholds::default();
    let limits = Limits::default();
    let mut rec = load_record_at(&args.record, &args.exe)
        .unwrap_or_else(|| LearnRecord::new(&args.exe, args.version.as_deref()));
    rec.set_goal(args.goal, &limits);
    if rec.begin_session(args.version.as_deref()) {
        emit(serde_json::json!({ "event": "needs_relearn" }));
    }

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::Builder::new().name("relay-learn-stdin".into()).spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                match line {
                    Ok(l) if l.trim() == "stop" => break,
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            // `stop`, or the core went away: either way, save and leave.
            stop.store(true, Ordering::Relaxed);
        })?;
    }

    let cap = AudioCapture::start(AudioSource::Process { pid: args.pid })
        .context("process loopback of the game")?;
    let mut rate = cap.sample_rate;
    let mut an = Analyzer::new(rate);
    emit(serde_json::json!({ "event": "learning", "rate": rate, "progress": rec.progress(&th) }));

    let mut last_fold = Instant::now();
    let mut last_save = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if let Some(block) = cap.next(Duration::from_millis(200)) {
            if block.sample_rate != rate {
                // The endpoint changed format: keep what was learned, restart
                // the filterbank at the new rate (never resample).
                rec.absorb(&an.take_stats(), &th);
                rate = block.sample_rate;
                an = Analyzer::new(rate);
            }
            an.push_interleaved(&block.samples, block.channels as usize);
        }
        if last_fold.elapsed() >= FOLD_EVERY {
            last_fold = Instant::now();
            #[cfg(windows)]
            an.set_input_idle_ms(input_idle_ms());
            rec.absorb(&an.take_stats(), &th);
            let outcome = rec.checkpoint(&th, &limits);
            if outcome.offers() {
                emit(serde_json::json!({ "event": "candidate", "progress": rec.progress(&th) }));
            }
        }
        if last_save.elapsed() >= SAVE_EVERY {
            last_save = Instant::now();
            if let Err(e) = save_record_at(&args.record, &rec) {
                tracing::warn!(error = %e, "saving the learning record failed");
            }
        }
    }
    rec.absorb(&an.take_stats(), &th);
    save_record_at(&args.record, &rec)?;
    emit(serde_json::json!({ "event": "stopped", "progress": rec.progress(&th) }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_parse_and_are_required() {
        let a = LearnArgs::parse(&s(&[
            "--pid", "42", "--exe", "game.exe", "--record", "r.json", "--goal", "dialogue",
            "--version", "1.2.3.4",
        ]))
        .unwrap();
        assert_eq!(a.pid, 42);
        assert_eq!(a.goal, Goal::Dialogue);
        assert_eq!(a.version.as_deref(), Some("1.2.3.4"));
        assert!(LearnArgs::parse(&s(&["--exe", "g.exe", "--record", "r"])).is_err());
        assert!(LearnArgs::parse(&s(&["--pid", "1", "--exe", "g.exe", "--record", "r", "--goal", "loud"]))
            .is_err());
        assert!(LearnArgs::parse(&s(&["--pid", "1", "--exe", "g.exe", "--record", "r", "--x", "1"]))
            .is_err());
    }

    #[test]
    fn idle_time_is_a_plain_number() {
        // Whatever the machine is doing, the call works without any handle.
        let _ = input_idle_ms();
    }
}
