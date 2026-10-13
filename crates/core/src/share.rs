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
    /// Which process `audio_pid` meant (r54): set by the service when the
    /// share starts, checked against the live process before every spawn and
    /// again in the engine, so a resumed share never captures a recycled PID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_app: Option<crate::proc_identity::ProcIdentity>,
    /// Also send the default microphone, as a second Opus track alongside
    /// the desktop/game mix rather than instead of it. Mic-only is `audio:
    /// false` with `mic: true`.
    #[serde(default)]
    pub mic: bool,
    /// Also send everything on the PC except the shared app, as a third
    /// track (S37). Only meaningful with `audio_pid`; ignored otherwise.
    #[serde(default)]
    pub rest: bool,
    /// Also present the captured picture as "Relay Camera" on this PC (S36),
    /// for a streaming program running here. The service settles it from the
    /// preset's wish, consent, registration and whether a receive already
    /// holds the camera; a client's value is only a wish.
    #[serde(default)]
    pub vcam: bool,
    /// Also publish the share as an NDI® source on the LAN (S51). The
    /// service sets it from the saved Share setting; a client's value is
    /// overwritten.
    #[serde(default)]
    pub ndi: bool,
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
    /// The microphone endpoint (S40). The service fills it from the saved
    /// mixer choice; `None` = the System default. A saved device that is
    /// unplugged falls back to the default in the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_device: Option<String>,
    /// Where the call coming back plays (S40); `None` = the System default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    /// The source to start on; `None` = the primary display. The service
    /// keeps the share's intent record on whatever the engine last switched
    /// to, so a reconnect resumes that source instead of the desktop (r59).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceTarget>,
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
    /// The call coming back from the receiver (S19); sender side only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<FaderLevel>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EngineCmd {
    Stop,
    /// Per-track gain and mute, live, on either engine (S37).
    Mixer {
        faders: FaderSet,
    },
    /// Point a device-backed track at an endpoint, or back at the System
    /// default (`None`), live (S40).
    Device {
        track: DeviceTrack,
        #[serde(default)]
        device: Option<String>,
    },
    Record {
        on: bool,
    },
    ReplaySave,
    Switch {
        target: SourceTarget,
    },
    /// Receiver only (S43b): start or stop Relay Camera live, waiting or
    /// mid-share, with no reconnect.
    Vcam {
        on: bool,
    },
    /// Receiver only (r54): stop sending the call app's audio back, live,
    /// with no restart. Only `false` is acted on: turning a return route on
    /// needs a new track, which needs a new connection.
    Return {
        on: bool,
    },
    /// Either engine (S51): publish as an NDI® source, or stop, live.
    Ndi {
        on: bool,
    },
    /// Retune the in-app preview: thumbnails per second, 0 = off.
    Preview {
        fps: u32,
    },
    /// Receiver only: embed the stream window in the app window `owner`, or
    /// pop it out into a window of its own (S29), or make it a clean feed of
    /// a fixed size for call apps (S50).
    Host {
        mode: HostMode,
        #[serde(default)]
        owner: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feed: Option<CleanFeed>,
    },
    /// Receiver only (S50): what this PC is sharing now, `None` = nothing.
    /// The stream window is hidden from capture only while it is covered.
    LocalShare {
        #[serde(default)]
        target: Option<SourceTarget>,
    },
}

/// Mirror of `relay_capture::command::DeviceTrack` (S40): the sender's mic
/// input, or where an engine plays audio (the receiver's received mix, the
/// sender's call return).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceTrack {
    Mic,
    Output,
}

/// One active audio endpoint, for the mixer's device pickers (S40).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
}

/// Active endpoints in both directions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevices {
    pub render: Vec<AudioDevice>,
    pub capture: Vec<AudioDevice>,
}

/// Mirror of `relay_capture::command::HostMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostMode {
    Embedded,
    Popout,
    /// A borderless window of a fixed size for call apps and OBS (S50).
    Clean,
}

/// Mirror of `relay_capture::command::CleanFeed`: a clean feed's fixed
/// client size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CleanFeed {
    #[default]
    #[serde(rename = "1920x1080")]
    Fhd,
    #[serde(rename = "2560x1440")]
    Qhd,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Publish the received stream as an NDI® source, "Relay (from
    /// <sender>)" (S51). Service-set from the saved Receive setting.
    #[serde(default)]
    pub ndi: bool,
    /// Render decoded audio to this endpoint id (interim virtual-mic route).
    /// Also service-set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_route: Option<String>,
    /// The app window's HWND. When set, the engine creates the stream window
    /// embedded in it (S29) instead of as a window of its own. The Tauri
    /// shell fills this in; the core passes it through as `--host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<u64>,
    /// Send the call app's output back to the sender (S19): the PID of the
    /// call app on this PC. `None` = no return route; an old request or a
    /// pre-S19 record reads as off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_pid: Option<u32>,
    /// Which process `return_pid` meant (r54): image path and creation time,
    /// pinned by the service when the user starts receiving and re-checked on
    /// every spawn and resume. Never trusted from a client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_app: Option<crate::proc_identity::ProcIdentity>,
    /// Where received audio plays (S40), from the saved mixer choice; `None`
    /// = the System default. `mic_route` wins when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    /// What this PC is sharing as the receiver starts (S50), passed as
    /// `--local-share`. Service-set from the running share, never from a
    /// client, and never persisted: a resumed receive is told afresh.
    #[serde(skip)]
    pub local_share: Option<SourceTarget>,
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
    /// The sender said goodbye: a deliberate stop, not a drop.
    SenderStopped,
    /// The other PC refused a no-code connection. Final: not a drop.
    Refused { message: String },
    /// Receiver: a PC tried the wrong code; the wait goes on under a new one.
    WrongCode { name: String },
    /// Receiver (r54): the call app's audio is not (or no longer) sent back.
    /// `reason` is `user` for a live Off, else why the engine refused it.
    ReturnOff { reason: String },
    /// Sender (r54): the shared app's audio is not captured, because its PID
    /// no longer names the process the core pinned.
    AudioOff { reason: String },
}

/// What the user is told when a call app picked earlier is gone (r54).
pub const RETURN_GONE_TEXT: &str =
    "The call app you picked has closed, so its audio is not sent back. Pick it again on Receive.";
/// What the user is told when the app whose sound was shared is gone (r54).
pub const AUDIO_GONE_TEXT: &str =
    "The app whose sound you were sharing has closed. The share goes on without its sound.";

impl ReceiveRequest {
    /// r54: pin the call app to the live process (a fresh Start), or check
    /// the pinned one still is that process (a resume, a restart, a replay).
    /// Anything that does not check out is dropped here, before an engine
    /// could capture it. `Some` = it was dropped, and why.
    pub fn settle_return(
        &mut self,
        fresh: bool,
        probe: &dyn crate::proc_identity::ProcessProbe,
    ) -> Option<crate::proc_identity::Stale> {
        use crate::proc_identity::{settle, Settled};
        match settle(self.return_pid, self.return_app.as_ref(), fresh, probe) {
            Settled::None => {
                self.return_pid = None;
                self.return_app = None;
                None
            }
            Settled::Keep(id) => {
                self.return_pid = Some(id.pid);
                self.return_app = Some(id);
                None
            }
            Settled::Drop(why) => {
                self.return_pid = None;
                self.return_app = None;
                Some(why)
            }
        }
    }
}

impl ShareRequest {
    /// r54: the same check for the app whose audio a share captures. A stale
    /// one leaves the share without program audio -- never the whole
    /// desktop mix instead, which could carry a call nobody chose to send.
    pub fn settle_audio_app(
        &mut self,
        fresh: bool,
        probe: &dyn crate::proc_identity::ProcessProbe,
    ) -> Option<crate::proc_identity::Stale> {
        use crate::proc_identity::{settle, Settled};
        if !self.audio {
            self.audio_app = None;
            return None;
        }
        match settle(self.audio_pid, self.audio_app.as_ref(), fresh, probe) {
            Settled::None => {
                self.audio_pid = None;
                self.audio_app = None;
                None
            }
            Settled::Keep(id) => {
                self.audio_pid = Some(id.pid);
                self.audio_app = Some(id);
                None
            }
            Settled::Drop(why) => {
                self.audio_pid = None;
                self.audio_app = None;
                self.audio = false;
                self.rest = false;
                Some(why)
            }
        }
    }
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
        // The engine re-checks the process right before it captures it.
        if let Some(app) = req.audio_app.as_ref().filter(|a| a.pid == pid) {
            args.push("--audio-pid-image".into());
            args.push(app.image.clone());
            args.push("--audio-pid-created".into());
            args.push(app.created.to_string());
        }
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
    if req.vcam {
        args.push("--vcam".into());
    }
    if req.ndi {
        args.push("--ndi".into());
    }
    if let Some(t) = req.source.filter(|t| *t != INITIAL_SHARE_TARGET) {
        args.push("--source".into());
        args.push(serde_json::to_string(&t).expect("a source target serialises"));
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
    // S40: only a pinned device goes on the line; the default is no flag.
    if req.mic {
        if let Some(id) = req.mic_device.as_deref().filter(|d| !d.is_empty()) {
            args.push("--mic-device".into());
            args.push(id.into());
        }
    }
    if let Some(id) = req.output_device.as_deref().filter(|d| !d.is_empty()) {
        args.push("--output-device".into());
        args.push(id.into());
    }
    args
}

/// S43b: the command that brings a running receiver in line with the camera
/// routing rule (`vdevice::receive_routing`, the same rule `--vcam` is
/// decided by at spawn). `None` when nothing is receiving. The receiver acts
/// only on a change, so sending the current state again is harmless.
pub fn receive_vcam_sync(receiving: bool, camera_ok: bool) -> Option<EngineCmd> {
    receiving.then_some(EngineCmd::Vcam { on: camera_ok })
}

/// S50: the `local_share` command that brings a running receiver in line
/// with what this PC is sharing now, or `None` when nothing needs saying —
/// not receiving, or the receiver was already told exactly this. `told` is
/// what it was last given (at spawn or by an earlier command).
pub fn local_share_sync(
    receiving: bool,
    told: Option<Option<SourceTarget>>,
    now: Option<SourceTarget>,
) -> Option<EngineCmd> {
    (receiving && told != Some(now)).then_some(EngineCmd::LocalShare { target: now })
}

/// The capture target a sender's `source` line reports, if it carries one.
pub fn source_target_of(data: &serde_json::Value) -> Option<SourceTarget> {
    data.get("target").and_then(|t| serde_json::from_value(t.clone()).ok())
}

/// What a sender captures before its first `source` line: the primary
/// display, as `relay_capture::transport::sender` starts on.
pub const INITIAL_SHARE_TARGET: SourceTarget = SourceTarget::Display { index: 0 };

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
    if req.ndi {
        args.push("--ndi".into());
    }
    if let Some(ep) = req.mic_route.as_deref().filter(|e| !e.is_empty()) {
        args.push("--mic-route".into());
        args.push(ep.into());
    }
    if let Some(h) = req.host.filter(|h| *h != 0) {
        args.push("--host".into());
        args.push(h.to_string());
    }
    if let Some(pid) = req.return_pid.filter(|p| *p != 0) {
        args.push("--return-pid".into());
        args.push(pid.to_string());
        if let Some(app) = req.return_app.as_ref().filter(|a| a.pid == pid) {
            args.push("--return-image".into());
            args.push(app.image.clone());
            args.push("--return-created".into());
            args.push(app.created.to_string());
        }
    }
    if let Some(id) = req.output_device.as_deref().filter(|d| !d.is_empty()) {
        args.push("--output-device".into());
        args.push(id.into());
    }
    push_local_share(&mut args, req);
    args
}

fn push_local_share(args: &mut Vec<String>, req: &ReceiveRequest) {
    if let Some(t) = req.local_share {
        if let Ok(json) = serde_json::to_string(&t) {
            args.push("--local-share".into());
            args.push(json);
        }
    }
}

/// The `relay-share host-stub` command line: the same flags as `recv`, on the
/// stand-in that decodes nothing. Replaces the `recv` line entirely.
fn stub_args(req: &ReceiveRequest) -> Vec<String> {
    let mut args = vec!["host-stub".to_string()];
    if let Some(h) = req.host.filter(|h| *h != 0) {
        args.push("--host".into());
        args.push(h.to_string());
    }
    push_local_share(&mut args, req);
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
        Some("error") if v.get("refused").and_then(|r| r.as_bool()) == Some(true) => {
            Some(ShareEvent::Refused {
                message: v.get("message").and_then(|m| m.as_str()).unwrap_or("refused").to_string(),
            })
        }
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
        Some("sender_stopped") => Some(ShareEvent::SenderStopped),
        Some("wrong_code") => Some(ShareEvent::WrongCode {
            name: v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
        }),
        Some("return_off") => Some(ShareEvent::ReturnOff {
            reason: v.get("reason").and_then(|r| r.as_str()).unwrap_or("").to_string(),
        }),
        Some("audio_off") => Some(ShareEvent::AudioOff {
            reason: v.get("reason").and_then(|r| r.as_str()).unwrap_or("").to_string(),
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
    #[test]
    fn vcam_command_wire_shape_and_when_sent() {
        // Must match relay_capture::command's own test byte for byte.
        assert_eq!(
            serde_json::to_string(&EngineCmd::Vcam { on: true }).unwrap(),
            r#"{"cmd":"vcam","on":true}"#
        );
        assert_eq!(receive_vcam_sync(false, true), None, "nothing receiving: nothing sent");
        assert_eq!(receive_vcam_sync(true, true), Some(EngineCmd::Vcam { on: true }));
        assert_eq!(receive_vcam_sync(true, false), Some(EngineCmd::Vcam { on: false }));
    }

    use super::*;

    mod r54 {
        use super::*;
        use crate::proc_identity::{ProcIdentity, ProcessProbe, Stale};

        struct One(Option<ProcIdentity>);
        impl ProcessProbe for One {
            fn identity(&self, pid: u32) -> Option<ProcIdentity> {
                self.0.clone().filter(|i| i.pid == pid)
            }
        }
        fn app(pid: u32, image: &str, created: u64) -> ProcIdentity {
            ProcIdentity { pid, image: image.into(), created }
        }
        fn recv(pid: Option<u32>, stored: Option<ProcIdentity>) -> ReceiveRequest {
            ReceiveRequest { return_pid: pid, return_app: stored, ..Default::default() }
        }

        #[test]
        fn a_resumed_receive_with_a_reused_pid_drops_the_route() {
            // PC2: 2772 was a test process; now it is someone else.
            let mut req = recv(Some(2772), Some(app(2772, r"C:\t\tone.exe", 1)));
            let probe = One(Some(app(2772, r"C:\d\Discord.exe", 2)));
            assert_eq!(req.settle_return(false, &probe), Some(Stale::Reused));
            assert_eq!((req.return_pid, req.return_app.clone()), (None, None));
            assert!(!recv_args(&req).iter().any(|a| a.starts_with("--return")));
        }

        #[test]
        fn the_same_exe_with_a_new_start_time_is_refused() {
            let mut req = recv(Some(10), Some(app(10, r"C:\d\Discord.exe", 1)));
            let probe = One(Some(app(10, r"C:\d\Discord.exe", 5)));
            assert_eq!(req.settle_return(false, &probe), Some(Stale::Reused));
            assert_eq!(req.return_pid, None);
        }

        #[test]
        fn a_pre_fix_record_with_a_bare_pid_is_dropped() {
            let mut req = recv(Some(2772), None);
            let probe = One(Some(app(2772, r"C:\d\Discord.exe", 2)));
            assert_eq!(req.settle_return(false, &probe), Some(Stale::Gone));
            assert_eq!(req.return_pid, None);
        }

        #[test]
        fn a_valid_call_app_is_kept_and_a_fresh_pick_is_pinned() {
            let live = app(4242, r"C:\d\Discord.exe", 7);
            let probe = One(Some(live.clone()));
            let mut req = recv(Some(4242), None);
            assert_eq!(req.settle_return(true, &probe), None);
            assert_eq!(req.return_app.as_ref(), Some(&live));
            // The pinned request resumes fine while the process lives.
            assert_eq!(req.settle_return(false, &probe), None);
            assert_eq!(req.return_pid, Some(4242));
            // A dead pick is dropped even when fresh.
            let mut dead = recv(Some(4243), None);
            assert_eq!(dead.settle_return(true, &probe), Some(Stale::Gone));
        }

        #[test]
        fn a_stale_shared_app_turns_program_audio_off_not_to_the_desktop_mix() {
            let mut req: ShareRequest =
                serde_json::from_str(r#"{"code":"1","audio_pid":4321,"rest":true}"#).unwrap();
            req.audio_app = Some(app(4321, r"C:\g\game.exe", 1));
            let probe = One(Some(app(4321, r"C:\other.exe", 9)));
            assert_eq!(req.settle_audio_app(false, &probe), Some(Stale::Reused));
            assert!(!req.audio && !req.rest && req.audio_pid.is_none());
            let args = send_args(&req);
            assert!(args.contains(&"--no-audio".to_string()));
            assert!(!args.iter().any(|a| a == "--audio-pid" || a == "--audio-rest"));
        }

        #[test]
        fn a_valid_shared_app_is_pinned_and_passed_on() {
            let live = app(4321, r"C:\g\game.exe", 3);
            let probe = One(Some(live.clone()));
            let mut req: ShareRequest =
                serde_json::from_str(r#"{"code":"1","audio_pid":4321}"#).unwrap();
            assert_eq!(req.settle_audio_app(true, &probe), None);
            let args = send_args(&req);
            let i = args.iter().position(|a| a == "--audio-pid-created").expect("identity passed");
            assert_eq!(args[i + 1], "3");
        }
    }

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
    fn ndi_command_and_flags() {
        // Must match relay_capture::command's own test byte for byte.
        assert_eq!(
            serde_json::to_string(&EngineCmd::Ndi { on: true }).unwrap(),
            r#"{"cmd":"ndi","on":true}"#
        );
        let mut req: ShareRequest = serde_json::from_str(r#"{"code":"1"}"#).unwrap();
        assert!(!req.ndi, "off unless the service turns it on");
        assert!(!send_args(&req).contains(&"--ndi".to_string()));
        req.ndi = true;
        assert!(send_args(&req).contains(&"--ndi".to_string()));
        let mut recv: ReceiveRequest = serde_json::from_str("{}").unwrap();
        assert!(!recv_args(&recv).contains(&"--ndi".to_string()));
        recv.ndi = true;
        assert_eq!(recv_args(&recv), ["recv", "--ndi"]);
    }

    /// r59: a reconnect starts on the source the share was on. The primary
    /// display is the engine's default and is not spelled out.
    #[test]
    fn send_args_carry_a_non_default_source() {
        let mut req: ShareRequest = serde_json::from_str(r#"{"code":"123456"}"#).unwrap();
        req.source = Some(SourceTarget::Window { hwnd: 0x51DE });
        let a = send_args(&req);
        let i = a.iter().position(|x| x == "--source").expect("--source");
        assert_eq!(a[i + 1], r#"{"kind":"window","hwnd":20958}"#);
        req.source = Some(INITIAL_SHARE_TARGET);
        assert!(!send_args(&req).iter().any(|x| x == "--source"));
        // Older intent files have no source and still parse.
        assert_eq!(req.source.and(None::<SourceTarget>), None);
        let old: ShareRequest = serde_json::from_str(r#"{"code":"1"}"#).unwrap();
        assert_eq!(old.source, None);
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
                call: None,
            },
        };
        assert_eq!(
            serde_json::to_string(&cmd).unwrap(),
            r#"{"cmd":"mixer","faders":{"rest":{"gain":0.5,"mute":false},"mic":{"gain":1.0,"mute":true}}}"#
        );
        // S19: the Call fader travels the same way, and only when mentioned.
        let cmd = EngineCmd::Mixer {
            faders: FaderSet {
                call: Some(FaderLevel { gain: 0.5, mute: false }),
                ..Default::default()
            },
        };
        assert_eq!(
            serde_json::to_string(&cmd).unwrap(),
            r#"{"cmd":"mixer","faders":{"call":{"gain":0.5,"mute":false}}}"#
        );
    }

    /// S36: the camera flag rides on the send line only when the service
    /// left it set; every existing request is byte-identical.
    #[test]
    fn send_args_vcam_only_when_asked() {
        let on: ShareRequest = serde_json::from_str(r#"{"code":"1","vcam":true}"#).unwrap();
        assert!(send_args(&on).contains(&"--vcam".to_string()));
        let off: ShareRequest = serde_json::from_str(r#"{"code":"1"}"#).unwrap();
        assert!(!send_args(&off).contains(&"--vcam".to_string()));
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
        // S19: the return route only when a call app was picked; 0 is "none".
        let req: ReceiveRequest = serde_json::from_str(r#"{"return_pid":4242}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv", "--return-pid", "4242"]);
        // r54: the pinned identity goes to the engine, which re-checks it.
        let req: ReceiveRequest = serde_json::from_str(
            r#"{"return_pid":4242,"return_app":{"pid":4242,"image":"C:\\d\\Discord.exe","created":99}}"#,
        )
        .unwrap();
        assert_eq!(
            recv_args(&req),
            [
                "recv",
                "--return-pid",
                "4242",
                "--return-image",
                r"C:\d\Discord.exe",
                "--return-created",
                "99"
            ]
        );
        let req: ReceiveRequest = serde_json::from_str(r#"{"return_pid":0}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv"]);
        // S40: a pinned output goes on the line; the default is no flag.
        let req: ReceiveRequest = serde_json::from_str(r#"{"output_device":"{spk}"}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv", "--output-device", "{spk}"]);
        let req: ReceiveRequest = serde_json::from_str(r#"{"output_device":""}"#).unwrap();
        assert_eq!(recv_args(&req), ["recv"]);
        // S50: what this PC is sharing, service-set; a client cannot send it.
        let mut req: ReceiveRequest =
            serde_json::from_str(r#"{"local_share":{"kind":"display","index":3}}"#).unwrap();
        assert_eq!(req.local_share, None, "not settable by a client");
        req.local_share = Some(SourceTarget::Display { index: 1 });
        assert_eq!(recv_args(&req), ["recv", "--local-share", r#"{"kind":"display","index":1}"#]);
        assert_eq!(
            stub_args(&req),
            ["host-stub", "--local-share", r#"{"kind":"display","index":1}"#]
        );
    }

    #[test]
    fn send_args_devices_only_when_pinned() {
        let mut req: ShareRequest = serde_json::from_str(r#"{"code":"1"}"#).unwrap();
        req.mic_device = Some("{mic}".into());
        req.output_device = Some("{spk}".into());
        let args = send_args(&req);
        // No mic track, no mic device.
        assert!(!args.iter().any(|a| a == "--mic-device"));
        assert!(args.ends_with(&["--output-device".to_string(), "{spk}".to_string()]));
        req.mic = true;
        let args = send_args(&req);
        let i = args.iter().position(|a| a == "--mic-device").unwrap();
        assert_eq!(args[i + 1], "{mic}");
        req.mic_device = None;
        req.output_device = Some(String::new());
        let args = send_args(&req);
        assert!(!args.iter().any(|a| a.ends_with("-device")));
    }

    #[test]
    fn device_wire_shape_is_locked() {
        // Must match relay_capture::command's own test byte for byte.
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
        let d: AudioDevices = serde_json::from_str(
            r#"{"render":[{"id":"a","name":"Speakers","is_default":true}],"capture":[]}"#,
        )
        .unwrap();
        assert!(d.render[0].is_default);
    }

    #[test]
    fn decode_line_maps_engine_events() {
        let ev = decode_line(r#"{"event":"stats","bitrate_mbps":57.2,"fps":60.0}"#).unwrap();
        let ShareEvent::Stats { data } = ev else { panic!("want Stats") };
        assert_eq!(data["bitrate_mbps"], 57.2);

        let ev = decode_line(r#"{"event":"connected","peer":"den-pc","rtt_ms":0.4}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Connected { peer, .. } if peer == "den-pc"));

        let ev = decode_line(r#"{"event":"waiting","code":"123456","name":"studio"}"#).unwrap();
        assert!(
            matches!(ev, ShareEvent::Waiting { code, name } if code == "123456" && name == "studio")
        );

        let ev = decode_line(r#"{"event":"paired","sender":"studio"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Paired { sender, .. } if sender == "studio"));

        let ev = decode_line(r#"{"event":"error","where":"video","message":"boom"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Error { message } if message == "boom"));

        let ev = decode_line(r#"{"event":"stopped"}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Exited { ok: true, code: Some(0) }));
    }

    #[test]
    fn paired_and_connected_carry_whether_a_code_was_used() {
        // S35: `trusted` says the peer connected as a remembered PC. Absent
        // (an engine that predates it) means a code was used.
        let ev = decode_line(r#"{"event":"paired","sender":"studio","trusted":true}"#).unwrap();
        assert!(matches!(ev, ShareEvent::Paired { trusted: true, .. }));
        let ev = decode_line(r#"{"event":"paired","sender":"studio"}"#).unwrap();
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
    fn a_refusal_is_its_own_event_and_a_plain_error_is_not() {
        let line = r#"{"event":"error","where":"fatal","message":"no","refused":true}"#;
        assert!(
            matches!(decode_line(line), Some(ShareEvent::Refused { message }) if message == "no")
        );
        let line = r#"{"event":"error","where":"fatal","message":"boom","refused":false}"#;
        assert!(matches!(decode_line(line), Some(ShareEvent::Error { .. })));
        assert!(matches!(
            decode_line(r#"{"event":"sender_stopped"}"#),
            Some(ShareEvent::SenderStopped)
        ));
    }

    #[test]
    fn share_event_serializes_tagged_for_the_ui() {
        // The Tauri shell matches on `event` — lock the tag format.
        let s = serde_json::to_string(&ShareEvent::Exited { ok: false, code: Some(1) }).unwrap();
        assert_eq!(s, r#"{"event":"exited","ok":false,"code":1}"#);
        let s = serde_json::to_string(&ShareEvent::Waiting {
            code: "123456".into(),
            name: "studio".into(),
        })
        .unwrap();
        assert_eq!(s, r#"{"event":"waiting","code":"123456","name":"studio"}"#);
    }

    /// The same strings `relay_capture::command` locks on the engine side.
    #[test]
    fn s50_wire_shapes_match_the_engine() {
        assert_eq!(
            serde_json::to_string(&EngineCmd::Host {
                mode: HostMode::Clean,
                owner: 0,
                feed: Some(CleanFeed::Qhd)
            })
            .unwrap(),
            r#"{"cmd":"host","mode":"clean","owner":0,"feed":"2560x1440"}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::Host {
                mode: HostMode::Embedded,
                owner: 7,
                feed: None
            })
            .unwrap(),
            r#"{"cmd":"host","mode":"embedded","owner":7}"#,
            "an embed line is what it was before S50"
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::LocalShare {
                target: Some(SourceTarget::Display { index: 0 })
            })
            .unwrap(),
            r#"{"cmd":"local_share","target":{"kind":"display","index":0}}"#
        );
        assert_eq!(
            serde_json::to_string(&EngineCmd::LocalShare { target: None }).unwrap(),
            r#"{"cmd":"local_share","target":null}"#
        );
    }

    /// A running receiver is told when this PC starts, switches or stops
    /// sharing, once per change, and never when nothing is receiving.
    #[test]
    fn local_share_is_told_once_per_change() {
        let d0 = Some(SourceTarget::Display { index: 0 });
        let d1 = Some(SourceTarget::Display { index: 1 });
        let say = |t| Some(EngineCmd::LocalShare { target: t });
        assert_eq!(local_share_sync(false, None, d0), None, "nothing receiving");
        // Spawned while not sharing: told `None` by the absence of the flag.
        assert_eq!(local_share_sync(true, Some(None), None), None);
        assert_eq!(local_share_sync(true, Some(None), d0), say(d0), "a local share starts");
        assert_eq!(local_share_sync(true, Some(d0), d0), None, "same again: quiet");
        assert_eq!(local_share_sync(true, Some(d0), d1), say(d1), "the share switches monitor");
        assert_eq!(local_share_sync(true, Some(d1), None), say(None), "the share stops");
        // Never told anything yet (should not happen, but say it rather than guess).
        assert_eq!(local_share_sync(true, None, None), say(None));
    }

    #[test]
    fn a_source_line_names_its_target() {
        let v = serde_json::json!({
            "event": "source", "target": {"kind": "display", "index": 2}, "width": 1, "height": 1
        });
        assert_eq!(source_target_of(&v), Some(SourceTarget::Display { index: 2 }));
        let v = serde_json::json!({ "event": "source", "target": {"kind": "window", "hwnd": 9} });
        assert_eq!(source_target_of(&v), Some(SourceTarget::Window { hwnd: 9 }));
        assert_eq!(source_target_of(&serde_json::json!({ "event": "source" })), None);
        assert_eq!(INITIAL_SHARE_TARGET, SourceTarget::Display { index: 0 });
    }
}
