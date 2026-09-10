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
            discover_receivers,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Relay");
}
