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
    /// The running share engine (a child process), if any.
    share: Option<crate::share::ShareEngine>,
    /// Last share request, so the Ctrl+Alt+S hotkey can re-start it.
    last_share: Option<crate::share::ShareRequest>,
    /// The running receive engine (a child process), if any.
    receive: Option<crate::share::ShareEngine>,
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
            share: None,
            last_share: None,
            receive: None,
        }));
        let (events, _) = broadcast::channel(64);
        let (tx, rx) = mpsc::unbounded_channel();
        let service = Service { inner, events, shutdown: tx.clone() };

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = rt.block_on(service.main(tx, rx));

        // Stop any share/receive child so it never outlives the core.
        let (share, receive) = {
            let mut g = service.inner.lock();
            (g.share.take(), g.receive.take())
        };
        if let Some(engine) = share {
            engine.stop();
        }
        if let Some(engine) = receive {
            engine.stop();
        }

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

        let handler = Arc::new(IpcHandler {
            inner: self.inner.clone(),
            shutdown: self.shutdown.clone(),
            events: self.events.clone(),
        });
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
            HotkeyAction::ToggleShare => {
                let sharing = self.inner.lock().share.is_some();
                if sharing {
                    kill_share(&self.inner, &self.events);
                } else {
                    let last = self.inner.lock().last_share.clone();
                    match last {
                        Some(req) => {
                            spawn_share(&self.inner, &self.events, req);
                        }
                        None => {
                            let _ = self.events.send(Event::Notice {
                                text: "Ctrl+Alt+S: open Relay and press Start to configure the first share".into(),
                            });
                        }
                    }
                }
            }
            HotkeyAction::TogglePreview => {
                // Preview is a UI concern; relay the intent for the UI to toggle.
                let _ = self.events.send(Event::Notice { text: "preview toggled".into() });
            }
        }
    }
}

struct IpcHandler {
    inner: Arc<Mutex<Inner>>,
    shutdown: mpsc::UnboundedSender<CoreEvent>,
    events: broadcast::Sender<Event>,
}

/// Spawn the share engine and a thread that relays its NDJSON as IPC events.
fn spawn_share(
    inner: &Arc<Mutex<Inner>>,
    events: &broadcast::Sender<Event>,
    req: crate::share::ShareRequest,
) -> Reply {
    use crate::share::{ShareEngine, ShareEvent};
    let mut g = inner.lock();
    if g.share.is_some() {
        return Reply::Error { message: "a share is already running".into() };
    }
    let (tx, rx) = std::sync::mpsc::channel::<ShareEvent>();
    let engine = match ShareEngine::start(&req, tx) {
        Ok(e) => e,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    g.share = Some(engine);
    g.last_share = Some(req);
    g.state.sharing = crate::types::ShareState::Sharing { peer: String::new() };
    drop(g);

    let events2 = events.clone();
    let inner2 = inner.clone();
    std::thread::Builder::new()
        .name("relay-share-pump".into())
        .spawn(move || {
            crate::share::pump(rx, |ev| match ev {
                ShareEvent::Stats { data } => {
                    let _ = events2.send(Event::ShareStats { data });
                }
                ShareEvent::Connected { peer } => {
                    inner2.lock().state.sharing =
                        crate::types::ShareState::Sharing { peer: peer.clone() };
                    let _ = events2.send(Event::ShareStatus {
                        sharing: true,
                        peer: Some(peer),
                        message: None,
                    });
                }
                ShareEvent::Error { message } => {
                    let _ = events2.send(Event::ShareStatus {
                        sharing: true,
                        peer: None,
                        message: Some(message),
                    });
                }
                ShareEvent::Waiting { .. } | ShareEvent::Paired { .. } => {}
                ShareEvent::Exited { ok, .. } => {
                    let mut ig = inner2.lock();
                    ig.share = None;
                    ig.state.sharing = crate::types::ShareState::Off;
                    drop(ig);
                    let _ = events2.send(Event::ShareStatus {
                        sharing: false,
                        peer: None,
                        message: (!ok).then(|| "share engine stopped unexpectedly".to_string()),
                    });
                }
            });
            // Channel closed (child stdout closed): make sure state is cleared.
            let mut ig = inner2.lock();
            if ig.share.is_some() {
                ig.share = None;
                ig.state.sharing = crate::types::ShareState::Off;
                drop(ig);
                let _ =
                    events2.send(Event::ShareStatus { sharing: false, peer: None, message: None });
            }
        })
        .ok();

    let _ = events.send(Event::ShareStatus { sharing: true, peer: None, message: None });
    Reply::Ok
}

/// Spawn the receive engine (advertise + render) and relay its events.
fn spawn_receive(
    inner: &Arc<Mutex<Inner>>,
    events: &broadcast::Sender<Event>,
    req: crate::share::ReceiveRequest,
) -> Reply {
    use crate::share::{ShareEngine, ShareEvent};
    let mut g = inner.lock();
    if g.receive.is_some() {
        return Reply::Error { message: "already receiving".into() };
    }
    let (tx, rx) = std::sync::mpsc::channel::<ShareEvent>();
    let engine = match ShareEngine::start_receive(&req, tx) {
        Ok(e) => e,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    g.receive = Some(engine);
    drop(g);

    let events2 = events.clone();
    let inner2 = inner.clone();
    std::thread::Builder::new()
        .name("relay-receive-pump".into())
        .spawn(move || {
            crate::share::pump(rx, |ev| match ev {
                ShareEvent::Waiting { code, .. } => {
                    let _ = events2.send(Event::ReceiveStatus {
                        receiving: true,
                        code: Some(code),
                        sender: None,
                        message: None,
                    });
                }
                ShareEvent::Paired { sender } => {
                    let _ = events2.send(Event::ReceiveStatus {
                        receiving: true,
                        code: None,
                        sender: Some(sender),
                        message: None,
                    });
                }
                ShareEvent::Stats { data } => {
                    let _ = events2.send(Event::ShareStats { data });
                }
                ShareEvent::Error { message } => {
                    let _ = events2.send(Event::ReceiveStatus {
                        receiving: true,
                        code: None,
                        sender: None,
                        message: Some(message),
                    });
                }
                ShareEvent::Exited { .. } | ShareEvent::Connected { .. } => {}
            });
            let mut ig = inner2.lock();
            if ig.receive.is_some() {
                ig.receive = None;
                drop(ig);
                let _ = events2.send(Event::ReceiveStatus {
                    receiving: false,
                    code: None,
                    sender: None,
                    message: None,
                });
            }
        })
        .ok();
    Reply::Ok
}

fn kill_receive(inner: &Arc<Mutex<Inner>>, events: &broadcast::Sender<Event>) -> Reply {
    let engine = inner.lock().receive.take();
    match engine {
        Some(engine) => {
            engine.stop();
            let _ = events.send(Event::ReceiveStatus {
                receiving: false,
                code: None,
                sender: None,
                message: None,
            });
            Reply::Ok
        }
        None => Reply::Error { message: "not receiving".into() },
    }
}

fn kill_share(inner: &Arc<Mutex<Inner>>, events: &broadcast::Sender<Event>) -> Reply {
    let engine = inner.lock().share.take();
    match engine {
        Some(engine) => {
            engine.stop();
            inner.lock().state.sharing = crate::types::ShareState::Off;
            let _ = events.send(Event::ShareStatus { sharing: false, peer: None, message: None });
            Reply::Ok
        }
        None => Reply::Error { message: "no share is running".into() },
    }
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
            Method::StartShare { request } => {
                drop(g);
                spawn_share(&self.inner, &self.events, *request)
            }
            Method::StopShare => {
                drop(g);
                kill_share(&self.inner, &self.events)
            }
            Method::StartReceive { request } => {
                drop(g);
                spawn_receive(&self.inner, &self.events, *request)
            }
            Method::StopReceive => {
                drop(g);
                kill_receive(&self.inner, &self.events)
            }
            Method::DiscoverReceivers => {
                drop(g);
                match crate::share::discover_receivers(2000) {
                    Ok(receivers) => Reply::Receivers { receivers },
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
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
