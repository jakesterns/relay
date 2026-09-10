//! Share-engine supervision. The core spawns `relay-share` as a child process
//! per share and tears it down fully afterwards, so the always-on core's RSS
//! never grows for sharing. NDJSON on the child's stdout is parsed into
//! [`ShareEvent`]s and relayed to the UI; `stop\n` on its stdin (and, as a
//! backstop, killing it) ends the share.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// How a share should be started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareRequest {
    /// Receiver instance name (mDNS) or `ip:port`; empty = first discovered.
    #[serde(default)]
    pub peer: Option<String>,
    pub code: String,
    #[serde(default = "default_bitrate")]
    pub bitrate_mbps: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default = "default_true")]
    pub audio: bool,
    /// Capture just this process's audio (game-only) instead of the desktop mix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_pid: Option<u32>,
    #[serde(default = "default_true")]
    pub cursor: bool,
}

fn default_bitrate() -> u32 {
    60
}
fn default_fps() -> u32 {
    60
}
fn default_true() -> bool {
    true
}

/// How to run the receiver side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiveRequest {
    /// mDNS instance name; empty = hostname.
    #[serde(default)]
    pub name: Option<String>,
    /// Fixed pairing code; None = the engine generates one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Lines the engine emits (a decoded subset of the child's NDJSON, plus process lifecycle).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ShareEvent {
    /// One `stats` line from the engine, forwarded verbatim to the strip.
    Stats { data: serde_json::Value },
    /// The engine connected to a receiver.
    Connected { peer: String },
    /// Receiver is advertising and waiting with this pairing code.
    Waiting { code: String, name: String },
    /// Receiver paired with a sender.
    Paired { sender: String },
    /// The engine exited; `ok` is false on crash or non-zero exit.
    Exited { ok: bool, code: Option<i32> },
    /// A structured error line from the engine.
    Error { message: String },
}

/// The path to `relay-share`, assumed to sit next to `relay-core`.
pub fn share_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current exe")?;
    let dir = exe.parent().context("exe has no parent")?;
    let cand = dir.join(if cfg!(windows) { "relay-share.exe" } else { "relay-share" });
    Ok(cand)
}

/// A running share child. Dropping it stops and reaps the process.
pub struct ShareEngine {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl ShareEngine {
    /// Spawn `relay-share send` for `req`, forwarding decoded events on `tx`.
    /// The reader thread owns stdout and ends when the child closes it.
    pub fn start(req: &ShareRequest, tx: Sender<ShareEvent>) -> Result<Self> {
        let bin = share_binary()?;
        anyhow::ensure!(
            bin.exists(),
            "share engine not found at {} (build relay-capture)",
            bin.display()
        );
        let mut cmd = Command::new(&bin);
        cmd.args(send_args(req));
        // The engine watches for a closed stdin to know the core died.
        cmd.env("RELAY_SPAWNED", "1");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());

        Self::spawn_with(cmd, tx)
    }

    /// Spawn `relay-share recv` for `req`, forwarding decoded events on `tx`.
    pub fn start_receive(req: &ReceiveRequest, tx: Sender<ShareEvent>) -> Result<Self> {
        let bin = share_binary()?;
        anyhow::ensure!(
            bin.exists(),
            "share engine not found at {} (build relay-capture)",
            bin.display()
        );
        let mut cmd = Command::new(&bin);
        cmd.args(recv_args(req));
        cmd.env("RELAY_SPAWNED", "1");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        Self::spawn_with(cmd, tx)
    }

    fn spawn_with(mut cmd: Command, tx: Sender<ShareEvent>) -> Result<Self> {
        let mut child = cmd.spawn().context("spawning relay-share")?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().context("no child stdout")?;
        std::thread::Builder::new().name("relay-share-reader".into()).spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Some(ev) = decode_line(&line) {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
            }
        })?;

        Ok(Self { child, stdin })
    }

    /// Ask the engine to stop gracefully, then ensure it is gone.
    pub fn stop(mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(b"stop\n");
            let _ = stdin.flush();
        }
        // Give it time to tear down WebRTC + encoder cleanly (DTLS close,
        // MFT drain). ~3 s is comfortably longer than a normal teardown.
        for _ in 0..150 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        warn!("share engine did not exit gracefully; killing");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Poll whether the child has exited (crash detection). Returns the exit
    /// code if it has.
    pub fn poll_exit(&mut self) -> Option<Option<i32>> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            _ => None,
        }
    }
}

impl Drop for ShareEngine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `relay-share send` command line for a request. Kept pure for tests:
/// this mapping is half of the core↔engine contract (NDJSON is the other).
fn send_args(req: &ShareRequest) -> Vec<String> {
    let mut args = vec!["send".into(), "--code".into(), req.code.clone()];
    if let Some(peer) = req.peer.as_deref().filter(|p| !p.is_empty()) {
        args.push("--peer".into());
        args.push(peer.into());
    }
    args.push("--bitrate".into());
    args.push(req.bitrate_mbps.to_string());
    args.push("--fps".into());
    args.push(req.fps.to_string());
    if !req.audio {
        args.push("--no-audio".into());
    } else if let Some(pid) = req.audio_pid {
        args.push("--audio-pid".into());
        args.push(pid.to_string());
    }
    if !req.cursor {
        args.push("--no-cursor".into());
    }
    args
}

/// The `relay-share recv` command line for a request.
fn recv_args(req: &ReceiveRequest) -> Vec<String> {
    let mut args = vec!["recv".to_string()];
    if let Some(name) = req.name.as_deref().filter(|n| !n.is_empty()) {
        args.push("--name".into());
        args.push(name.into());
    }
    if let Some(code) = req.code.as_deref().filter(|c| !c.is_empty()) {
        args.push("--code".into());
        args.push(code.into());
    }
    args
}

fn decode_line(line: &str) -> Option<ShareEvent> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("event").and_then(|e| e.as_str()) {
        Some("stats") => Some(ShareEvent::Stats { data: v }),
        Some("connected") => Some(ShareEvent::Connected {
            peer: v.get("peer").and_then(|p| p.as_str()).unwrap_or("").to_string(),
        }),
        Some("waiting") => Some(ShareEvent::Waiting {
            code: v.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string(),
            name: v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
        }),
        Some("paired") => Some(ShareEvent::Paired {
            sender: v.get("sender").and_then(|s| s.as_str()).unwrap_or("").to_string(),
        }),
        Some("error") => Some(ShareEvent::Error {
            message: v.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string(),
        }),
        Some("stopped") => Some(ShareEvent::Exited { ok: true, code: Some(0) }),
        other => {
            debug!(?other, "ignoring engine line");
            None
        }
    }
}

/// Discover Relay receivers on the LAN by running `relay-share`'s browser.
/// Returns the raw JSON array the engine prints.
pub fn discover_receivers(timeout_ms: u64) -> Result<serde_json::Value> {
    let bin = share_binary()?;
    let out = Command::new(&bin)
        .arg("discover")
        .arg("--timeout-ms")
        .arg(timeout_ms.to_string())
        .stderr(Stdio::null())
        .output()
        .context("running discover")?;
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing discover output")?;
    Ok(v)
}

/// Drain forwarded events, applying them to a callback until the channel closes.
pub fn pump(rx: Receiver<ShareEvent>, mut on_event: impl FnMut(ShareEvent)) {
    while let Ok(ev) = rx.recv() {
        on_event(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_request_minimal_json_gets_defaults() {
        let req: ShareRequest = serde_json::from_str(r#"{"code":"123456"}"#).unwrap();
        assert_eq!(req.code, "123456");
        assert_eq!(req.peer, None);
        assert_eq!(req.bitrate_mbps, 60);
        assert_eq!(req.fps, 60);
        assert!(req.audio);
        assert_eq!(req.audio_pid, None);
        assert!(req.cursor);
    }

    #[test]
    fn send_args_default_request() {
        let req: ShareRequest = serde_json::from_str(r#"{"code":"123456"}"#).unwrap();
        assert_eq!(send_args(&req), ["send", "--code", "123456", "--bitrate", "60", "--fps", "60"]);
    }

    #[test]
    fn send_args_full_request() {
        let req: ShareRequest = serde_json::from_str(
            r#"{"code":"1","peer":"den-pc","bitrate_mbps":80,"fps":30,"audio":true,"audio_pid":4321,"cursor":false}"#,
        )
        .unwrap();
        assert_eq!(
            send_args(&req),
            [
                "send",
                "--code",
                "1",
                "--peer",
                "den-pc",
                "--bitrate",
                "80",
                "--fps",
                "30",
                "--audio-pid",
                "4321",
                "--no-cursor"
            ]
        );
    }

    #[test]
    fn send_args_no_audio_suppresses_audio_pid() {
        let req: ShareRequest =
            serde_json::from_str(r#"{"code":"1","audio":false,"audio_pid":4321}"#).unwrap();
        let args = send_args(&req);
        assert!(args.contains(&"--no-audio".to_string()));
        assert!(!args.iter().any(|a| a == "--audio-pid"));
    }

    #[test]
    fn send_args_empty_peer_is_skipped() {
        let req: ShareRequest = serde_json::from_str(r#"{"code":"1","peer":""}"#).unwrap();
        assert!(!send_args(&req).iter().any(|a| a == "--peer"));
    }

    #[test]
    fn recv_args_variants() {
        let req: ReceiveRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv"]);

        let req: ReceiveRequest =
            serde_json::from_str(r#"{"name":"den-pc","code":"555555"}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv", "--name", "den-pc", "--code", "555555"]);

        // Empty strings behave like absent fields.
        let req: ReceiveRequest = serde_json::from_str(r#"{"name":"","code":""}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv"]);
    }

    #[test]
    fn decode_line_maps_engine_events() {
        let ev = decode_line(r#"{"event":"stats","bitrate_mbps":57.2,"fps":60.0}"#).unwrap();
        let ShareEvent::Stats { data } = ev else { panic!("want Stats") };
        assert_eq!(data["bitrate_mbps"], 57.2);

        let ev = decode_line(r#"{"event":"connected","peer":"den-pc","rtt_ms":0.4}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Connected { peer } if peer == "den-pc"));

        let ev = decode_line(r#"{"event":"waiting","code":"123456","name":"jake"}"#).unwrap();
        assert!(
            matches!(ev, ShareEvent::Waiting { code, name } if code == "123456" && name == "jake")
        );

        let ev = decode_line(r#"{"event":"paired","sender":"jake"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Paired { sender } if sender == "jake"));

        let ev = decode_line(r#"{"event":"error","where":"video","message":"boom"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Error { message } if message == "boom"));

        let ev = decode_line(r#"{"event":"stopped"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Exited { ok: true, code: Some(0) }));
    }

    #[test]
    fn decode_line_tolerates_missing_fields_and_junk() {
        // Missing fields fall back to empty strings, not a dropped event.
        let ev = decode_line(r#"{"event":"connected"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Connected { peer } if peer.is_empty()));
        let ev = decode_line(r#"{"event":"error"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Error { message } if message == "error"));

        // Unknown events, non-JSON, and JSON without `event` are ignored.
        assert!(decode_line(r#"{"event":"video_up","encoder":"NVIDIA"}"#).is_none());
        assert!(decode_line(r#"{"event":"link","kind":"wired"}"#).is_none());
        assert!(decode_line("not json at all").is_none());
        assert!(decode_line(r#"{"no_event":true}"#).is_none());
        assert!(decode_line("").is_none());
    }

    #[test]
    fn share_event_serializes_tagged_for_the_ui() {
        // The Tauri shell matches on `event` — lock the tag format.
        let s = serde_json::to_string(&ShareEvent::Exited { ok: false, code: Some(1) }).unwrap();
        assert_eq!(s, r#"{"event":"exited","ok":false,"code":1}"#);
        let s = serde_json::to_string(&ShareEvent::Waiting {
            code: "123456".into(),
            name: "jake".into(),
        })
        .unwrap();
        assert_eq!(s, r#"{"event":"waiting","code":"123456","name":"jake"}"#);
    }
}
