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

/// Lines the engine emits (a decoded subset of the child's NDJSON, plus process lifecycle).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ShareEvent {
    /// One `stats` line from the engine, forwarded verbatim to the strip.
    Stats { data: serde_json::Value },
    /// The engine connected to a receiver.
    Connected { peer: String },
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
        cmd.arg("send").arg("--code").arg(&req.code);
        if let Some(peer) = req.peer.as_deref().filter(|p| !p.is_empty()) {
            cmd.arg("--peer").arg(peer);
        }
        cmd.arg("--bitrate").arg(req.bitrate_mbps.to_string());
        cmd.arg("--fps").arg(req.fps.to_string());
        if !req.audio {
            cmd.arg("--no-audio");
        } else if let Some(pid) = req.audio_pid {
            cmd.arg("--audio-pid").arg(pid.to_string());
        }
        if !req.cursor {
            cmd.arg("--no-cursor");
        }
        // The engine watches for a closed stdin to know the core died.
        cmd.env("RELAY_SPAWNED", "1");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());

        let mut child = cmd.spawn().with_context(|| format!("spawning {}", bin.display()))?;
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

fn decode_line(line: &str) -> Option<ShareEvent> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("event").and_then(|e| e.as_str()) {
        Some("stats") => Some(ShareEvent::Stats { data: v }),
        Some("connected") => Some(ShareEvent::Connected {
            peer: v.get("peer").and_then(|p| p.as_str()).unwrap_or("").to_string(),
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
