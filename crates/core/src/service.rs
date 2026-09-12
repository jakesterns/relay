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
use crate::hardware::MonitorProbe;
use crate::hardware::{
    ConnectedHardware, HardwareProbe, HardwareStore, HardwareView, NoopHardwareProbe, ProbeReport,
};
use crate::hotkeys::{self, HotkeyAction};
use crate::ipc::{Event, Method, Reply};
use crate::presets::PresetStore;
use crate::profiles::ProfileStore;
use crate::types::{AudioChainState, CoreState, DisplayState, DisplayVia, Foreground};
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
    /// Production backends (real hardware probe), or the file-backed recorder
    /// when `RELAY_RECORDING_BACKEND=<path>` is set (integration tests only —
    /// the probe stays no-op there so tests never depend on host hardware).
    pub fn from_env() -> Self {
        match std::env::var(RECORDING_ENV) {
            Ok(path) if !path.is_empty() => {
                let rec = Arc::new(FileRecorder::at(path));
                Self { audio: rec.clone(), display: rec, hardware: Arc::new(NoopHardwareProbe) }
            }
            _ => {
                #[cfg(windows)]
                {
                    Self {
                        audio: Arc::new(crate::audio_apo::ApoAudioControl),
                        hardware: Arc::new(crate::hardware::probe_win::WindowsHardwareProbe),
                        display: Arc::new(crate::display_backend::WinDisplay::new()),
                        ..Self::default()
                    }
                }
                #[cfg(not(windows))]
                Self::default()
            }
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
    /// The user's hardware library (`hardware.json`).
    library: HardwareStore,
    /// Last probe, kept so library edits can re-resolve without re-probing.
    last_report: ProbeReport,
    /// Cached selection view of `last_report`; focus changes use this instead
    /// of probing (probing on every alt-tab would cost idle CPU).
    connected: ConnectedHardware,
    /// Set by `ApplyProfile` over IPC: stay applied regardless of focus until
    /// `RestoreAll` or the profile is applied automatically anyway.
    pinned: bool,
    /// The running share engine (a child process), if any.
    share: Option<crate::share::ShareEngine>,
    /// Last share request, so the Ctrl+Alt+S hotkey can re-start it.
    last_share: Option<crate::share::ShareRequest>,
    /// The running receive engine (a child process), if any.
    receive: Option<crate::share::ShareEngine>,
    /// Where A/B listening-test renders go.
    previews_dir: std::path::PathBuf,
    /// Where pre-install FX property-store backups live (`<endpoint>.json`).
    apo_backup_dir: std::path::PathBuf,
    /// The full path set (virtual-device consent + registration record).
    paths: Paths,
    /// What the applier reported for the active profile's audio chain; the
    /// exclusive-mode watcher restores this when exclusivity clears.
    applied_audio: AudioChainState,
    /// The active profile configures audio processing, so the watcher runs.
    audio_watch: bool,
    /// Share presets + recording settings (`presets.json`).
    presets: PresetStore,
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

        let presets = PresetStore::load(paths.presets_file())?;
        let library = HardwareStore::load(paths.hardware_file())?;
        let report = backends.hardware.probe(false);
        let connected = library.connected(&report);
        let state = CoreState {
            hardware: HardwareView::from_report(report.clone(), &library),
            ..Default::default()
        };

        let inner = Arc::new(Mutex::new(Inner {
            store,
            applier,
            state,
            meter: FootprintMeter::new(),
            hardware: backends.hardware,
            library,
            last_report: report,
            connected,
            pinned: false,
            share: None,
            last_share: None,
            receive: None,
            previews_dir: paths.previews_dir(),
            apo_backup_dir: paths.apo_backup_dir(),
            paths: paths.clone(),
            applied_audio: AudioChainState::Bypass,
            audio_watch: false,
            presets,
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
        let winloop = WinLoop::spawn(tx.clone(), hotkeys::defaults())?;

        // WASAPI endpoint notifications (default switch, plug/unplug). Display
        // changes arrive via the winloop's hidden window. Keep the handle
        // alive; dropping unregisters.
        #[cfg(windows)]
        let _audio_watch = match crate::hardware::watch_win::AudioWatcher::start(tx.clone()) {
            Ok(w) => Some(w),
            Err(e) => {
                warn!(error = %e, "audio device watcher unavailable");
                None
            }
        };
        let _ = tx;

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

        // Startup touched COM/WASAPI/display DLLs (probe, watcher). Hand those
        // pages back so idle RSS reflects steady state.
        crate::footprint::trim_working_set();

        // 1 s cadence for the exclusive-mode watcher (only probes while a
        // profile with audio processing is active); footprint every 5th tick.
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut ticks = 0u32;
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Some(CoreEvent::ForegroundChanged(fg)) => self.on_foreground(fg),
                        Some(CoreEvent::Hotkey(action)) => self.on_hotkey(action),
                        // Bursts are fine: the handler compares against the
                        // last probe and does nothing when nothing changed.
                        Some(CoreEvent::HardwareChanged) => self.on_hardware_changed(),
                        Some(CoreEvent::SessionLock(locked)) => self.on_session_lock(locked),
                        Some(CoreEvent::Shutdown) | None => break,
                    }
                }
                _ = tick.tick() => {
                    ticks = ticks.wrapping_add(1);
                    if ticks % 5 == 0 {
                        let mut g = self.inner.lock();
                        g.state.footprint = g.meter.sample();
                    }
                    self.refresh_audio_chain();
                    self.recheck_monitor();
                }
            }
        }
        winloop.stop();
        Ok(())
    }

    /// Probe, and if anything actually changed, re-run selection for the
    /// current foreground window — this is the "unplug the headset mid-game
    /// and the other profile takes over" path, no focus change involved.
    fn on_hardware_changed(&self) {
        let mut g = self.inner.lock();
        let report = g.hardware.probe(false);
        if report == g.last_report {
            return;
        }
        info!(
            endpoints = report.endpoints.len(),
            monitors = report.monitors.len(),
            "hardware changed"
        );
        g.connected = g.library.connected(&report);
        g.state.hardware = HardwareView::from_report(report.clone(), &g.library);
        g.last_report = report;
        drop(g);
        reselect(&self.inner, &self.events);
        crate::footprint::trim_working_set();
    }

    /// Lock: restore everything (the user is not looking at the game).
    /// Unlock: re-select for whatever holds the foreground.
    fn on_session_lock(&self, locked: bool) {
        info!(locked, "session lock change");
        if locked {
            let mut g = self.inner.lock();
            if g.applier.is_applied() {
                if let Err(e) = g.applier.restore() {
                    warn!(error = %e, "restore on session lock failed");
                }
            }
            g.state.active_profile = None;
            g.state.audio_chain = AudioChainState::Bypass;
            g.state.display_state = DisplayState::Default;
            g.state.display_via = DisplayVia::default();
            let state = Box::new(g.state.clone());
            drop(g);
            let _ = self.events.send(Event::StateChanged { state });
        } else {
            reselect(&self.inner, &self.events);
        }
    }

    /// Slow-path safety net for monitor moves no hook reports (Win+Shift+
    /// Arrow, app-initiated moves): while a profile is applied, compare the
    /// foreground window's monitor against the one we recorded; on change,
    /// route through the normal foreground path (restore old → apply new).
    fn recheck_monitor(&self) {
        #[cfg(windows)]
        {
            let needs = {
                let g = self.inner.lock();
                g.applier.is_applied() && g.state.foreground.is_some()
            };
            if !needs {
                return;
            }
            let Some(now) = crate::winloop::current_foreground() else { return };
            let stale = {
                let g = self.inner.lock();
                g.state
                    .foreground
                    .as_ref()
                    .map(|fg| fg.pid == now.pid && fg.hmonitor != now.hmonitor)
                    .unwrap_or(false)
            };
            if stale {
                info!(hmonitor = now.hmonitor, "game window moved monitors");
                self.on_foreground(now);
            }
        }
    }

    fn on_foreground(&self, fg: Foreground) {
        let mut g = self.inner.lock();
        g.state.foreground = Some(fg.clone());
        select_and_apply(&mut g, &fg);
        let state = Box::new(g.state.clone());
        drop(g);
        let _ = self.events.send(Event::StateChanged { state });
        // Catch a WASAPI-exclusive stream right at game launch, not only on
        // the next watcher tick (the DoD asks for detection within 1 s).
        self.refresh_audio_chain();
    }

    /// While a profile with audio processing is active, probe the default
    /// render endpoint for an exclusive-mode stream and surface
    /// `ExclusiveBypassed` in the state (and back) as it changes.
    fn refresh_audio_chain(&self) {
        #[cfg(windows)]
        {
            let watching = {
                let g = self.inner.lock();
                g.audio_watch && g.state.active_profile.is_some()
            };
            if !watching {
                return;
            }
            let exclusive = match relay_audio::sessions::probe_default_render() {
                Ok(s) => s.exclusive,
                Err(e) => {
                    tracing::debug!(error = %e, "exclusive-mode probe failed");
                    false
                }
            };
            let mut g = self.inner.lock();
            let desired =
                if exclusive { AudioChainState::ExclusiveBypassed } else { g.applied_audio };
            if g.state.audio_chain != desired {
                if exclusive {
                    info!("foreground game holds the endpoint in WASAPI-exclusive mode; APO chain is bypassed");
                }
                g.state.audio_chain = desired;
                let state = Box::new(g.state.clone());
                drop(g);
                let _ = self.events.send(Event::StateChanged { state });
            }
        }
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
                    g.applied_audio = AudioChainState::Bypass;
                    g.audio_watch = false;
                    g.state.display_via = DisplayVia::default();
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
            HotkeyAction::SaveReplay => {
                let reply = engine_command(&self.inner, &crate::share::EngineCmd::ReplaySave);
                if let Reply::Error { message } = reply {
                    let _ =
                        self.events.send(Event::Notice { text: format!("Ctrl+Alt+R: {message}") });
                }
            }
        }
    }
}

/// Forward one command to the running share engine's stdin.
fn engine_command(inner: &Arc<Mutex<Inner>>, cmd: &crate::share::EngineCmd) -> Reply {
    let mut g = inner.lock();
    match g.share.as_mut() {
        Some(engine) => match engine.command(cmd) {
            Ok(()) => Reply::Ok,
            Err(e) => Reply::Error { message: e.to_string() },
        },
        None => Reply::Error { message: "no share is running".into() },
    }
}

struct IpcHandler {
    inner: Arc<Mutex<Inner>>,
    shutdown: mpsc::UnboundedSender<CoreEvent>,
    events: broadcast::Sender<Event>,
}

/// The monitor hosting `hmonitor`, falling back to the primary (monitors are
/// sorted primary-first). Advertised DDC codes are merged in from the library
/// because the fast probe skips the slow capability query.
fn resolve_target(g: &Inner, hmonitor: i64) -> Option<MonitorProbe> {
    let monitors = &g.last_report.monitors;
    let mut t = monitors
        .iter()
        .find(|m| hmonitor != 0 && m.hmonitor == hmonitor)
        .or_else(|| monitors.first())
        .cloned()?;
    if t.ddc.is_none() {
        if let Some(known) = g.library.monitors.iter().find(|m| m.id == t.id) {
            t.ddc = known.ddcci.clone();
        }
    }
    Some(t)
}

/// Pick and apply (or restore) for one foreground window, using the cached
/// connected-hardware view. Mutates state only; the caller broadcasts.
fn select_and_apply(g: &mut Inner, fg: &Foreground) {
    let hw = g.connected.clone();
    let pick = g.store.select(&fg.exe, &fg.title, &hw).cloned();
    match pick {
        Some(profile) => {
            g.pinned = false;
            let target = resolve_target(g, fg.hmonitor);
            match g.applier.apply(&profile, target.as_ref()) {
                Ok(applied) => {
                    g.state.active_profile = Some(profile.summary());
                    g.state.audio_chain = applied.audio;
                    g.state.display_state = applied.display;
                    g.applied_audio = applied.audio;
                    g.audio_watch = crate::audio_bridge::wants_processing(&profile.audio);
                    g.state.display_via = applied.via;
                }
                Err(e) => {
                    warn!(error = %e, profile = %profile.name, "apply failed");
                    g.state.active_profile = None;
                    g.state.audio_chain = AudioChainState::Bypass;
                    g.state.display_state = DisplayState::Default;
                    g.applied_audio = AudioChainState::Bypass;
                    g.audio_watch = false;
                    g.state.display_via = DisplayVia::default();
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
            g.applied_audio = AudioChainState::Bypass;
            g.audio_watch = false;
            g.state.display_via = DisplayVia::default();
        }
    }
}

/// Re-run selection for whatever is in the foreground (no focus change
/// needed) and broadcast the new state. Used after hardware or library edits.
fn reselect(inner: &Arc<Mutex<Inner>>, events: &broadcast::Sender<Event>) {
    let mut g = inner.lock();
    if let Some(fg) = g.state.foreground.clone() {
        select_and_apply(&mut g, &fg);
    }
    let state = Box::new(g.state.clone());
    drop(g);
    let _ = events.send(Event::StateChanged { state });
}

/// Recompute the connected view from the last probe after a library edit
/// (bindings may resolve differently), then reselect.
fn library_changed(inner: &Arc<Mutex<Inner>>, events: &broadcast::Sender<Event>) {
    let mut g = inner.lock();
    g.connected = g.library.connected(&g.last_report);
    g.state.hardware = HardwareView::from_report(g.last_report.clone(), &g.library);
    drop(g);
    reselect(inner, events);
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
                ShareEvent::Recording { on, path } => {
                    let _ = events2.send(Event::RecordingStatus { on, path });
                }
                ShareEvent::ReplaySaved { path, ms } => {
                    let _ = events2.send(Event::ReplaySaved { path, ms });
                }
                ShareEvent::SourceChanged { data } => {
                    let _ = events2.send(Event::SourceChanged { data });
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
    // Virtual-device routing is decided here from consent + registration,
    // never by the client: no opt-in, no camera, no mic route.
    let mut req = req;
    (req.vcam, req.mic_route) = crate::vdevice::receive_routing(&g.paths);
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
                ShareEvent::Exited { .. }
                | ShareEvent::Connected { .. }
                | ShareEvent::Recording { .. }
                | ShareEvent::ReplaySaved { .. }
                | ShareEvent::SourceChanged { .. } => {}
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
                // Pinned applies target the monitor the user is looking at
                // (the current foreground window's), or the primary.
                let hmon = g.state.foreground.as_ref().map(|f| f.hmonitor).unwrap_or(0);
                let target = resolve_target(&g, hmon);
                match g.applier.apply(&profile, target.as_ref()) {
                    Ok(applied) => {
                        g.pinned = true;
                        g.state.active_profile = Some(profile.summary());
                        g.state.audio_chain = applied.audio;
                        g.state.display_state = applied.display;
                        g.applied_audio = applied.audio;
                        g.audio_watch = crate::audio_bridge::wants_processing(&profile.audio);
                        g.state.display_via = applied.via;
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
                        g.applied_audio = AudioChainState::Bypass;
                        g.audio_watch = false;
                        g.state.display_via = DisplayVia::default();
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
            Method::StartSharePreset { preset, code, peer } => {
                let Some(def) = g.presets.get(&preset).cloned() else {
                    return Reply::Error { message: format!("no preset `{preset}`") };
                };
                let game_pid = g.state.foreground.as_ref().map(|f| f.pid);
                let req = crate::presets::to_share_request(
                    &def,
                    code,
                    peer,
                    game_pid,
                    &g.presets.recording,
                );
                drop(g);
                spawn_share(&self.inner, &self.events, req)
            }
            Method::Record { on } => {
                drop(g);
                engine_command(&self.inner, &crate::share::EngineCmd::Record { on })
            }
            Method::SaveReplay => {
                drop(g);
                engine_command(&self.inner, &crate::share::EngineCmd::ReplaySave)
            }
            Method::SwitchSource { target } => {
                drop(g);
                engine_command(&self.inner, &crate::share::EngineCmd::Switch { target })
            }
            Method::ListPresets => Reply::Presets {
                presets: g.presets.all().to_vec(),
                recording: g.presets.recording.clone(),
            },
            Method::SavePreset { preset } => {
                if preset.id.trim().is_empty() || preset.name.trim().is_empty() {
                    return Reply::Error { message: "preset needs an id and a name".into() };
                }
                g.presets.upsert(*preset);
                match g.presets.save() {
                    Ok(()) => Reply::Ok,
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::DeletePreset { id } => {
                if !g.presets.remove(&id) {
                    return Reply::Error { message: "no such preset".into() };
                }
                match g.presets.save() {
                    Ok(()) => Reply::Ok,
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::SetRecordingSettings { settings } => {
                g.presets.recording = settings;
                match g.presets.save() {
                    Ok(()) => Reply::Ok,
                    Err(e) => Reply::Error { message: e.to_string() },
                }
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
            Method::ListHardware => Reply::Hardware {
                headsets: g.library.headsets.clone(),
                monitors: g.library.monitors.clone(),
                interfaces: g.library.interfaces.clone(),
                connected: Box::new(g.state.hardware.clone()),
            },
            Method::SaveHardware { item } => {
                match item {
                    crate::ipc::HardwareItem::Headset(h) => {
                        if h.id.0.trim().is_empty() || h.name.trim().is_empty() {
                            return Reply::Error {
                                message: "headset needs an id and a name".into(),
                            };
                        }
                        g.library.upsert_headset(*h);
                    }
                    crate::ipc::HardwareItem::Monitor(m) => {
                        if m.id.0.trim().is_empty() || m.name.trim().is_empty() {
                            return Reply::Error {
                                message: "monitor needs an id and a name".into(),
                            };
                        }
                        g.library.upsert_monitor(*m);
                    }
                }
                match g.library.save() {
                    Ok(()) => {
                        drop(g);
                        library_changed(&self.inner, &self.events);
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::DeleteHardware { id } => {
                if !g.library.remove(&id) {
                    return Reply::Error { message: "no such hardware".into() };
                }
                match g.library.save() {
                    Ok(()) => {
                        drop(g);
                        library_changed(&self.inner, &self.events);
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::ProbeHardware => {
                let report = g.hardware.probe(true);
                // Remember the advertised VCP codes on known library monitors.
                let mut dirty = false;
                for probed in &report.monitors {
                    if let Some(ddc) = &probed.ddc {
                        if let Some(known) =
                            g.library.monitors.iter_mut().find(|m| m.id == probed.id)
                        {
                            if known.ddcci.as_ref() != Some(ddc) {
                                known.ddcci = Some(ddc.clone());
                                dirty = true;
                            }
                        }
                    }
                }
                if dirty {
                    if let Err(e) = g.library.save() {
                        warn!(error = %e, "saving probed DDC capabilities failed");
                    }
                }
                g.connected = g.library.connected(&report);
                g.state.hardware = HardwareView::from_report(report.clone(), &g.library);
                g.last_report = report.clone();
                drop(g);
                reselect(&self.inner, &self.events);
                Reply::Probe { report: Box::new(report) }
            }
            Method::ImportCurve { headset, csv } => {
                let points = match crate::hardware::autoeq::parse_curve(&csv) {
                    Ok(p) => p,
                    Err(e) => return Reply::Error { message: format!("curve not imported: {e}") },
                };
                let Some(h) = g.library.headset_mut(&headset) else {
                    return Reply::Error { message: "no such headset".into() };
                };
                h.curve = Some(points.clone());
                match g.library.save() {
                    Ok(()) => Reply::Curve { points },
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::RenderPreview { id, wav } => {
                let Some(profile) = g.store.get(id).cloned() else {
                    return Reply::Error { message: "no such profile".into() };
                };
                let dir = g.previews_dir.clone();
                drop(g);
                let wav = wav.map(std::path::PathBuf::from);
                match crate::audio_bridge::render_preview(&profile.audio, wav.as_deref(), &dir) {
                    Ok(p) => Reply::Preview {
                        original: p.original.display().to_string(),
                        processed: p.processed.display().to_string(),
                        sample_rate: p.sample_rate,
                        hrtf_applied: p.hrtf_applied,
                    },
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            Method::ApoStatus => {
                drop(g);
                Reply::Apo { status: crate::audio_apo::apo_status() }
            }
            #[cfg(windows)]
            Method::InstallApo => {
                let dir = g.apo_backup_dir.clone();
                drop(g);
                match crate::audio_apo::install_live(&dir) {
                    Ok(endpoint) => {
                        let _ = self.events.send(Event::Notice {
                            text: format!("Relay APO registered on {endpoint}"),
                        });
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            #[cfg(windows)]
            Method::UninstallApo => {
                let dir = g.apo_backup_dir.clone();
                drop(g);
                match crate::audio_apo::uninstall_live(&dir) {
                    Ok(endpoint) => {
                        let _ = self.events.send(Event::Notice {
                            text: format!("Endpoint {endpoint} restored to its original state"),
                        });
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            #[cfg(not(windows))]
            Method::InstallApo | Method::UninstallApo => {
                Reply::Error { message: "Windows only".into() }
            }
            Method::VdeviceStatus => {
                let paths = g.paths.clone();
                drop(g);
                match crate::vdevice::status(&paths) {
                    Ok(status) => Reply::Vdevice { status: Box::new(status) },
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            Method::SetVdeviceConsent { apo, camera, microphone } => {
                let paths = g.paths.clone();
                drop(g);
                match crate::vdevice::set_consent(&paths, apo, camera, microphone) {
                    Ok(_) => Reply::Ok,
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            Method::VdeviceDryRun => {
                drop(g);
                Reply::DryRun { lines: crate::vdevice::camera_dry_run() }
            }
            #[cfg(windows)]
            Method::InstallVcam => {
                let paths = g.paths.clone();
                drop(g);
                match crate::vdevice::install_camera_live(&paths) {
                    Ok(()) => {
                        let _ = self
                            .events
                            .send(Event::Notice { text: "Relay Camera registered".into() });
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            #[cfg(windows)]
            Method::UninstallVcam => {
                let paths = g.paths.clone();
                drop(g);
                match crate::vdevice::uninstall_camera_live(&paths) {
                    Ok(()) => {
                        let _ =
                            self.events.send(Event::Notice { text: "Relay Camera removed".into() });
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            #[cfg(not(windows))]
            Method::InstallVcam | Method::UninstallVcam => {
                Reply::Error { message: "Windows only".into() }
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
