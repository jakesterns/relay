//! Tauri shell for Relay.
//!
//! The shell owns no machine state. Every command is a thin call over IPC to
//! the always-on core; if the core is not running the commands return an
//! error and the frontend shows its offline / mock state.

use relay_core::ipc::{Method, Reply};
use relay_core::types::{CoreState, ProcessInfo, Profile, ProfileSummary};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

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
async fn start_share_preset(preset: String, code: String, peer: Option<String>) -> CmdResult<()> {
    match call(Method::StartSharePreset { preset, code, peer }).await? {
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

#[tauri::command]
async fn start_receive(request: relay_core::share::ReceiveRequest) -> CmdResult<()> {
    match call(Method::StartReceive { request: Box::new(request) }).await? {
        Reply::Ok => Ok(()),
        other => Err(unexpected(other).into()),
    }
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
}

#[tauri::command]
async fn list_hardware() -> CmdResult<HardwareReply> {
    match call(Method::ListHardware).await? {
        Reply::Hardware { headsets, monitors, interfaces, connected } => {
            Ok(HardwareReply { headsets, monitors, interfaces, connected: *connected })
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
async fn uninstall_plan(keep_data: bool) -> CmdResult<Vec<String>> {
    match call(Method::UninstallPlan { keep_data }).await? {
        Reply::DryRun { lines } => Ok(lines),
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
                                Event::ShareStats { data } => {
                                    let _ = app.emit("core://share-stats", data);
                                }
                                Event::ShareStatus { sharing, peer, message } => {
                                    let _ = app.emit(
                                        "core://share-status",
                                        serde_json::json!({
                                            "sharing": sharing,
                                            "peer": peer,
                                            "message": message,
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
                                Event::ReceiveStatus { receiving, code, sender, message } => {
                                    let _ = app.emit(
                                        "core://receive-status",
                                        serde_json::json!({
                                            "receiving": receiving,
                                            "code": code,
                                            "sender": sender,
                                            "message": message,
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

pub fn run() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .compact()
        .init();

    tauri::Builder::default()
        .setup(|app| {
            spawn_event_bridge(app.handle().clone());
            Ok(())
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
            start_share,
            stop_share,
            start_share_preset,
            record,
            save_replay,
            switch_source,
            list_presets,
            save_preset,
            delete_preset,
            set_recording_settings,
            start_receive,
            stop_receive,
            discover_receivers,
            list_hardware,
            save_hardware,
            delete_hardware,
            probe_hardware,
            import_curve,
            render_preview,
            apo_status,
            install_apo,
            uninstall_apo,
            vdevice_status,
            set_vdevice_consent,
            vdevice_dry_run,
            install_vcam,
            uninstall_vcam,
            uninstall_plan,
            launch_uninstaller,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Relay");
}
