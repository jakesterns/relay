//! S47: learned game display — per game × monitor.
//!
//! The core's half of "Learn this game's look": the on-disk store
//! (`learned-display.json`), the sampler child (`relay-share look`, spawned
//! only while a learning-enabled game has focus), the overlay that folds a
//! learned look into a profile's display settings before the normal
//! backup-then-apply path, and the versioned import/export file.
//!
//! The maths lives in `relay_display::learn` (pure). Nothing here sees a
//! pixel: the sampler prints statistics only. The always-on core links no
//! capture code; it only reads lines from a child it starts and kills.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{bail, Context, Result};
use relay_display::learn::{
    realize, Adjustments, FrameReport, Learner, LookTargets, PanelCaps, PanelKind, Phase, Readiness,
};
use serde::{Deserialize, Serialize};

use crate::types::{DisplaySettings, MonitorId};

/// The sampler's rate. One frame a second is plenty for a look that takes
/// minutes to converge, and keeps the cost unmeasurable.
pub const SAMPLE_FPS: u32 = 1;

/// Exact wording the owner chose for the tournament notice (S47). A notice
/// only: no confirmation, no responsibility taken.
pub const TOURNAMENT_NOTICE: &str = "Relay's visual enhancements may not be allowed in some \
tournaments or professional environments. Check with your tournament host or rules.";

/// The privacy line shown beside the control.
pub const PRIVACY_NOTICE: &str = "Frames are analysed in memory at low resolution while the \
game has focus. No frames are recorded or saved, and nothing leaves this PC.";

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// One monitor's learning for one game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MonitorRecord {
    pub learner: Learner,
    /// The output was in HDR mode last time: the sampler skipped it.
    #[serde(default)]
    pub hdr_skipped: bool,
}

/// An imported look and its creator's note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedLook {
    pub look: LookTargets,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GameRecord {
    /// "Learn this game's look" is on.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub monitors: BTreeMap<String, MonitorRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<ImportedLook>,
}

impl GameRecord {
    /// The look in use on `monitor`, panel-neutral:
    /// 1. this monitor's applied learned look;
    /// 2. else the applied learned look with the most evidence on any other
    ///    monitor — the look belongs to the game, so a monitor switch only
    ///    re-fits it to the new panel ([`realize`]), with no relearn;
    /// 3. else an imported look.
    ///
    /// `None` = the profile as written.
    pub fn effective(&self, monitor: &MonitorId) -> Option<LookTargets> {
        let applied =
            |m: &MonitorRecord| m.learner.use_learned.then_some(m.learner.applied).flatten();
        if let Some(look) = self.monitors.get(&monitor.0).and_then(applied) {
            return Some(look);
        }
        let other = self
            .monitors
            .values()
            .filter_map(|m| applied(m).map(|l| (l, m.learner.agg.frames)))
            .max_by_key(|(_, frames)| *frames)
            .map(|(l, _)| l);
        other.or(self.imported.as_ref().map(|i| i.look))
    }

    /// The record for `monitor`, created on first use. A new record for a
    /// game whose look is already in use (imported, or learned on another
    /// monitor) starts *from* that look, so fine-tuning follows the usual
    /// freeze rule instead of starting from nothing.
    pub fn monitor_mut(&mut self, monitor: &MonitorId) -> &mut MonitorRecord {
        let seed =
            (!self.monitors.contains_key(&monitor.0)).then(|| self.effective(monitor)).flatten();
        let rec = self.monitors.entry(monitor.0.clone()).or_default();
        if let Some(look) = seed {
            rec.learner.use_learned = true;
            rec.learner.applied = Some(look);
        }
        rec
    }

    /// What the user sees for this game on `monitor`.
    pub fn status(&self, monitor: Option<&MonitorId>) -> LookStatus {
        let m = monitor.and_then(|id| self.monitors.get(&id.0));
        if m.is_some_and(|m| m.hdr_skipped) {
            return LookStatus::HdrSkipped;
        }
        let in_use = monitor
            .and_then(|id| self.effective(id))
            .or_else(|| self.imported.as_ref().map(|i| i.look));
        if let Some(look) = in_use {
            let from_import = self.imported.as_ref().is_some_and(|i| i.look == look);
            return if from_import { LookStatus::AppliedImported } else { LookStatus::Applied };
        }
        match m {
            Some(m) if m.learner.phase() == Phase::Converged => LookStatus::Ready,
            _ if self.enabled => LookStatus::Learning,
            _ => LookStatus::Off,
        }
    }
}

/// One word for the UI per game × monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookStatus {
    Off,
    Learning,
    /// Converged; waiting for the user's Apply.
    Ready,
    Applied,
    /// An imported file's look is in use.
    AppliedImported,
    /// The monitor was in HDR mode; nothing was learned.
    HdrSkipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreFile {
    pub version: u32,
    #[serde(default)]
    pub games: BTreeMap<String, GameRecord>,
}

pub const STORE_VERSION: u32 = 1;

/// `learned-display.json`. Keys are lower-case exe names.
pub struct LearnStore {
    path: PathBuf,
    pub file: StoreFile,
}

pub fn key(exe: &str) -> String {
    exe.to_ascii_lowercase()
}

impl LearnStore {
    pub fn load(path: PathBuf) -> Self {
        let file = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StoreFile>(&b).ok())
            .filter(|f| f.version == STORE_VERSION)
            .unwrap_or(StoreFile { version: STORE_VERSION, games: BTreeMap::new() });
        Self { path, file }
    }

    pub fn save(&self) -> Result<()> {
        crate::profiles::write_atomic(&self.path, &serde_json::to_vec_pretty(&self.file)?)
    }

    pub fn game(&self, exe: &str) -> Option<&GameRecord> {
        self.file.games.get(&key(exe))
    }

    pub fn game_mut(&mut self, exe: &str) -> &mut GameRecord {
        self.file.games.entry(key(exe)).or_default()
    }

    pub fn is_enabled(&self, exe: &str) -> bool {
        self.game(exe).is_some_and(|g| g.enabled)
    }
}

// ---------------------------------------------------------------------------
// Panel + overlay
// ---------------------------------------------------------------------------

/// What the learner may use on one monitor. The black equalizer stays off
/// until a quirks row is verified *and* carries a level range; no row does
/// today, so the learner never writes DDC/CI on any shipped model.
pub fn panel_caps(panel_label: &str) -> PanelCaps {
    PanelCaps { kind: PanelKind::from_label(panel_label), black_equalizer_max: None }
}

/// The panel type to use: what the user wrote in the library, else a guess
/// from the model name / EDID id (`true` = guessed, so the UI asks the user
/// to confirm), else Unknown.
pub fn panel_kind(label: &str, name: &str, id: &str) -> (PanelKind, bool) {
    match PanelKind::from_label(label) {
        PanelKind::Unknown => match PanelKind::guess_from_model(name, id) {
            Some(k) => (k, true),
            None => (PanelKind::Unknown, false),
        },
        k => (k, false),
    }
}

pub fn caps_for(label: &str, name: &str, id: &str) -> PanelCaps {
    PanelCaps { kind: panel_kind(label, name, id).0, black_equalizer_max: None }
}

/// Fold learned adjustments into a profile's display settings. The profile
/// stays the user's taste; the learned look is a correction on top, and the
/// result goes through the ordinary capture → apply → restore path.
pub fn overlay(display: &mut DisplaySettings, adj: &Adjustments) {
    let gpu = &mut display.gpu;
    gpu.gamma = ((gpu.gamma * adj.gamma) * 100.0).round() / 100.0;
    gpu.gamma = gpu.gamma.clamp(0.5, 2.0);
    gpu.shadow_lift = gpu.shadow_lift.max(adj.shadow_lift);
    gpu.vibrance = (gpu.vibrance + adj.vibrance - 50).clamp(0, 100);
    // At most one DDC/CI write may ride on the restore path (one write is
    // ~100 ms of the 200 ms budget, measured in M2). A learned black
    // equalizer is used only when the profile itself writes no DDC code.
    let m = &display.monitor;
    let profile_ddc = m.brightness.is_some()
        || m.contrast.is_some()
        || m.black_equalizer.is_some()
        || m.response.is_some()
        || m.sharpness.is_some();
    if !profile_ddc {
        display.monitor.black_equalizer = adj.black_equalizer;
    }
}

/// Game build fingerprint from the exe file on disk: size and modified time.
/// Read with ordinary file metadata — nothing is opened in the game process.
pub fn build_fingerprint(image: &Path) -> String {
    match std::fs::metadata(image) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}-{}", m.len(), mtime)
        }
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// View (IPC)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorLearnView {
    pub monitor: MonitorId,
    pub monitor_name: String,
    pub panel: PanelKind,
    /// The panel type was guessed from the model, not set by the user.
    #[serde(default)]
    pub panel_guessed: bool,
    pub phase: Phase,
    pub readiness: Readiness,
    #[serde(default)]
    pub converged: Option<LookTargets>,
    #[serde(default)]
    pub applied: Option<LookTargets>,
    pub use_learned: bool,
    pub hdr_skipped: bool,
    pub status: LookStatus,
    /// What the effective look does on this panel, for the UI.
    #[serde(default)]
    pub adjustments: Option<Adjustments>,
    pub excluded: u64,
    /// Why frames were skipped, by kind, so a tester can see the cause.
    #[serde(default)]
    pub excluded_by: relay_display::learn::converge::Excluded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnView {
    pub exe: String,
    /// Learning (or, with an import, fine-tuning) is on for this game.
    pub enabled: bool,
    /// Game-level state, for the card header.
    pub status: LookStatus,
    /// The sampler is running for this game right now.
    pub sampling: bool,
    pub monitors: Vec<MonitorLearnView>,
    #[serde(default)]
    pub imported: Option<ImportedLook>,
    pub privacy: String,
    pub tournament: String,
}

/// `names` maps monitor id → (display name, panel label).
pub fn view(
    store: &LearnStore,
    exe: &str,
    sampling: bool,
    names: &dyn Fn(&MonitorId) -> (String, String),
) -> LearnView {
    let rec = store.game(exe).cloned().unwrap_or_default();
    let monitors = rec
        .monitors
        .iter()
        .map(|(id, m)| {
            let id = MonitorId(id.clone());
            let (name, panel) = names(&id);
            let (kind, guessed) = panel_kind(&panel, &name, &id.0);
            let caps = PanelCaps { kind, ..panel_caps(&panel) };
            let l = &m.learner;
            let ex = l.excluded;
            MonitorLearnView {
                monitor: id.clone(),
                monitor_name: name,
                panel: caps.kind,
                panel_guessed: guessed,
                phase: l.phase(),
                readiness: l.readiness(),
                converged: l.converged,
                applied: l.applied,
                use_learned: l.use_learned,
                hdr_skipped: m.hdr_skipped,
                status: rec.status(Some(&id)),
                adjustments: rec.effective(&id).map(|look| realize(&look, &caps)),
                excluded: ex.total(),
                excluded_by: ex,
            }
        })
        .collect();
    LearnView {
        exe: key(exe),
        enabled: rec.enabled,
        status: rec.status(None),
        sampling,
        monitors,
        imported: rec.imported.clone(),
        privacy: PRIVACY_NOTICE.into(),
        tournament: TOURNAMENT_NOTICE.into(),
    }
}

// ---------------------------------------------------------------------------
// Import / export
// ---------------------------------------------------------------------------

pub const FILE_FORMAT: &str = "relay-game-display";
pub const FILE_VERSION: u32 = 1;
/// Larger than any honest file by two orders of magnitude.
pub const FILE_MAX_BYTES: usize = 16 * 1024;
pub const NOTE_MAX_CHARS: usize = 500;
pub const EXE_MAX_CHARS: usize = 64;

/// Game identity in an exported file. The exe *name* only — never a path,
/// which would carry a user name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileGame {
    pub exe: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// How much evidence stood behind the look. Counts only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FileEvidence {
    pub frames: u64,
    pub scenes: u32,
}

/// The versioned "game display" file. Unknown fields are refused, so
/// nothing (a monitor serial, a path) can ride along unnoticed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameDisplayFile {
    pub format: String,
    pub version: u32,
    pub game: FileGame,
    pub look: FileLook,
    #[serde(default)]
    pub evidence: FileEvidence,
    #[serde(default)]
    pub note: String,
}

/// The look, panel-neutral, 0..=1 per axis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLook {
    pub shadow: f32,
    pub saturation: f32,
    pub highlight: f32,
}

impl From<LookTargets> for FileLook {
    fn from(l: LookTargets) -> Self {
        Self { shadow: l.shadow, saturation: l.saturation, highlight: l.highlight }
    }
}

impl From<FileLook> for LookTargets {
    fn from(l: FileLook) -> Self {
        Self { shadow: l.shadow, saturation: l.saturation, highlight: l.highlight }
    }
}

fn clean_text(s: &str, max: usize, what: &str) -> Result<String> {
    if s.chars().count() > max {
        bail!("{what} is longer than {max} characters");
    }
    if s.chars().any(|c| c.is_control() && c != '\n') {
        bail!("{what} contains control characters");
    }
    Ok(s.trim().to_string())
}

pub fn valid_exe(exe: &str) -> Result<()> {
    let ok = !exe.is_empty()
        && exe.chars().count() <= EXE_MAX_CHARS
        && exe.to_ascii_lowercase().ends_with(".exe")
        && !exe.contains(['\\', '/', ':'])
        && !exe.chars().any(char::is_control);
    if !ok {
        bail!("the game must be an exe file name like game.exe, with no folder");
    }
    Ok(())
}

impl GameDisplayFile {
    pub fn new(
        exe: &str,
        name: Option<String>,
        look: LookTargets,
        ev: FileEvidence,
        note: &str,
    ) -> Self {
        Self {
            format: FILE_FORMAT.into(),
            version: FILE_VERSION,
            game: FileGame { exe: key(exe), name },
            look: look.into(),
            evidence: ev,
            note: note.trim().to_string(),
        }
    }

    /// Parse and validate. Every rejection is a sentence for the UI.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > FILE_MAX_BYTES {
            bail!("this file is too large to be a Relay game display file");
        }
        let f: GameDisplayFile =
            serde_json::from_str(text).context("this is not a Relay game display file")?;
        if f.format != FILE_FORMAT {
            bail!("this is not a Relay game display file");
        }
        if f.version != FILE_VERSION {
            bail!("this file is version {}; this Relay reads version {FILE_VERSION}", f.version);
        }
        valid_exe(&f.game.exe)?;
        if let Some(n) = &f.game.name {
            clean_text(n, 100, "the game name")?;
        }
        clean_text(&f.note, NOTE_MAX_CHARS, "the note")?;
        if !LookTargets::from(f.look).is_valid() {
            bail!("the look values must be numbers from 0 to 1");
        }
        Ok(f)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// Export the current game layer: the look in use (applied learned, with
/// the most evidence; else the imported one), or failing that the settled
/// result waiting for Apply. Panel-neutral, so it fits any monitor.
pub fn export(store: &LearnStore, exe: &str, name: Option<String>, note: &str) -> Result<String> {
    let note = clean_text(note, NOTE_MAX_CHARS, "the note")?;
    let rec = store.game(exe).context("nothing has been learned for this game yet")?;
    let evidence = |l: &Learner| FileEvidence { frames: l.agg.frames, scenes: l.scenes() as u32 };
    let applied = rec
        .monitors
        .values()
        .filter(|m| m.learner.use_learned)
        .filter_map(|m| m.learner.applied.map(|a| (a, &m.learner)))
        .max_by_key(|(_, l)| l.agg.frames)
        .map(|(a, l)| (a, evidence(l)));
    let imported = rec.imported.as_ref().map(|i| (i.look, FileEvidence::default()));
    let settled = rec
        .monitors
        .values()
        .filter_map(|m| m.learner.converged.map(|c| (c, &m.learner)))
        .max_by_key(|(_, l)| l.agg.frames)
        .map(|(c, l)| (c, evidence(l)));
    let Some((look, ev)) = applied.or(imported).or(settled) else {
        bail!("this game's look has not settled yet; keep playing and try again")
    };
    GameDisplayFile::new(exe, name, look, ev, &note).to_json()
}

/// Import into `exe`'s record. The file must be for the same game.
///
/// The imported look applies at once ("applied (imported)") and learning is
/// switched off for the game; the user may turn on "Keep learning to
/// fine-tune for my monitor", which learns under the same convergence and
/// freeze rules, starting from the imported look. Any learned look applied
/// before is set aside so the import is what is in use.
pub fn import(store: &mut LearnStore, exe: &str, text: &str) -> Result<()> {
    let f = GameDisplayFile::parse(text)?;
    if !f.game.exe.eq_ignore_ascii_case(exe) {
        bail!("this file is for {}, not {}", f.game.exe, key(exe));
    }
    let look: LookTargets = f.look.into();
    let g = store.game_mut(exe);
    g.imported = Some(ImportedLook { look, note: f.note });
    g.enabled = false;
    for m in g.monitors.values_mut() {
        m.learner.relearn();
        m.learner.use_learned = true;
        m.learner.applied = Some(look);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sampler child
// ---------------------------------------------------------------------------

/// One decoded line from `relay-share look`.
#[derive(Debug, Clone, PartialEq)]
pub enum SamplerLine {
    Frame(Box<FrameReport>),
    Hdr,
    Error(String),
}

pub fn decode_line(line: &str) -> Option<SamplerLine> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("event")?.as_str()? {
        "look" => serde_json::from_value(v.get("report")?.clone())
            .ok()
            .map(|r| SamplerLine::Frame(Box::new(r))),
        "look_hdr" => Some(SamplerLine::Hdr),
        "error" => Some(SamplerLine::Error(
            v.get("message").and_then(|m| m.as_str()).unwrap_or("sampler error").to_string(),
        )),
        _ => None,
    }
}

/// Everything the sampler has sent, read *after* checking whether it exited,
/// so a fast exit (HDR: one line, then gone) never loses its last line.
/// Returns the lines and whether the sampler is finished.
pub fn drain(rx: &Receiver<SamplerLine>, exited: bool) -> (Vec<SamplerLine>, bool) {
    let mut lines: Vec<SamplerLine> = rx.try_iter().collect();
    let mut done = exited;
    if exited {
        // The reader thread may still be forwarding the final lines: wait
        // for it to hang up (stdout closed), bounded.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(l) => lines.push(l),
                Err(_) => break,
            }
        }
    } else if matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)) {
        done = true;
    }
    (lines, done)
}

/// `relay-share look --hmonitor N --fps F`.
pub fn sampler_args(hmonitor: i64, fps: u32) -> Vec<String> {
    vec!["look".into(), "--hmonitor".into(), hmonitor.to_string(), "--fps".into(), fps.to_string()]
}

/// The running sampler. Dropping it stops and reaps the child (its own PID,
/// never by image name).
pub struct Sampler {
    child: Child,
    stdin: Option<ChildStdin>,
    pub exe: String,
    pub monitor: MonitorId,
    pub rx: Receiver<SamplerLine>,
    /// Set while the game is out of focus: the sampler is told to stop
    /// sampling, and is dropped once `LOOK_BLUR_GRACE` passes.
    pub paused_since: Option<std::time::Instant>,
}

/// Alt-Tab grace, the same as the audio learner's: a game out of focus for
/// less than this keeps its sampler (paused) and resumes without a restart.
pub const LOOK_BLUR_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

pub fn grace_expired(paused_since: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    paused_since.is_some_and(|t| now.saturating_duration_since(t) >= LOOK_BLUR_GRACE)
}

impl Sampler {
    fn send(&mut self, line: &str) {
        if let Some(s) = self.stdin.as_mut() {
            let _ = s.write_all(line.as_bytes());
            let _ = s.flush();
        }
    }

    /// Stop sampling (the game lost focus); keep the process.
    pub fn pause(&mut self) {
        if self.paused_since.is_none() {
            self.send("pause\n");
            self.paused_since = Some(std::time::Instant::now());
        }
    }

    pub fn resume(&mut self) {
        if self.paused_since.take().is_some() {
            self.send("resume\n");
        }
    }

    pub fn start(exe: &str, monitor: MonitorId, hmonitor: i64) -> Result<Self> {
        let bin = crate::share::share_binary()?;
        anyhow::ensure!(bin.exists(), "sampler not found at {}", bin.display());
        let mut cmd = Command::new(&bin);
        cmd.args(sampler_args(hmonitor, SAMPLE_FPS));
        cmd.env("RELAY_SPAWNED", "1");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().context("spawning the look sampler")?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().context("no sampler stdout")?;
        let (tx, rx): (Sender<SamplerLine>, Receiver<SamplerLine>) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("relay-look-reader".into()).stack_size(64 * 1024).spawn(
            move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some(ev) = decode_line(&line) {
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                }
            },
        )?;
        Ok(Self { child, stdin, exe: key(exe), monitor, rx, paused_since: None })
    }

    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.write_all(b"stop\n");
        }
        // Ours, by handle: never by image name (a parallel relay-share is
        // somebody else's share).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests;
