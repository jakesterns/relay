//! Share presets as data (`presets.json`): Game / DAW / Desktop ship as
//! built-ins the user can edit, plus the recording settings (folder + disk
//! budget). A profile's `SharePreset` chip names one of these by id; starting
//! a share with a preset resolves it to a concrete [`ShareRequest`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::profiles::write_atomic;
use crate::share::ShareRequest;

const FILE_VERSION: u32 = 1;

/// Which audio goes with the share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PresetAudio {
    /// Default render endpoint loopback (everything you hear).
    #[default]
    System,
    /// The focused game's process tree only.
    Game,
    /// Default microphone.
    Mic,
    /// No audio track.
    Off,
}

/// One share preset. Encoder codec is HEVC by construction (the engine has no
/// other path); GOP is fixed by the engine's latency tuning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharePresetDef {
    /// Stable id: `game` / `daw` / `desktop` for built-ins.
    pub id: String,
    pub name: String,
    pub bitrate_mbps: u32,
    pub fps: u32,
    /// Encode size cap `(w, h)`; `None` = native capture size. The capture is
    /// GPU-scaled into this, the peer connection never renegotiates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<(u32, u32)>,
    #[serde(default)]
    pub audio: PresetAudio,
    #[serde(default = "default_true")]
    pub cursor: bool,
    /// Start continuous recording with the share.
    #[serde(default)]
    pub record: bool,
    /// Replay ring window in seconds; 0 = off.
    #[serde(default = "default_replay")]
    pub replay_secs: u32,
}

fn default_true() -> bool {
    true
}
fn default_replay() -> u32 {
    60
}

/// Where recordings land and how much disk they may use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingSettings {
    /// `None` = the default `%USERPROFILE%\Videos\Relay`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default = "default_cap")]
    pub cap_gb: u32,
    #[serde(default = "default_floor")]
    pub free_floor_gb: u32,
}

fn default_cap() -> u32 {
    50
}
fn default_floor() -> u32 {
    10
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self { dir: None, cap_gb: default_cap(), free_floor_gb: default_floor() }
    }
}

impl RecordingSettings {
    /// The folder recordings go to, resolving the default against the user
    /// profile directory.
    pub fn resolved_dir(&self) -> String {
        match &self.dir {
            Some(d) if !d.trim().is_empty() => d.clone(),
            _ => {
                let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into());
                format!("{}\\Videos\\Relay", home.trim_end_matches('\\'))
            }
        }
    }
}

/// The built-in presets, in UI order. `Game` follows the game (process-only
/// audio, native size); `DAW` is 1440p60 with the default endpoint's audio
/// untouched (48 k, no processing — the engine never resamples or applies
/// DSP to share audio); `Desktop` is the plain screen-share.
pub fn builtins() -> Vec<SharePresetDef> {
    vec![
        SharePresetDef {
            id: "game".into(),
            name: "Game".into(),
            bitrate_mbps: 60,
            fps: 60,
            size: None,
            audio: PresetAudio::Game,
            cursor: false,
            record: false,
            replay_secs: 60,
        },
        SharePresetDef {
            id: "daw".into(),
            name: "DAW".into(),
            bitrate_mbps: 40,
            fps: 60,
            size: Some((2560, 1440)),
            audio: PresetAudio::System,
            cursor: true,
            record: false,
            replay_secs: 0,
        },
        SharePresetDef {
            id: "desktop".into(),
            name: "Desktop".into(),
            bitrate_mbps: 60,
            fps: 60,
            size: None,
            audio: PresetAudio::System,
            cursor: true,
            record: false,
            replay_secs: 0,
        },
    ]
}

#[derive(Debug, Serialize, Deserialize)]
struct PresetsFile {
    version: u32,
    #[serde(default)]
    recording: RecordingSettings,
    presets: Vec<SharePresetDef>,
}

#[derive(Debug)]
pub struct PresetStore {
    path: PathBuf,
    pub recording: RecordingSettings,
    presets: Vec<SharePresetDef>,
}

impl PresetStore {
    /// Load from disk; a missing file seeds the built-ins. Built-ins that
    /// were deleted from the file stay deleted (the user's call).
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let file: PresetsFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?;
                Ok(Self { path, recording: file.recording, presets: file.presets })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self { path, recording: RecordingSettings::default(), presets: builtins() })
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn in_memory() -> Self {
        Self { path: PathBuf::new(), recording: RecordingSettings::default(), presets: builtins() }
    }

    pub fn save(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let file = PresetsFile {
            version: FILE_VERSION,
            recording: self.recording.clone(),
            presets: self.presets.clone(),
        };
        write_atomic(&self.path, &serde_json::to_vec_pretty(&file)?)
    }

    pub fn all(&self) -> &[SharePresetDef] {
        &self.presets
    }

    pub fn get(&self, id: &str) -> Option<&SharePresetDef> {
        self.presets.iter().find(|p| p.id == id)
    }

    pub fn upsert(&mut self, preset: SharePresetDef) {
        match self.presets.iter_mut().find(|p| p.id == preset.id) {
            Some(slot) => *slot = preset,
            None => self.presets.push(preset),
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.presets.len();
        self.presets.retain(|p| p.id != id);
        self.presets.len() != before
    }
}

/// Resolve a preset into the concrete share request. `code`/`peer` come from
/// the pairing UI; `game_pid` is the focused game when the preset wants
/// process-only audio (falling back to the system mix when there is none).
pub fn to_share_request(
    preset: &SharePresetDef,
    code: String,
    peer: Option<String>,
    game_pid: Option<u32>,
    recording: &RecordingSettings,
) -> ShareRequest {
    let (audio, audio_pid, mic) = match preset.audio {
        PresetAudio::System => (true, None, false),
        PresetAudio::Game => (true, game_pid, false),
        PresetAudio::Mic => (true, None, true),
        PresetAudio::Off => (false, None, false),
    };
    ShareRequest {
        peer,
        code,
        bitrate_mbps: preset.bitrate_mbps,
        fps: preset.fps,
        size: preset.size,
        audio,
        audio_pid,
        mic,
        cursor: preset.cursor,
        preset: Some(preset.id.clone()),
        record: preset.record,
        replay_secs: preset.replay_secs,
        record_dir: Some(recording.resolved_dir()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_match_the_plan() {
        let b = builtins();
        assert_eq!(b.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["game", "daw", "desktop"]);
        let daw = &b[1];
        // DAW preset: 1440p60, default endpoint audio, nothing touched.
        assert_eq!(daw.size, Some((2560, 1440)));
        assert_eq!(daw.fps, 60);
        assert_eq!(daw.audio, PresetAudio::System);
        let game = &b[0];
        assert_eq!(game.audio, PresetAudio::Game);
        assert_eq!(game.replay_secs, 60, "replay buffer defaults to 60 s");
        assert!(!game.cursor, "games render their own cursor");
    }

    #[test]
    fn store_seeds_builtins_and_round_trips_edits() {
        let dir = std::env::temp_dir().join(format!("relay-presets-{}", std::process::id()));
        let path = dir.join("presets.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = PresetStore::load(&path).unwrap();
        assert_eq!(store.all().len(), 3);
        let mut daw = store.get("daw").unwrap().clone();
        daw.bitrate_mbps = 25;
        store.upsert(daw);
        store.recording.dir = Some(r"D:\Captures".into());
        store.save().unwrap();

        let again = PresetStore::load(&path).unwrap();
        assert_eq!(again.get("daw").unwrap().bitrate_mbps, 25);
        assert_eq!(again.recording.dir.as_deref(), Some(r"D:\Captures"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deleted_builtin_stays_deleted_and_custom_presets_survive() {
        let dir = std::env::temp_dir().join(format!("relay-presets2-{}", std::process::id()));
        let path = dir.join("presets.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = PresetStore::load(&path).unwrap();
        assert!(store.remove("desktop"));
        assert!(!store.remove("desktop"), "double delete is a no-op");
        store.upsert(SharePresetDef {
            id: "podcast".into(),
            name: "Podcast".into(),
            bitrate_mbps: 20,
            fps: 30,
            size: Some((1920, 1080)),
            audio: PresetAudio::Mic,
            cursor: true,
            record: true,
            replay_secs: 0,
        });
        store.save().unwrap();

        let again = PresetStore::load(&path).unwrap();
        assert!(again.get("desktop").is_none());
        assert_eq!(again.get("podcast").unwrap().fps, 30);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preset_to_request_mapping() {
        let rec = RecordingSettings::default();
        let game = &builtins()[0];
        let req = to_share_request(game, "123456".into(), Some("den-pc".into()), Some(4321), &rec);
        assert_eq!(req.code, "123456");
        assert_eq!(req.audio_pid, Some(4321), "game preset captures the game's audio only");
        assert_eq!(req.replay_secs, 60);
        assert!(!req.record);
        assert!(req.record_dir.as_deref().unwrap().ends_with(r"\Videos\Relay"));
        assert_eq!(req.preset.as_deref(), Some("game"));

        // No focused game → system mix, not a broken pid.
        let req = to_share_request(game, "1".into(), None, None, &rec);
        assert_eq!(req.audio_pid, None);
        assert!(req.audio);

        let daw = &builtins()[1];
        let req = to_share_request(daw, "1".into(), None, None, &rec);
        assert_eq!(req.size, Some((2560, 1440)), "DAW defaults to 1440p60");
        assert!(!req.mic);

        let custom = SharePresetDef { audio: PresetAudio::Off, ..daw.clone() };
        let req = to_share_request(&custom, "1".into(), None, None, &rec);
        assert!(!req.audio);
    }

    #[test]
    fn recording_dir_resolves_default_and_override() {
        let rec = RecordingSettings { dir: Some(r"D:\Cap".into()), ..Default::default() };
        assert_eq!(rec.resolved_dir(), r"D:\Cap");
        let rec = RecordingSettings::default();
        assert!(rec.resolved_dir().ends_with(r"\Videos\Relay"));
        assert_eq!(rec.cap_gb, 50);
        assert_eq!(rec.free_floor_gb, 10);
    }
}
