//! IPC between the core and the Tauri shell (or the CLI).
//!
//! Transport: a Windows named pipe, newline-delimited JSON, one message per
//! line. Requests carry an `id` that is echoed on the response. After a
//! `subscribe` request the server also pushes `event` lines on that connection.
//!
//! Hardening: the pipe carries a DACL that admits only the SID of the current
//! user, remote clients are rejected, any line over [`IPC_MAX_LINE`] closes the
//! connection, and a connection that has not subscribed is dropped after
//! [`IDLE_TIMEOUT`] without a request. Handler execution is bounded by
//! [`REQUEST_TIMEOUT`].
//!
//! The TypeScript mirror of these shapes is `ui/src/lib/ipc.ts`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::config::IPC_MAX_LINE;
use crate::hardware::{
    AudioInterface, HardwareView, Headset, Monitor, MonitorVendorControls, ProbeReport,
};
use crate::presets::{RecordingSettings, SharePresetDef};
use crate::share::{ReceiveRequest, ShareRequest, SourceTarget};
use crate::types::{CoreState, HeadsetId, ProcessInfo, Profile, ProfileSummary};

/// A connection that has neither subscribed nor sent a request for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound on the handling time of one request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const NEWLINE: u8 = b'\n';

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Method {
    Ping,
    Status,
    ListProfiles,
    GetProfile {
        id: Uuid,
    },
    SaveProfile {
        profile: Box<Profile>,
    },
    DeleteProfile {
        id: Uuid,
    },
    /// Manually apply (ignores focus until blur/restore).
    ApplyProfile {
        id: Uuid,
    },
    RestoreAll,
    /// Processes that currently own a visible top-level window.
    ListProcesses,
    GetAutostart,
    SetAutostart {
        enabled: bool,
    },
    /// App preferences that are not about any one game (`settings.json`).
    GetUiPrefs,
    SetUiPrefs {
        prefs: crate::uiprefs::UiPrefs,
    },
    /// Spawn the share engine (a child process) to share this PC to a peer.
    StartShare {
        request: Box<ShareRequest>,
    },
    /// Stop the running share engine.
    StopShare,
    /// Start a share from a preset id; the core resolves it (game audio pid
    /// from the focused window, recording folder from settings).
    StartSharePreset {
        preset: String,
        /// May be empty when `peer_id` is set.
        code: String,
        #[serde(default)]
        peer: Option<String>,
        /// A remembered PC to connect to without a code (S35). The service
        /// resolves it; the client never sees a fingerprint.
        #[serde(default)]
        peer_id: Option<String>,
    },
    /// Toggle continuous recording on the running share.
    Record {
        on: bool,
    },
    /// Save the replay buffer of the running share to disk.
    SaveReplay,
    /// Swap the running share's capture source (no renegotiation).
    SwitchSource {
        target: SourceTarget,
    },
    /// Per-track gain and mute on the running share or receive, live (S37).
    SetMixer {
        side: crate::share::MixerSide,
        faders: crate::share::FaderSet,
    },
    /// Active render and capture endpoints for the mixer's device pickers
    /// (S40). Read-only.
    ListAudioDevices,
    /// Point one device-backed track at an endpoint, or back at the System
    /// default (`device` null), live on a running share or receive, and
    /// saved for the next one (S40).
    SetAudioDevice {
        side: crate::share::MixerSide,
        track: crate::share::DeviceTrack,
        #[serde(default)]
        device: Option<String>,
    },
    /// Presets and recording settings.
    ListPresets,
    SavePreset {
        preset: Box<SharePresetDef>,
    },
    DeletePreset {
        id: String,
    },
    SetRecordingSettings {
        settings: RecordingSettings,
    },
    /// Start receiving: advertise over mDNS and render an incoming share.
    StartReceive {
        request: Box<ReceiveRequest>,
    },
    /// Stop receiving.
    StopReceive,
    /// Move the receiver's stream window between the app window (`owner`,
    /// the shell's HWND) and a window of its own (S29).
    HostReceive {
        mode: crate::share::HostMode,
        #[serde(default)]
        owner: u64,
    },
    /// Browse the LAN for Relay receivers (blocks briefly).
    DiscoverReceivers,
    /// PCs this one has paired with (S35), favourites first.
    ListPeers,
    /// Stop remembering a PC. It needs a code again, like a stranger; takes
    /// effect at the next connection attempt on this end.
    ForgetPeer {
        id: String,
    },
    SetPeerFavourite {
        id: String,
        favourite: bool,
    },
    /// The hardware library plus what is connected right now.
    ListHardware,
    /// Create or update a library headset/monitor.
    SaveHardware {
        item: HardwareItem,
    },
    /// Remove a library entry (headset, monitor or interface) by its id.
    DeleteHardware {
        id: String,
    },
    /// Replace what an output feeds (S41): the ordered listening devices
    /// the user said are connected to it. `endpoint` is the listening key
    /// from `HardwareView::endpoints`/`listening`. Headsets must already be
    /// in the library.
    SetListeningDevices {
        endpoint: String,
        devices: Vec<crate::hardware::ListeningDevice>,
    },
    /// Pick which of an output's listening devices is on the user's head
    /// (or that it is the speakers) -- the quick switch (S41).
    SetActiveListening {
        endpoint: String,
        device: crate::hardware::ListeningDevice,
    },
    /// Full re-probe including the slow DDC/CI capability query; refreshes the
    /// cached connected state and stores VCP lists on known library monitors.
    ProbeHardware,
    /// Parse AutoEQ results text (local file contents or paste) and attach it
    /// to a headset as its measured curve.
    ImportCurve {
        headset: HeadsetId,
        csv: String,
    },
    /// Search the bundled headphone catalogue by name. Read-only, offline.
    SearchCatalog {
        query: String,
    },
    /// Add a catalogue model to the library: fetch its measurement (once,
    /// then cached), attach it as the curve, and bind it to an endpoint.
    /// The only request that reaches the network.
    AddHeadsetFromCatalog {
        entry: Box<crate::hardware::catalog::CatalogEntry>,
        #[serde(default)]
        endpoint: Option<String>,
    },
    /// Render the A/B listening pair for a profile's audio chain into the
    /// previews directory. `wav` is an optional source clip; without it a
    /// synthetic demo (footsteps / explosion / reference beep) is used.
    RenderPreview {
        id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wav: Option<String>,
    },
    /// What this PC can actually do with video (HEVC and H.264): hardware
    /// encoders (needed to share) and any decoder (needed to receive). Runs `relay-share probe`,
    /// so it costs a short-lived child process -- call it when a screen opens,
    /// not on a timer.
    ShareCapabilities,
    /// Will an inbound share connection actually reach `relay-share.exe`, or
    /// is Windows Firewall dropping it? Read-only: a registry walk plus three
    /// policy reads, no elevation and no prompt. Cheap enough to call when a
    /// screen opens, but not on a timer.
    FirewallStatus,
    /// Is the endpoint APO registered, per render endpoint (S42)? Read-only
    /// registry probe plus the backups on disk.
    ApoStatus,
    /// S44: is Windows' protected-audiodg check off (unsigned audio effects
    /// load), did Relay turn it off, and what was it before? Read-only.
    AudioEffectsStatus,
    /// Register the APO on one render endpoint (default output when
    /// `endpoint` is absent), backup-then-apply. Refused unless the
    /// live-write gate is set — VM / installer only; the UI goes through
    /// `RunElevated`.
    InstallApo {
        #[serde(default)]
        endpoint: Option<String>,
    },
    /// Restore one endpoint's FX property store from its install backup.
    /// Same gate.
    UninstallApo {
        #[serde(default)]
        endpoint: Option<String>,
    },
    /// Virtual-device state: Windows support, registration, consent, OBS /
    /// VB-Cable detection. Read-only.
    VdeviceStatus,
    /// Record the first-run consent decision (camera / microphone opt-ins).
    /// Never installs anything by itself.
    SetVdeviceConsent {
        apo: bool,
        camera: bool,
        microphone: bool,
    },
    /// The dry-run listing for the consent screen: exactly what a camera
    /// install would create. Read-only.
    VdeviceDryRun,
    /// Register the camera media source (record-then-apply into
    /// `installed.json`). Refused without consent, and gated like the APO
    /// (`RELAY_VDEVICE_ALLOW_LIVE_WRITE` + elevation).
    InstallVcam,
    /// Remove the recorded camera registration; empties `installed.json`.
    /// Same gate.
    UninstallVcam,
    /// Everything the uninstaller would touch on this machine, in order.
    /// Read-only — the same plan `relay-core uninstall --dry-run` prints, so
    /// the Settings card can never promise something the uninstaller does
    /// not do.
    UninstallPlan {
        /// Preview with `%LOCALAPPDATA%\Relay` kept (the default).
        #[serde(default = "crate::ipc::default_true")]
        keep_data: bool,
    },
    /// Exactly what an elevated install or removal would change on this
    /// machine, read-only — the listing shown *before* the UAC prompt. For
    /// the two removal ops it is the uninstall card's own listing, narrowed
    /// to that step, so the two can never disagree.
    ElevationPlan {
        op: crate::elevate::ElevatedOp,
    },
    /// Ask for administrator rights and run one op in `relay-elevate.exe`.
    /// Blocks until the helper exits. Declining the prompt is an answer, not
    /// an error: the reply says so and nothing on the PC changed.
    RunElevated {
        op: crate::elevate::ElevatedOp,
    },
    /// Hand over to the Windows uninstaller (one installer, one uninstaller)
    /// and stop the core. Does not itself remove anything.
    LaunchUninstaller,
    /// The app window closed with the core staying up (S38): show the
    /// notification-area message, if that preference is on.
    WindowClosed,
    /// The user has seen the last-crash line; clear it and mark the records
    /// seen (S38).
    AckCrash,
    /// S45: what the updater knows and is doing.
    UpdateStatus,
    /// S45: "Check now" — check regardless of the daily limit.
    CheckForUpdates,
    /// S45: the user said Install now. Downloads, verifies, and installs
    /// once no share or game profile is active.
    InstallUpdate,
    /// S45: "Later" — hide the offer until the next check finds it again.
    UpdateLater,
    /// S45: "Skip this version".
    SkipUpdate {
        version: String,
    },
    Subscribe,
    Shutdown,
}

pub(crate) fn default_true() -> bool {
    true
}

/// One library entry for `SaveHardware`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HardwareItem {
    Headset(Box<Headset>),
    Monitor(Box<Monitor>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Pong,
    Status {
        state: Box<CoreState>,
    },
    Profiles {
        profiles: Vec<ProfileSummary>,
    },
    Profile {
        profile: Box<Profile>,
    },
    Processes {
        processes: Vec<ProcessInfo>,
    },
    Autostart {
        enabled: bool,
    },
    UiPrefs {
        prefs: crate::uiprefs::UiPrefs,
    },
    /// Reply to every S45 update method.
    Update {
        status: crate::update::UpdateStatus,
    },
    /// Reply to `ListAudioDevices` (S40).
    AudioDevices {
        devices: crate::share::AudioDevices,
    },
    Receivers {
        receivers: serde_json::Value,
    },
    /// Remembered PCs. Carries the fingerprint, which is public (it is in
    /// every SDP this PC sends); nothing secret leaves the core here.
    Peers {
        peers: Vec<crate::peers::Peer>,
    },
    Hardware {
        headsets: Vec<Headset>,
        monitors: Vec<Monitor>,
        interfaces: Vec<AudioInterface>,
        connected: Box<HardwareView>,
        /// Monitors with at least one *verified* vendor DDC/CI control.
        /// Absent or empty means every vendor slider stays disabled; the
        /// client must not infer a control from the advertised opcode list.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        vendor_controls: Vec<MonitorVendorControls>,
    },
    Probe {
        report: Box<ProbeReport>,
    },
    Curve {
        points: Vec<(f32, f32)>,
    },
    Catalog {
        entries: Vec<crate::hardware::catalog::CatalogEntry>,
    },
    Preview {
        original: String,
        processed: String,
        sample_rate: u32,
        hrtf_applied: bool,
    },
    /// Encoder/decoder names as the share engine reported them. Empty lists
    /// are the answer, not a failure: they mean this PC cannot do that half.
    Capabilities {
        can_share: bool,
        can_receive: bool,
        /// Display adapters, named so the UI can say which GPU it means.
        #[serde(default)]
        adapters: Vec<String>,
        encoders: Vec<String>,
        decoders: Vec<String>,
        /// `"hevc"` / `"h264"` this PC can send and receive (S27).
        #[serde(default)]
        share_codecs: Vec<String>,
        #[serde(default)]
        receive_codecs: Vec<String>,
    },
    Apo {
        status: crate::audio_apo::ApoStatus,
    },
    /// S44: the protected-audiodg switch.
    AudioEffects {
        status: crate::audiodg::Status,
    },
    /// What Windows Firewall will do to an incoming share on this PC.
    Firewall {
        status: Box<crate::firewall::FirewallStatus>,
    },
    Vdevice {
        status: Box<crate::vdevice::VdeviceStatus>,
    },
    DryRun {
        lines: Vec<String>,
    },
    /// Result of a `RunElevated`. `declined` is the UAC prompt being
    /// dismissed — the one case where nothing at all was attempted.
    Elevation {
        declined: bool,
        ok: bool,
        lines: Vec<String>,
    },
    Presets {
        presets: Vec<SharePresetDef>,
        recording: RecordingSettings,
    },
    Ok,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    StateChanged {
        state: Box<CoreState>,
    },
    Notice {
        text: String,
    },
    /// The core is shutting down on purpose (the tray's "Quit Relay", or the
    /// window's close-and-quit preference). The shell closes its window on
    /// this rather than falling back to the offline state, which would tell
    /// the user something went wrong when nothing did.
    Quitting,
    /// Instrument-strip stats from the share engine (verbatim JSON).
    ShareStats {
        data: serde_json::Value,
    },
    /// The share engine started, connected, or stopped.
    ShareStatus {
        sharing: bool,
        peer: Option<String>,
        message: Option<String>,
        /// Connected without a code, as a remembered PC (S35).
        #[serde(default)]
        trusted: bool,
    },
    /// Continuous recording started/stopped on the share engine.
    RecordingStatus {
        on: bool,
        path: Option<String>,
    },
    /// A replay clip was saved.
    ReplaySaved {
        path: String,
        ms: u64,
    },
    /// The share's capture source changed (engine `source` line, verbatim).
    SourceChanged {
        data: serde_json::Value,
    },
    /// The receiver's stream window (S29): it exists, or changed hosting
    /// mode. `mode` is `embedded`, `popout` or `none`; `excluded_from_capture`
    /// is what Windows reports back for the B9 guard, not what was asked.
    /// The shell positions the window from this; the webview only reads the
    /// stream size and mode.
    StreamWindow {
        hwnd: u64,
        width: u32,
        height: u32,
        mode: String,
        excluded_from_capture: bool,
    },
    /// The user closed the popped-out stream window; it is hidden and waits
    /// to be embedded again.
    StreamPopoutClosed,
    /// The receive engine's state: advertising with a code, paired, or stopped.
    ReceiveStatus {
        receiving: bool,
        code: Option<String>,
        sender: Option<String>,
        message: Option<String>,
        /// The negotiated video codec, once the first frame names it.
        #[serde(default)]
        codec: Option<String>,
        /// The sender connected without a code, as a remembered PC, and
        /// DTLS proved it (S35).
        #[serde(default)]
        trusted: bool,
        /// The share ended because the sender stopped it (it said goodbye),
        /// not because it dropped. Receiving carries on either way.
        #[serde(default)]
        ended_by_sender: bool,
        /// The engine is gone but the core is bringing the receiver straight
        /// back (S38). The page keeps its receiving state rather than
        /// flashing Idle / "ended" before the restart lands.
        #[serde(default)]
        restarting: bool,
        /// The call app the running receiver is returning audio from (S19),
        /// whoever started it -- so the Call card shows what the receiver
        /// is doing, not only what this window picked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        return_pid: Option<u32>,
        /// That call app's exe name, so the card can name it even when it is
        /// silent (and so missing from the "playing sound" list) right now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        return_exe: Option<String>,
    },
    /// A base64 JPEG thumbnail of the live capture, for the Share screen's
    /// preview. Only sent while a share was started with previews on.
    SharePreview {
        width: u32,
        height: u32,
        jpeg: String,
    },
}

/// One line on the wire is exactly one of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Outbound {
    Response { id: u64, result: Reply },
    Event(Event),
}

/// Read one newline-terminated line into `buf`, refusing to buffer more than
/// `max` bytes. `Ok(None)` at EOF. Shared by server and client so neither
/// side can be made to allocate without bound.
pub async fn read_line_capped<R>(
    rd: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Option<()>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    buf.clear();
    loop {
        let available = rd.fill_buf().await?;
        if available.is_empty() {
            return if buf.is_empty() { Ok(None) } else { Ok(Some(())) };
        }
        let (chunk, done) = match available.iter().position(|b| *b == NEWLINE) {
            Some(i) => (&available[..i], true),
            None => (available, false),
        };
        if buf.len() + chunk.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ipc line exceeds {max} bytes"),
            ));
        }
        buf.extend_from_slice(chunk);
        let used = chunk.len() + usize::from(done);
        rd.consume(used);
        if done {
            return Ok(Some(()));
        }
    }
}

#[cfg(windows)]
pub mod security {
    //! Security descriptor that admits only the current user.
    use std::ffi::c_void;

    use anyhow::{Context, Result};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// String SID of the user this process runs as, e.g. `S-1-5-21-...-1001`.
    pub fn current_user_sid() -> Result<String> {
        let mut token = HANDLE::default();
        // SAFETY: opening our own token for read.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .context("opening process token")?;
        let result = (|| -> Result<String> {
            let mut len = 0u32;
            // SAFETY: size query; a failure with ERROR_INSUFFICIENT_BUFFER is expected.
            let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
            anyhow::ensure!(len > 0, "GetTokenInformation reported zero size");
            let mut buf = vec![0u8; len as usize];
            // SAFETY: `buf` is `len` bytes, as reported by the size query.
            unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    Some(buf.as_mut_ptr() as *mut c_void),
                    len,
                    &mut len,
                )
            }
            .context("reading token user")?;
            // SAFETY: the buffer holds a TOKEN_USER written by the OS.
            let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
            let mut s = PWSTR::null();
            // SAFETY: `user.User.Sid` is valid for the life of `buf`.
            unsafe { ConvertSidToStringSidW(user.User.Sid, &mut s) }.context("SID to string")?;
            // SAFETY: `s` is a NUL-terminated string allocated by the OS.
            let out = unsafe { s.to_string() }?;
            // SAFETY: freeing exactly what ConvertSidToStringSidW allocated.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
            }
            Ok(out)
        })();
        // SAFETY: closing the token handle we opened above.
        unsafe {
            let _ = CloseHandle(token);
        }
        result
    }

    /// SDDL granting generic-all to one SID and nothing to anyone else.
    pub fn sddl_for(sid: &str) -> String {
        format!("D:P(A;;GA;;;{sid})")
    }

    /// `SECURITY_ATTRIBUTES` pointing at a protected DACL for the current user.
    pub struct PipeSecurity {
        sd: PSECURITY_DESCRIPTOR,
        attrs: SECURITY_ATTRIBUTES,
    }

    // SAFETY: the descriptor is an immutable OS allocation owned by this struct.
    unsafe impl Send for PipeSecurity {}

    impl PipeSecurity {
        pub fn current_user_only() -> Result<Self> {
            let sddl = sddl_for(&current_user_sid()?);
            let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            // SAFETY: `wide` is NUL-terminated; `sd` receives an OS allocation.
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(wide.as_ptr()),
                    SDDL_REVISION_1,
                    &mut sd,
                    None,
                )
            }
            .with_context(|| format!("parsing SDDL {sddl}"))?;
            let attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            Ok(Self { sd, attrs })
        }

        /// Raw pointer for `ServerOptions::create_with_security_attributes_raw`.
        pub fn as_ptr(&mut self) -> *mut c_void {
            &mut self.attrs as *mut SECURITY_ATTRIBUTES as *mut c_void
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            // SAFETY: freeing the descriptor allocated by the conversion call.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
        }
    }
}

#[cfg(windows)]
pub mod server {
    use super::*;
    use crate::config::pipe_name;
    use anyhow::{Context, Result};
    use std::future::Future;
    use std::sync::Arc;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::sync::broadcast;
    use tracing::{debug, warn};

    /// Handles one decoded request. Implemented by the service.
    pub trait Handler: Send + Sync + 'static {
        fn handle(&self, method: Method) -> impl Future<Output = Reply> + Send;
    }

    fn create(
        name: &str,
        sec: &mut security::PipeSecurity,
        first: bool,
    ) -> Result<NamedPipeServer> {
        // SAFETY: `sec` outlives the call and points at a valid SECURITY_ATTRIBUTES.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, sec.as_ptr())
        }
        .with_context(|| format!("creating pipe {name}"))
    }

    pub async fn serve<H: Handler>(
        handler: Arc<H>,
        events: broadcast::Sender<Event>,
    ) -> Result<()> {
        let name = pipe_name();
        let mut sec = security::PipeSecurity::current_user_only()?;
        let mut server = create(&name, &mut sec, true)?;
        loop {
            server.connect().await?;
            let connected = server;
            server = create(&name, &mut sec, false)?;
            let h = handler.clone();
            let rx = events.subscribe();
            tokio::spawn(async move {
                if let Err(e) = connection(connected, h, rx).await {
                    debug!(error = %e, "ipc connection closed");
                }
            });
        }
    }

    async fn connection<H: Handler>(
        pipe: NamedPipeServer,
        handler: Arc<H>,
        mut events: broadcast::Receiver<Event>,
    ) -> Result<()> {
        let (rd, mut wr) = tokio::io::split(pipe);
        let mut rd = BufReader::new(rd);
        let mut buf = Vec::new();
        let mut subscribed = false;
        loop {
            let idle = tokio::time::sleep(IDLE_TIMEOUT);
            tokio::select! {
                line = read_line_capped(&mut rd, &mut buf, IPC_MAX_LINE) => {
                    if line?.is_none() { return Ok(()) }
                    let text = std::str::from_utf8(&buf).unwrap_or("");
                    if text.trim().is_empty() { continue; }
                    let out = match serde_json::from_str::<Request>(text) {
                        Ok(req) => {
                            if matches!(req.method, Method::Subscribe) { subscribed = true; }
                            let handled = tokio::time::timeout(REQUEST_TIMEOUT, handler.handle(req.method));
                            let result = match handled.await {
                                Ok(r) => r,
                                Err(_) => Reply::Error { message: "request timed out".into() },
                            };
                            Outbound::Response { id: req.id, result }
                        }
                        Err(e) => Outbound::Response {
                            id: 0,
                            result: Reply::Error { message: format!("bad request: {e}") },
                        },
                    };
                    write_line(&mut wr, &out).await?;
                }
                ev = events.recv(), if subscribed => {
                    match ev {
                        Ok(ev) => write_line(&mut wr, &Outbound::Event(ev)).await?,
                        Err(broadcast::error::RecvError::Lagged(n)) => warn!(n, "ipc subscriber lagged"),
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
                _ = idle, if !subscribed => {
                    debug!("ipc connection idle; closing");
                    return Ok(());
                }
            }
        }
    }

    async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &Outbound) -> Result<()> {
        let mut bytes = serde_json::to_vec(msg)?;
        bytes.push(NEWLINE);
        w.write_all(&bytes).await?;
        w.flush().await?;
        Ok(())
    }
}

#[cfg(windows)]
pub mod client {
    use super::*;
    use crate::config::pipe_name;
    use anyhow::{bail, Context, Result};
    use tokio::io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use windows::Win32::Foundation::ERROR_PIPE_BUSY;

    pub struct Client {
        reader: BufReader<ReadHalf<NamedPipeClient>>,
        writer: WriteHalf<NamedPipeClient>,
        buf: Vec<u8>,
        next_id: u64,
    }

    impl Client {
        /// Connect to the pipe named by [`pipe_name`], retrying briefly if every
        /// instance is busy.
        pub async fn connect() -> Result<Self> {
            Self::connect_to(&pipe_name()).await
        }

        pub async fn connect_to(name: &str) -> Result<Self> {
            let pipe = loop {
                match ClientOptions::new().open(name) {
                    Ok(p) => break p,
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(e) => {
                        return Err(e).context("connecting to relay-core (is the service running?)")
                    }
                }
            };
            let (rd, writer) = tokio::io::split(pipe);
            Ok(Self { reader: BufReader::new(rd), writer, buf: Vec::new(), next_id: 1 })
        }

        async fn next_line(&mut self) -> Result<Option<Outbound>> {
            match read_line_capped(&mut self.reader, &mut self.buf, IPC_MAX_LINE).await? {
                None => Ok(None),
                Some(()) => Ok(Some(serde_json::from_slice(&self.buf)?)),
            }
        }

        pub async fn call(&mut self, method: Method) -> Result<Reply> {
            let id = self.next_id;
            self.next_id += 1;
            let mut bytes = serde_json::to_vec(&Request { id, method })?;
            bytes.push(NEWLINE);
            self.writer.write_all(&bytes).await?;
            self.writer.flush().await?;
            loop {
                let Some(msg) = self.next_line().await? else {
                    bail!("relay-core closed the connection")
                };
                match msg {
                    Outbound::Response { id: rid, result } if rid == id => return Ok(result),
                    Outbound::Response { .. } => continue,
                    Outbound::Event(_) => continue, // caller uses `next_event` for those
                }
            }
        }

        /// After `Method::Subscribe`, await pushed events.
        pub async fn next_event(&mut self) -> Result<Option<Event>> {
            loop {
                let Some(msg) = self.next_line().await? else { return Ok(None) };
                if let Outbound::Event(ev) = msg {
                    return Ok(Some(ev));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_shape_is_flat() {
        let r = Request { id: 7, method: Method::GetProfile { id: Uuid::nil() } };
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "get_profile");
        assert_eq!(v["params"]["id"], Uuid::nil().to_string());
        let back: Request = serde_json::from_value(v).unwrap();
        assert!(matches!(back.method, Method::GetProfile { .. }));
    }

    /// S44: locks the shape `ui/src/lib/ipc.ts` mirrors for the audio-effects
    /// switch — the status call, its reply, and the elevated op.
    #[test]
    fn audio_effects_wire_shape() {
        let r: Request =
            serde_json::from_str(r#"{"id":1,"method":"audio_effects_status"}"#).unwrap();
        assert!(matches!(r.method, Method::AudioEffectsStatus));
        let r: Request = serde_json::from_str(
            r#"{"id":2,"method":"run_elevated","params":{"op":{"set_audio_effects_allowed":{"on":true,"restart_audio":false}}}}"#,
        )
        .unwrap();
        assert!(matches!(
            r.method,
            Method::RunElevated {
                op: crate::elevate::ElevatedOp::SetAudioEffectsAllowed {
                    on: true,
                    restart_audio: false
                }
            }
        ));
        let reply = serde_json::to_value(Reply::AudioEffects {
            status: crate::audiodg::Status {
                value: Some(1),
                allowed: true,
                changed_by_relay: true,
                prior: Some(None),
                set_elsewhere: false,
                unknown: false,
            },
        })
        .unwrap();
        assert_eq!(
            reply,
            serde_json::json!({ "type": "audio_effects", "status": {
                "value": 1, "allowed": true, "changed_by_relay": true, "prior": null,
                "set_elsewhere": false, "unknown": false } })
        );
    }

    /// Locks the wire shape that `ui/src/lib/ipc.ts` mirrors.
    #[test]
    fn apo_methods_wire_shape() {
        let ep = "{f8ae226b-a4e3-45ab-97fc-3977dad232d1}";
        let r: Request = serde_json::from_str(&format!(
            r#"{{"id":1,"method":"install_apo","params":{{"endpoint":"{ep}"}}}}"#
        ))
        .unwrap();
        assert!(matches!(r.method, Method::InstallApo { endpoint: Some(ref e) } if e == ep));
        let r: Request =
            serde_json::from_str(r#"{"id":2,"method":"uninstall_apo","params":{}}"#).unwrap();
        assert!(matches!(r.method, Method::UninstallApo { endpoint: None }));
        let r: Request = serde_json::from_str(&format!(
            r#"{{"id":3,"method":"run_elevated","params":{{"op":{{"install_apo":{{"endpoint":"{ep}"}}}}}}}}"#
        ))
        .unwrap();
        match r.method {
            Method::RunElevated { op } => assert_eq!(op.endpoint(), Some(ep)),
            other => panic!("{other:?}"),
        }
        let r: Request = serde_json::from_str(
            r#"{"id":4,"method":"elevation_plan","params":{"op":"install_camera"}}"#,
        )
        .unwrap();
        assert!(matches!(
            r.method,
            Method::ElevationPlan { op: crate::elevate::ElevatedOp::InstallCamera }
        ));
        let v = serde_json::to_value(Method::InstallApo { endpoint: None }).unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "method": "install_apo", "params": { "endpoint": null } })
        );
        let reply = serde_json::to_value(Reply::Apo {
            status: crate::audio_apo::ApoStatus {
                installed: false,
                endpoint: None,
                running: false,
                endpoints: vec![crate::audio_apo::EndpointApo {
                    endpoint: ep.into(),
                    name: "Headphones".into(),
                    is_default: true,
                    installed: true,
                    backed_up: true,
                    running: false,
                }],
                audio_engine: crate::audio_apo::AudioEngineRegistration::Registered,
            },
        })
        .unwrap();
        let e = &reply["status"]["endpoints"][0];
        for k in ["endpoint", "name", "is_default", "installed", "backed_up", "running"] {
            assert!(e.get(k).is_some(), "missing {k}");
        }
    }

    #[test]
    fn hardware_methods_wire_shape() {
        use crate::hardware::{Headset, HeadsetKind};
        let m = Method::SaveHardware {
            item: HardwareItem::Headset(Box::new(Headset {
                id: crate::types::HeadsetId("hd560s".into()),
                name: "HD 560S".into(),
                kind: HeadsetKind::Headphone,
                curve: Some(vec![(20.0, -4.0)]),
                source: "oratory1990".into(),
                endpoints: vec!["ep:c:abc".into()],
            })),
        };
        let v = serde_json::to_value(Request { id: 1, method: m }).unwrap();
        assert_eq!(v["method"], "save_hardware");
        assert_eq!(v["params"]["item"]["kind"], "headset");
        assert_eq!(v["params"]["item"]["value"]["id"], "hd560s");
        assert_eq!(v["params"]["item"]["value"]["kind"], "headphone");
        assert_eq!(v["params"]["item"]["value"]["curve"][0][0], 20.0);

        let r = Reply::Curve { points: vec![(20.0, -4.0)] };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["type"], "curve");
        assert_eq!(v["points"][0][1], -4.0);

        let v = serde_json::to_value(Request {
            id: 2,
            method: Method::ImportCurve {
                headset: crate::types::HeadsetId("hd560s".into()),
                csv: "20,-4\n100,0".into(),
            },
        })
        .unwrap();
        assert_eq!(v["method"], "import_curve");
        assert_eq!(v["params"]["headset"], "hd560s");
    }

    /// Locks the S41 listening-device wire shape `ui/src/lib/ipc.ts` mirrors.
    #[test]
    fn listening_methods_wire_shape() {
        use crate::hardware::ListeningDevice;
        let hd = ListeningDevice::Headset { id: crate::types::HeadsetId("hd560s".into()) };
        let v = serde_json::to_value(Request {
            id: 3,
            method: Method::SetListeningDevices {
                endpoint: "ep:c:rode".into(),
                devices: vec![hd.clone(), ListeningDevice::Speakers],
            },
        })
        .unwrap();
        assert_eq!(v["method"], "set_listening_devices");
        assert_eq!(v["params"]["endpoint"], "ep:c:rode");
        assert_eq!(v["params"]["devices"][0]["kind"], "headset");
        assert_eq!(v["params"]["devices"][0]["id"], "hd560s");
        assert_eq!(v["params"]["devices"][1]["kind"], "speakers");

        let v = serde_json::to_value(Request {
            id: 4,
            method: Method::SetActiveListening { endpoint: "ep:c:rode".into(), device: hd },
        })
        .unwrap();
        assert_eq!(v["method"], "set_active_listening");
        assert_eq!(v["params"]["device"]["id"], "hd560s");
        let back: Request = serde_json::from_value(v).unwrap();
        assert!(matches!(back.method, Method::SetActiveListening { .. }));
    }

    /// The exact payload `api.setListeningDevices` sends (ui/src/lib/ipc.ts,
    /// locked there by Listening.test.tsx) parses; bare id strings do not.
    #[test]
    fn listening_devices_parse_from_the_ui_payload_only() {
        use crate::hardware::ListeningDevice;
        let ui = r#"{"id":1,"method":"set_listening_devices","params":{"endpoint":"ep:c:rode#aaaa","devices":[{"kind":"headset","id":"hd560s"},{"kind":"speakers"}]}}"#;
        let r: Request = serde_json::from_str(ui).unwrap();
        match r.method {
            Method::SetListeningDevices { endpoint, devices } => {
                assert_eq!(endpoint, "ep:c:rode#aaaa");
                assert_eq!(devices[1], ListeningDevice::Speakers);
            }
            other => panic!("{other:?}"),
        }
        let bare = r#"{"id":1,"method":"set_listening_devices","params":{"endpoint":"x","devices":["hd560s"]}}"#;
        assert!(serde_json::from_str::<Request>(bare).is_err());
    }

    #[test]
    fn audio_device_methods_wire_shape() {
        // Mirrored by ui/src/lib/ipc.ts (listAudioDevices / setAudioDevice).
        let m = Method::SetAudioDevice {
            side: crate::share::MixerSide::Send,
            track: crate::share::DeviceTrack::Mic,
            device: Some("{mic}".into()),
        };
        let v = serde_json::to_value(Request { id: 1, method: m }).unwrap();
        assert_eq!(v["method"], "set_audio_device");
        assert_eq!(v["params"]["side"], "send");
        assert_eq!(v["params"]["track"], "mic");
        assert_eq!(v["params"]["device"], "{mic}");
        // A missing device is the System default.
        let r: Request = serde_json::from_str(
            r#"{"id":2,"method":"set_audio_device","params":{"side":"receive","track":"output"}}"#,
        )
        .unwrap();
        assert!(matches!(r.method, Method::SetAudioDevice { device: None, .. }));
        let r: Request = serde_json::from_str(r#"{"id":3,"method":"list_audio_devices"}"#).unwrap();
        assert!(matches!(r.method, Method::ListAudioDevices));
        let reply = Reply::AudioDevices { devices: crate::share::AudioDevices::default() };
        let v = serde_json::to_value(&reply).unwrap();
        assert_eq!(v["type"], "audio_devices");
        assert!(v["devices"]["render"].is_array() && v["devices"]["capture"].is_array());
    }

    #[test]
    fn outbound_distinguishes_response_from_event() {
        let resp = Outbound::Response { id: 1, result: Reply::Pong };
        let ev = Outbound::Event(Event::Notice { text: "hi".into() });
        let r: Outbound = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        let e: Outbound = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert!(matches!(r, Outbound::Response { id: 1, .. }));
        assert!(matches!(e, Outbound::Event(Event::Notice { .. })));
    }

    #[tokio::test]
    async fn capped_reader_splits_lines_and_rejects_oversize() {
        let data: &[u8] = b"one\ntwo\n";
        let mut rd = tokio::io::BufReader::new(data);
        let mut buf = Vec::new();
        assert!(read_line_capped(&mut rd, &mut buf, 16).await.unwrap().is_some());
        assert_eq!(buf, b"one");
        assert!(read_line_capped(&mut rd, &mut buf, 16).await.unwrap().is_some());
        assert_eq!(buf, b"two");
        assert!(read_line_capped(&mut rd, &mut buf, 16).await.unwrap().is_none());

        let big = [b'x'; 64];
        let mut rd = tokio::io::BufReader::new(&big[..]);
        let err = read_line_capped(&mut rd, &mut buf, 16).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[cfg(windows)]
    #[test]
    fn sddl_names_current_user_only() {
        let sid = security::current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "got {sid}");
        assert_eq!(security::sddl_for(&sid), format!("D:P(A;;GA;;;{sid})"));
        let mut sec = security::PipeSecurity::current_user_only().unwrap();
        assert!(!sec.as_ptr().is_null());
    }
}
