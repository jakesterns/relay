//! Share presets as data (`presets.json`): Game / DAW / Desktop and, since
//! S50, Discord (1080p60) and Discord 720p30 for calls ship as built-ins the
//! user can edit, plus the recording settings (folder + disk
//! budget). A profile's `SharePreset` chip names one of these by id; starting
//! a share with a preset resolves it to a concrete [`ShareRequest`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::profiles::write_atomic;
use crate::share::{RecordingContainer, ShareRequest, DEFAULT_PREVIEW_FPS};

/// 2 (S50): the call presets were added. A version-1 file gains them once,
/// on load; after that a deleted one stays deleted like any built-in.
const FILE_VERSION: u32 = 2;

/// Built-ins each file version introduced after the first, so an older file
/// gets exactly the ones it has never seen.
const ADDED_IN: &[(u32, &[&str])] = &[(2, &["discord", "discord-720"])];

/// Which desktop audio goes with the share. The microphone is a separate,
/// independent choice — see [`PresetAudio`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DesktopAudio {
    /// Default render endpoint loopback (everything you hear).
    #[default]
    System,
    /// The focused game's process tree only.
    Game,
    /// No desktop audio track.
    Off,
}

/// The share's audio *source set*. Since S2 the sender can carry two Opus
/// tracks, so the microphone is no longer one of four mutually exclusive
/// choices — it rides alongside whatever desktop source is picked. See
/// `docs/dev/dual-audio-decision.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresetAudio {
    pub desktop: DesktopAudio,
    pub mic: bool,
    /// With `Game`: also send everything else on the PC as its own track,
    /// so the receiver can mix or mute it separately (S37). Meaningless with
    /// `System` (that already is everything) and `Off`; ignored there.
    pub rest: bool,
}

impl PresetAudio {
    /// No audio at all.
    pub fn off() -> Self {
        Self { desktop: DesktopAudio::Off, mic: false, rest: false }
    }

    pub fn desktop(desktop: DesktopAudio) -> Self {
        Self { desktop, mic: false, rest: false }
    }

    /// Does this preset ask for any audio track at all?
    pub fn is_silent(self) -> bool {
        self.desktop == DesktopAudio::Off && !self.mic
    }
}

/// On-disk shape. The old four-way string (`system` / `game` / `mic` / `off`)
/// still reads, so an existing `presets.json` keeps its meaning: `mic` was
/// mic-*instead-of*-desktop, which is exactly `{desktop: off, mic: true}`.
/// Writing always uses the new object form.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PresetAudioRepr {
    Set {
        #[serde(default)]
        desktop: DesktopAudio,
        #[serde(default)]
        mic: bool,
        #[serde(default)]
        rest: bool,
    },
    Legacy(LegacyAudio),
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum LegacyAudio {
    System,
    Game,
    Mic,
    Off,
}

impl From<PresetAudioRepr> for PresetAudio {
    fn from(r: PresetAudioRepr) -> Self {
        match r {
            PresetAudioRepr::Set { desktop, mic, rest } => Self { desktop, mic, rest },
            PresetAudioRepr::Legacy(LegacyAudio::System) => Self::desktop(DesktopAudio::System),
            PresetAudioRepr::Legacy(LegacyAudio::Game) => Self::desktop(DesktopAudio::Game),
            PresetAudioRepr::Legacy(LegacyAudio::Mic) => {
                Self { desktop: DesktopAudio::Off, mic: true, rest: false }
            }
            PresetAudioRepr::Legacy(LegacyAudio::Off) => Self::off(),
        }
    }
}

impl From<PresetAudio> for PresetAudioRepr {
    fn from(a: PresetAudio) -> Self {
        PresetAudioRepr::Set { desktop: a.desktop, mic: a.mic, rest: a.rest }
    }
}

impl Serialize for PresetAudio {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        PresetAudioRepr::from(*self).serialize(ser)
    }
}

impl<'de> Deserialize<'de> for PresetAudio {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        PresetAudioRepr::deserialize(de).map(Into::into)
    }
}

/// One share preset. The codec is not a preset field: each share negotiates
/// HEVC or H.264 with its receiver (S27). GOP is fixed by the engine's latency
/// tuning.
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
    /// Container for recordings and replay saves made under this preset.
    #[serde(default)]
    pub container: RecordingContainer,
    /// Also show the share as "Relay Camera" on this PC while it runs (S36),
    /// for OBS-style software here. A wish: the service still needs consent,
    /// registration and Windows 11. Old files read it as off.
    #[serde(default)]
    pub vcam: bool,
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
            audio: PresetAudio::desktop(DesktopAudio::Game),
            cursor: false,
            record: false,
            replay_secs: 60,
            container: RecordingContainer::Mp4,
            vcam: false,
        },
        SharePresetDef {
            id: "daw".into(),
            name: "DAW".into(),
            bitrate_mbps: 40,
            fps: 60,
            size: Some((2560, 1440)),
            audio: PresetAudio::desktop(DesktopAudio::System),
            cursor: true,
            record: false,
            replay_secs: 0,
            container: RecordingContainer::Mp4,
            vcam: false,
        },
        SharePresetDef {
            id: "desktop".into(),
            name: "Desktop".into(),
            bitrate_mbps: 60,
            fps: 60,
            size: None,
            audio: PresetAudio::desktop(DesktopAudio::System),
            cursor: true,
            record: false,
            replay_secs: 0,
            container: RecordingContainer::Mp4,
            vcam: false,
        },
        // S50: for a receiving PC that passes the share on to a call. Discord,
        // Zoom, Teams and Meet all re-encode what they capture, and none of
        // them sends more than 1080p60 (most calls far less), so a 4K or
        // native-size feed only costs bandwidth and makes them downscale.
        // 1080p60 at 20 Mb/s is clean enough that their re-encode starts from
        // a near-perfect picture; 720p30 at 8 Mb/s is for a slow call or a
        // free Discord account, which streams 720p30.
        SharePresetDef {
            id: "discord".into(),
            name: "Discord".into(),
            bitrate_mbps: 20,
            fps: 60,
            size: Some((1920, 1080)),
            audio: PresetAudio::desktop(DesktopAudio::System),
            cursor: true,
            record: false,
            replay_secs: 0,
            container: RecordingContainer::Mp4,
            vcam: false,
        },
        SharePresetDef {
            id: "discord-720".into(),
            name: "Discord 720p30".into(),
            bitrate_mbps: 8,
            fps: 30,
            size: Some((1280, 720)),
            audio: PresetAudio::desktop(DesktopAudio::System),
            cursor: true,
            record: false,
            replay_secs: 0,
            container: RecordingContainer::Mp4,
            vcam: false,
        },
    ]
}

/// Add the built-ins introduced after `version` that `presets` lacks, in
/// built-in order, after the ones already there.
fn add_new_builtins(version: u32, presets: &mut Vec<SharePresetDef>) {
    let all = builtins();
    for (since, ids) in ADDED_IN {
        if version >= *since {
            continue;
        }
        for id in *ids {
            if presets.iter().any(|p| p.id == *id) {
                continue;
            }
            if let Some(def) = all.iter().find(|p| p.id == *id) {
                presets.push(def.clone());
            }
        }
    }
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
                let mut file: PresetsFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?;
                add_new_builtins(file.version, &mut file.presets);
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
    let (audio, audio_pid) = match preset.audio.desktop {
        DesktopAudio::System => (true, None),
        DesktopAudio::Game => (true, game_pid),
        DesktopAudio::Off => (false, None),
    };
    // `mic` is additive: it asks for a *second* Opus track, so a preset can
    // now carry the game and the microphone at once.
    let mic = preset.audio.mic;
    // The rest of the PC only exists as a track beside a game (S37).
    let rest = preset.audio.rest && preset.audio.desktop == DesktopAudio::Game;
    ShareRequest {
        peer,
        code,
        // Set by the service from the request that named a remembered PC.
        peer_id: None,
        trusted: None,
        bitrate_mbps: preset.bitrate_mbps,
        fps: preset.fps,
        size: preset.size,
        audio,
        audio_pid,
        // Pinned to the live process by the service at spawn (r54).
        audio_app: None,
        mic,
        rest,
        cursor: preset.cursor,
        preset: Some(preset.id.clone()),
        record: preset.record,
        replay_secs: preset.replay_secs,
        record_dir: Some(recording.resolved_dir()),
        container: preset.container,
        vcam: preset.vcam,
        // Filled by the service from the saved Share setting (S51).
        ndi: false,
        // The app window is open when someone starts a share from it, so a
        // couple of thumbnails a second is what they expect to see.
        preview_fps: DEFAULT_PREVIEW_FPS,
        // Filled by the service from the saved mixer picks (S40).
        mic_device: None,
        output_device: None,
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_match_the_plan() {
        let b = builtins();
        assert_eq!(
            b.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["game", "daw", "desktop", "discord", "discord-720"]
        );
        let daw = &b[1];
        // DAW preset: 1440p60, default endpoint audio, nothing touched.
        assert_eq!(daw.size, Some((2560, 1440)));
        assert_eq!(daw.fps, 60);
        assert_eq!(daw.audio, PresetAudio::desktop(DesktopAudio::System));
        let game = &b[0];
        assert_eq!(game.audio, PresetAudio::desktop(DesktopAudio::Game));
        assert_eq!(game.replay_secs, 60, "replay buffer defaults to 60 s");
        assert!(!game.cursor, "games render their own cursor");
    }

    /// S50: sizes and rates call apps handle well, at a bitrate their
    /// re-encode can start clean from. The existing three are untouched.
    #[test]
    fn call_presets_are_sized_for_call_apps() {
        let b = builtins();
        let d = b.iter().find(|p| p.id == "discord").unwrap();
        assert_eq!(d.name, "Discord");
        assert_eq!((d.size, d.fps, d.bitrate_mbps), (Some((1920, 1080)), 60, 20));
        assert_eq!(d.audio, PresetAudio::desktop(DesktopAudio::System), "the call hears the PC");
        let s = b.iter().find(|p| p.id == "discord-720").unwrap();
        assert_eq!((s.size, s.fps, s.bitrate_mbps), (Some((1280, 720)), 30, 8));
        for p in [d, s] {
            assert_eq!(p.replay_secs, 0, "{}: no replay ring on a call feed", p.id);
            assert!(!p.record);
            let req = to_share_request(p, "1".into(), None, None, &RecordingSettings::default());
            assert_eq!(req.size, p.size);
            assert_eq!(req.fps, p.fps);
            assert!(req.audio && req.audio_pid.is_none());
        }
        // The three that were there before S50 keep their values.
        assert_eq!((b[0].bitrate_mbps, b[0].fps, b[0].size), (60, 60, None));
        assert_eq!((b[1].bitrate_mbps, b[1].fps, b[1].size), (40, 60, Some((2560, 1440))));
        assert_eq!((b[2].bitrate_mbps, b[2].fps, b[2].size), (60, 60, None));
    }

    /// A presets.json written before S50 gains the call presets once, keeps
    /// every edit and deletion it had, and a later deletion of a call preset
    /// sticks.
    #[test]
    fn an_old_file_gains_the_call_presets_once() {
        let dir = std::env::temp_dir().join(format!("relay-presets-s50-{}", std::process::id()));
        let path = dir.join("presets.json");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut old = builtins();
        old.truncate(3);
        old.retain(|p| p.id != "desktop");
        old[0].bitrate_mbps = 33;
        let v1 = serde_json::json!({ "version": 1, "presets": old });
        std::fs::write(&path, serde_json::to_vec(&v1).unwrap()).unwrap();

        let mut store = PresetStore::load(&path).unwrap();
        let ids: Vec<&str> = store.all().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["game", "daw", "discord", "discord-720"], "desktop stays deleted");
        assert_eq!(store.get("game").unwrap().bitrate_mbps, 33, "edits kept");
        assert!(store.remove("discord-720"));
        store.save().unwrap();

        let again = PresetStore::load(&path).unwrap();
        assert!(again.get("discord-720").is_none(), "a v2 file is not re-seeded");
        assert!(again.get("discord").is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn store_seeds_builtins_and_round_trips_edits() {
        let dir = std::env::temp_dir().join(format!("relay-presets-{}", std::process::id()));
        let path = dir.join("presets.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = PresetStore::load(&path).unwrap();
        assert_eq!(store.all().len(), 5);
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
            audio: PresetAudio { desktop: DesktopAudio::Off, mic: true, rest: false },
            cursor: true,
            record: true,
            replay_secs: 0,
            container: RecordingContainer::Mp4,
            vcam: false,
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

        let custom = SharePresetDef { audio: PresetAudio::off(), ..daw.clone() };
        let req = to_share_request(&custom, "1".into(), None, None, &rec);
        assert!(!req.audio);
    }

    /// The point of S2: a preset can name the game *and* the microphone, and
    /// the request carries both.
    #[test]
    fn a_preset_can_ask_for_game_audio_and_the_microphone_together() {
        let rec = RecordingSettings::default();
        let both = SharePresetDef {
            audio: PresetAudio { desktop: DesktopAudio::Game, mic: true, rest: false },
            ..builtins()[0].clone()
        };
        let req = to_share_request(&both, "1".into(), None, Some(99), &rec);
        assert!(req.audio, "program track still asked for");
        assert_eq!(req.audio_pid, Some(99), "and it is still the game only");
        assert!(req.mic, "plus a second microphone track");
    }

    /// An existing `presets.json` written before S2 must keep its meaning:
    /// `"audio": "mic"` meant mic *instead of* the desktop mix.
    #[test]
    fn legacy_audio_strings_still_load_with_the_same_meaning() {
        let parse = |json: &str| serde_json::from_str::<PresetAudio>(json).unwrap();
        assert_eq!(parse("\"system\""), PresetAudio::desktop(DesktopAudio::System));
        assert_eq!(parse("\"game\""), PresetAudio::desktop(DesktopAudio::Game));
        assert_eq!(parse("\"off\""), PresetAudio::off());
        assert_eq!(
            parse("\"mic\""),
            PresetAudio { desktop: DesktopAudio::Off, mic: true, rest: false },
            "legacy mic was mic-instead-of-desktop"
        );
        // And a legacy mic preset still resolves to a mic-only share.
        let rec = RecordingSettings::default();
        let legacy = SharePresetDef { audio: parse("\"mic\""), ..builtins()[2].clone() };
        let req = to_share_request(&legacy, "1".into(), None, None, &rec);
        assert!(!req.audio);
        assert!(req.mic);
    }

    #[test]
    fn audio_sets_round_trip_through_json() {
        for a in [
            PresetAudio::off(),
            PresetAudio::desktop(DesktopAudio::System),
            PresetAudio::desktop(DesktopAudio::Game),
            PresetAudio { desktop: DesktopAudio::Game, mic: true, rest: false },
            PresetAudio { desktop: DesktopAudio::Off, mic: true, rest: false },
        ] {
            let json = serde_json::to_string(&a).unwrap();
            assert_eq!(serde_json::from_str::<PresetAudio>(&json).unwrap(), a, "{json}");
        }
        assert!(PresetAudio::off().is_silent());
        assert!(!PresetAudio { desktop: DesktopAudio::Off, mic: true, rest: false }.is_silent());
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
