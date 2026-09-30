//! Commands the core writes to the share engine's stdin, one per line:
//! the legacy bare `stop`, or JSON tagged with `cmd`. This is the third leg
//! of the core↔engine contract (args and NDJSON events are the others), so
//! the shapes are locked by tests here and mirrored in `relay-core::share`.

use serde::{Deserialize, Serialize};

/// What to capture; `Switch` retargets a running share without touching the
/// peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceTarget {
    /// A monitor by enumeration index (0 = primary).
    Display { index: usize },
    /// A top-level window by HWND.
    Window { hwnd: u64 },
    /// A rectangle on one monitor, in that monitor's coordinates.
    Region { display: usize, x: u32, y: u32, w: u32, h: u32 },
}

/// One fader's setting (S37). `gain` is linear, 0.0–2.0; unity is 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FaderLevel {
    pub gain: f32,
    #[serde(default)]
    pub mute: bool,
}

/// A mixer command: any subset of the faders. A fader left out is
/// left alone, so a slider move sends one field, not four.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FaderSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<FaderLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<FaderLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic: Option<FaderLevel>,
    /// The call coming back from the receiver (S19); sender only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<FaderLevel>,
}

/// A device-backed track the user can point at an endpoint (S40). `Mic` is
/// the sender's microphone input; `Output` is where this engine plays audio
/// (the receiver's received mix, the sender's call return).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceTrack {
    Mic,
    Output,
}

// `PartialEq` but not `Eq`: `FaderLevel::gain` is an f32.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EngineCmd {
    Stop,
    /// Per-track gain and mute, live (S37). Both engines take it: the sender
    /// applies it before encoding, the receiver before its one mix.
    Mixer {
        faders: FaderSet,
    },
    /// Point a device-backed track at an endpoint id, or back at the System
    /// default (`device` absent or null), live, without restarting (S40).
    Device {
        track: DeviceTrack,
        #[serde(default)]
        device: Option<String>,
    },
    /// Toggle continuous recording.
    Record {
        on: bool,
    },
    /// Save the replay ring to disk.
    ReplaySave,
    /// Swap the capture source.
    Switch {
        target: SourceTarget,
    },
    /// Retune the in-app preview: thumbnails per second, 0 = off.
    Preview {
        fps: u32,
    },
    /// Receiver only: where the stream window lives. `owner` is the app
    /// window's HWND (0 when popping out). See `render::host`.
    Host {
        mode: HostMode,
        #[serde(default)]
        owner: u64,
    },
}

/// How the receiver's window is hosted. `Embedded` = a frameless popup owned
/// by the app window, positioned by the app over its video area; `Popout` =
/// an ordinary top-level window of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostMode {
    Embedded,
    Popout,
}

/// Parse one stdin line. `stop` (the M4 wire format) still works; everything
/// else must be a `cmd`-tagged JSON object. Unknown input → `None` (ignored,
/// never fatal — the engine must survive a confused core).
pub fn parse_line(line: &str) -> Option<EngineCmd> {
    // A BOM-happy writer (PowerShell's default stdin encoding, say) prefixes
    // the first line with U+FEFF; strip it or the first command is lost.
    let line = line.trim_start_matches('\u{feff}').trim();
    if line == "stop" {
        return Some(EngineCmd::Stop);
    }
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_stop_still_works() {
        assert_eq!(parse_line("stop"), Some(EngineCmd::Stop));
        assert_eq!(parse_line("  stop  "), Some(EngineCmd::Stop));
        assert_eq!(parse_line(r#"{"cmd":"stop"}"#), Some(EngineCmd::Stop));
    }

    #[test]
    fn leading_bom_is_stripped() {
        assert_eq!(parse_line("\u{feff}stop"), Some(EngineCmd::Stop));
        assert_eq!(parse_line("\u{feff}{\"cmd\":\"replay_save\"}"), Some(EngineCmd::ReplaySave));
    }

    #[test]
    fn host_wire_shape_is_locked() {
        assert_eq!(
            serde_json::to_string(&EngineCmd::Host { mode: HostMode::Embedded, owner: 0x1234 })
                .unwrap(),
            r#"{"cmd":"host","mode":"embedded","owner":4660}"#
        );
        assert_eq!(
            parse_line(r#"{"cmd":"host","mode":"popout"}"#),
            Some(EngineCmd::Host { mode: HostMode::Popout, owner: 0 })
        );
    }

    #[test]
    fn wire_shapes_are_locked() {
        // The core serialises these exact strings; a shape change here must
        // change `relay-core::share` too.
        assert_eq!(
            serde_json::to_string(&EngineCmd::Record { on: true }).unwrap(),
            r#"{"cmd":"record","on":true}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::ReplaySave).unwrap(),
            r#"{"cmd":"replay_save"}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Switch {
                target: SourceTarget::Window { hwnd: 0x51DE }
            })
            .unwrap(),
            r#"{"cmd":"switch","target":{"kind":"window","hwnd":20958}}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Switch {
                target: SourceTarget::Region { display: 1, x: 10, y: 20, w: 1280, h: 720 }
            })
            .unwrap(),
            r#"{"cmd":"switch","target":{"kind":"region","display":1,"x":10,"y":20,"w":1280,"h":720}}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Preview { fps: 2 }).unwrap(),
            r#"{"cmd":"preview","fps":2}"#
        );
    }

    #[test]
    fn device_wire_shape_is_locked() {
        assert_eq!(
            serde_json::to_string(&EngineCmd::Device {
                track: DeviceTrack::Mic,
                device: Some("{0.0.1.00000000}.{abc}".into())
            })
            .unwrap(),
            r#"{"cmd":"device","track":"mic","device":"{0.0.1.00000000}.{abc}"}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Device { track: DeviceTrack::Output, device: None })
                .unwrap(),
            r#"{"cmd":"device","track":"output","device":null}"#
        );
        // Absent means the default, the same as null.
        assert_eq!(
            parse_line(r#"{"cmd":"device","track":"output"}"#),
            Some(EngineCmd::Device { track: DeviceTrack::Output, device: None })
        );
        assert_eq!(parse_line(r#"{"cmd":"device","track":"speaker"}"#), None);
    }

    #[test]
    fn round_trips_and_rejects_junk() {
        for cmd in [
            EngineCmd::Stop,
            EngineCmd::Record { on: false },
            EngineCmd::ReplaySave,
            EngineCmd::Switch { target: SourceTarget::Display { index: 1 } },
            EngineCmd::Preview { fps: 0 },
            EngineCmd::Device { track: DeviceTrack::Mic, device: Some("x".into()) },
        ] {
            let s = serde_json::to_string(&cmd).unwrap();
            assert_eq!(parse_line(&s), Some(cmd.clone()), "{s}");
        }
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("start"), None);
        assert_eq!(parse_line(r#"{"cmd":"warp"}"#), None);
        assert_eq!(parse_line(r#"{"no":"cmd"}"#), None);
        assert_eq!(parse_line("not json"), None);
    }
}
