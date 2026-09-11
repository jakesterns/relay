//! Domain types shared by the core service, the sibling crates and (via JSON)
//! the Tauri shell. Keep these plain data: no OS handles, no behaviour.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifies a headset / IEM in the hardware library. Stable string key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HeadsetId(pub String);

/// Identifies a monitor in the hardware library (EDID-derived key later).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MonitorId(pub String);

/// How a profile decides that a foreground window belongs to "its" game.
/// Matching is on the process image name only: no window hooks, no memory reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameMatch {
    /// Executable file name, case-insensitive, e.g. `cod.exe`.
    pub exe: String,
    /// Optional substring the window title must contain (for launchers that
    /// host several games in one process).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
}

impl GameMatch {
    pub fn exe(exe: impl Into<String>) -> Self {
        Self { exe: exe.into(), title_contains: None }
    }

    pub fn matches(&self, exe_name: &str, title: &str) -> bool {
        if !self.exe.eq_ignore_ascii_case(exe_name) {
            return false;
        }
        match &self.title_contains {
            Some(needle) => title.to_lowercase().contains(&needle.to_lowercase()),
            None => true,
        }
    }
}

/// One parametric EQ band. Gain in dB, Q dimensionless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limiter {
    /// Only content below this frequency is limited ("explosion tamer").
    pub below_hz: f32,
    pub threshold_db: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AudioSettings {
    #[serde(default)]
    pub bands: Vec<EqBand>,
    #[serde(default)]
    pub hrtf: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limiter: Option<Limiter>,
    /// Route the processed signal into the share feed too ("call hears what you hear").
    #[serde(default)]
    pub apply_to_share: bool,
}

/// GPU-side colour controls (NvAPI / ADLX). Units follow the vendor APIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuColor {
    /// 0..=100, 50 is neutral on NVIDIA.
    pub vibrance: i32,
    pub gamma: f32,
    pub contrast: i32,
    pub shadow_lift: i32,
    pub hue_deg: i32,
}

impl Default for GpuColor {
    fn default() -> Self {
        Self { vibrance: 50, gamma: 1.0, contrast: 0, shadow_lift: 0, hue_deg: 0 }
    }
}

/// Monitor-side controls over DDC/CI. `None` means "leave as-is".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MonitorSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub black_equalizer: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DisplaySettings {
    #[serde(default)]
    pub gpu: GpuColor,
    #[serde(default)]
    pub monitor: MonitorSettings,
    /// Apply on focus, restore on blur. Off means the profile is display-inert.
    #[serde(default = "default_true")]
    pub follow_focus: bool,
    /// Never touch monitors other than the one the game is on.
    #[serde(default = "default_true")]
    pub leave_other_monitors: bool,
    /// Send the unfiltered frame to the share feed.
    #[serde(default = "default_true")]
    pub share_true_colors: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SharePreset {
    Game,
    Daw,
    Desktop,
    #[default]
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProfileStatus {
    #[default]
    Draft,
    Ready,
}

/// Profile = game × headset × monitor → settings. Several rows per game are
/// allowed; `profiles::select` picks the one matching connected hardware.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub note: String,
    pub game: GameMatch,
    /// `None` = any headset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headset: Option<HeadsetId>,
    /// `None` = any monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<MonitorId>,
    #[serde(default)]
    pub audio: AudioSettings,
    #[serde(default)]
    pub display: DisplaySettings,
    #[serde(default)]
    pub share: SharePreset,
    #[serde(default)]
    pub status: ProfileStatus,
}

impl Profile {
    pub fn new(name: impl Into<String>, game: GameMatch) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            note: String::new(),
            game,
            headset: None,
            monitor: None,
            audio: AudioSettings::default(),
            display: DisplaySettings::default(),
            share: SharePreset::Off,
            status: ProfileStatus::Draft,
        }
    }

    pub fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            id: self.id,
            name: self.name.clone(),
            note: self.note.clone(),
            exe: self.game.exe.clone(),
            headset: self.headset.clone(),
            monitor: self.monitor.clone(),
            share: self.share,
            status: self.status,
        }
    }
}

/// Lightweight view of a profile for lists and status lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub name: String,
    pub note: String,
    pub exe: String,
    pub headset: Option<HeadsetId>,
    pub monitor: Option<MonitorId>,
    pub share: SharePreset,
    pub status: ProfileStatus,
}

/// What the core is doing right now. Mirrors the "Now" block in the rail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CoreState {
    pub active_profile: Option<ProfileSummary>,
    pub foreground: Option<Foreground>,
    pub sharing: ShareState,
    pub audio_chain: AudioChainState,
    pub display_state: DisplayState,
    pub footprint: Footprint,
    /// Connected endpoints/monitors and the resolved headset (M1).
    #[serde(default)]
    pub hardware: crate::hardware::HardwareView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Foreground {
    pub pid: u32,
    pub exe: String,
    pub title: String,
}

/// A running process that owns a visible window (for the exe picker).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub exe: String,
    pub title: String,
    /// The enumerated top-level window, for source switching.
    #[serde(default)]
    pub hwnd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ShareState {
    #[default]
    Off,
    Sharing {
        peer: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AudioChainState {
    /// APO is pass-through, zero allocations.
    #[default]
    Bypass,
    Active,
    /// The game opened the endpoint in WASAPI-exclusive mode: APO is bypassed
    /// by Windows and the user must be told.
    ExclusiveBypassed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DisplayState {
    #[default]
    Default,
    Applied,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Footprint {
    pub rss_bytes: u64,
    pub cpu_percent: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_match_is_case_insensitive_on_exe() {
        let m = GameMatch::exe("CoD.exe");
        assert!(m.matches("cod.exe", "Call of Duty"));
        assert!(!m.matches("valorant.exe", "Call of Duty"));
    }

    #[test]
    fn game_match_title_filter() {
        let m = GameMatch { exe: "launcher.exe".into(), title_contains: Some("Elden".into()) };
        assert!(m.matches("launcher.exe", "ELDEN RING"));
        assert!(!m.matches("launcher.exe", "Sekiro"));
    }

    #[test]
    fn profile_round_trips_through_json() {
        let mut p = Profile::new("Call of Duty", GameMatch::exe("cod.exe"));
        p.audio.bands.push(EqBand { freq_hz: 3000.0, gain_db: 4.5, q: 1.0 });
        p.display.gpu.vibrance = 68;
        let json = serde_json::to_string(&p).unwrap();
        let back: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
