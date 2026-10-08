//! S48: "Learn from a video file" — the core's side.
//!
//! The user picks a local gameplay video (mp4 / mkv / mov / webm); the core
//! spawns `relay-share learn-file`, which decodes the file faster than real
//! time through Media Foundation and feeds the audio to the same S46
//! analyser (straight into the game's `game-eq\<exe>.json` record, like the
//! live learner) and sampled frames to the same S47 analyser (a learner it
//! prints back once at the end). The core folds that look into the game's
//! per-monitor records with [`relay_display::learn::Learner::merge`], so file
//! evidence and live evidence share one record, weighted by how much of each
//! there is, and live play refines a file-learned start.
//!
//! Local files only: a path must name an existing file on this PC (no URL,
//! no network share), and the file is opened for reading by Media Foundation
//! only — never copied, uploaded or changed. Downloading from YouTube or
//! other sites is not offered (their terms of service).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use relay_display::learn::Learner as LookLearner;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Paths;
use crate::types::{MonitorId, Profile};

/// The containers offered in the picker and accepted by the helper.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "webm"];
/// At most this many files are listed.
pub const MAX_LISTED: usize = 50;
/// How long a cancel waits for the helper to leave before killing it.
pub const CANCEL_TIMEOUT: Duration = Duration::from_secs(3);

/// Shown beside the picker.
pub const PRIVACY_NOTICE: &str = "Relay reads the file on this PC to learn the game's sound and \
look. It keeps statistics only; the file is never copied, uploaded or changed.";
/// Shown beside the picker: why there is no "paste a link".
pub const LOCAL_ONLY_NOTICE: &str = "Local files only. Relay does not download videos from \
YouTube or other sites; their terms do not allow it.";

/// A video Relay can learn from, for the picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoFile {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    /// Seconds since 1970, for "newest first" and the UI's date.
    pub modified_unix: u64,
    /// In Relay's recording folder (listed first).
    pub relay: bool,
}

fn has_video_extension(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(e)))
}

/// A path the user gave, checked: a local, existing, non-empty video file
/// with one of [`VIDEO_EXTENSIONS`]. URLs and network paths are refused —
/// Relay learns from files on this PC only. Every error is a sentence.
pub fn validate_video_path(raw: &str) -> Result<PathBuf> {
    let s = raw.trim().trim_matches('"');
    if s.is_empty() {
        bail!("choose a video file");
    }
    let lower = s.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("www.") || lower.contains("youtube") {
        bail!("Relay learns from video files on this PC only; it does not download from websites");
    }
    if s.starts_with("\\\\") || s.starts_with("//") {
        bail!("choose a file on this PC, not on a network share");
    }
    let p = PathBuf::from(s);
    if !p.is_absolute() {
        bail!("choose a file by its full path");
    }
    if !has_video_extension(&p) {
        bail!("Relay can learn from .mp4, .mkv, .mov and .webm videos");
    }
    let meta = std::fs::metadata(&p).with_context(|| format!("{} was not found", p.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", p.display());
    }
    if meta.len() == 0 {
        bail!("{} is empty", p.display());
    }
    Ok(p)
}

fn modified_unix(m: &std::fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A file name for display: Windows Game Bar puts invisible direction and
/// zero-width marks into its clip names (r58), which the UI showed as-is.
/// Only the label changes; the path is used untouched.
fn display_name(name: &str) -> String {
    name.chars()
        .filter(|c| {
            !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
        })
        .collect()
}

/// Videos in each folder (top level only), newest first per folder, in the
/// folders' order: Relay's recording folder first, then the user's Videos
/// folder. A file seen twice is listed once (as a Relay recording).
pub fn list_videos(folders: &[(PathBuf, bool)]) -> Vec<VideoFile> {
    let mut out: Vec<VideoFile> = Vec::new();
    for (dir, relay) in folders {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut here: Vec<VideoFile> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let m = e.metadata().ok()?;
                let name = display_name(&p.file_name()?.to_string_lossy());
                (m.is_file() && m.len() > 0 && has_video_extension(&p)).then(|| VideoFile {
                    name,
                    path: p.display().to_string(),
                    size_bytes: m.len(),
                    modified_unix: modified_unix(&m),
                    relay: *relay,
                })
            })
            .filter(|v| !out.iter().any(|o| o.path.eq_ignore_ascii_case(&v.path)))
            .collect();
        here.sort_by(|a, b| b.modified_unix.cmp(&a.modified_unix).then(a.name.cmp(&b.name)));
        out.extend(here);
    }
    out.truncate(MAX_LISTED);
    out
}

/// The folders to list: the recording folder, then `%USERPROFILE%\Videos`.
pub fn video_folders(recording_dir: &str) -> Vec<(PathBuf, bool)> {
    let mut v = vec![(PathBuf::from(recording_dir), true)];
    if let Ok(home) = std::env::var("USERPROFILE") {
        v.push((PathBuf::from(home).join("Videos"), false));
    }
    v
}

// ---------------------------------------------------------------------------
// Job status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileJobState {
    Running,
    Done,
    Cancelled,
    Failed,
}

/// What the UI shows about the current (or last) file job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnFileStatus {
    pub profile: Uuid,
    pub exe: String,
    /// The file's name only (the path stays in the core).
    pub file_name: String,
    pub state: FileJobState,
    /// 0..=1; `None` when the file does not say how long it is.
    pub progress: Option<f32>,
    pub position_secs: f32,
    pub duration_secs: Option<f32>,
    /// Content seconds per wall-clock second.
    pub speed: Option<f32>,
    /// Seconds of game audio and frames of game video that were analysed.
    pub audio_secs: f32,
    pub look_frames: u64,
    /// Gameplay frames of those (menus, cutscenes and the like left out).
    #[serde(default)]
    pub look_gameplay_frames: u64,
    /// The helper's own CPU time, seconds (all cores).
    #[serde(default)]
    pub cpu_secs: Option<f32>,
    /// Plain-English notes ("the video could not be decoded; the sound was
    /// learned").
    #[serde(default)]
    pub notes: Vec<String>,
    /// An error, for Failed.
    #[serde(default)]
    pub message: Option<String>,
    pub privacy: String,
    pub local_only: String,
}

impl LearnFileStatus {
    pub fn new(profile: Uuid, exe: &str, file: &Path) -> Self {
        Self {
            profile,
            exe: exe.to_ascii_lowercase(),
            file_name: file
                .file_name()
                .map(|n| display_name(&n.to_string_lossy()))
                .unwrap_or_default(),
            state: FileJobState::Running,
            progress: Some(0.0),
            position_secs: 0.0,
            duration_secs: None,
            speed: None,
            audio_secs: 0.0,
            look_frames: 0,
            look_gameplay_frames: 0,
            cpu_secs: None,
            notes: Vec::new(),
            message: None,
            privacy: PRIVACY_NOTICE.into(),
            local_only: LOCAL_ONLY_NOTICE.into(),
        }
    }
}

/// One line from `relay-share learn-file`.
#[derive(Debug, Clone, PartialEq)]
pub enum FileLine {
    Progress { pos: f32, duration: Option<f32>, speed: Option<f32> },
    Note(String),
    LookResult(Box<LookLearner>),
    Done { audio_secs: f32, look_frames: u64, speed: Option<f32>, cpu_secs: Option<f32> },
    Cancelled,
    Error(String),
}

fn num(v: &serde_json::Value, k: &str) -> Option<f32> {
    v.get(k).and_then(|x| x.as_f64()).map(|x| x as f32).filter(|x| x.is_finite())
}

pub fn decode_file_line(line: &str) -> Option<FileLine> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(match v.get("event")?.as_str()? {
        "progress" => FileLine::Progress {
            pos: num(&v, "pos_secs")?,
            duration: num(&v, "duration_secs").filter(|d| *d > 0.0),
            speed: num(&v, "speed"),
        },
        "note" => FileLine::Note(v.get("message")?.as_str()?.to_string()),
        "look_result" => {
            FileLine::LookResult(Box::new(serde_json::from_value(v.get("learner")?.clone()).ok()?))
        }
        "done" => FileLine::Done {
            audio_secs: num(&v, "audio_secs").unwrap_or(0.0),
            look_frames: v.get("look_frames").and_then(|x| x.as_u64()).unwrap_or(0),
            speed: num(&v, "speed"),
            cpu_secs: num(&v, "cpu_secs"),
        },
        "cancelled" => FileLine::Cancelled,
        "error" => FileLine::Error(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("learning from the file failed")
                .into(),
        ),
        _ => return None,
    })
}

/// Fold one line into the status. Returns the look learner when it arrives.
pub fn apply_line(st: &mut LearnFileStatus, line: FileLine) -> Option<LookLearner> {
    match line {
        FileLine::Progress { pos, duration, speed } => {
            st.position_secs = pos;
            st.duration_secs = duration.or(st.duration_secs);
            st.speed = speed.or(st.speed);
            st.progress = st.duration_secs.map(|d| (pos / d).clamp(0.0, 1.0));
        }
        FileLine::Note(n) => {
            if !st.notes.contains(&n) {
                st.notes.push(n);
            }
        }
        FileLine::LookResult(l) => {
            st.look_gameplay_frames = l.agg.frames;
            return Some(*l);
        }
        FileLine::Done { audio_secs, look_frames, speed, cpu_secs } => {
            st.state = FileJobState::Done;
            st.audio_secs = audio_secs;
            st.look_frames = look_frames;
            st.speed = speed.or(st.speed);
            st.cpu_secs = cpu_secs;
            st.progress = Some(1.0);
        }
        FileLine::Cancelled => st.state = FileJobState::Cancelled,
        FileLine::Error(m) => {
            st.state = FileJobState::Failed;
            st.message = Some(m);
        }
    }
    None
}

/// Merge a file-learned look into every listed monitor's record for the
/// game (the look is panel-neutral, so each monitor gets the same
/// evidence). Returns how many monitors it went into.
pub fn merge_look(
    store: &mut crate::learned_display::LearnStore,
    exe: &str,
    monitors: &[MonitorId],
    look: &LookLearner,
) -> usize {
    if look.agg.frames == 0 && look.excluded.total() == 0 {
        return 0;
    }
    let g = store.game_mut(exe);
    for id in monitors {
        g.monitor_mut(id).learner.merge(look);
        g.take_offer_if_auto(id);
    }
    monitors.len()
}

// ---------------------------------------------------------------------------
// The helper child
// ---------------------------------------------------------------------------

/// `relay-share learn-file ...`.
pub fn helper_args(file: &Path, exe: &str, record: &Path, goal: &str) -> Vec<String> {
    vec![
        "learn-file".into(),
        "--file".into(),
        file.display().to_string(),
        "--exe".into(),
        exe.into(),
        "--record".into(),
        record.display().to_string(),
        "--goal".into(),
        goal.into(),
    ]
}

/// The running (or finished) file job. Dropping it kills the child (by its
/// own handle, never by image name).
pub struct FileJob {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Receiver<FileLine>,
    pub status: LearnFileStatus,
    /// The look learner, once the helper sent it; taken by the core.
    pub look: Option<LookLearner>,
    cancel_at: Option<Instant>,
}

impl FileJob {
    pub fn spawn(paths: &Paths, profile: &Profile, file: &Path) -> Result<Self> {
        let bin = crate::share::share_binary()?;
        anyhow::ensure!(bin.exists(), "relay-share not found at {}", bin.display());
        let record = crate::game_eq::record_file(paths, &profile.game.exe);
        let goal = profile.audio.game_eq_goal.unwrap_or_default();
        let goal = serde_json::to_string(&goal)?.trim_matches('"').to_string();
        let mut cmd = Command::new(&bin);
        cmd.args(helper_args(file, &profile.game.exe, &record, &goal));
        cmd.env("RELAY_SPAWNED", "1");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
            cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        let mut child = cmd.spawn().context("starting the video learner")?;
        let stdin = child.stdin.take();
        let out = child.stdout.take().context("no learner stdout")?;
        let (tx, rx): (Sender<FileLine>, Receiver<FileLine>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("relay-learn-file-reader".into())
            .stack_size(256 * 1024)
            .spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(l) = decode_file_line(&line) {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
            }
        })?;
        tracing::info!(exe = %profile.game.exe, "learning from a video file");
        Ok(Self {
            child: Some(child),
            stdin,
            rx,
            status: LearnFileStatus::new(profile.id, &profile.game.exe, file),
            look: None,
            cancel_at: None,
        })
    }

    pub fn running(&self) -> bool {
        self.status.state == FileJobState::Running
    }

    /// Ask the helper to stop. It leaves without saving anything.
    pub fn cancel(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.write_all(b"stop\n");
            let _ = s.flush();
        }
        self.cancel_at.get_or_insert_with(Instant::now);
    }

    /// Read what the helper sent. Returns true when the job just finished
    /// (Done, Cancelled or Failed).
    pub fn poll(&mut self) -> bool {
        if !self.running() && self.child.is_none() {
            return false;
        }
        let exited = self.child.as_mut().is_none_or(|c| !matches!(c.try_wait(), Ok(None)));
        let mut lines: Vec<FileLine> = self.rx.try_iter().collect();
        if exited {
            // The reader may still be forwarding the last lines.
            while let Ok(l) = self.rx.recv_timeout(Duration::from_millis(300)) {
                lines.push(l);
            }
        }
        let was = self.status.state;
        for l in lines {
            if let Some(look) = apply_line(&mut self.status, l) {
                self.look = Some(look);
            }
        }
        if self.cancel_at.is_some() && self.status.state == FileJobState::Running {
            if exited {
                self.status.state = FileJobState::Cancelled;
            } else if self.cancel_at.is_some_and(|t| t.elapsed() >= CANCEL_TIMEOUT) {
                if let Some(c) = self.child.as_mut() {
                    let _ = c.kill();
                }
                self.status.state = FileJobState::Cancelled;
            }
        }
        if exited && self.status.state == FileJobState::Running {
            self.status.state = FileJobState::Failed;
            self.status.message.get_or_insert_with(|| "the video learner stopped early".into());
        }
        if self.status.state != FileJobState::Running {
            if let Some(mut c) = self.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        was == FileJobState::Running && self.status.state != FileJobState::Running
    }
}

impl Drop for FileJob {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            if let Some(mut s) = self.stdin.take() {
                let _ = s.write_all(b"stop\n");
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_display::learn::{FrameClass, FrameReport, FrameStats};

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("relay-learn-file-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_local_existing_video_files_are_accepted() {
        let d = tmp();
        let good = d.join("Clip.MKV");
        std::fs::write(&good, b"x").unwrap();
        assert_eq!(validate_video_path(&format!("\"{}\"", good.display())).unwrap(), good);
        let empty = d.join("empty.mp4");
        std::fs::write(&empty, b"").unwrap();
        let text = d.join("notes.txt");
        std::fs::write(&text, b"x").unwrap();
        for (bad, why) in [
            ("https://www.youtube.com/watch?v=abc".to_string(), "websites"),
            ("youtube.com/watch?v=abc".to_string(), "websites"),
            ("\\\\nas\\share\\clip.mp4".to_string(), "network"),
            ("clip.mp4".to_string(), "full path"),
            (text.display().to_string(), ".mp4"),
            (empty.display().to_string(), "empty"),
            (d.join("missing.mp4").display().to_string(), "not found"),
            (String::new(), "choose"),
        ] {
            let e = validate_video_path(&bad).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn relay_recordings_are_listed_first_then_other_videos_newest_first() {
        let (rec, vids) = (tmp(), tmp());
        std::fs::write(rec.join("relay-1.mp4"), b"a").unwrap();
        std::fs::write(rec.join("relay-2.mkv"), b"b").unwrap();
        std::fs::write(rec.join("notes.txt"), b"c").unwrap();
        std::fs::write(vids.join("other.webm"), b"d").unwrap();
        std::fs::write(vids.join("empty.mov"), b"").unwrap();
        let l = list_videos(&[(rec.clone(), true), (vids.clone(), false), (rec.clone(), false)]);
        let names: Vec<&str> = l.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(l.len(), 3, "{names:?}");
        assert!(l[0].relay && l[1].relay && !l[2].relay, "{l:?}");
        assert_eq!(l[2].name, "other.webm");
        assert!(l.iter().all(|v| v.size_bytes > 0));
        // A folder that does not exist is simply skipped.
        assert!(list_videos(&[(rec.join("nope"), true)]).is_empty());
        std::fs::remove_dir_all(rec).ok();
        std::fs::remove_dir_all(vids).ok();
    }

    #[test]
    fn helper_lines_update_the_status() {
        let mut st = LearnFileStatus::new(Uuid::nil(), "Game.exe", Path::new("C:\\v\\clip.mp4"));
        assert_eq!((st.exe.as_str(), st.file_name.as_str()), ("game.exe", "clip.mp4"));
        assert_eq!(display_name("Call of Duty\u{200E} 2026\u{200B}.mp4"), "Call of Duty 2026.mp4");
        let p = decode_file_line(
            r#"{"event":"progress","pos_secs":30,"duration_secs":120,"speed":14.5}"#,
        )
        .unwrap();
        apply_line(&mut st, p);
        assert_eq!(st.progress, Some(0.25));
        assert_eq!(st.speed, Some(14.5));
        apply_line(&mut st, decode_file_line(r#"{"event":"note","message":"no audio"}"#).unwrap());
        apply_line(&mut st, decode_file_line(r#"{"event":"note","message":"no audio"}"#).unwrap());
        assert_eq!(st.notes, vec!["no audio".to_string()]);
        let mut l = LookLearner::new("");
        l.observe(&FrameReport {
            class: FrameClass::Gameplay,
            stats: FrameStats { mean_luma: 0.3, ..Default::default() },
        });
        let line = serde_json::json!({ "event": "look_result", "learner": l }).to_string();
        let got = apply_line(&mut st, decode_file_line(&line).unwrap()).unwrap();
        assert_eq!(got.agg.frames, 1);
        assert_eq!(st.look_gameplay_frames, 1);
        apply_line(
            &mut st,
            decode_file_line(
                r#"{"event":"done","audio_secs":120,"look_frames":240,"speed":15,"cpu_secs":3.5}"#,
            )
            .unwrap(),
        );
        assert_eq!(st.state, FileJobState::Done);
        assert_eq!((st.audio_secs, st.look_frames, st.cpu_secs), (120.0, 240, Some(3.5)));
        assert_eq!(st.progress, Some(1.0));
        let mut c = LearnFileStatus::new(Uuid::nil(), "g.exe", Path::new("x.mkv"));
        apply_line(&mut c, decode_file_line(r#"{"event":"cancelled"}"#).unwrap());
        assert_eq!(c.state, FileJobState::Cancelled);
        apply_line(
            &mut c,
            decode_file_line(r#"{"event":"error","message":"no decoder"}"#).unwrap(),
        );
        assert_eq!((c.state, c.message.as_deref()), (FileJobState::Failed, Some("no decoder")));
        assert!(decode_file_line("garbage").is_none());
        assert!(decode_file_line(r#"{"event":"progress"}"#).is_none());
        // An unknown duration gives no percentage, never a made-up one.
        let mut u = LearnFileStatus::new(Uuid::nil(), "g.exe", Path::new("x.mkv"));
        apply_line(&mut u, decode_file_line(r#"{"event":"progress","pos_secs":30}"#).unwrap());
        assert_eq!(u.progress, None);
    }

    #[test]
    fn a_file_look_merges_into_every_listed_monitor() {
        let dir = tmp();
        let mut store = crate::learned_display::LearnStore::load(dir.join("ld.json"));
        let mut l = LookLearner::new("");
        for i in 0..700 {
            l.observe(&FrameReport {
                class: FrameClass::Gameplay,
                stats: FrameStats {
                    mean_luma: [0.05, 0.3, 0.7][i % 3],
                    crush_frac: 0.14,
                    crushed_detail: 0.02,
                    sat_mean: 0.4,
                    sat_p90: 0.6,
                    motion: 0.1,
                    ..Default::default()
                },
            });
        }
        let mons = [MonitorId("A".into()), MonitorId("B".into())];
        assert_eq!(merge_look(&mut store, "Game.exe", &mons, &l), 2);
        let g = store.game("game.exe").unwrap();
        for m in &mons {
            assert_eq!(g.monitors[&m.0].learner.agg.frames, 700);
            assert!(g.offer(m).is_some(), "settled from the file: offered, not applied");
            assert!(g.effective(m).is_none());
        }
        // Nothing learned: nothing merged.
        assert_eq!(merge_look(&mut store, "game.exe", &mons, &LookLearner::new("")), 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn helper_arguments_name_the_file_and_the_record() {
        let a = helper_args(
            Path::new("C:\\v\\a b.mp4"),
            "game.exe",
            Path::new("C:\\r.json"),
            "dialogue",
        );
        assert_eq!(a[0], "learn-file");
        assert_eq!(a[2], "C:\\v\\a b.mp4");
        assert_eq!(a[a.len() - 1], "dialogue");
        assert_eq!(VIDEO_EXTENSIONS, &["mp4", "mkv", "mov", "webm"]);
    }
}
