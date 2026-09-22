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
    /// Six-digit code from the receiver. May be empty when `peer_id` names a
    /// remembered PC (S35).
    #[serde(default)]
    pub code: String,
    /// A remembered peer (`relay_core::peers`) to connect to without a code.
    /// The service resolves it to the receiver's name and fingerprint; the
    /// client only ever names the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// The remembered receiver's DTLS fingerprint, resolved by the service
    /// from `peer_id`. Skipped by serde on purpose: a client cannot supply
    /// it, so the only way to connect without a code is through a peer the
    /// store actually holds.
    #[serde(skip)]
    pub trusted: Option<String>,
    #[serde(default = "default_bitrate")]
    pub bitrate_mbps: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Encode size cap `(w, h)`; `None` = native capture size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<(u32, u32)>,
    #[serde(default = "default_true")]
    pub audio: bool,
    /// Capture just this process's audio (game-only) instead of the desktop mix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_pid: Option<u32>,
    /// Also send the default microphone, as a second Opus track alongside
    /// the desktop/game mix rather than instead of it. Mic-only is `audio:
    /// false` with `mic: true`.
    #[serde(default)]
    pub mic: bool,
    /// Also send everything on the PC except the shared app, as a third
    /// track (S37). Only meaningful with `audio_pid`; ignored otherwise.
    #[serde(default)]
    pub rest: bool,
    #[serde(default = "default_true")]
    pub cursor: bool,
    /// The preset this request was resolved from (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Start continuous recording with the share.
    #[serde(default)]
    pub record: bool,
    /// Replay ring window in seconds; 0 = off.
    #[serde(default)]
    pub replay_secs: u32,
    /// Thumbnails per second for the app window's preview. 0 = off, which is
    /// the default: the engine does no readback at all unless asked.
    #[serde(default)]
    pub preview_fps: u32,
    /// Recording folder; `None` disables recording and the replay ring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_dir: Option<String>,
    /// Container for recordings and replay saves.
    #[serde(default)]
    pub container: RecordingContainer,
}

/// Container the recorder muxes into. Both carry the identical HEVC + Opus
/// bitstream teed off the share — the choice never re-encodes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingContainer {
    /// Fragmented MP4. The default: widest tool support.
    #[default]
    Mp4,
    /// Matroska. Survives a crash mid-file — a recording cut off without a
    /// clean stop still imports into editors, where an fMP4 does not. See
    /// `docs/dev/container-compat.md`.
    Mkv,
}

impl RecordingContainer {
    /// File extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
        }
    }
}

/// Mirror of `relay_capture::command::SourceTarget` (the core does not link
/// the capture crate); the wire shape is locked by tests on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceTarget {
    Display { index: usize },
    Window { hwnd: u64 },
    Region { display: usize, x: u32, y: u32, w: u32, h: u32 },
}

/// Thumbnails per second the app window asks for. Two is enough to see what
/// you are sharing and cheap enough to be invisible in the latency budget.
pub const DEFAULT_PREVIEW_FPS: u32 = 2;

/// Mirror of `relay_capture::command::FaderLevel` (S37).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FaderLevel {
    pub gain: f32,
    #[serde(default)]
    pub mute: bool,
}

/// Mirror of `relay_capture::command::FaderSet`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FaderSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<FaderLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<FaderLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic: Option<FaderLevel>,
}

/// Which engine a mixer command is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MixerSide {
    Send,
    Receive,
}

/// Mirror of `relay_capture::command::EngineCmd`, serialised onto the
/// engine's stdin one line at a time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EngineCmd {
    Stop,
    /// Per-track gain and mute, live, on either engine (S37).
    Mixer {
        faders: FaderSet,
    },
    Record {
        on: bool,
    },
    ReplaySave,
    Switch {
        target: SourceTarget,
    },
    /// Retune the in-app preview: thumbnails per second, 0 = off.
    Preview {
        fps: u32,
    },
    /// Receiver only: embed the stream window in the app window `owner`, or
    /// pop it out into a window of its own (S29).
    Host {
        mode: HostMode,
        #[serde(default)]
        owner: u64,
    },
}

/// Mirror of `relay_capture::command::HostMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostMode {
    Embedded,
    Popout,
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
    /// Mirror decoded video into the Relay virtual camera. The service sets
    /// this from the consent + registration state, not the client.
    #[serde(default)]
    pub vcam: bool,
    /// Render decoded audio to this endpoint id (interim virtual-mic route).
    /// Also service-set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_route: Option<String>,
    /// The app window's HWND. When set, the engine creates the stream window
    /// embedded in it (S29) instead of as a window of its own. The Tauri
    /// shell fills this in; the core passes it through as `--host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<u64>,
}

/// Lines the engine emits (a decoded subset of the child's NDJSON, plus process lifecycle).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ShareEvent {
    /// One `stats` line from the engine, forwarded verbatim to the strip.
    Stats { data: serde_json::Value },
    /// The engine connected to a receiver. `trusted`: without a code, as a
    /// remembered PC (S35).
    Connected {
        peer: String,
        #[serde(default)]
        trusted: bool,
    },
    /// Receiver is advertising and waiting with this pairing code.
    Waiting { code: String, name: String },
    /// Receiver paired with a sender. `trusted`: it connected without a code
    /// and DTLS proved it was the PC we remembered.
    Paired {
        sender: String,
        #[serde(default)]
        trusted: bool,
    },
    /// The engine exited; `ok` is false on crash or non-zero exit.
    Exited { ok: bool, code: Option<i32> },
    /// A structured error line from the engine.
    Error { message: String },
    /// Continuous recording started or stopped (with the file path when on).
    Recording { on: bool, path: Option<String> },
    /// A replay clip landed on disk.
    ReplaySaved { path: String, ms: u64 },
    /// The capture source switched (verbatim target JSON for the UI).
    SourceChanged { data: serde_json::Value },
    /// A JPEG thumbnail of what is being captured, base64 in the engine line.
    Preview { width: u32, height: u32, jpeg: String },
    /// The video codec the share negotiated: `"hevc"` or `"h264"`.
    Codec { codec: String },
    /// The receiver's stream window exists (S29): its HWND, the stream size
    /// it was made for, and how it is hosted (`embedded`, `popout`, `none`).
    RenderUp { hwnd: u64, width: u32, height: u32, host: String, excluded_from_capture: bool },
    /// The receiver's window changed hosting mode, and whether Windows
    /// confirms it is excluded from screen capture (B9).
    Host { mode: String, hwnd: u64, excluded_from_capture: bool },
    /// The user closed (or pressed Esc in) the popped-out window. It is
    /// hidden, not gone: the shell embeds it again.
    HostClose,
}

/// The path to `relay-share`, assumed to sit next to `relay-core`.
pub fn share_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current exe")?;
    let dir = exe.parent().context("exe has no parent")?;
    let cand = dir.join(if cfg!(windows) { "relay-share.exe" } else { "relay-share" });
    Ok(cand)
}

/// What this PC can do with video, from `relay-share probe`. A share runs on
/// HEVC or H.264, negotiated per share (S27), so each half is possible when
/// *either* codec is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// GPUs with a display attached, so the "cannot share" banner can name the
    /// hardware it is talking about instead of saying "this GPU".
    pub adapters: Vec<String>,
    /// Hardware encoder MFTs for either codec. Empty = this PC cannot send.
    pub encoders: Vec<String>,
    /// Decoder MFTs for either codec. Empty = this PC cannot receive.
    pub decoders: Vec<String>,
    /// Codecs this PC can send, preference order: `"hevc"`, `"h264"`.
    pub share_codecs: Vec<String>,
    /// Codecs this PC can receive. Missing `"hevc"` is not a failure: shares
    /// to this PC negotiate H.264, at a higher bitrate for the same picture.
    pub receive_codecs: Vec<String>,
}

/// Ask the share engine what the machine supports. A short-lived child, so
/// this belongs on a screen-open, not a tick.
pub fn capabilities() -> Result<Capabilities> {
    let bin = share_binary()?;
    anyhow::ensure!(bin.exists(), "share engine not found at {}", bin.display());
    let out = Command::new(&bin).arg("probe").stderr(Stdio::null()).output().context("probing")?;
    anyhow::ensure!(out.status.success(), "relay-share probe exited with {}", out.status);
    Ok(parse_probe(&String::from_utf8_lossy(&out.stdout)))
}

/// Pull the friendly names out of a probe report. Written against the JSON
/// rather than the struct because the core does not link the capture crate.
fn parse_probe(json: &str) -> Capabilities {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Capabilities::default(),
    };
    // Deduplicated, order preserved: the probe enumerates sync and async MFTs
    // separately, so one adapter answering both is listed twice. That reads as
    // two GPUs to anyone shown the list.
    let names = |key: &str| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let Some(arr) = v.get(key).and_then(|a| a.as_array()) else {
            return out;
        };
        for name in arr.iter().filter_map(|e| e.get("friendly_name").and_then(|n| n.as_str())) {
            if !out.iter().any(|existing| existing == name) {
                out.push(name.to_string());
            }
        }
        out
    };
    // Prefer the "any decoder" list: the HEVC Video Extension and the H.264
    // decoder Windows ships are both software-category MFTs that decode on
    // the GPU once given a device.
    let mut hevc_decoders = names("hevc_any_decoders");
    if hevc_decoders.is_empty() {
        hevc_decoders = names("hevc_hardware_decoders");
    }
    let h264_decoders = names("h264_any_decoders");
    let hevc_encoders = names("hevc_hardware_encoders");
    let h264_encoders = names("h264_hardware_encoders");
    let adapters = v
        .get("adapters")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let union = |a: &[String], b: &[String]| -> Vec<String> {
        let mut out = a.to_vec();
        for n in b {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
        out
    };
    let codecs = |hevc: &[String], h264: &[String]| -> Vec<String> {
        [("hevc", hevc), ("h264", h264)]
            .into_iter()
            .filter(|(_, list)| !list.is_empty())
            .map(|(c, _)| c.to_string())
            .collect()
    };
    Capabilities {
        adapters,
        encoders: union(&hevc_encoders, &h264_encoders),
        decoders: union(&hevc_decoders, &h264_decoders),
        share_codecs: codecs(&hevc_encoders, &h264_encoders),
        receive_codecs: codecs(&hevc_decoders, &h264_decoders),
    }
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
        // `RELAY_RECEIVE_STUB=1`: a receiver that paints a moving pattern in
        // the real window with the real host commands and no stream. The
        // only way to exercise in-app hosting on one PC, where a real
        // receiver would capture itself (B9). Never set by the product.
        if std::env::var_os("RELAY_RECEIVE_STUB").is_some_and(|v| v == "1") {
            warn!("RELAY_RECEIVE_STUB is set: spawning the pattern stub, not a receiver");
            cmd.args(stub_args(req));
        } else {
            cmd.args(recv_args(req));
        }
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

    /// Send one command line to the engine's stdin (record toggle, replay
    /// save, source switch).
    pub fn command(&mut self, cmd: &EngineCmd) -> Result<()> {
        let stdin = self.stdin.as_mut().context("engine stdin already closed")?;
        let mut line = serde_json::to_vec(cmd)?;
        line.push(b'\n');
        stdin.write_all(&line)?;
        stdin.flush()?;
        Ok(())
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
    let mut args = vec!["send".into()];
    match req.trusted.as_deref() {
        // A remembered receiver: its fingerprint stands in for the code.
        Some(fp) => {
            args.push("--trusted".into());
            args.push(fp.into());
        }
        None => {
            args.push("--code".into());
            args.push(req.code.clone());
        }
    }
    if let Some(peer) = req.peer.as_deref().filter(|p| !p.is_empty()) {
        args.push("--peer".into());
        args.push(peer.into());
    }
    args.push("--bitrate".into());
    args.push(req.bitrate_mbps.to_string());
    args.push("--fps".into());
    args.push(req.fps.to_string());
    if let Some((w, h)) = req.size {
        args.push("--size".into());
        args.push(format!("{w}x{h}"));
    }
    if !req.audio {
        args.push("--no-audio".into());
    } else if let Some(pid) = req.audio_pid {
        args.push("--audio-pid".into());
        args.push(pid.to_string());
    }
    // Independent of the program source: `--audio-mic` adds a track.
    if req.mic {
        args.push("--audio-mic".into());
    }
    // Only with an app to be the rest of: the engine ignores it otherwise,
    // and not sending it keeps the line identical for every existing preset.
    if req.rest && req.audio && req.audio_pid.is_some() {
        args.push("--audio-rest".into());
    }
    if !req.cursor {
        args.push("--no-cursor".into());
    }
    if let Some(dir) = req.record_dir.as_deref().filter(|d| !d.is_empty()) {
        args.push("--record-dir".into());
        args.push(dir.into());
        if req.record {
            args.push("--record".into());
        }
        if req.replay_secs > 0 {
            args.push("--replay-secs".into());
            args.push(req.replay_secs.to_string());
        }
        if req.container != RecordingContainer::Mp4 {
            args.push("--container".into());
            args.push(req.container.extension().into());
        }
    }
    if req.preview_fps > 0 {
        args.push("--preview-fps".into());
        args.push(req.preview_fps.to_string());
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
    if req.vcam {
        args.push("--vcam".into());
    }
    if let Some(ep) = req.mic_route.as_deref().filter(|e| !e.is_empty()) {
        args.push("--mic-route".into());
        args.push(ep.into());
    }
    if let Some(h) = req.host.filter(|h| *h != 0) {
        args.push("--host".into());
        args.push(h.to_string());
    }
    args
}

/// The `relay-share host-stub` command line: the same flags as `recv`, on the
/// stand-in that decodes nothing. Replaces the `recv` line entirely.
fn stub_args(req: &ReceiveRequest) -> Vec<String> {
    let mut args = vec!["host-stub".to_string()];
    if let Some(h) = req.host.filter(|h| *h != 0) {
        args.push("--host".into());
        args.push(h.to_string());
    }
    args
}

fn decode_line(line: &str) -> Option<ShareEvent> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("event").and_then(|e| e.as_str()) {
        Some("stats") => Some(ShareEvent::Stats { data: v }),
        Some("connected") => Some(ShareEvent::Connected {
            peer: v.get("peer").and_then(|p| p.as_str()).unwrap_or("").to_string(),
            trusted: v.get("trusted").and_then(|t| t.as_bool()).unwrap_or(false),
        }),
        Some("waiting") => Some(ShareEvent::Waiting {
            code: v.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string(),
            name: v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
        }),
        Some("paired") => Some(ShareEvent::Paired {
            sender: v.get("sender").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            trusted: v.get("trusted").and_then(|t| t.as_bool()).unwrap_or(false),
        }),
        Some("error") => Some(ShareEvent::Error {
            message: v.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string(),
        }),
        Some("recording") => Some(ShareEvent::Recording {
            on: v.get("on").and_then(|o| o.as_bool()).unwrap_or(false),
            path: v.get("path").and_then(|p| p.as_str()).map(str::to_string),
        }),
        Some("source") => Some(ShareEvent::SourceChanged { data: v }),
        Some("preview") => Some(ShareEvent::Preview {
            width: v.get("width").and_then(|w| w.as_u64()).unwrap_or(0) as u32,
            height: v.get("height").and_then(|h| h.as_u64()).unwrap_or(0) as u32,
            jpeg: v.get("jpeg").and_then(|j| j.as_str())?.to_string(),
        }),
        Some("replay_saved") => Some(ShareEvent::ReplaySaved {
            path: v.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string(),
            ms: v.get("ms").and_then(|m| m.as_u64()).unwrap_or(0),
        }),
        Some("codec") => {
            Some(ShareEvent::Codec { codec: v.get("codec").and_then(|c| c.as_str())?.to_string() })
        }
        Some("render_up") => Some(ShareEvent::RenderUp {
            hwnd: v.get("hwnd").and_then(|h| h.as_u64()).unwrap_or(0),
            width: v.get("width").and_then(|w| w.as_u64()).unwrap_or(0) as u32,
            height: v.get("height").and_then(|h| h.as_u64()).unwrap_or(0) as u32,
            host: v.get("host").and_then(|h| h.as_str()).unwrap_or("none").to_string(),
            excluded_from_capture: v
                .get("excluded_from_capture")
                .and_then(|e| e.as_bool())
                .unwrap_or(false),
        }),
        Some("host") => Some(ShareEvent::Host {
            mode: v.get("mode").and_then(|m| m.as_str()).unwrap_or("none").to_string(),
            hwnd: v.get("hwnd").and_then(|h| h.as_u64()).unwrap_or(0),
            excluded_from_capture: v
                .get("excluded_from_capture")
                .and_then(|e| e.as_bool())
                .unwrap_or(false),
        }),
        Some("host_close") => Some(ShareEvent::HostClose),
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
    fn send_args_size_mic_and_recording() {
        let req: ShareRequest = serde_json::from_str(
            r#"{"code":"1","size":[2560,1440],"mic":true,"record":true,"replay_secs":60,"record_dir":"C:\\V\\Relay"}"#,
        )
        .unwrap();
        assert_eq!(
            send_args(&req),
            [
                "send",
                "--code",
                "1",
                "--bitrate",
                "60",
                "--fps",
                "60",
                "--size",
                "2560x1440",
                "--audio-mic",
                "--record-dir",
                "C:\\V\\Relay",
                "--record",
                "--replay-secs",
                "60",
            ]
        );

        // No record_dir → no recording flags at all, even if record was set.
        let req: ShareRequest =
            serde_json::from_str(r#"{"code":"1","record":true,"replay_secs":60}"#).unwrap();
        let args = send_args(&req);
        assert!(!args.iter().any(|a| a.starts_with("--record") || a == "--replay-secs"));

        // Game-only audio and the mic now travel together: two tracks.
        let req: ShareRequest =
            serde_json::from_str(r#"{"code":"1","mic":true,"audio_pid":42}"#).unwrap();
        let args = send_args(&req);
        assert!(args.contains(&"--audio-pid".to_string()));
        assert!(args.contains(&"--audio-mic".to_string()));
    }

    /// Mic-only: the engine gets `--no-audio --audio-mic`, which is what it
    /// used to get from a legacy `mic` preset, so nothing changes for one.
    #[test]
    fn send_args_mic_without_program_audio() {
        let req: ShareRequest =
            serde_json::from_str(r#"{"code":"1","audio":false,"mic":true}"#).unwrap();
        let args = send_args(&req);
        assert!(args.contains(&"--no-audio".to_string()));
        assert!(args.contains(&"--audio-mic".to_string()));
    }

    /// S37: the rest-of-PC track only rides beside a game, never the desktop
    /// mix, and only when asked for -- every existing preset's line is
    /// byte-identical.
    #[test]
    fn send_args_rest_only_beside_a_game() {
        let with: ShareRequest =
            serde_json::from_str(r#"{"code":"1","audio_pid":4321,"rest":true}"#).unwrap();
        assert!(send_args(&with).contains(&"--audio-rest".to_string()));
        let desktop: ShareRequest = serde_json::from_str(r#"{"code":"1","rest":true}"#).unwrap();
        assert!(!send_args(&desktop).contains(&"--audio-rest".to_string()));
        let plain: ShareRequest = serde_json::from_str(r#"{"code":"1","audio_pid":4321}"#).unwrap();
        assert!(!send_args(&plain).contains(&"--audio-rest".to_string()));
    }

    /// S37: the mixer command's wire shape, as the engine parses it. A fader
    /// left out is left out, so a slider move is one field.
    #[test]
    fn mixer_wire_shape_is_locked() {
        let cmd = EngineCmd::Mixer {
            faders: FaderSet {
                app: None,
                rest: Some(FaderLevel { gain: 0.5, mute: false }),
                mic: Some(FaderLevel { gain: 1.0, mute: true }),
            },
        };
        assert_eq!(
            serde_json::to_string(&cmd).unwrap(),
            r#"{"cmd":"mixer","faders":{"rest":{"gain":0.5,"mute":false},"mic":{"gain":1.0,"mute":true}}}"#
        );
    }

    /// And no mic asked for means no mic flag: single-track peers and
    /// presets are untouched.
    #[test]
    fn send_args_without_mic_are_unchanged() {
        let req: ShareRequest = serde_json::from_str(r#"{"code":"1"}"#).unwrap();
        assert!(!send_args(&req).contains(&"--audio-mic".to_string()));
    }

    /// Shape copied from a real `relay-share probe` run on this dev machine.
    #[test]
    fn probe_report_maps_to_capabilities() {
        let json = r#"{
          "adapters": ["Intel(R) UHD Graphics 630", "NVIDIA GeForce RTX 4060"],
          "hevc_hardware_encoders": [
            {"friendly_name":"NVIDIA HEVC Encoder MFT","hardware_url":"vidpn"}
          ],
          "hevc_hardware_decoders": [],
          "hevc_any_decoders": [
            {"friendly_name":"Microsoft HEVC Video Extension","hardware_url":null}
          ],
          "wgc_supported": true
        }"#;
        let c = parse_probe(json);
        assert_eq!(c.adapters, ["Intel(R) UHD Graphics 630", "NVIDIA GeForce RTX 4060"]);
        assert_eq!(c.encoders, ["NVIDIA HEVC Encoder MFT"]);
        assert_eq!(c.decoders, ["Microsoft HEVC Video Extension"]);
        assert_eq!(c.decoders, ["Microsoft HEVC Video Extension"], "software MFT still counts");

        // No extension installed: receiving is off, sharing is unaffected.
        let json = json.replace(
            r#"{"friendly_name":"Microsoft HEVC Video Extension","hardware_url":null}"#,
            "",
        );
        let c = parse_probe(&json);
        assert!(c.decoders.is_empty());

        assert_eq!(c.encoders.len(), 1);

        // A probe report without the key (an older engine) still parses; the
        // banner falls back to "this PC's GPU" rather than breaking.
        let c = parse_probe(r#"{"hevc_hardware_encoders":[],"hevc_any_decoders":[]}"#);
        assert!(c.adapters.is_empty());

        // A probe that failed to produce JSON reads as "nothing", never a panic.
        assert_eq!(parse_probe("boom"), Capabilities::default());
        assert_eq!(parse_probe(""), Capabilities::default());

        // One adapter answering both the sync and async MFT enumerations is
        // listed twice -- exactly what this dev machine reports. Listing it
        // twice would read as two GPUs.
        let dupes = r#"{
          "hevc_hardware_encoders": [
            {"friendly_name":"AMDh265Encoder","hardware_url":"vidpn"},
            {"friendly_name":"NVIDIA HEVC Encoder MFT","hardware_url":"vidpn"},
            {"friendly_name":"AMDh265Encoder","hardware_url":"vidpn"}
          ],
          "hevc_any_decoders": [],
          "hevc_hardware_decoders": []
        }"#;
        let c = parse_probe(dupes);
        assert_eq!(
            c.encoders,
            ["AMDh265Encoder", "NVIDIA HEVC Encoder MFT"],
            "deduplicated, first-seen order kept"
        );
    }

    /// S27: a PC with an HEVC encoder but no HEVC decoder -- the Windows 10
    /// test PC exactly -- can still receive, over H.264.
    #[test]
    fn h264_makes_a_pc_without_hevc_decode_a_receiver() {
        let json = r#"{
          "adapters": ["NVIDIA GeForce RTX 2080"],
          "hevc_hardware_encoders": [{"friendly_name":"NVIDIA HEVC Encoder MFT"}],
          "hevc_hardware_decoders": [],
          "hevc_any_decoders": [],
          "h264_hardware_encoders": [{"friendly_name":"NVIDIA H.264 Encoder MFT"}],
          "h264_any_decoders": [{"friendly_name":"Microsoft H264 Video Decoder MFT"}],
          "wgc_supported": true
        }"#;
        let c = parse_probe(json);
        assert_eq!(c.share_codecs, ["hevc", "h264"]);
        assert_eq!(c.receive_codecs, ["h264"], "no HEVC decode, and that is fine");
        assert_eq!(c.encoders, ["NVIDIA HEVC Encoder MFT", "NVIDIA H.264 Encoder MFT"]);
        assert_eq!(c.decoders, ["Microsoft H264 Video Decoder MFT"]);

        // An engine from before S27 reports HEVC only; codecs follow from it.
        let old = r#"{"hevc_hardware_encoders":[{"friendly_name":"X"}],"hevc_any_decoders":[]}"#;
        let c = parse_probe(old);
        assert_eq!(c.share_codecs, ["hevc"]);
        assert!(c.receive_codecs.is_empty());
    }

    #[test]
    fn codec_line_decodes() {
        let ev = decode_line(r#"{"event":"codec","codec":"h264"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Codec { codec } if codec == "h264"));
        // The sender's version also lists what was offered and answered.
        let ev = decode_line(
            r#"{"event":"codec","codec":"hevc","offered":["hevc"],"answered":["hevc"]}"#,
        );
        assert!(matches!(ev, Some(ShareEvent::Codec { codec }) if codec == "hevc"));
        assert!(decode_line(r#"{"event":"codec"}"#).is_none(), "no codec, no event");
    }

    /// Locks the stdin command strings to `relay_capture::command`'s shapes.
    #[test]
    fn engine_cmd_wire_matches_the_capture_side() {
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
                target: SourceTarget::Region { display: 0, x: 1, y: 2, w: 3, h: 4 }
            })
            .unwrap(),
            r#"{"cmd":"switch","target":{"kind":"region","display":0,"x":1,"y":2,"w":3,"h":4}}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Preview { fps: DEFAULT_PREVIEW_FPS }).unwrap(),
            r#"{"cmd":"preview","fps":2}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Preview { fps: 0 }).unwrap(),
            r#"{"cmd":"preview","fps":0}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Switch {
                target: SourceTarget::Window { hwnd: 20958 }
            })
            .unwrap(),
            r#"{"cmd":"switch","target":{"kind":"window","hwnd":20958}}"#
        );
        assert_eq!(serde_json::to_string(&EngineCmd::Stop).unwrap(), r#"{"cmd":"stop"}"#);
    }

    #[test]
    fn decode_line_maps_recording_events() {
        let ev =
            decode_line(r#"{"event":"recording","on":true,"path":"C:\\V\\Relay\\a.mp4"}"#).unwrap();
        assert!(
            matches!(ev, ShareEvent::Recording { on: true, path: Some(p) } if p.ends_with("a.mp4"))
        );
        let ev = decode_line(r#"{"event":"replay_saved","path":"r.mp4","ms":420}"#).unwrap();
        assert!(matches!(ev, ShareEvent::ReplaySaved { path, ms: 420 } if path == "r.mp4"));
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

        // Virtual-device routing (service-set).
        let req: ReceiveRequest =
            serde_json::from_str(r#"{"vcam":true,"mic_route":"{0.0.0.00000000}.{ep}"}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv", "--vcam", "--mic-route", "{0.0.0.00000000}.{ep}"]);
        let req: ReceiveRequest = serde_json::from_str(r#"{"host":133742}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv", "--host", "133742"]);
        assert_eq!(stub_args(&req), ["host-stub", "--host", "133742"]);
        // A zero handle is "no window", not a window called 0.
        let req: ReceiveRequest = serde_json::from_str(r#"{"host":0}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv"]);
        let req: ReceiveRequest = serde_json::from_str(r#"{"mic_route":""}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv"]);
    }

    #[test]
    fn decode_line_maps_engine_events() {
        let ev = decode_line(r#"{"event":"stats","bitrate_mbps":57.2,"fps":60.0}"#).unwrap();
        let ShareEvent::Stats { data } = ev else { panic!("want Stats") };
        assert_eq!(data["bitrate_mbps"], 57.2);

        let ev = decode_line(r#"{"event":"connected","peer":"den-pc","rtt_ms":0.4}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Connected { peer, .. } if peer == "den-pc"));

        let ev = decode_line(r#"{"event":"waiting","code":"123456","name":"jake"}"#).unwrap();
        assert!(
            matches!(ev, ShareEvent::Waiting { code, name } if code == "123456" && name == "jake")
        );

        let ev = decode_line(r#"{"event":"paired","sender":"jake"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Paired { sender, .. } if sender == "jake"));

        let ev = decode_line(r#"{"event":"error","where":"video","message":"boom"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Error { message } if message == "boom"));

        let ev = decode_line(r#"{"event":"stopped"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Exited { ok: true, code: Some(0) }));
    }

    #[test]
    fn paired_and_connected_carry_whether_a_code_was_used() {
        // S35: `trusted` says the peer connected as a remembered PC. Absent
        // (an engine that predates it) means a code was used.
        let ev = decode_line(r#"{"event":"paired","sender":"jake","trusted":true}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Paired { trusted: true, .. }));
        let ev = decode_line(r#"{"event":"paired","sender":"jake"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Paired { trusted: false, .. }));
        let ev = decode_line(r#"{"event":"connected","peer":"den-pc","trusted":true}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Connected { trusted: true, .. }));
    }

    #[test]
    fn decode_line_tolerates_missing_fields_and_junk() {
        // Missing fields fall back to empty strings, not a dropped event.
        let ev = decode_line(r#"{"event":"connected"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Connected { peer, .. } if peer.is_empty()));
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
