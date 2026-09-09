//! Wires focus events → profile selection → apply/restore, and serves IPC.
//! Single-threaded tokio runtime; the only other thread is the Win32 loop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::apply::{Applier, AudioControl, DisplayControl, FileRecorder, Noop};
use crate::backup::BackupFile;
use crate::config::Paths;
use crate::footprint::FootprintMeter;
use crate::hardware::{HardwareProbe, NoopHardwareProbe};
use crate::hotkeys::{self, HotkeyAction};
use crate::ipc::{Event, Method, Reply};
use crate::profiles::ProfileStore;
use crate::types::{AudioChainState, CoreState, DisplayState, Foreground};
use crate::winloop::{CoreEvent, WinLoop};

pub struct Backends {
    pub audio: Arc<dyn AudioControl>,
    pub display: Arc<dyn DisplayControl>,
    pub hardware: Arc<dyn HardwareProbe>,
}

impl Default for Backends {
    fn default() -> Self {
        let noop = Arc::new(Noop);
        Self { audio: noop.clone(), display: noop, hardware: Arc::new(NoopHardwareProbe) }
    }
}

/// Environment variable naming a file for the [`FileRecorder`] test backend.
pub const RECORDING_ENV: &str = "RELAY_RECORDING_BACKEND";

impl Backends {
    /// Production no-op backends, or the file-backed recorder when
    /// `RELAY_RECORDING_BACKEND=<path>` is set (integration tests only).
    pub fn from_env() -> Self {
        match std::env::var(RECORDING_ENV) {
            Ok(path) if !path.is_empty() => {
                let rec = Arc::new(FileRecorder::at(path));
                Self { audio: rec.clone(), display: rec, hardware: Arc::new(NoopHardwareProbe) }
            }
            _ => Self::default(),
        }
    }
}

/// Everything the IPC handler and the event loop share.
struct Inner {
    store: ProfileStore,
    applier: Applier,
    state: CoreState,
    meter: FootprintMeter,
    hardware: Arc<dyn HardwareProbe>,
    /// Set by `ApplyProfile` over IPC: stay applied regardless of focus until
    /// `RestoreAll` or the profile is applied automatically anyway.
    pinned: bool,
}

pub struct Service {
    inner: Arc<Mutex<Inner>>,
    events: broadcast::Sender<Event>,
    shutdown: mpsc::UnboundedSender<CoreEvent>,
}

impl Service {
    /// Blocks until shutdown. Restores any pending state on start and always
    /// restores on the way out.
    pub fn run(paths: Paths, backends: Backends) -> Result<()> {
        paths.ensure()?;
        info!(root = %paths.root().display(), "relay-core starting");

        let store = ProfileStore::load(paths.profiles_file())?;
        let mut applier =
            Applier::new(backends.audio, backends.display, BackupFile::at(paths.backup_file()));
        if applier.recover_on_start()? {
            info!("restored original state left over from a previous run");
        }

        let inner = Arc::new(Mutex::new(Inner {
            store,
            applier,
            state: CoreState::default(),
            meter: FootprintMeter::new(),
            hardware: backends.hardware,
            pinned: false,
        }));
        let (events, _) = broadcast::channel(64);
        let (tx, rx) = mpsc::unbounded_channel();
        let service = Service { inner, events, shutdown: tx.clone() };

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = rt.block_on(service.main(tx, rx));

        // Belt and braces: whatever happened, put the machine back.
        let mut g = service.inner.lock();
        if let Err(e) = g.applier.restore() {
            warn!(error = %e, "restore on exit failed; will retry on next start");
        }
        info!("relay-core stopped");
        result
    }

    async fn main(
        &self,
        tx: mpsc::UnboundedSender<CoreEvent>,
        mut rx: mpsc::UnboundedReceiver<CoreEvent>,
    ) -> Result<()> {
        let winloop = WinLoop::spawn(tx, hotkeys::defaults())?;

        let handler =
            Arc::new(IpcHandler { inner: self.inner.clone(), shutdown: self.shutdown.clone() });
        let events_tx = self.events.clone();
        #[cfg(windows)]
        tokio::spawn(async move {
            if let Err(e) = crate::ipc::server::serve(handler, events_tx).await {
                warn!(error = %e, "ipc server stopped");
            }
        });
        #[cfg(not(windows))]
        let _ = (handler, events_tx);

        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Some(CoreEvent::ForegroundChanged(fg)) => self.on_foreground(fg),
                        Some(CoreEvent::Hotkey(action)) => self.on_hotkey(action),
                        Some(CoreEvent::Shutdown) | None => break,
                    }
                }
                _ = tick.tick() => {
                    let mut g = self.inner.lock();
                    g.state.footprint = g.meter.sample();
                }
            }
        }
        winloop.stop();
        Ok(())
    }

    fn on_foreground(&self, fg: Foreground) {
        let mut g = self.inner.lock();
        g.state.foreground = Some(fg.clone());

        let hw = g.hardware.probe();
        let pick = g.store.select(&fg.exe, &fg.title, &hw).cloned();
        match pick {
            Some(profile) => {
                g.pinned = false;
                match g.applier.apply(&profile) {
                    Ok(applied) => {
                        g.state.active_profile = Some(profile.summary());
                        g.state.audio_chain = applied.audio;
                        g.state.display_state = applied.display;
                    }
                    Err(e) => {
                        warn!(error = %e, profile = %profile.name, "apply failed");
                        g.state.active_profile = None;
                        g.state.audio_chain = AudioChainState::Bypass;
                        g.state.display_state = DisplayState::Default;
                    }
                }
            }
            None if g.pinned => {}
            None => {
                if g.applier.is_applied() {
                    if let Err(e) = g.applier.restore() {
                        warn!(error = %e, "restore on blur failed");
                    }
                }
                g.state.active_profile = None;
                g.state.audio_chain = AudioChainState::Bypass;
                g.state.display_state = DisplayState::Default;
            }
        }
        let state = Box::new(g.state.clone());
        drop(g);
        let _ = self.events.send(Event::StateChanged { state });
    }

    fn on_hotkey(&self, action: HotkeyAction) {
        info!(?action, "hotkey");
        match action {
            HotkeyAction::ToggleProfile => {
                let mut g = self.inner.lock();
                if g.applier.is_applied() {
                    if let Err(e) = g.applier.restore() {
                        warn!(error = %e, "restore via hotkey failed");
                    }
                    g.pinned = true; // stay off until focus changes again
                    g.state.active_profile = None;
                    g.state.audio_chain = AudioChainState::Bypass;
                    g.state.display_state = DisplayState::Default;
                    let state = Box::new(g.state.clone());
                    drop(g);
                    let _ = self.events.send(Event::StateChanged { state });
                }
            }
            HotkeyAction::ToggleShare | HotkeyAction::TogglePreview => {
                // Share is a separate on-demand process; the core only relays the intent.
                let _ = self.events.send(Event::Notice {
                    text: format!("{action:?} pressed (share engine not wired yet)"),
                });
            }
        }
    }
}

struct IpcHandler {
    inner: Arc<Mutex<Inner>>,
    shutdown: mpsc::UnboundedSender<CoreEvent>,
}

impl IpcHandler {
    fn handle_sync(&self, method: Method) -> Reply {
        let mut g = self.inner.lock();
        match method {
            Method::Ping => Reply::Pong,
            Method::Status => {
                g.state.footprint = g.meter.sample();
                Reply::Status { state: Box::new(g.state.clone()) }
            }
            Method::ListProfiles => {
                Reply::Profiles { profiles: g.store.all().iter().map(|p| p.summary()).collect() }
            }
            Method::GetProfile { id } => match g.store.get(id) {
                Some(p) => Reply::Profile { profile: Box::new(p.clone()) },
                None => Reply::Error { message: "no such profile".into() },
            },
            Method::SaveProfile { profile } => {
                g.store.upsert(*profile);
                match g.store.save() {
                    Ok(()) => Reply::Ok,
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::DeleteProfile { id } => {
                if !g.store.remove(id) {
                    return Reply::Error { message: "no such profile".into() };
                }
                match g.store.save() {
                    Ok(()) => Reply::Ok,
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::ApplyProfile { id } => {
                let Some(profile) = g.store.get(id).cloned() else {
                    return Reply::Error { message: "no such profile".into() };
                };
                match g.applier.apply(&profile) {
                    Ok(applied) => {
                        g.pinned = true;
                        g.state.active_profile = Some(profile.summary());
                        g.state.audio_chain = applied.audio;
                        g.state.display_state = applied.display;
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::RestoreAll => {
                g.pinned = false;
                match g.applier.restore() {
                    Ok(()) => {
                        g.state.active_profile = None;
                        g.state.audio_chain = AudioChainState::Bypass;
                        g.state.display_state = DisplayState::Default;
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::ListProcesses => {
                Reply::Processes { processes: crate::processes::list_windowed() }
            }
            Method::GetAutostart => match crate::autostart::is_enabled() {
                Ok(enabled) => Reply::Autostart { enabled },
                Err(e) => Reply::Error { message: e.to_string() },
            },
            Method::SetAutostart { enabled } => match crate::autostart::set(enabled) {
                Ok(()) => Reply::Autostart { enabled },
                Err(e) => Reply::Error { message: e.to_string() },
            },
            Method::Subscribe => Reply::Ok,
            Method::Shutdown => {
                let _ = self.shutdown.send(CoreEvent::Shutdown);
                Reply::Ok
            }
        }
    }
}

#[cfg(windows)]
impl crate::ipc::server::Handler for IpcHandler {
    async fn handle(&self, method: Method) -> Reply {
        self.handle_sync(method)
    }
}
