//! S46: learned game EQ — the core's side.
//!
//! The analysis itself never runs in the core. While a profiled game with
//! learning on (and a goal chosen) has focus, the core spawns
//! `relay-share learn` for the game's PID; that helper captures the game's
//! audio by process loopback, keeps only aggregate statistics, and writes
//! them to `game-eq\<exe>.json` (see [`record_file`]). The core reads that
//! record when the UI asks, applies the user's actions, and stops the helper
//! on blur, exit or restore. Nothing here touches the game process: the
//! exe's file version comes from its file on disk ([`exe_version`]).
//!
//! What the profile stores: the goal, the learning switch, auto-apply, and
//! the applied game layer (learned, imported, or imported-then-tuned). The
//! layer is independent of the headset; the S41 correction underneath it is
//! what follows the listening device.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use relay_audio::learn::state::same_curve;
use relay_audio::learn::{
    GameEqFile, GameEqLayer, Goal, LayerSource, LearnRecord, LearnStatus, Limits, Thresholds,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Paths;
use crate::types::{Foreground, Profile};

/// How long a stop waits for the helper's final save before killing it.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// One request from the UI about a profile's game EQ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GameEqAction {
    /// Read-only.
    Status,
    /// "Learn this game's sound" on or off.
    SetLearning { enabled: bool },
    /// Choose (or change) the goal. Re-derives from the saved aggregates.
    SetGoal { goal: Goal },
    /// Take a newly converged curve without asking.
    SetAutoApply { enabled: bool },
    /// Apply the curve on offer.
    Apply,
    /// Forget the evidence and learn again (the applied layer stays).
    Relearn,
    /// Remove the game layer and the learned evidence.
    Reset,
    /// Import a game EQ file (its text).
    Import { text: String },
    /// Export the applied layer, with an optional note.
    Export {
        #[serde(default)]
        note: String,
    },
}

impl GameEqAction {
    /// Actions that rewrite the record: the helper must be stopped first so
    /// its final save cannot overwrite them.
    pub fn touches_record(&self) -> bool {
        matches!(self, GameEqAction::SetGoal { .. } | GameEqAction::Relearn | GameEqAction::Reset)
    }
}

/// What the UI shows for one profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEqStatus {
    pub exe: String,
    pub state: LearnStatus,
    /// Learning switched on (the user's choice, or the default).
    pub learning_on: bool,
    /// On, but waiting for the goal question.
    pub needs_goal: bool,
    /// The helper is listening right now.
    pub learning_now: bool,
    pub goal: Option<Goal>,
    pub auto_apply: bool,
    /// Where the applied layer came from, if one is applied.
    pub source: Option<LayerSource>,
    pub progress: u8,
    pub active_minutes: f32,
    /// Evidence so far, and what readiness needs.
    pub targets: u64,
    pub maskers: u64,
    pub min_targets: u64,
    pub min_maskers: u64,
    pub distinct_voices: u32,
    pub exe_version: Option<String>,
    /// The applied layer's curve.
    pub applied: Option<Vec<(f32, f32)>>,
    /// What Apply would apply now (blended, for an imported layer).
    pub offer: Option<Vec<(f32, f32)>>,
    pub note: String,
    pub last_error: Option<String>,
    /// A one-off message about the action just taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Evidence per sound class, by name, in the record's array order.
    #[serde(default)]
    pub classes: Vec<ClassCount>,
    /// Frames left out of the statistics, by reason (the rolling window).
    #[serde(default)]
    pub excluded: Excluded,
    /// What the 90 % → ready step is waiting on.
    #[serde(default)]
    pub convergence: Option<relay_audio::learn::state::Convergence>,
    /// S48: seconds of active play still needed, from the event rate so far
    /// (`None` while unknown; 0 once a curve is ready).
    #[serde(default)]
    pub eta_secs: Option<u64>,
}

/// Frames the learner did not learn from, by reason. Music is not here: it
/// is a class (a masker), not an exclusion.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Excluded {
    /// One speaker talking over the game (commentary, routed voice chat).
    pub overlay_voice: u64,
    /// Codec band-limited in-game player chat.
    pub player_chat: u64,
    /// No input for over 20 s: cutscenes, menus, idle / AFK.
    pub cutscene_or_idle: u64,
    pub silence: u64,
    pub clipped: u64,
    pub volume_change: u64,
}

/// One sound class's evidence in the rolling window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassCount {
    pub class: relay_audio::learn::SoundClass,
    pub events: u64,
    pub frames: u64,
}

/// An export: the file text and where a copy was written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEqExport {
    pub text: String,
    pub path: String,
}

/// A file-name-safe form of an exe name.
fn safe_name(exe: &str) -> String {
    exe.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect()
}

/// `game-eq\<exe>.json`: one learning record per game exe.
pub fn record_file(paths: &Paths, exe: &str) -> PathBuf {
    paths.game_eq_dir().join(format!("{}.json", safe_name(exe)))
}

/// Where exports are written.
pub fn export_dir(paths: &Paths) -> PathBuf {
    paths.data_dir().join("exports")
}

/// The stored record for `exe`, or `None` (missing, unreadable, another
/// schema: all mean "start fresh").
pub fn load_record_at(path: &Path, exe: &str) -> Option<LearnRecord> {
    // Size first: a record is tens of KB; a huge file is not ours to parse.
    if std::fs::metadata(path).ok()?.len() > MAX_RECORD_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    if text.len() as u64 > MAX_RECORD_BYTES {
        return None;
    }
    LearnRecord::from_json(&text, exe)
}

/// Largest record file read, bytes.
pub const MAX_RECORD_BYTES: u64 = 1 << 20;

/// How long anyone waits for another holder of a record's lock.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Exclusive hold on one game's record (`<record>.lock`, opened with no
/// sharing). The learner helper holds it for its whole run, so a helper that
/// is still stopping — and about to make its final save — blocks the core's
/// Reset / Relearn / goal change and any new helper until it has exited.
pub struct RecordLock {
    _file: std::fs::File,
}

pub fn lock_record(record: &Path, timeout: Duration) -> Result<RecordLock> {
    let path = record.with_extension("lock");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let until = Instant::now() + timeout;
    loop {
        let mut o = std::fs::OpenOptions::new();
        o.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            o.share_mode(0);
        }
        match o.open(&path) {
            Ok(f) => return Ok(RecordLock { _file: f }),
            Err(e) if Instant::now() < until => {
                tracing::debug!(error = %e, "record busy; waiting");
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => bail!("the learning record is busy ({e})"),
        }
    }
}

/// Outcome of one [`apply_action`].
#[derive(Debug, Default)]
pub struct ActionResult {
    /// The profile changed and must be saved.
    pub changed: bool,
    pub export: Option<GameEqExport>,
    /// Something to tell the user (e.g. an import was made safe).
    pub notice: Option<String>,
}

fn done(changed: bool) -> Result<ActionResult> {
    Ok(ActionResult { changed, ..Default::default() })
}

pub fn load_record(paths: &Paths, exe: &str) -> Option<LearnRecord> {
    load_record_at(&record_file(paths, exe), exe)
}

pub fn save_record_at(path: &Path, rec: &LearnRecord) -> Result<()> {
    let json = serde_json::to_vec(rec)?;
    crate::profiles::write_atomic(path, &json)
}

/// The goal the record should be derived for.
fn goal_of(profile: &Profile) -> Goal {
    profile.audio.game_eq_goal.unwrap_or_default()
}

/// The status of `profile`'s game EQ.
pub fn status(paths: &Paths, profile: &Profile, learning_now: bool) -> GameEqStatus {
    status_with(load_record(paths, &profile.game.exe).as_ref(), profile, learning_now)
}

/// [`status`] with the record already loaded.
pub fn status_with(
    rec: Option<&LearnRecord>,
    profile: &Profile,
    learning_now: bool,
) -> GameEqStatus {
    let audio = &profile.audio;
    let th = Thresholds::default();
    let layer = audio.game_eq.as_ref();
    let applied = layer.map(|l| l.curve.clone());
    let on = audio.learning_on();
    let offer = rec.and_then(|r| r.candidate.as_deref()).map(|c| {
        layer.map(|l| l.offer(c)).unwrap_or_else(|| relay_audio::learn::derive::guard_curve(c))
    });

    let needs_relearn = rec.is_some_and(|r| r.needs_relearn) && offer.is_none();
    let state = if needs_relearn {
        if applied.is_some() || on {
            LearnStatus::NeedsRelearn
        } else {
            LearnStatus::Off
        }
    } else {
        match (&offer, &applied) {
            (Some(o), Some(a)) if same_curve(o, a) => LearnStatus::Applied,
            // Imported with learning off: applied as imported, no offer.
            (Some(_), Some(_)) if !on => LearnStatus::Applied,
            (Some(_), _) if on => LearnStatus::Ready,
            (_, Some(_)) => LearnStatus::Applied,
            _ if on => LearnStatus::Learning,
            _ => LearnStatus::Off,
        }
    };
    let goal = audio.game_eq_goal;
    let th_c = Thresholds::default();
    let convergence = rec.map(|r| r.convergence(&th_c));
    let excluded = rec
        .map(|r| {
            let w = r.window();
            Excluded {
                overlay_voice: w.overlay_voice_frames,
                player_chat: w.chat_frames,
                cutscene_or_idle: w.input_idle_frames,
                silence: w.silent_frames,
                clipped: w.clipped_frames,
                volume_change: w.level_jump_frames,
            }
        })
        .unwrap_or_default();
    // S48: what readiness needs now, scaled to the game's own spread.
    let (min_targets, min_maskers) = match rec {
        Some(r) => {
            let mut r = r.clone();
            r.goal = goal.unwrap_or_default();
            r.required(&th)
        }
        None => (th.min_cues, th.min_maskers),
    };
    let eta_secs = rec.and_then(|r| r.eta_secs(&th));
    let (targets, maskers, minutes, voices, classes) = match rec {
        Some(r) => {
            let w = r.window();
            let (t, m) = goal.unwrap_or_default().evidence(&w);
            let classes = relay_audio::learn::SoundClass::ALL
                .iter()
                .map(|&c| ClassCount {
                    class: c,
                    events: w.events[c.index()],
                    frames: w.class_frames[c.index()],
                })
                .collect();
            (t, m, r.active_minutes(), w.distinct_voices() as u32, classes)
        }
        None => (0, 0, 0.0, 0, Vec::new()),
    };
    GameEqStatus {
        exe: profile.game.exe.clone(),
        state,
        learning_on: on,
        needs_goal: on && goal.is_none(),
        learning_now,
        goal,
        auto_apply: audio.game_eq_auto_apply,
        source: layer.map(|l| l.source),
        progress: rec.map(|r| r.progress(&th)).unwrap_or(0),
        active_minutes: (minutes * 10.0).round() / 10.0,
        targets,
        maskers,
        min_targets,
        min_maskers,
        distinct_voices: voices,
        exe_version: rec.and_then(|r| r.exe_version.clone()),
        applied,
        offer,
        note: layer.map(|l| l.note.clone()).unwrap_or_default(),
        last_error: rec.and_then(|r| r.last_error.clone()),
        notice: None,
        classes,
        excluded,
        convergence,
        eta_secs,
    }
}

/// Take the offer into the profile. Returns false when there is none.
pub fn take_offer(profile: &mut Profile, rec: &LearnRecord) -> bool {
    let Some(candidate) = rec.candidate.as_deref() else { return false };
    let layer = match profile.audio.game_eq.take() {
        Some(l) if l.is_imported() => GameEqLayer {
            curve: l.offer(candidate),
            source: LayerSource::Tuned,
            exe_version: rec.exe_version.clone().or(l.exe_version),
            note: l.note,
            base: l.base.or(Some(l.curve)),
        },
        _ => GameEqLayer::learned(
            relay_audio::learn::derive::guard_curve(candidate),
            rec.exe_version.clone(),
        ),
    };
    profile.audio.game_eq = Some(layer);
    true
}

/// Apply one action to `profile` (and its record on disk). The caller saves
/// the profile when this returns `Ok(true, _)` and has already stopped the
/// helper when [`GameEqAction::touches_record`].
pub fn apply_action(
    paths: &Paths,
    profile: &mut Profile,
    action: &GameEqAction,
) -> Result<ActionResult> {
    let exe = profile.game.exe.clone();
    let file = record_file(paths, &exe);
    // Wait out a stopping helper's final save before touching the record.
    let _lock =
        if action.touches_record() { Some(lock_record(&file, LOCK_TIMEOUT)?) } else { None };
    match action {
        GameEqAction::Status => done(false),
        GameEqAction::SetLearning { enabled } => {
            profile.audio.learn_game_eq = Some(*enabled);
            done(true)
        }
        GameEqAction::SetAutoApply { enabled } => {
            profile.audio.game_eq_auto_apply = *enabled;
            done(true)
        }
        GameEqAction::SetGoal { goal } => {
            profile.audio.game_eq_goal = Some(*goal);
            if let Some(mut rec) = load_record_at(&file, &exe) {
                // Same aggregates, new goal: the curve is re-derived now.
                if rec.set_goal(*goal, &Limits::default()) {
                    // A learned layer follows the goal at once; an imported
                    // one keeps waiting for the user's Apply.
                    let learned = profile
                        .audio
                        .game_eq
                        .as_ref()
                        .is_some_and(|l| l.source == LayerSource::Learned);
                    if learned {
                        take_offer(profile, &rec);
                    }
                }
                save_record_at(&file, &rec)?;
            }
            done(true)
        }
        GameEqAction::Apply => {
            let rec = load_record_at(&file, &exe).context("nothing has been learned yet")?;
            if !take_offer(profile, &rec) {
                bail!("there is no learned curve to apply yet");
            }
            done(true)
        }
        GameEqAction::Relearn => {
            let mut rec =
                load_record_at(&file, &exe).unwrap_or_else(|| LearnRecord::new(&exe, None));
            rec.relearn();
            rec.goal = goal_of(profile);
            save_record_at(&file, &rec)?;
            done(false)
        }
        GameEqAction::Reset => {
            profile.audio.game_eq = None;
            profile.audio.learn_game_eq = None;
            let _ = std::fs::remove_file(&file);
            done(true)
        }
        GameEqAction::Import { text } => {
            let f = GameEqFile::parse(text)?;
            if !f.game.exe.eq_ignore_ascii_case(&exe) {
                bail!("this file is for {}, not {}", f.game.exe, exe);
            }
            profile.audio.game_eq = Some(f.to_layer());
            // Imported: applied as it is, learning off unless asked for.
            profile.audio.learn_game_eq = None;
            if profile.audio.game_eq_goal.is_none() {
                profile.audio.game_eq_goal = f.goal;
            }
            let notice = f.adjusted().then(|| {
                "The imported curve went past Relay's hearing-safety limits and was \
                 adjusted to fit them."
                    .to_string()
            });
            Ok(ActionResult { changed: true, export: None, notice })
        }
        GameEqAction::Export { note } => {
            // Nothing applied yet but a curve on offer: export the offer.
            let offered = match profile.audio.game_eq.as_ref() {
                Some(_) => None,
                None => load_record_at(&file, &exe).and_then(|r| r.candidate).map(|c| {
                    GameEqLayer::learned(relay_audio::learn::derive::guard_curve(&c), None)
                }),
            };
            let layer = offered
                .as_ref()
                .or(profile.audio.game_eq.as_ref())
                .context("there is no game EQ to export yet")?;
            let f =
                GameEqFile::export(&exe, &profile.name, layer, profile.audio.game_eq_goal, note);
            let text = f.to_json();
            // Round-trip through our own validator: never write a file we
            // would refuse to read.
            GameEqFile::parse(&text)?;
            let stem = safe_name(exe.trim_end_matches(".exe").trim_end_matches(".EXE"));
            let path = export_dir(paths).join(format!("{stem}-game-eq.json"));
            crate::profiles::write_atomic(&path, text.as_bytes())?;
            Ok(ActionResult {
                export: Some(GameEqExport { text, path: path.display().to_string() }),
                ..Default::default()
            })
        }
    }
}

/// The exe's file version from its version resource, read from the file on
/// disk (`GetFileVersionInfoW` on the path; no handle to the process).
#[cfg(windows)]
pub fn exe_version(path: &str) -> Option<String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };
    if path.is_empty() {
        return None;
    }
    let wide = HSTRING::from(path);
    // SAFETY: plain Win32 calls on a NUL-terminated path we own and a buffer
    // sized by the first call; the returned pointer is into that buffer and
    // only read while it is alive.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 || size > 1 << 20 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, buf.as_mut_ptr().cast()).ok()?;
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let root = HSTRING::from("\\");
        if !VerQueryValueW(buf.as_ptr().cast(), PCWSTR(root.as_ptr()), &mut ptr, &mut len).as_bool()
            || ptr.is_null()
            || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = &*(ptr as *const VS_FIXEDFILEINFO);
        if info.dwSignature != 0xFEEF_04BD {
            return None;
        }
        Some(format!(
            "{}.{}.{}.{}",
            info.dwFileVersionMS >> 16,
            info.dwFileVersionMS & 0xffff,
            info.dwFileVersionLS >> 16,
            info.dwFileVersionLS & 0xffff
        ))
    }
}

#[cfg(not(windows))]
pub fn exe_version(_path: &str) -> Option<String> {
    None
}

/// After the game loses focus the learner pauses (learns nothing) for this
/// long before it is stopped, so Alt-Tab and back resumes it instead of
/// starting a new helper. The profile itself still restores on blur at once.
pub const LEARNER_BLUR_GRACE: Duration = Duration::from_secs(15);

/// What to do with the learner helper on a focus change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearnerStep {
    /// Nothing runs and nothing should.
    Nothing,
    /// Start one.
    Start,
    /// The right one is listening already.
    Keep,
    /// Pause it (focus left); the grace period starts.
    Pause,
    /// The same game came back within the grace period.
    Resume,
    /// A different game or process: stop the old one, start a new one.
    Restart,
    /// The game is still in front but its profile no longer learns (set to
    /// Draft, deleted, disabled, learning turned off): stop now, no grace.
    Stop,
}

/// The learner state machine. `current` is the running helper (profile, pid,
/// paused); `want` the one the focused window calls for, if any;
/// `focused_pid` the process in front. The blur grace is for focus loss
/// only: with the game still in front and nothing wanted, it stops at once.
pub fn learner_step(
    current: Option<(Uuid, u32, bool)>,
    want: Option<(Uuid, u32)>,
    focused_pid: u32,
) -> LearnerStep {
    match (current, want) {
        (Some((_, pid, _)), None) if pid != 0 && pid == focused_pid => LearnerStep::Stop,
        (None, None) => LearnerStep::Nothing,
        (None, Some(_)) => LearnerStep::Start,
        (Some((p, pid, paused)), Some((wp, wpid))) if p == wp && pid == wpid => {
            if paused {
                LearnerStep::Resume
            } else {
                LearnerStep::Keep
            }
        }
        (Some(_), Some(_)) => LearnerStep::Restart,
        // Out of focus: pause once; while paused the grace runs on the tick.
        (Some((_, _, true)), None) => LearnerStep::Keep,
        (Some((_, _, false)), None) => LearnerStep::Pause,
    }
}

/// The grace period of a paused learner has run out.
pub fn grace_expired(paused_since: Option<Instant>, now: Instant) -> bool {
    paused_since.is_some_and(|t| now.saturating_duration_since(t) >= LEARNER_BLUR_GRACE)
}

/// The running learner helper for one focused game.
pub struct Learner {
    child: Child,
    stdin: Option<ChildStdin>,
    pub profile: Uuid,
    pub pid: u32,
    fresh: Arc<AtomicBool>,
    /// Set while paused for a blur; the tick stops it after the grace.
    pub paused_since: Option<Instant>,
}

impl Learner {
    /// Spawn `relay-share learn` for the focused game.
    pub fn spawn(paths: &Paths, profile: &Profile, fg: &Foreground) -> Result<Self> {
        let bin = crate::share::share_binary()?;
        let version = exe_version(&fg.image);
        let record = record_file(paths, &profile.game.exe);
        let goal = goal_of(profile);
        let mut cmd = Command::new(&bin);
        cmd.arg("learn")
            .arg("--pid")
            .arg(fg.pid.to_string())
            .arg("--exe")
            .arg(&profile.game.exe)
            .arg("--record")
            .arg(&record)
            .arg("--goal")
            .arg(serde_json::to_string(&goal)?.trim_matches('"'));
        if let Some(v) = &version {
            cmd.arg("--version").arg(v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        let stdin = child.stdin.take();
        let fresh = Arc::new(AtomicBool::new(false));
        if let Some(out) = child.stdout.take() {
            let flag = fresh.clone();
            std::thread::Builder::new()
                .name("relay-learn-reader".into())
                .stack_size(64 * 1024)
                .spawn(move || {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if line.contains("\"candidate\"") {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }
                })?;
        }
        tracing::info!(exe = %profile.game.exe, pid = fg.pid, ?version, "learning the game's sound");
        Ok(Self { child, stdin, profile: profile.id, pid: fg.pid, fresh, paused_since: None })
    }

    /// Learn nothing until [`Self::resume`]; the capture stays open.
    pub fn pause(&mut self) {
        if self.paused_since.is_none() {
            self.send(b"pause\n");
            self.paused_since = Some(Instant::now());
        }
    }

    pub fn resume(&mut self) {
        if self.paused_since.take().is_some() {
            self.send(b"resume\n");
        }
    }

    fn send(&mut self, line: &[u8]) {
        if let Some(s) = self.stdin.as_mut() {
            let _ = s.write_all(line);
            let _ = s.flush();
        }
    }

    /// A new curve was offered since the last call.
    pub fn take_fresh(&self) -> bool {
        self.fresh.swap(false, Ordering::Relaxed)
    }

    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask the helper to save and exit; wait up to [`STOP_TIMEOUT`], then kill
    /// it (our own child, by handle — never by image name).
    pub fn stop_blocking(mut self) {
        self.ask_stop();
        let until = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < until {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// The same, without holding the caller up.
    pub fn stop(self) {
        let _ = std::thread::Builder::new()
            .name("relay-learn-stop".into())
            .stack_size(64 * 1024)
            .spawn(move || self.stop_blocking());
    }

    fn ask_stop(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.write_all(b"stop\n");
            let _ = s.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GameMatch;
    use relay_audio::learn::Stats;

    fn paths() -> (Paths, PathBuf) {
        let dir = std::env::temp_dir().join(format!("relay-game-eq-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Paths::at(dir.clone()), dir)
    }

    fn profile() -> Profile {
        let mut p = Profile::new("Some Game", GameMatch::exe("Game.exe"));
        p.audio.bands.push(crate::types::EqBand { freq_hz: 1000.0, gain_db: 1.0, q: 1.0 });
        p
    }

    /// A record with a candidate, as the helper would leave it.
    fn ready_record(exe: &str, curve: Vec<(f32, f32)>) -> LearnRecord {
        let mut r = LearnRecord::new(exe, Some("1.0.0.0"));
        r.absorb(&Stats::default(), &Thresholds::default());
        r.candidate = Some(curve);
        r.candidate_gains = Some(vec![0.0; relay_audio::learn::NBANDS]);
        r
    }

    /// A test curve, already through the safety guard (as every stored
    /// curve is).
    fn curve(db: f32) -> Vec<(f32, f32)> {
        relay_audio::learn::derive::guard_curve(&[
            (20.0, -db),
            (100.0, -db),
            (1000.0, 0.0),
            (3150.0, db),
            (16000.0, 0.0),
        ])
    }

    fn at(c: &[(f32, f32)], hz: f32) -> f32 {
        c.iter().find(|p| p.0 == hz).unwrap().1
    }

    #[test]
    fn goal_first_then_learning_then_apply() {
        let (paths, dir) = paths();
        let mut p = profile();
        let st = status(&paths, &p, false);
        assert!(st.learning_on && st.needs_goal, "{st:?}");
        assert_eq!(st.state, LearnStatus::Learning);
        apply_action(&paths, &mut p, &GameEqAction::SetGoal { goal: Goal::Awareness }).unwrap();
        assert!(p.audio.learning_active());
        save_record_at(&record_file(&paths, "game.exe"), &ready_record("game.exe", curve(3.0)))
            .unwrap();
        let st = status(&paths, &p, true);
        assert_eq!(st.state, LearnStatus::Ready);
        assert_eq!(st.offer.as_deref(), Some(&curve(3.0)[..]));
        apply_action(&paths, &mut p, &GameEqAction::Apply).unwrap();
        assert_eq!(p.audio.game_eq.as_ref().unwrap().source, LayerSource::Learned);
        assert_eq!(status(&paths, &p, true).state, LearnStatus::Applied);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_import_applies_at_once_with_learning_off_and_can_be_fine_tuned() {
        let (paths, dir) = paths();
        let mut p = profile();
        let layer = GameEqLayer::learned(curve(4.0), Some("2.0".into()));
        let text =
            GameEqFile::export("game.exe", "Some Game", &layer, None, "from a friend").to_json();
        apply_action(&paths, &mut p, &GameEqAction::Import { text }).unwrap();
        let st = status(&paths, &p, false);
        assert_eq!(st.state, LearnStatus::Applied);
        assert_eq!(st.source, Some(LayerSource::Imported));
        assert!(!st.learning_on, "learning is off for an imported game by default");
        assert_eq!(st.applied.as_deref(), Some(&curve(4.0)[..]));

        // "Keep learning to fine-tune": the offer blends towards the local result.
        apply_action(&paths, &mut p, &GameEqAction::SetLearning { enabled: true }).unwrap();
        apply_action(&paths, &mut p, &GameEqAction::SetGoal { goal: Goal::Awareness }).unwrap();
        save_record_at(&record_file(&paths, "game.exe"), &ready_record("game.exe", curve(0.0)))
            .unwrap();
        let st = status(&paths, &p, true);
        assert_eq!(st.state, LearnStatus::Ready);
        let offer = st.offer.unwrap();
        let (from, to) = (at(&curve(4.0), 3150.0), at(&curve(0.0), 3150.0));
        let mid = at(&offer, 3150.0);
        assert!(mid < from && mid > to, "between the import and the local result: {offer:?}");
        apply_action(&paths, &mut p, &GameEqAction::Apply).unwrap();
        let l = p.audio.game_eq.as_ref().unwrap();
        assert_eq!(l.source, LayerSource::Tuned);
        assert_eq!(l.base.as_deref(), Some(&curve(4.0)[..]), "still anchored to the import");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_file_for_another_game_is_refused() {
        let (paths, dir) = paths();
        let mut p = profile();
        let layer = GameEqLayer::learned(curve(1.0), None);
        let text = GameEqFile::export("other.exe", "", &layer, None, "").to_json();
        let err = apply_action(&paths, &mut p, &GameEqAction::Import { text }).unwrap_err();
        assert!(err.to_string().contains("other.exe"), "{err}");
        assert!(p.audio.game_eq.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn export_writes_the_applied_layer_learned_or_imported() {
        let (paths, dir) = paths();
        let mut p = profile();
        assert!(
            apply_action(&paths, &mut p, &GameEqAction::Export { note: String::new() }).is_err()
        );
        p.audio.game_eq = Some(GameEqLayer::learned(curve(2.0), None));
        let out = apply_action(&paths, &mut p, &GameEqAction::Export { note: "mine".into() })
            .unwrap()
            .export
            .unwrap();
        let back = GameEqFile::parse(&out.text).unwrap();
        assert_eq!(back.curve.as_deref(), Some(&curve(2.0)[..]));
        assert_eq!(std::fs::read_to_string(&out.path).unwrap(), out.text);
        // No hardware or personal fields in it.
        for word in ["headset", "correction", "endpoint", "user", "path"] {
            assert!(!out.text.to_lowercase().contains(word), "{word} in {}", out.text);
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn changing_goal_follows_at_once_for_a_learned_layer() {
        let (paths, dir) = paths();
        let mut p = profile();
        p.audio.game_eq_goal = Some(Goal::Awareness);
        // A real learned record, from synthetic statistics.
        let mut rec = LearnRecord::new("game.exe", None);
        let th = Thresholds {
            min_cues: 1,
            min_maskers: 0,
            min_active_secs: 0,
            checkpoint_secs: 1,
            ..Default::default()
        };
        let mut st = Stats { active_frames: 10_000, ..Stats::default() };
        st.events[0] = 50;
        st.class_frames[0] = 500;
        st.class_frames[8] = 9_000;
        let n = relay_audio::learn::NBANDS * relay_audio::learn::HIST_BINS;
        for b in 0..relay_audio::learn::NBANDS {
            // Footsteps at -60 rel. in the highs; ambience at -55 everywhere.
            if b >= 16 {
                st.class_hist[b * relay_audio::learn::HIST_BINS + 20] = 50;
            }
            st.class_hist[8 * n + b * relay_audio::learn::HIST_BINS + 22] = 9_000;
            st.frame_hist[b * relay_audio::learn::HIST_BINS + 22] = 9_500;
        }
        for _ in 0..4 {
            rec.absorb(&st, &th);
            rec.checkpoint(&th, &Limits::default());
        }
        assert!(rec.candidate.is_some(), "{rec:?}");
        save_record_at(&record_file(&paths, "game.exe"), &rec).unwrap();
        apply_action(&paths, &mut p, &GameEqAction::Apply).unwrap();
        let before = p.audio.game_eq.clone().unwrap().curve;
        apply_action(&paths, &mut p, &GameEqAction::SetGoal { goal: Goal::Immersion }).unwrap();
        let after = p.audio.game_eq.clone().unwrap().curve;
        assert_ne!(before, after, "re-derived and applied without relearning");
        assert_eq!(status(&paths, &p, false).state, LearnStatus::Applied);
        let saved = load_record(&paths, "game.exe").unwrap();
        assert_eq!(saved.goal, Goal::Immersion);
        assert_eq!(saved.total_active_frames, rec.total_active_frames);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn relearn_keeps_the_applied_layer_and_reset_removes_everything() {
        let (paths, dir) = paths();
        let mut p = profile();
        p.audio.game_eq = Some(GameEqLayer::learned(curve(2.0), None));
        let file = record_file(&paths, "game.exe");
        save_record_at(&file, &ready_record("game.exe", curve(2.0))).unwrap();
        apply_action(&paths, &mut p, &GameEqAction::Relearn).unwrap();
        assert!(p.audio.game_eq.is_some());
        assert!(load_record(&paths, "game.exe").unwrap().candidate.is_none());
        apply_action(&paths, &mut p, &GameEqAction::Reset).unwrap();
        assert!(p.audio.game_eq.is_none());
        assert!(!file.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_new_game_version_shows_needs_relearn_while_the_old_layer_applies() {
        let (paths, dir) = paths();
        let mut p = profile();
        p.audio.game_eq_goal = Some(Goal::Awareness);
        p.audio.game_eq = Some(GameEqLayer::learned(curve(2.0), Some("1.0.0.0".into())));
        let mut rec = ready_record("game.exe", curve(2.0));
        assert!(rec.begin_session(Some("1.1.0.0")));
        save_record_at(&record_file(&paths, "game.exe"), &rec).unwrap();
        let st = status(&paths, &p, true);
        assert_eq!(st.state, LearnStatus::NeedsRelearn);
        assert_eq!(st.applied.as_deref(), Some(&curve(2.0)[..]), "the old layer keeps applying");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_hostile_import_is_made_safe_and_says_so() {
        let (paths, dir) = paths();
        let mut p = profile();
        let text = r#"{"format":"relay-game-eq","schema":1,"game":{"exe":"Game.exe"},
            "curve":[[20,6],[80,6],[1000,-9],[16000,6]]}"#;
        let r = apply_action(&paths, &mut p, &GameEqAction::Import { text: text.into() }).unwrap();
        assert!(r.notice.is_some());
        let c = &p.audio.game_eq.as_ref().unwrap().curve;
        assert!(relay_audio::learn::derive::is_guarded(c), "{c:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_wild_record_on_disk_is_guarded_and_an_oversized_one_ignored() {
        let (paths, dir) = paths();
        let mut p = profile();
        let mut rec =
            ready_record("game.exe", vec![(20.0, 40.0), (1000.0, -40.0), (16000.0, 40.0)]);
        let file = record_file(&paths, "game.exe");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Written raw, as a hand edit would be.
        std::fs::write(&file, serde_json::to_vec(&rec).unwrap()).unwrap();
        apply_action(&paths, &mut p, &GameEqAction::Apply).unwrap();
        let c = &p.audio.game_eq.as_ref().unwrap().curve;
        assert!(relay_audio::learn::derive::is_guarded(c) && c.iter().all(|x| x.1.abs() <= 9.0));
        rec.exe = "game.exe".into();
        let mut big = serde_json::to_vec(&rec).unwrap();
        big.resize(MAX_RECORD_BYTES as usize + 10, b' ');
        std::fs::write(&file, big).unwrap();
        assert!(load_record(&paths, "game.exe").is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    /// A helper that is still stopping makes its last save after the user
    /// pressed Reset: Reset must wait for it, so the record stays gone.
    #[test]
    fn reset_waits_for_a_late_save_from_a_stopping_helper() {
        let (paths, dir) = paths();
        let file = record_file(&paths, "game.exe");
        let (tx, rx) = std::sync::mpsc::channel();
        let f2 = file.clone();
        let helper = std::thread::spawn(move || {
            let _l = lock_record(&f2, LOCK_TIMEOUT).unwrap();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            save_record_at(&f2, &ready_record("game.exe", vec![(20.0, 0.0), (16000.0, 0.0)]))
                .unwrap();
        });
        rx.recv().unwrap();
        let mut p = profile();
        apply_action(&paths, &mut p, &GameEqAction::Reset).unwrap();
        helper.join().unwrap();
        assert!(!file.exists(), "the late save landed before Reset, not after");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn export_before_apply_exports_the_offer() {
        let (paths, dir) = paths();
        let mut p = profile();
        save_record_at(&record_file(&paths, "game.exe"), &ready_record("game.exe", curve(2.0)))
            .unwrap();
        let out = apply_action(&paths, &mut p, &GameEqAction::Export { note: String::new() })
            .unwrap()
            .export
            .unwrap();
        let back = GameEqFile::parse(&out.text).unwrap();
        assert_eq!(back.curve.as_deref(), Some(&curve(2.0)[..]));
        assert!(p.audio.game_eq.is_none(), "exporting does not apply");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn status_says_what_was_left_out_and_what_the_gate_waits_on() {
        let (paths, dir) = paths();
        let p = profile();
        let mut rec = ready_record("game.exe", curve(1.0));
        let mut st = Stats { overlay_voice_frames: 3080, chat_frames: 12, ..Stats::default() };
        st.input_idle_frames = 40;
        rec.absorb(&st, &Thresholds::default());
        rec.checkpoints = vec![vec![0.0; 3], vec![0.1; 3], vec![0.9; 3]];
        save_record_at(&record_file(&paths, "game.exe"), &rec).unwrap();
        let s = status(&paths, &p, false);
        assert_eq!(s.excluded.overlay_voice, 3080);
        assert_eq!(s.excluded.player_chat, 12);
        assert_eq!(s.excluded.cutscene_or_idle, 40);
        let c = s.convergence.unwrap();
        assert_eq!((c.agreeing, c.needed), (1, 2));
        assert!((c.max_delta_db - 0.8).abs() < 1e-6);
        assert_eq!(s.eta_secs, Some(0), "a curve is ready");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn status_names_the_sound_classes() {
        let (paths, dir) = paths();
        let p = profile();
        save_record_at(&record_file(&paths, "game.exe"), &ready_record("game.exe", curve(1.0)))
            .unwrap();
        let st = status(&paths, &p, false);
        let names: Vec<String> =
            st.classes.iter().map(|c| serde_json::to_string(&c.class).unwrap()).collect();
        assert_eq!(names.len(), relay_audio::learn::NCLASSES);
        assert_eq!(names[0], "\"footsteps\"");
        assert_eq!(names[8], "\"ambience\"");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn alt_tab_pauses_and_resumes_without_a_restart() {
        use LearnerStep::*;
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(learner_step(None, None, 99), Nothing);
        assert_eq!(learner_step(None, Some((a, 10)), 10), Start);
        assert_eq!(learner_step(Some((a, 10, false)), Some((a, 10)), 10), Keep);
        // Alt-Tab away: pause, and stay paused (no restart) while out.
        assert_eq!(learner_step(Some((a, 10, false)), None, 99), Pause);
        assert_eq!(learner_step(Some((a, 10, true)), None, 99), Keep);
        // Back within the grace: resume the same helper.
        assert_eq!(learner_step(Some((a, 10, true)), Some((a, 10)), 10), Resume);
        // Another game, or the game relaunched (new pid): a new helper.
        assert_eq!(learner_step(Some((a, 10, true)), Some((b, 11)), 11), Restart);
        assert_eq!(learner_step(Some((a, 10, false)), Some((a, 12)), 12), Restart);
        // r54: the game is still in front but its profile went to Draft, was
        // deleted or disabled, or learning was turned off: stop, no grace.
        assert_eq!(learner_step(Some((a, 10, false)), None, 10), Stop);
        assert_eq!(learner_step(Some((a, 10, true)), None, 10), Stop);
        // The grace runs out only after LEARNER_BLUR_GRACE.
        let t = Instant::now();
        assert!(!grace_expired(None, t + LEARNER_BLUR_GRACE * 2));
        assert!(!grace_expired(Some(t), t + LEARNER_BLUR_GRACE - Duration::from_millis(1)));
        assert!(grace_expired(Some(t), t + LEARNER_BLUR_GRACE));
        assert_eq!(LEARNER_BLUR_GRACE, Duration::from_secs(15));
    }

    #[test]
    fn record_names_are_file_safe() {
        assert_eq!(safe_name("Some Game.exe"), "some_game.exe");
        assert_eq!(safe_name("..\\x.exe"), ".._x.exe");
    }
}
