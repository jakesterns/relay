//! Tauri shell for Relay.
//!
//! The shell owns no machine state. Every command is a thin call over IPC to
//! the always-on core; if the core is not running the commands return an
//! error and the frontend shows its offline / mock state.

use relay_core::ipc::{Method, Reply};
use relay_core::types::{CoreState, ProcessInfo, Profile, ProfileSummary};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

mod stream_host;
mod window_state;

#[derive(Debug, Serialize)]
struct CmdError {
    message: String,
}

impl From<anyhow::Error> for CmdError {
    fn from(e: anyhow::Error) -> Self {
        Self { message: format!("{e:#}") }
    }
}

type CmdResult<T> = Result<T, CmdError>;

/// The user's close preference, mirrored here so the window's close handler
/// can read it without waiting.
///
/// The handler must not block: the window should disappear the instant it is
/// closed, and asking the core over IPC first would put a round-trip in front
/// of that. Mirroring the one bit that matters keeps the decision free.
///
/// Kept fresh from three places: once at startup, on every read, and on every
/// write. Defaults to false, the value that costs nothing to be wrong about —
/// the window closes and the core keeps running, as it always has.
static CLOSE_QUITS_CORE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set once the window's close has been handled, so the exit that follows
/// cannot re-enter the handler.
static CLOSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn remember_close_pref(prefs: &relay_core::uiprefs::UiPrefs) {
    CLOSE_QUITS_CORE.store(
        prefs.close_action == relay_core::uiprefs::CloseAction::QuitRelay,
        std::sync::atomic::Ordering::Relaxed,
    );
}

async fn call(method: Method) -> anyhow::Result<Reply> {
    #[cfg(windows)]
    {
        let mut c = relay_core::ipc::client::Client::connect().await?;
        c.call(method).await
    }
    #[cfg(not(windows))]
    {
        let _ = method;
        anyhow::bail!("relay-core IPC is Windows-only")
    }
}

fn unexpected(reply: Reply) -> anyhow::Error {
    match reply {
        Reply::Error { message } => anyhow::anyhow!(message),
        other => anyhow::anyhow!("unexpected reply: {other:?}"),
    }
}

#[tauri::command]
async fn core_status() -> CmdResult<CoreState> {
    match call(Method::Status).await? {
        Reply::Status { state } => Ok(*state),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn list_profiles() -> CmdResult<Vec<ProfileSummary>> {
    match call(Method::ListProfiles).await? {
        Reply::Profiles { profiles } => Ok(profiles),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn get_profile(id: Uuid) -> CmdResult<Profile> {
    match call(Method::GetProfile { id }).await? {
        Reply::Profile { profile } => Ok(*profile),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn save_profile(profile: Profile) -> CmdResult<()> {
    match call(Method::SaveProfile { profile: Box::new(profile) }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn delete_profile(id: Uuid) -> CmdResult<()> {
    match call(Method::DeleteProfile { id }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn apply_profile(id: Uuid) -> CmdResult<()> {
    match call(Method::ApplyProfile { id }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn restore_all() -> CmdResult<()> {
    match call(Method::RestoreAll).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn list_processes() -> CmdResult<Vec<ProcessInfo>> {
    match call(Method::ListProcesses).await? {
        Reply::Processes { processes } => Ok(processes),
        other => Err(unexpected(other).into()),
    }
}

/// Start the core if it is not already up, and wait until it answers.
///
/// This is what makes Relay an app rather than a service with a viewer: the
/// window is opened from the Start Menu, finds nothing listening, and fixes
/// that itself. Errors come back already worded for a person to read — see
/// `relay_core::startup`.
#[tauri::command]
async fn start_core() -> CmdResult<bool> {
    match relay_core::startup::ensure_running().await {
        Ok(relay_core::startup::Started::Already) => Ok(false),
        Ok(relay_core::startup::Started::Launched) => Ok(true),
        Err(e) => Err(CmdError { message: e.message() }),
    }
}

/// The user has read the last-crash line (S38).
#[tauri::command]
async fn ack_crash() -> CmdResult<()> {
    match call(Method::AckCrash).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn get_ui_prefs() -> CmdResult<relay_core::uiprefs::UiPrefs> {
    match call(Method::GetUiPrefs).await? {
        Reply::UiPrefs { prefs } => {
            remember_close_pref(&prefs);
            Ok(prefs)
        }
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn set_ui_prefs(
    prefs: relay_core::uiprefs::UiPrefs,
) -> CmdResult<relay_core::uiprefs::UiPrefs> {
    match call(Method::SetUiPrefs { prefs }).await? {
        Reply::UiPrefs { prefs } => {
            remember_close_pref(&prefs);
            Ok(prefs)
        }
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn get_autostart() -> CmdResult<bool> {
    match call(Method::GetAutostart).await? {
        Reply::Autostart { enabled } => Ok(enabled),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn set_autostart(enabled: bool) -> CmdResult<bool> {
    match call(Method::SetAutostart { enabled }).await? {
        Reply::Autostart { enabled } => Ok(enabled),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn start_share(request: relay_core::share::ShareRequest) -> CmdResult<()> {
    match call(Method::StartShare { request: Box::new(request) }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn stop_share() -> CmdResult<()> {
    match call(Method::StopShare).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn start_share_preset(
    preset: String,
    code: String,
    peer: Option<String>,
    peer_id: Option<String>,
) -> CmdResult<()> {
    match call(Method::StartSharePreset { preset, code, peer, peer_id }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn record(on: bool) -> CmdResult<()> {
    match call(Method::Record { on }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn save_replay() -> CmdResult<()> {
    match call(Method::SaveReplay).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn switch_source(target: relay_core::share::SourceTarget) -> CmdResult<()> {
    match call(Method::SwitchSource { target }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

/// Per-track gain and mute, live (S37).
#[tauri::command]
async fn set_mixer(
    side: relay_core::share::MixerSide,
    faders: relay_core::share::FaderSet,
) -> CmdResult<()> {
    match call(Method::SetMixer { side, faders }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

/// Mirrors `Reply::Presets`; the frontend gets one object.
#[derive(Debug, serde::Serialize)]
struct PresetsReply {
    presets: Vec<relay_core::presets::SharePresetDef>,
    recording: relay_core::presets::RecordingSettings,
}

#[tauri::command]
async fn list_presets() -> CmdResult<PresetsReply> {
    match call(Method::ListPresets).await? {
        Reply::Presets { presets, recording } => Ok(PresetsReply { presets, recording }),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn save_preset(preset: relay_core::presets::SharePresetDef) -> CmdResult<()> {
    match call(Method::SavePreset { preset: Box::new(preset) }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn delete_preset(id: String) -> CmdResult<()> {
    match call(Method::DeletePreset { id }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn set_recording_settings(settings: relay_core::presets::RecordingSettings) -> CmdResult<()> {
    match call(Method::SetRecordingSettings { settings }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

/// Start receiving. The request leaves the page without a host window; this
/// window's handle is filled in here, so the engine creates the stream
/// window embedded in it (S29). The page never sees an HWND.
#[tauri::command]
async fn start_receive(
    window: tauri::Window,
    mut request: relay_core::share::ReceiveRequest,
) -> CmdResult<()> {
    request.host = host_hwnd(&window);
    match call(Method::StartReceive { request: Box::new(request) }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

/// This window's HWND as the engine wants it, or `None` off Windows.
fn host_hwnd(window: &tauri::Window) -> Option<u64> {
    #[cfg(windows)]
    {
        window.hwnd().ok().map(|h| h.0 as isize as u64).filter(|h| *h != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        None
    }
}

/// The page measured the Receive screen's video area (CSS px, viewport
/// relative), or left the screen (`None`). The stream window follows.
#[tauri::command]
fn set_video_area(window: tauri::Window, area: Option<stream_host::Area>) {
    stream_host::set_area(area);
    stream_host::apply(&window);
}

/// Embed the stream in this window or pop it out into one of its own. The
/// engine confirms with a `host` event; until then nothing here changes.
#[tauri::command]
async fn set_stream_mode(
    window: tauri::Window,
    mode: relay_core::share::HostMode,
) -> CmdResult<()> {
    let owner = host_hwnd(&window).unwrap_or(0);
    match call(Method::HostReceive { mode, owner }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

/// Whether a stream window exists right now and how it is hosted, for a
/// Receive screen that mounts mid-receive.
#[tauri::command]
fn stream_status() -> stream_host::StreamStatus {
    stream_host::status()
}

#[tauri::command]
async fn stop_receive() -> CmdResult<()> {
    match call(Method::StopReceive).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

/// Mirrors `Reply::Hardware`; the frontend gets one object.
#[derive(Debug, serde::Serialize)]
struct HardwareReply {
    headsets: Vec<relay_core::hardware::Headset>,
    monitors: Vec<relay_core::hardware::Monitor>,
    interfaces: Vec<relay_core::hardware::AudioInterface>,
    connected: relay_core::hardware::HardwareView,
    /// Monitors with at least one *verified* vendor DDC/CI control. Forwarded
    /// verbatim: the UI must not infer a control from the advertised opcode
    /// list, so dropping this here would silently re-enable guessed sliders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    vendor_controls: Vec<relay_core::hardware::MonitorVendorControls>,
}

#[tauri::command]
async fn list_hardware() -> CmdResult<HardwareReply> {
    match call(Method::ListHardware).await? {
        Reply::Hardware { headsets, monitors, interfaces, connected, vendor_controls } => {
            Ok(HardwareReply {
                headsets,
                monitors,
                interfaces,
                connected: *connected,
                vendor_controls,
            })
        }
        other => Err(unexpected(other).into()),
    }
}

#[derive(Debug, Serialize)]
struct PreviewOut {
    original: String,
    processed: String,
    sample_rate: u32,
    hrtf_applied: bool,
}

#[tauri::command]
async fn render_preview(id: Uuid, wav: Option<String>) -> CmdResult<PreviewOut> {
    match call(Method::RenderPreview { id, wav }).await? {
        Reply::Preview { original, processed, sample_rate, hrtf_applied } => {
            Ok(PreviewOut { original, processed, sample_rate, hrtf_applied })
        }
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn save_hardware(item: relay_core::ipc::HardwareItem) -> CmdResult<()> {
    match call(Method::SaveHardware { item }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn delete_hardware(id: String) -> CmdResult<()> {
    match call(Method::DeleteHardware { id }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn probe_hardware() -> CmdResult<relay_core::hardware::ProbeReport> {
    match call(Method::ProbeHardware).await? {
        Reply::Probe { report } => Ok(*report),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn import_curve(headset: String, csv: String) -> CmdResult<Vec<(f32, f32)>> {
    match call(Method::ImportCurve { headset: relay_core::types::HeadsetId(headset), csv }).await? {
        Reply::Curve { points } => Ok(points),
        other => Err(unexpected(other).into()),
    }
}

/// What this PC can do with video (HEVC, H.264). Spawns a probe child, so
/// call it when a screen opens rather than on every refresh.
#[tauri::command]
async fn share_capabilities() -> CmdResult<serde_json::Value> {
    match call(Method::ShareCapabilities).await? {
        Reply::Capabilities {
            can_share,
            can_receive,
            adapters,
            encoders,
            decoders,
            share_codecs,
            receive_codecs,
        } => Ok(serde_json::json!({
            "can_share": can_share,
            "can_receive": can_receive,
            "adapters": adapters,
            "encoders": encoders,
            "decoders": decoders,
            "share_codecs": share_codecs,
            "receive_codecs": receive_codecs,
        })),
        other => Err(unexpected(other).into()),
    }
}

/// Whether Windows Firewall will let an incoming share through. Read-only
/// and unelevated; the Share and Receive screens call it when they open so a
/// blocked machine is named as blocked rather than looking like a dead LAN.
#[tauri::command]
async fn firewall_status() -> CmdResult<relay_core::firewall::FirewallStatus> {
    match call(Method::FirewallStatus).await? {
        Reply::Firewall { status } => Ok(*status),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn apo_status() -> CmdResult<relay_core::audio_apo::ApoStatus> {
    match call(Method::ApoStatus).await? {
        Reply::Apo { status } => Ok(status),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn install_apo() -> CmdResult<()> {
    match call(Method::InstallApo).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn uninstall_apo() -> CmdResult<()> {
    match call(Method::UninstallApo).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn vdevice_status() -> CmdResult<relay_core::vdevice::VdeviceStatus> {
    match call(Method::VdeviceStatus).await? {
        Reply::Vdevice { status } => Ok(*status),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn set_vdevice_consent(apo: bool, camera: bool, microphone: bool) -> CmdResult<()> {
    match call(Method::SetVdeviceConsent { apo, camera, microphone }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn vdevice_dry_run() -> CmdResult<Vec<String>> {
    match call(Method::VdeviceDryRun).await? {
        Reply::DryRun { lines } => Ok(lines),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn install_vcam() -> CmdResult<()> {
    match call(Method::InstallVcam).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn uninstall_vcam() -> CmdResult<()> {
    match call(Method::UninstallVcam).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn search_catalog(
    query: String,
) -> CmdResult<Vec<relay_core::hardware::catalog::CatalogEntry>> {
    match call(Method::SearchCatalog { query }).await? {
        Reply::Catalog { entries } => Ok(entries),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn add_headset_from_catalog(
    entry: relay_core::hardware::catalog::CatalogEntry,
    endpoint: Option<String>,
) -> CmdResult<()> {
    match call(Method::AddHeadsetFromCatalog { entry: Box::new(entry), endpoint }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn uninstall_plan(keep_data: bool) -> CmdResult<Vec<String>> {
    match call(Method::UninstallPlan { keep_data }).await? {
        Reply::DryRun { lines } => Ok(lines),
        other => Err(unexpected(other).into()),
    }
}

/// What an elevated install/removal would change, before the UAC prompt.
#[tauri::command]
async fn elevation_plan(op: relay_core::elevate::ElevatedOp) -> CmdResult<Vec<String>> {
    match call(Method::ElevationPlan { op }).await? {
        Reply::DryRun { lines } => Ok(lines),
        other => Err(unexpected(other).into()),
    }
}

/// Raise the UAC prompt and run one op. Declining is not an error — it comes
/// back as `declined: true` with the sentence that says nothing changed.
#[tauri::command]
async fn run_elevated(op: relay_core::elevate::ElevatedOp) -> CmdResult<serde_json::Value> {
    match call(Method::RunElevated { op }).await? {
        Reply::Elevation { declined, ok, lines } => Ok(serde_json::json!({
            "declined": declined, "ok": ok, "lines": lines,
        })),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn launch_uninstaller() -> CmdResult<()> {
    match call(Method::LaunchUninstaller).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn discover_receivers() -> CmdResult<serde_json::Value> {
    match call(Method::DiscoverReceivers).await? {
        Reply::Receivers { receivers } => Ok(receivers),
        other => Err(unexpected(other).into()),
    }
}

// Remembered PCs (S35).
#[tauri::command]
async fn list_peers() -> CmdResult<Vec<relay_core::peers::Peer>> {
    match call(Method::ListPeers).await? {
        Reply::Peers { peers } => Ok(peers),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn forget_peer(id: String) -> CmdResult<()> {
    match call(Method::ForgetPeer { id }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

#[tauri::command]
async fn set_peer_favourite(id: String, favourite: bool) -> CmdResult<()> {
    match call(Method::SetPeerFavourite { id, favourite }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
}

/// Keep a subscription open to the core and forward its events to the webview
/// as `core://state` and `core://notice`. Reconnects while the window is open.
#[cfg(windows)]
fn spawn_event_bridge(app: AppHandle) {
    use relay_core::ipc::{client::Client, Event};
    tauri::async_runtime::spawn(async move {
        loop {
            match Client::connect().await {
                Ok(mut c) => {
                    if c.call(Method::Subscribe).await.is_ok() {
                        while let Ok(Some(ev)) = c.next_event().await {
                            match ev {
                                Event::StateChanged { state } => {
                                    let _ = app.emit("core://state", *state);
                                }
                                Event::Notice { text } => {
                                    let _ = app.emit("core://notice", text);
                                }
                                // The core is stopping on purpose (tray quit).
                                // Close with it rather than sitting there
                                // reporting an offline service, which would
                                // look like a fault instead of a choice.
                                Event::Quitting => {
                                    // `exit` skips the close event, which is
                                    // where the window state is normally saved.
                                    if let Some(w) = app.get_webview_window("main") {
                                        window_state::persist(&w.as_ref().window());
                                    }
                                    app.exit(0);
                                    return;
                                }
                                Event::ShareStats { data } => {
                                    let _ = app.emit("core://share-stats", data);
                                }
                                Event::ShareStatus { sharing, peer, message, trusted } => {
                                    let _ = app.emit(
                                        "core://share-status",
                                        serde_json::json!({
                                            "sharing": sharing,
                                            "peer": peer,
                                            "message": message,
                                            "trusted": trusted,
                                        }),
                                    );
                                }
                                Event::RecordingStatus { on, path } => {
                                    let _ = app.emit(
                                        "core://recording-status",
                                        serde_json::json!({ "on": on, "path": path }),
                                    );
                                }
                                Event::ReplaySaved { path, ms } => {
                                    let _ = app.emit(
                                        "core://replay-saved",
                                        serde_json::json!({ "path": path, "ms": ms }),
                                    );
                                }
                                Event::SourceChanged { data } => {
                                    let _ = app.emit("core://source-changed", data);
                                }
                                Event::SharePreview { width, height, jpeg } => {
                                    let _ = app.emit(
                                        "core://share-preview",
                                        serde_json::json!({
                                            "width": width, "height": height, "jpeg": jpeg,
                                        }),
                                    );
                                }
                                Event::StreamWindow {
                                    hwnd,
                                    width,
                                    height,
                                    mode,
                                    excluded_from_capture,
                                } => {
                                    stream_host::on_window(
                                        hwnd,
                                        width,
                                        height,
                                        &mode,
                                        excluded_from_capture,
                                    );
                                    if let Some(w) = app.get_webview_window("main") {
                                        stream_host::apply(&w.as_ref().window());
                                    }
                                    let _ = app.emit("core://stream", stream_host::status());
                                }
                                // Closing the popped-out window means "back into
                                // the app", never "stop". The engine embeds itself
                                // (it remembers the owner) and its `host` event
                                // follows; nothing to ask for here. It used to be
                                // asked for from here, and that round trip is what
                                // took up to 47 s on the second PC.
                                Event::StreamPopoutClosed => {
                                    tracing::info!("popped-out stream window closed");
                                }
                                Event::ReceiveStatus {
                                    receiving,
                                    code,
                                    sender,
                                    message,
                                    codec,
                                    trusted,
                                    ..
                                } => {
                                    tracing::info!(
                                        receiving,
                                        ?sender,
                                        ?message,
                                        trusted,
                                        "receive status from the core"
                                    );
                                    stream_host::on_receive_status(
                                        receiving,
                                        code.as_deref(),
                                        sender.as_deref(),
                                        codec.as_deref(),
                                    );
                                    if !receiving {
                                        let _ = app.emit("core://stream", stream_host::status());
                                    }
                                    let _ = app.emit(
                                        "core://receive-status",
                                        serde_json::json!({
                                            "receiving": receiving,
                                            "code": code,
                                            "sender": sender,
                                            "message": message,
                                            "codec": codec,
                                            "trusted": trusted,
                                        }),
                                    );
                                }
                            }
                        }
                    }
                    let _ = app.emit("core://offline", ());
                }
                Err(_) => {
                    let _ = app.emit("core://offline", ());
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

#[cfg(not(windows))]
fn spawn_event_bridge(_app: AppHandle) {}

/// Try to bring a core up as soon as the window exists, reporting progress to
/// the frontend so the offline screen can say "starting…" instead of "dead".
///
/// Fire-and-forget: the event bridge above reconnects on its own, so a core
/// that comes up late still lands. The frontend can re-run this from a button
/// if it failed — `ensure_running` is safe to call repeatedly.
fn spawn_core_autostart(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let _ = app.emit("core://starting", ());
        match relay_core::startup::ensure_running().await {
            Ok(_) => {
                load_close_pref().await;
                let _ = app.emit("core://started", ());
            }
            Err(e) => {
                let _ = app.emit("core://start-failed", e.message());
            }
        }
    });
}

/// Read the close preference once the core is up, so the close handler has a
/// real answer rather than the default. Anything unreadable leaves the default
/// in place, which is "keep running".
async fn load_close_pref() {
    if let Ok(Reply::UiPrefs { prefs }) = call(Method::GetUiPrefs).await {
        remember_close_pref(&prefs);
    }
}

/// One window per session. Returns the held lock when this process is the
/// window, or `None` when it handed focus to the window that already exists
/// and should exit.
///
/// A second launch — the Start Menu shortcut clicked again, the installer's
/// "run Relay" — used to open a second window, each with its own IPC
/// subscription. Now it brings the first one forward and leaves.
///
/// The wait covers the two moments the other window cannot be focused: it is
/// still starting (the window is created hidden until its position is
/// restored), or it is closing and holding the name for its last few seconds
/// while it tells the core to stop. In the first case it appears and gets
/// focus; in the second the name frees up and this launch becomes the window,
/// so the click is never lost.
#[cfg(windows)]
fn single_instance() -> Option<Option<relay_core::instance::InstanceLock>> {
    use relay_core::instance::InstanceLock;

    let name = relay_core::config::ui_mutex_name();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        match InstanceLock::acquire(&name) {
            Ok(Some(lock)) => return Some(Some(lock)),
            // A mutex that cannot be created at all says nothing about another
            // window. Opening a possible second window beats opening none.
            Err(e) => {
                tracing::warn!("single-instance check unavailable: {e:#}");
                return Some(None);
            }
            Ok(None) => {}
        }
        if relay_core::launcher::focus_ui() || std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// A rotating `logs\ui.log` beside the core's, or `None` if it cannot be
/// opened. The shell had no log at all until S29, which meant the one
/// process that places the stream window could not say what it did with it.
fn ui_log_writer() -> Option<relay_core::logging::SharedWriter> {
    let paths = relay_core::config::Paths::default_for_user().ok()?;
    let _ = std::fs::create_dir_all(paths.log_dir());
    relay_core::logging::SharedWriter::open(
        paths.log_dir().join("ui.log"),
        relay_core::logging::MAX_BYTES,
        relay_core::logging::KEEP,
    )
    .ok()
}

pub fn run() {
    match ui_log_writer() {
        Some(file) => tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_target(false)
            .with_ansi(false)
            .compact()
            .with_writer(file)
            .init(),
        None => tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_target(false)
            .compact()
            .init(),
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "relay-ui starting");
    // S38: a panic in the shell leaves a record beside the core's.
    if let Ok(paths) = relay_core::config::Paths::default_for_user() {
        relay_core::crash::install_panic_hook(relay_core::crash::dir(&paths), "relay-ui");
    }

    // Held until the process ends; the OS releases the name then.
    #[cfg(windows)]
    let _instance = match single_instance() {
        Some(lock) => lock,
        None => return,
    };

    tauri::Builder::default()
        .setup(|app| {
            if let Some(w) = app.get_webview_window("main") {
                window_state::restore(&w.as_ref().window());
            }
            spawn_event_bridge(app.handle().clone());
            spawn_core_autostart(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // The stream window is a separate native window sitting over the
            // video area; every change to where this window is or how big it
            // is has to be followed, or the picture is left behind.
            if matches!(
                event,
                tauri::WindowEvent::Moved(_)
                    | tauri::WindowEvent::Resized(_)
                    | tauri::WindowEvent::ScaleFactorChanged { .. }
            ) {
                stream_host::apply(window);
            }
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Act once. The `exit` on the quit path can raise this event
                // again for the same window; re-entering would queue a second
                // shutdown and a second exit.
                if CLOSING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                // Before anything else: on either path below the window is
                // about to be gone.
                window_state::persist(window);
                if !CLOSE_QUITS_CORE.load(std::sync::atomic::Ordering::Relaxed) {
                    // The default, and the reason the window and the core are
                    // separate processes: the close proceeds untouched, this
                    // process ends and gives its ~25 MB back, and the core
                    // carries on applying profiles with the notification-area
                    // icon there to say so.
                    //
                    // S38: tell the core, so it can say so in a balloon (a
                    // preference, on by default). Bounded and blocking on
                    // purpose: this process is about to exit, and a task
                    // spawned here would be dropped with it.
                    let _ = tauri::async_runtime::block_on(async {
                        tokio::time::timeout(
                            std::time::Duration::from_millis(500),
                            call(Method::WindowClosed),
                        )
                        .await
                    });
                    return;
                }
                // The user asked for closing to mean quitting Relay. The close
                // still is not prevented — the window goes now — but the core
                // has to be told to stop, and that cannot be done from here
                // synchronously, so the exit is made explicit rather than
                // racing the window count against the task below.
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    // The core's own teardown restores audio and display, so
                    // this cannot leave a game profile applied.
                    //
                    // Bounded, and the bound matters: the core acts on the
                    // request as soon as it reads it, so its reply can be lost
                    // in its own shutdown. Exiting must never wait on an
                    // answer that may not be coming.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        call(Method::Shutdown),
                    )
                    .await;
                    app.exit(0);
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            core_status,
            list_profiles,
            get_profile,
            save_profile,
            delete_profile,
            apply_profile,
            restore_all,
            list_processes,
            get_autostart,
            set_autostart,
            start_core,
            get_ui_prefs,
            set_ui_prefs,
            start_share,
            stop_share,
            start_share_preset,
            record,
            save_replay,
            switch_source,
            set_mixer,
            list_presets,
            save_preset,
            delete_preset,
            set_recording_settings,
            start_receive,
            stop_receive,
            set_video_area,
            set_stream_mode,
            stream_status,
            discover_receivers,
            list_peers,
            forget_peer,
            set_peer_favourite,
            ack_crash,
            list_hardware,
            save_hardware,
            delete_hardware,
            probe_hardware,
            import_curve,
            render_preview,
            share_capabilities,
            firewall_status,
            apo_status,
            install_apo,
            uninstall_apo,
            vdevice_status,
            set_vdevice_consent,
            vdevice_dry_run,
            install_vcam,
            uninstall_vcam,
            uninstall_plan,
            elevation_plan,
            run_elevated,
            launch_uninstaller,
            search_catalog,
            add_headset_from_catalog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Relay");
}
