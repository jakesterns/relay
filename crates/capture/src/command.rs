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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EngineCmd {
    Stop,
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
    }

    #[test]
    fn round_trips_and_rejects_junk() {
        for cmd in [
            EngineCmd::Stop,
            EngineCmd::Record { on: false },
            EngineCmd::ReplaySave,
            EngineCmd::Switch { target: SourceTarget::Display { index: 1 } },
        ] {
            let s = serde_json::to_string(&cmd).unwrap();
            assert_eq!(parse_line(&s), Some(cmd), "{s}");
        }
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("start"), None);
        assert_eq!(parse_line(r#"{"cmd":"warp"}"#), None);
        assert_eq!(parse_line(r#"{"no":"cmd"}"#), None);
        assert_eq!(parse_line("not json"), None);
    }
}
