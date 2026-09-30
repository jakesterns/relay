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
use crate::types::{AudioChainState, CoreState, DisplayState, DisplayVia, Foreground, Profile};
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
        // The simulated display rig wins when both are set: it is the more
        // specific of the two test backends, and the crash-restore harness
        // wants the recorder for audio and the rig for display.
        if let Ok(path) = std::env::var(crate::display_sim::SIM_ENV) {
            if !path.is_empty() {
                let audio: Arc<dyn AudioControl> = match std::env::var(RECORDING_ENV) {
                    Ok(rec) if !rec.is_empty() => Arc::new(FileRecorder::at(rec)),
                    _ => Arc::new(Noop),
                };
                return Self {
                    audio,
                    display: Arc::new(crate::display_backend::DisplayAdapter::with_io(
                        crate::display_sim::SimIo::from_env(path),
                    )),
                    hardware: Arc::new(crate::display_sim::SimHardwareProbe),
                };
            }
        }
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
    /// Thumbnails per second the running engine was last told to emit, so
    /// Ctrl+Alt+P knows which way to flip. Reset when a share starts.
    preview_fps: u32,
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
    /// App preferences: what closing the window means, and the two S38
    /// switches (bring a share back; say so in the tray on close).
    prefs: crate::uiprefs::PrefsStore,
    /// S38: what the user started and has not stopped, mirrored to
    /// `active-stream.json` so a fresh core can resume it. Present while a
    /// share or receive is up *or being brought back*; cleared by a
    /// deliberate stop or by giving up. One file holds one record — a send
    /// wins over a receive if both exist, which nothing in the UI can
    /// produce today.
    send_intent: Option<crate::resilience::Record>,
    recv_intent: Option<crate::resilience::Record>,
    /// The reconnect episode in progress, if the engine is down and the
    /// intent says it should not be.
    send_episode: Option<crate::resilience::Episode>,
    recv_episode: Option<crate::resilience::Episode>,
    /// The app window the receiver's stream is hosted in. Kept in memory
    /// only (the record on disk drops it: a window handle is meaningless to
    /// a fresh core), so a receiver brought back after a share ends is
    /// embedded again rather than coming up with no window at all.
    recv_host: Option<u64>,
    /// When the current receive engine started. A restarted receiver that
    /// has stayed up for a while ends the episode even if no share arrived:
    /// otherwise the episode's clock ran on and the next unrelated failure,
    /// minutes later, was taken as "three minutes of trying" and gave up.
    recv_started: Option<std::time::Instant>,
}

/// Write the intent to disk, or remove the file when there is none.
fn persist_intent(g: &Inner) {
    let path = crate::resilience::path_in(&g.paths);
    match g.send_intent.as_ref().or(g.recv_intent.as_ref()) {
        Some(rec) => {
            if let Err(e) = rec.save(&path) {
                warn!(error = %e, "could not write active-stream.json");
            }
        }
        None => crate::resilience::Record::clear(&path),
    }
}

/// Turn a recorded send back into a request the engine can act on now.
///
/// The peer is looked up by *name* in the remembered-PCs store: a share that
/// began with a code remembered its receiver, so it resumes with none. A peer
/// that is no longer remembered can still be reached with the code the user
/// typed — the receiver comes back with the same one — and only a peer that
/// is neither remembered nor coded is a dead end, which is reported as such.
fn resume_send_request(
    rec: &crate::resilience::Record,
    paths: &Paths,
) -> Result<crate::share::ShareRequest, String> {
    let mut req = rec.share.clone().ok_or_else(|| "no share request was recorded".to_string())?;
    req.trusted = None;
    req.peer_id = None;
    let peer = rec.peer.clone().unwrap_or_default();
    let store = crate::peers::Store::load(&crate::peers::path_in(paths));
    match store.peers.iter().find(|p| p.name.eq_ignore_ascii_case(&peer)) {
        Some(p) => {
            req.peer_id = Some(p.id.clone());
            req.code.clear();
        }
        None if req.code.trim().is_empty() => {
            return Err(format!("{peer} is not remembered and there is no code to use"));
        }
        None => req.peer = Some(peer),
    }
    Ok(req)
}

pub struct Service {
    inner: Arc<Mutex<Inner>>,
    events: broadcast::Sender<Event>,
    /// The newest receive status that went past. Events are not replayed, so
    /// a window opened while a receive is already running -- after a resume
    /// from `active-stream.json`, or just reopening the app -- would show Idle
    /// and offer Start receiving over a live receiver. Re-sent on Subscribe.
    last_recv: Arc<std::sync::Mutex<Option<Event>>>,
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
        let prefs = crate::uiprefs::PrefsStore::load(paths.settings_file());
        let library = HardwareStore::load(paths.hardware_file())?;
        let report = backends.hardware.probe(false);
        let connected = library.connected(&report);
        let state = CoreState {
            hardware: HardwareView::from_report(report.clone(), &library),
            build: crate::types::BuildInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
                data_dir: paths.root().display().to_string(),
                log_file: paths.log_file().display().to_string(),
            },
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
            preview_fps: 0,
            receive: None,
            previews_dir: paths.previews_dir(),
            apo_backup_dir: paths.apo_backup_dir(),
            paths: paths.clone(),
            applied_audio: AudioChainState::Bypass,
            audio_watch: false,
            presets,
            prefs,
            send_intent: None,
            recv_intent: None,
            send_episode: None,
            recv_episode: None,
            recv_host: None,
            recv_started: None,
        }));
        let (events, _) = broadcast::channel(64);
        let (tx, rx) = mpsc::unbounded_channel();
        let last_recv = Arc::new(std::sync::Mutex::new(None));
        {
            let mut rx = events.subscribe();
            let last = last_recv.clone();
            std::thread::Builder::new()
                .name("relay-last-receive".into())
                .stack_size(64 * 1024)
                .spawn(move || loop {
                    match rx.blocking_recv() {
                        Ok(ev @ Event::ReceiveStatus { .. }) => {
                            *last.lock().unwrap() = Some(ev);
                        }
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                })?;
        }
        let service = Service { inner, events, last_recv, shutdown: tx.clone() };

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = rt.block_on(service.main(tx, rx));

        // Reached only by a deliberate shutdown: Quit in the tray, `relay-core
        // shutdown`, the window's quit preference. A crash or a power cut
        // never gets here — which is exactly what leaves `active-stream.json`
        // behind for the next start to act on (S38).
        {
            let g = service.inner.lock();
            crate::resilience::Record::clear(&crate::resilience::path_in(&g.paths));
        }

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
            last_recv: self.last_recv.clone(),
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

        // S38: say if the last run ended badly, and pick up a stream the user
        // never stopped. Both after the IPC server is up, so the first window
        // to connect sees the result; the resume itself happens on the tick.
        self.report_crash_once();
        self.resume_if_recorded();

        // 1 s cadence for the exclusive-mode watcher (only probes while a
        // profile with audio processing is active); footprint every 5th tick.
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut ticks = 0u32;
        // Watches for the profiled app exiting, which fires no foreground
        // event when nothing else takes focus. 100 ms keeps restore-on-exit
        // inside the 200 ms budget; the branch is disabled while no profile
        // is applied, so an idle core never wakes for it.
        let mut exit_watch = tokio::time::interval(Duration::from_millis(100));
        exit_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let profiled = self.inner.lock().state.active_profile.is_some();
            tokio::select! {
                _ = exit_watch.tick(), if profiled => self.recheck_focus(),
                ev = rx.recv() => {
                    match ev {
                        Some(CoreEvent::ForegroundChanged(fg)) => self.on_foreground(fg),
                        Some(CoreEvent::Hotkey(action)) => self.on_hotkey(action),
                        // Bursts are fine: the handler compares against the
                        // last probe and does nothing when nothing changed.
                        Some(CoreEvent::HardwareChanged) => self.on_hardware_changed(),
                        Some(CoreEvent::SessionLock(locked)) => self.on_session_lock(locked),
                        Some(CoreEvent::Tray(cmd)) => {
                            if self.on_tray(cmd) {
                                break;
                            }
                        }
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
                    self.supervise();
                }
            }
        }
        winloop.stop();
        Ok(())
    }

    /// Handle one notification-area menu pick. Returns `true` when the core
    /// should stop; the caller breaks out of the loop and `run`'s teardown
    /// does the restore, so quitting from the tray can never leave a game
    /// profile applied.
    fn on_tray(&self, cmd: crate::tray::TrayCommand) -> bool {
        use crate::tray::TrayCommand;
        match cmd {
            TrayCommand::Open => {
                #[cfg(windows)]
                if let Err(e) = crate::launcher::open_ui() {
                    warn!(error = %format!("{e:#}"), "could not open the Relay window");
                }
                false
            }
            TrayCommand::Restore => {
                let mut g = self.inner.lock();
                let text = match restore_all(&mut g) {
                    Ok(()) => "Everything restored. Your audio and display are back the way Windows had them.".to_string(),
                    Err(e) => format!("Could not restore everything: {e}"),
                };
                let state = Box::new(g.state.clone());
                drop(g);
                let _ = self.events.send(Event::StateChanged { state });
                let _ = self.events.send(Event::Notice { text });
                false
            }
            TrayCommand::Quit => {
                // Tell the window to close itself before we go, so the user
                // is not left looking at a live UI reporting a dead core.
                let _ = self.events.send(Event::Quitting);
                true
            }
        }
    }

    /// S38: one sentence about the last crash, once. It goes into `CoreState`
    /// rather than out as a `Notice`, because a notice sent before any window
    /// is open reaches nobody; the state is read by every client that
    /// connects, and the window clears it with `AckCrash`.
    fn report_crash_once(&self) {
        let mut g = self.inner.lock();
        let dir = crate::crash::dir(&g.paths);
        if let Some(file) = crate::crash::unseen(&dir) {
            let text = crate::crash::summary(&file);
            warn!(%text, "reporting a crash record from the last run");
            g.state.last_crash = Some(text);
        }
    }

    /// S38: a share or receive the user never stopped, left behind by a
    /// crash, a power cut or a reboot. Becomes a reconnect episode — attempt
    /// one is due after the usual first delay — so a receiver that is itself
    /// still coming up gets the same patience as one that dropped mid-share.
    fn resume_if_recorded(&self) {
        let (path, resilience) = {
            let g = self.inner.lock();
            (crate::resilience::path_in(&g.paths), g.prefs.get().resilience)
        };
        let Some(record) = crate::resilience::Record::load(&path) else { return };
        if !resilience {
            info!("active-stream.json is present but resilience is off; not resuming");
            crate::resilience::Record::clear(&path);
            return;
        }
        let now = std::time::Instant::now();
        let text = match record.kind {
            crate::resilience::Kind::Send => {
                let peer = record.peer.clone().unwrap_or_default();
                let mut g = self.inner.lock();
                g.send_intent = Some(record);
                g.send_episode = Some(crate::resilience::Episode::begin(now));
                g.state.sharing =
                    crate::types::ShareState::Reconnecting { peer: peer.clone(), attempt: 0 };
                format!("Relay is restoring your share to {peer}.")
            }
            crate::resilience::Kind::Receive => {
                let mut g = self.inner.lock();
                g.recv_intent = Some(record);
                g.recv_episode = Some(crate::resilience::Episode::begin(now));
                "Relay is going back to receiving, as it was before.".to_string()
            }
        };
        info!(%text, "resuming from active-stream.json");
        let _ = self.events.send(Event::Notice { text: text.clone() });
        crate::winloop::balloon("Relay", &text);
    }

    /// S38: the reconnect schedule, on the 1 s tick. Decides under the lock
    /// and acts outside it, because spawning takes the lock itself.
    fn supervise(&self) {
        enum Next {
            Nothing,
            Attempt(u32, crate::resilience::Record),
            GiveUp(crate::resilience::Record),
        }
        let now = std::time::Instant::now();

        // ---- send ----
        let next = {
            let mut g = self.inner.lock();
            if g.share.is_some() {
                Next::Nothing
            } else {
                match (g.send_intent.clone(), g.send_episode.as_mut()) {
                    (Some(rec), Some(ep)) if ep.gave_up(now) => Next::GiveUp(rec),
                    (Some(rec), Some(ep)) => match ep.due(now) {
                        Some(n) => Next::Attempt(n, rec),
                        None => Next::Nothing,
                    },
                    _ => Next::Nothing,
                }
            }
        };
        match next {
            Next::Nothing => {}
            Next::GiveUp(rec) => self.give_up_send(&rec.peer.unwrap_or_default(), None),
            Next::Attempt(n, rec) => {
                let peer = rec.peer.clone().unwrap_or_default();
                let req = {
                    let g = self.inner.lock();
                    resume_send_request(&rec, &g.paths)
                };
                match req {
                    Err(why) => {
                        warn!(%why, "the share cannot be resumed");
                        self.give_up_send(&peer, Some(why));
                    }
                    Ok(req) => {
                        {
                            let mut g = self.inner.lock();
                            if let Some(r) = g.send_intent.as_mut() {
                                r.attempts = n;
                            }
                            g.state.sharing = crate::types::ShareState::Reconnecting {
                                peer: peer.clone(),
                                attempt: n,
                            };
                        }
                        info!(attempt = n, %peer, "reconnecting the share");
                        if let Reply::Error { message } =
                            spawn_share(&self.inner, &self.events, req, false)
                        {
                            warn!(%message, attempt = n, "reconnect attempt did not start");
                        }
                        self.push_state();
                    }
                }
            }
        }

        // ---- receive ----
        let next = {
            let mut g = self.inner.lock();
            if g.receive.is_some() {
                let settled = g
                    .recv_started
                    .is_some_and(|t| now.duration_since(t) >= std::time::Duration::from_secs(30));
                if settled && g.recv_episode.is_some() {
                    g.recv_episode = None;
                    if let Some(r) = g.recv_intent.as_mut() {
                        r.attempts = 0;
                    }
                }
                Next::Nothing
            } else {
                match (g.recv_intent.clone(), g.recv_episode.as_mut()) {
                    (Some(rec), Some(ep)) if ep.gave_up(now) => Next::GiveUp(rec),
                    (Some(rec), Some(ep)) => match ep.due(now) {
                        Some(n) => Next::Attempt(n, rec),
                        None => Next::Nothing,
                    },
                    _ => Next::Nothing,
                }
            }
        };
        match next {
            Next::Nothing => {}
            Next::GiveUp(_) => self.give_up_receive(),
            Next::Attempt(n, rec) => {
                let Some(mut req) = rec.receive.clone() else {
                    self.give_up_receive();
                    return;
                };
                if req.host.is_none() {
                    req.host = self.inner.lock().recv_host;
                }
                if let Some(r) = self.inner.lock().recv_intent.as_mut() {
                    r.attempts = n;
                }
                info!(attempt = n, "going back to receiving");
                if let Reply::Error { message } =
                    spawn_receive(&self.inner, &self.events, req, false)
                {
                    warn!(%message, attempt = n, "receive did not restart");
                }
            }
        }
    }

    /// Stop trying to bring a share back. Everything that referred to it
    /// clears, on screen and on disk, and the user is told in one sentence.
    fn give_up_send(&self, peer: &str, why: Option<String>) {
        {
            let mut g = self.inner.lock();
            g.send_intent = None;
            g.send_episode = None;
            g.state.sharing = crate::types::ShareState::Off;
            persist_intent(&g);
        }
        let detail = why.map(|w| format!(" ({w})")).unwrap_or_default();
        let text = format!("The share to {peer} dropped and did not come back{detail}.");
        warn!(%text, "gave up reconnecting");
        let _ = self.events.send(Event::ShareStatus {
            sharing: false,
            peer: None,
            message: Some(text.clone()),
            trusted: false,
        });
        let _ = self.events.send(Event::Notice { text: text.clone() });
        crate::winloop::balloon("Relay", &text);
        self.push_state();
    }

    fn give_up_receive(&self) {
        {
            let mut g = self.inner.lock();
            g.recv_intent = None;
            g.recv_episode = None;
            persist_intent(&g);
        }
        let text = "Relay stopped receiving: the share did not come back.".to_string();
        warn!(%text, "gave up restarting the receiver");
        let _ = self.events.send(Event::ReceiveStatus {
            receiving: false,
            code: None,
            sender: None,
            message: Some(text.clone()),
            codec: None,
            trusted: false,
            ended_by_sender: false,
            restarting: false,
            return_pid: None,
            return_exe: None,
        });
        let _ = self.events.send(Event::Notice { text: text.clone() });
        crate::winloop::balloon("Relay", &text);
    }

    /// Broadcast the current state, for the parts of the UI that read
    /// `state.sharing` rather than the share events.
    fn push_state(&self) {
        let state = Box::new(self.inner.lock().state.clone());
        let _ = self.events.send(Event::StateChanged { state });
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

    /// While a profile is applied, make sure the app it was applied for is
    /// still the one in front. Closing the game often hands the foreground to
    /// no window at all, and then no foreground event ever fires: the live
    /// test closed Notepad and its profile stayed on the monitor until the
    /// guard undid it (2026-09-29). The brief says restore on exit too.
    fn recheck_focus(&self) {
        #[cfg(windows)]
        {
            let stale = {
                let g = self.inner.lock();
                if g.state.active_profile.is_none() {
                    return;
                }
                // Only the process exiting counts. Comparing against
                // GetForegroundWindow every tick disagreed with the hook (it
                // named another app with no foreground event) and undid a
                // profile that was still wanted.
                let applied_pid = g.state.foreground.as_ref().map(|f| f.pid).filter(|p| *p != 0);
                applied_pid
                    .filter(|pid| crate::winloop::process_image_path(*pid).is_none())
                    .map(|_| crate::winloop::current_foreground())
            };
            if let Some(now) = stale {
                let fg = now.unwrap_or(Foreground {
                    pid: 0,
                    exe: String::new(),
                    title: String::new(),
                    hmonitor: 0,
                });
                info!(exe = %fg.exe, pid = fg.pid, "the profiled app exited; re-evaluating");
                self.on_foreground(fg);
            }
        }
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
                // A share being brought back counts as on: the hotkey's job
                // is to stop it, not to start a second one beside it.
                let sharing = {
                    let g = self.inner.lock();
                    g.share.is_some() || g.send_intent.is_some()
                };
                if sharing {
                    kill_share(&self.inner, &self.events);
                } else {
                    let last = self.inner.lock().last_share.clone();
                    match last {
                        Some(req) => {
                            spawn_share(&self.inner, &self.events, req, true);
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
                // Thumbnails cost a GPU scale and a readback per frame, so the
                // toggle reaches all the way into the engine rather than just
                // hiding the picture in the UI.
                let sharing = self.inner.lock().share.is_some();
                if !sharing {
                    let _ = self.events.send(Event::Notice {
                        text: "Ctrl+Alt+P: nothing to preview -- start a share first".into(),
                    });
                    return;
                }
                let fps = {
                    let mut g = self.inner.lock();
                    g.preview_fps =
                        if g.preview_fps > 0 { 0 } else { crate::share::DEFAULT_PREVIEW_FPS };
                    g.preview_fps
                };
                match engine_command(&self.inner, &crate::share::EngineCmd::Preview { fps }) {
                    Reply::Error { message } => {
                        let _ = self
                            .events
                            .send(Event::Notice { text: format!("Ctrl+Alt+P: {message}") });
                    }
                    _ => {
                        let _ = self.events.send(Event::Notice {
                            text: if fps > 0 { "Preview on".into() } else { "Preview off".into() },
                        });
                    }
                }
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

/// Put audio and display back the way Windows had them, and unpin.
///
/// Shared by `Method::RestoreAll` and the tray's "Restore everything" so the
/// two can never drift — the tray promise is that it is the same restore the
/// app performs, not a second, thinner one.
fn restore_all(g: &mut Inner) -> anyhow::Result<()> {
    g.pinned = false;
    g.applier.restore()?;
    g.state.active_profile = None;
    g.state.audio_chain = AudioChainState::Bypass;
    g.state.display_state = DisplayState::Default;
    g.applied_audio = AudioChainState::Bypass;
    g.audio_watch = false;
    g.state.display_via = DisplayVia::default();
    Ok(())
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

/// Like `engine_command`, for the receive engine.
fn receive_command(inner: &Arc<Mutex<Inner>>, cmd: &crate::share::EngineCmd) -> Reply {
    let mut g = inner.lock();
    match g.receive.as_mut() {
        Some(engine) => match engine.command(cmd) {
            Ok(()) => Reply::Ok,
            Err(e) => Reply::Error { message: e.to_string() },
        },
        None => Reply::Error { message: "not receiving".into() },
    }
}

struct IpcHandler {
    inner: Arc<Mutex<Inner>>,
    shutdown: mpsc::UnboundedSender<CoreEvent>,
    events: broadcast::Sender<Event>,
    last_recv: Arc<std::sync::Mutex<Option<Event>>>,
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
/// AutoEQ files live under `<source>/<rig> in-ear/...`, `over-ear`, `earbud`,
/// so the catalogue path says what kind of thing this is without asking.
fn kind_from_path(path: &str) -> crate::hardware::HeadsetKind {
    use crate::hardware::HeadsetKind;
    let p = path.to_ascii_lowercase();
    if p.contains("in-ear") || p.contains("in%20ear") {
        HeadsetKind::Iem
    } else {
        // Earbuds are closer to headphones than to IEMs for tuning purposes,
        // and the library has no separate kind for them.
        HeadsetKind::Headphone
    }
}

/// The correction curve to apply with `profile`: the one imported for the
/// headset the profile names, or failing that the headset currently plugged
/// in. A profile bound to no headset still gets the connected one's curve,
/// which is what someone swapping between two headsets expects.
fn correction_for(g: &Inner, profile: &Profile) -> Option<Vec<(f32, f32)>> {
    if !profile.audio.headset_correction {
        return None;
    }
    let id = profile.headset.as_ref().or(g.connected.headset.as_ref())?;
    g.library.headsets.iter().find(|h| &h.id == id)?.curve.clone()
}

/// connected-hardware view. Mutates state only; the caller broadcasts.
fn select_and_apply(g: &mut Inner, fg: &Foreground) {
    let hw = g.connected.clone();
    let pick = g.store.select(&fg.exe, &fg.title, &hw).cloned();
    match pick {
        Some(profile) => {
            g.pinned = false;
            let target = resolve_target(g, fg.hmonitor);
            let correction = correction_for(g, &profile);
            match g.applier.apply(&profile, target.as_ref(), correction.as_deref()) {
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

/// Turn a `peer_id` into what the engine needs, or say why a request cannot
/// start. This is the service's job on purpose (S35): the client names a
/// remembered PC by id and never handles a fingerprint, and
/// `ShareRequest::trusted` is serde-skipped so nothing over IPC can set it.
/// The only way to connect without a code is through a peer the store holds.
fn resolve_share_peer(req: &mut crate::share::ShareRequest) -> Result<(), String> {
    match req.peer_id.as_deref() {
        Some(id) => {
            let path = crate::peers::path().map_err(|e| e.to_string())?;
            match crate::peers::Store::load(&path).get(id) {
                Some(p) => {
                    req.peer = Some(p.name.clone());
                    req.trusted = Some(p.fingerprint.clone());
                    Ok(())
                }
                None => {
                    Err("That PC is no longer remembered. Pair with its code once and it will be."
                        .into())
                }
            }
        }
        None if req.code.trim().is_empty() => {
            Err("Enter the six-digit code shown on the receiving PC.".into())
        }
        None => Ok(()),
    }
}

/// Load, change, save the remembered-peers store. `false` from `f` means the
/// id was not there, which is reported rather than treated as done.
fn edit_peers(f: impl FnOnce(&mut crate::peers::Store) -> bool) -> Reply {
    let path = match crate::peers::path() {
        Ok(p) => p,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    let mut store = crate::peers::Store::load(&path);
    if !f(&mut store) {
        return Reply::Error { message: "no remembered PC with that id".into() };
    }
    match store.save(&path) {
        Ok(()) => Reply::Ok,
        Err(e) => Reply::Error { message: e.to_string() },
    }
}

/// Spawn the share engine and a thread that relays its NDJSON as IPC events.
/// `by_user`: the request came from a hand on a button or a hotkey, so it
/// becomes the recorded intent (S38). A reconnect attempt passes `false` and
/// leaves the intent and the `Reconnecting` state as they are.
fn spawn_share(
    inner: &Arc<Mutex<Inner>>,
    events: &broadcast::Sender<Event>,
    mut req: crate::share::ShareRequest,
    by_user: bool,
) -> Reply {
    use crate::share::{ShareEngine, ShareEvent};
    if let Err(message) = resolve_share_peer(&mut req) {
        return Reply::Error { message };
    }
    let mut g = inner.lock();
    if g.share.is_some() {
        return Reply::Error { message: "a share is already running".into() };
    }
    // S36: "Relay Camera" on this PC only if the preset asked, the user
    // consented, it is registered, this Windows has the API -- and nothing
    // else is feeding it. The ring has one writer, and a receive that is
    // showing an incoming share on the camera keeps it. Refused in words,
    // never silently.
    if req.vcam {
        let (camera_ok, _) = crate::vdevice::receive_routing(&g.paths);
        let receive_has_it = g.receive.is_some() && camera_ok;
        if !camera_ok || receive_has_it {
            req.vcam = false;
            let why = if receive_has_it {
                "Relay Camera is showing the share this PC is receiving; stop receiving to use it for this share."
            } else {
                "Relay Camera is not set up on this PC: it needs Windows 11 and the camera installed in Settings."
            };
            let _ = events.send(Event::Notice { text: why.to_string() });
        }
    }
    // S40: the mixer's saved device picks, decided here like the other
    // routing; nothing saved = the System default.
    {
        use crate::share::{DeviceTrack, MixerSide};
        let saved = &g.prefs.prefs().audio_devices;
        req.mic_device = saved.get(MixerSide::Send, DeviceTrack::Mic).map(str::to_string);
        req.output_device = saved.get(MixerSide::Send, DeviceTrack::Output).map(str::to_string);
    }
    let (tx, rx) = std::sync::mpsc::channel::<ShareEvent>();
    let engine = match ShareEngine::start(&req, tx) {
        Ok(e) => e,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    g.share = Some(engine);
    g.preview_fps = req.preview_fps;
    if by_user {
        let peer = req.peer.clone().unwrap_or_default();
        g.send_intent =
            Some(crate::resilience::Record::for_send(&req, &peer, crate::peers::now_unix()));
        g.send_episode = None;
        persist_intent(&g);
        g.state.sharing = crate::types::ShareState::Sharing { peer: String::new() };
    }
    g.last_share = Some(req);
    drop(g);

    let events2 = events.clone();
    let inner2 = inner.clone();
    std::thread::Builder::new()
        .name("relay-share-pump".into())
        .spawn(move || {
            // Set by a `refused` error line; read when the engine exits.
            let mut refused: Option<String> = None;
            crate::share::pump(rx, |ev| match ev {
                ShareEvent::Stats { data } => {
                    let _ = events2.send(Event::ShareStats { data });
                }
                ShareEvent::Preview { width, height, jpeg } => {
                    let _ = events2.send(Event::SharePreview { width, height, jpeg });
                }
                ShareEvent::Connected { peer, trusted } => {
                    let was_reconnecting = {
                        let mut ig = inner2.lock();
                        let back = ig.send_episode.take().is_some();
                        if let Some(rec) = ig.send_intent.as_mut() {
                            // The name the engine actually reached, so a
                            // resume after a reboot has something to look up.
                            rec.peer = Some(peer.clone());
                            rec.attempts = 0;
                        }
                        persist_intent(&ig);
                        ig.state.sharing = crate::types::ShareState::Sharing { peer: peer.clone() };
                        back
                    };
                    let _ = events2.send(Event::ShareStatus {
                        sharing: true,
                        peer: Some(peer.clone()),
                        message: None,
                        trusted,
                    });
                    let state = Box::new(inner2.lock().state.clone());
                    let _ = events2.send(Event::StateChanged { state });
                    if was_reconnecting {
                        let text = format!("The share to {peer} is back.");
                        info!(%text, "reconnected");
                        let _ = events2.send(Event::Notice { text: text.clone() });
                        crate::winloop::balloon("Relay", &text);
                    }
                }
                ShareEvent::Error { message } => {
                    let _ = events2.send(Event::ShareStatus {
                        sharing: true,
                        peer: None,
                        message: Some(message),
                        trusted: false,
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
                // The sender's stats lines carry the codec for the strip.
                ShareEvent::Waiting { .. }
                | ShareEvent::Paired { .. }
                | ShareEvent::Codec { .. }
                | ShareEvent::RenderUp { .. }
                | ShareEvent::Host { .. }
                | ShareEvent::HostClose
                | ShareEvent::SenderStopped
                | ShareEvent::WrongCode { .. } => {}
                ShareEvent::Refused { message } => refused = Some(message),
                ShareEvent::Exited { ok, code } => {
                    on_share_exit(&inner2, &events2, ok, code, refused.take())
                }
            });
            // Channel closed (child stdout closed) without an `Exited` line:
            // treat it as an exit we were not told about.
            let orphaned = inner2.lock().share.is_some();
            if orphaned {
                on_share_exit(&inner2, &events2, false, None, refused.take());
            }
        })
        .ok();

    let _ = events.send(Event::ShareStatus {
        sharing: true,
        peer: None,
        message: None,
        trusted: false,
    });
    Reply::Ok
}

/// The share engine is gone. Either the user stopped it — the intent was
/// cleared first, so there is nothing to do but report — or it died, in
/// which case S38 starts bringing it back, and says so once per episode.
fn on_share_exit(
    inner: &Arc<Mutex<Inner>>,
    events: &broadcast::Sender<Event>,
    ok: bool,
    code: Option<i32>,
    refused: Option<String>,
) {
    let mut g = inner.lock();
    g.share = None;
    // Refused by the other PC: a final answer, not a drop. No crash record,
    // no reconnecting -- clear the intent and say why.
    if let Some(message) = refused {
        g.send_intent = None;
        g.send_episode = None;
        persist_intent(&g);
        g.state.sharing = crate::types::ShareState::Off;
        let state = Box::new(g.state.clone());
        drop(g);
        info!(%message, "share refused by the receiver");
        let _ = events.send(Event::ShareStatus {
            sharing: false,
            peer: None,
            message: Some(message),
            trusted: false,
        });
        let _ = events.send(Event::StateChanged { state });
        return;
    }
    if !ok {
        let dir = crate::crash::dir(&g.paths);
        let log = g.paths.log_dir().join("share.log");
        if let Some(p) = crate::crash::record_engine_exit(&dir, "relay-share", code, Some(&log)) {
            warn!(record = %p.display(), ?code, "share engine exited unexpectedly");
        }
    }
    let resilient = g.send_intent.is_some() && g.prefs.get().resilience;
    if resilient {
        let peer = g.send_intent.as_ref().and_then(|r| r.peer.clone()).unwrap_or_default();
        let first = g.send_episode.is_none();
        if first {
            g.send_episode = Some(crate::resilience::Episode::begin(std::time::Instant::now()));
        }
        let attempt = g.send_episode.map(|e| e.attempts).unwrap_or(0);
        g.state.sharing = crate::types::ShareState::Reconnecting { peer: peer.clone(), attempt };
        let state = Box::new(g.state.clone());
        drop(g);
        let _ = events.send(Event::StateChanged { state });
        if first {
            let text = format!("The share to {peer} dropped. Relay is reconnecting.");
            warn!(%text, "share dropped; reconnecting");
            let _ = events.send(Event::Notice { text: text.clone() });
            crate::winloop::balloon("Relay", &text);
        }
    } else {
        g.state.sharing = crate::types::ShareState::Off;
        let state = Box::new(g.state.clone());
        drop(g);
        let _ = events.send(Event::ShareStatus {
            sharing: false,
            peer: None,
            message: (!ok).then(|| "share engine stopped unexpectedly".to_string()),
            trusted: false,
        });
        let _ = events.send(Event::StateChanged { state });
    }
}

/// The receive engine is gone. Same shape as [`on_share_exit`]: a cleared
/// intent means the user stopped it; a present one means bring it back.
/// `sender` is who was connected, for the sentence.
fn on_receive_exit(
    inner: &Arc<Mutex<Inner>>,
    events: &broadcast::Sender<Event>,
    last_failure: Option<String>,
    sender: Option<String>,
    ended_by_sender: bool,
    crashed: bool,
) {
    let mut g = inner.lock();
    g.receive = None;
    if let Some(reason) = last_failure.as_deref() {
        if crashed {
            let dir = crate::crash::dir(&g.paths);
            let log = g.paths.log_dir().join("share.log");
            let _ = crate::crash::record_engine_exit(&dir, "relay-share-recv", None, Some(&log));
            warn!(%reason, "receiver exited unexpectedly");
        } else {
            warn!(%reason, "receiver stopped on an error it reported");
        }
    }
    let resilient = g.recv_intent.is_some() && g.prefs.get().resilience;
    let first = resilient && g.recv_episode.is_none();
    if first {
        g.recv_episode = Some(crate::resilience::Episode::begin(std::time::Instant::now()));
    }
    drop(g);
    // Read before `last_failure` moves into the status below.
    let own = last_failure.is_some() && !crashed;
    let _ = events.send(Event::ReceiveStatus {
        receiving: false,
        code: None,
        sender: None,
        message: last_failure,
        codec: None,
        trusted: false,
        ended_by_sender,
        restarting: resilient,
        return_pid: None,
        return_exe: None,
    });
    if first && !ended_by_sender {
        // A failure this PC reported is this PC's fault, not the sender's:
        // "the share from X dropped" blamed the wrong machine (r39, B3).
        let text = match sender {
            Some(s) if own => {
                format!("This PC stopped showing the share from {s}. Relay is starting it again.")
            }
            Some(s) => format!("The share from {s} dropped. Relay is waiting for it to come back."),
            None => "Receiving stopped on its own. Relay is starting it again.".to_string(),
        };
        warn!(%text, "receive dropped; restarting");
        let _ = events.send(Event::Notice { text: text.clone() });
        crate::winloop::balloon("Relay", &text);
    }
}

/// Spawn the receive engine (advertise + render) and relay its events.
/// `by_user` as for [`spawn_share`].
fn spawn_receive(
    inner: &Arc<Mutex<Inner>>,
    events: &broadcast::Sender<Event>,
    req: crate::share::ReceiveRequest,
    by_user: bool,
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
    // S40: the saved output pick; nothing saved = the System default.
    req.output_device = g
        .prefs
        .prefs()
        .audio_devices
        .get(crate::share::MixerSide::Receive, crate::share::DeviceTrack::Output)
        .map(str::to_string);
    // S36: the reverse of the rule in `spawn_share` -- a share that is
    // feeding Relay Camera keeps it while it runs.
    if req.vcam && g.share.is_some() && g.last_share.as_ref().is_some_and(|r| r.vcam) {
        req.vcam = false;
        let _ = events.send(Event::Notice {
            text: "Relay Camera is showing the share this PC is sending; the incoming share plays in Relay only.".into(),
        });
    }
    let (tx, rx) = std::sync::mpsc::channel::<ShareEvent>();
    let engine = match ShareEngine::start_receive(&req, tx) {
        Ok(e) => e,
        Err(e) => return Reply::Error { message: e.to_string() },
    };
    g.receive = Some(engine);
    g.recv_started = Some(std::time::Instant::now());
    if req.host.is_some_and(|h| h != 0) {
        g.recv_host = req.host;
    }
    if by_user {
        g.recv_intent =
            Some(crate::resilience::Record::for_receive(&req, crate::peers::now_unix()));
        g.recv_episode = None;
        persist_intent(&g);
    }
    drop(g);

    // Every status line says which call app this receiver returns from.
    let ret_pid = req.return_pid.filter(|p| *p != 0);
    #[cfg(windows)]
    let ret_exe = ret_pid
        .and_then(crate::winloop::process_image_path)
        .map(|p| crate::winloop::exe_name(&p))
        .filter(|e| !e.is_empty());
    #[cfg(not(windows))]
    let ret_exe: Option<String> = None;
    let events2 = events.clone();
    let inner2 = inner.clone();
    std::thread::Builder::new()
        .name("relay-receive-pump".into())
        .spawn(move || {
            // Why the engine stopped, carried to the final ReceiveStatus so a
            // failure is never reported as a plain return to idle.
            let mut last_failure: Option<String> = None;
            // The process itself exited abnormally. A receiver that reported
            // why and exited cleanly is a failure, not a crash (B3, r38).
            let mut crashed = false;
            // Who was connected, for the one sentence S38 says if it drops.
            let mut last_sender: Option<String> = None;
            let mut ended_by_sender = false;
            // Whether the connected sender came in as a remembered PC. Kept so
            // later status lines (the codec) repeat who is connected instead
            // of wiping it: a sender-less line after `paired` made the screen
            // forget "remembered, no code" 0.4 s into every trusted share.
            let mut last_trusted = false;
            let mut streaming = false;
            let mut stream_size: (u32, u32) = (0, 0);
            crate::share::pump(rx, |ev| match ev {
                // The receiver renders into the app (or its own window), so a
                // preview from that side would be a picture of something
                // already on screen.
                ShareEvent::Preview { .. } => {}
                ShareEvent::Waiting { code, .. } => {
                    // Keep the code in the record, so a receive brought back
                    // after a crash, an update or a reboot shows the code the
                    // user may already have read out, not a new one.
                    {
                        let mut ig = inner2.lock();
                        let req = ig.recv_intent.as_mut().and_then(|r| r.receive.as_mut());
                        if let Some(req) = req.filter(|q| q.code.as_deref() != Some(code.as_str()))
                        {
                            req.code = Some(code.clone());
                            persist_intent(&ig);
                        }
                    }
                    let return_pid = inner2
                        .lock()
                        .recv_intent
                        .as_ref()
                        .and_then(|r| r.receive.as_ref())
                        .and_then(|q| q.return_pid)
                        .filter(|p| *p != 0);
                    let _ = events2.send(Event::ReceiveStatus {
                        receiving: true,
                        code: Some(code),
                        sender: None,
                        message: None,
                        codec: None,
                        trusted: false,
                        ended_by_sender: false,
                        restarting: false,
                        return_pid,
                        return_exe: ret_exe.clone(),
                    });
                }
                ShareEvent::Codec { codec } => {
                    let _ = events2.send(Event::ReceiveStatus {
                        receiving: true,
                        code: None,
                        sender: last_sender.clone(),
                        message: None,
                        codec: Some(codec),
                        trusted: last_trusted,
                        ended_by_sender: false,
                        restarting: false,
                        return_pid: ret_pid,
                        return_exe: ret_exe.clone(),
                    });
                }
                ShareEvent::Paired { sender, trusted } => {
                    last_sender = Some(sender.clone());
                    last_trusted = trusted;
                    let _ = events2.send(Event::ReceiveStatus {
                        receiving: true,
                        code: None,
                        sender: Some(sender),
                        message: None,
                        codec: None,
                        trusted,
                        ended_by_sender: false,
                        restarting: false,
                        return_pid: ret_pid,
                        return_exe: ret_exe.clone(),
                    });
                }
                ShareEvent::Stats { data } => {
                    // Media is flowing: the episode, if any, is over. Not on
                    // `paired` -- a receiver that pairs and then dies the same
                    // way every time (a dead host window, say) reset the
                    // counter on each try and never backed off or gave up.
                    if !streaming {
                        streaming = true;
                        let mut ig = inner2.lock();
                        ig.recv_episode = None;
                        if let Some(r) = ig.recv_intent.as_mut() {
                            r.attempts = 0;
                        }
                    }
                    let _ = events2.send(Event::ShareStats { data });
                }
                ShareEvent::Error { message } => {
                    // Remember it: if the engine then exits, this is the only
                    // explanation the user will ever get.
                    last_failure = Some(message.clone());
                    // A wrong code must still cost the guesser this code.
                    if message.contains("code mismatch") {
                        let mut ig = inner2.lock();
                        if let Some(req) = ig.recv_intent.as_mut().and_then(|r| r.receive.as_mut())
                        {
                            req.code = None;
                            persist_intent(&ig);
                        }
                    }
                    let _ = events2.send(Event::ReceiveStatus {
                        receiving: true,
                        code: None,
                        sender: None,
                        message: Some(message),
                        codec: None,
                        trusted: false,
                        ended_by_sender: false,
                        restarting: false,
                        return_pid: ret_pid,
                        return_exe: ret_exe.clone(),
                    });
                }
                // Keep *why* it stopped. An engine that dies during startup --
                // no decoder for the incoming codec, say -- emits an error or a
                // non-zero exit and then nothing. Dropping both here is what
                // made "Start receiving" look like it did nothing at all: the
                // UI flipped straight back to Idle with no reason given.
                ShareEvent::Exited { ok, code } => {
                    crashed = !ok;
                    if !ok && last_failure.is_none() {
                        last_failure = Some(match code {
                            Some(c) => format!("the receiver stopped unexpectedly (exit {c})"),
                            None => "the receiver stopped unexpectedly".to_string(),
                        });
                    }
                }
                // S29: the stream window. Its size and mode are remembered so
                // a later `host` line (which carries no size) still tells the
                // shell everything it needs.
                ShareEvent::RenderUp { hwnd, width, height, host, excluded_from_capture } => {
                    stream_size = (width, height);
                    if !excluded_from_capture {
                        warn!("stream window is NOT excluded from capture (B9)");
                    }
                    let _ = events2.send(Event::StreamWindow {
                        hwnd,
                        width,
                        height,
                        mode: host,
                        excluded_from_capture,
                    });
                }
                ShareEvent::Host { mode, hwnd, excluded_from_capture } => {
                    info!(mode, hwnd, excluded_from_capture, "stream window mode from the engine");
                    if !excluded_from_capture {
                        warn!(mode, "stream window is NOT excluded from capture (B9)");
                    }
                    let _ = events2.send(Event::StreamWindow {
                        hwnd,
                        width: stream_size.0,
                        height: stream_size.1,
                        mode,
                        excluded_from_capture,
                    });
                }
                ShareEvent::SenderStopped => ended_by_sender = true,
                // Only a sender is refused; a receiver never emits it.
                ShareEvent::Refused { .. } => {}
                ShareEvent::WrongCode { name } => {
                    let who = if name.is_empty() { "A PC".to_string() } else { name.clone() };
                    let text = format!(
                        "{who} tried to connect with the wrong code. The code has changed."
                    );
                    warn!(%text, "wrong pairing code");
                    let _ = events2.send(Event::Notice { text });
                }
                ShareEvent::HostClose => {
                    info!("receiver's popped-out window closed; it re-embeds itself");
                    let _ = events2.send(Event::StreamPopoutClosed);
                }
                ShareEvent::Connected { .. }
                | ShareEvent::Recording { .. }
                | ShareEvent::ReplaySaved { .. }
                | ShareEvent::SourceChanged { .. } => {}
            });
            let orphaned = inner2.lock().receive.is_some();
            if orphaned {
                on_receive_exit(
                    &inner2,
                    &events2,
                    last_failure.take(),
                    last_sender.take(),
                    ended_by_sender,
                    crashed,
                );
            }
        })
        .ok();
    Reply::Ok
}

/// The user's Stop. The intent goes first, so the exit that follows is read
/// as "stopped", never as "died" (S38). Stopping while Relay is between
/// reconnect attempts — no engine, an intent — is the same request and
/// succeeds the same way.
fn kill_receive(inner: &Arc<Mutex<Inner>>, events: &broadcast::Sender<Event>) -> Reply {
    let (engine, was_recovering) = {
        let mut g = inner.lock();
        let recovering = g.recv_intent.is_some() && g.receive.is_none();
        g.recv_intent = None;
        g.recv_episode = None;
        persist_intent(&g);
        (g.receive.take(), recovering)
    };
    match engine {
        Some(engine) => {
            engine.stop();
        }
        None if was_recovering => {}
        None => return Reply::Error { message: "not receiving".into() },
    }
    let _ = events.send(Event::ReceiveStatus {
        receiving: false,
        code: None,
        sender: None,
        message: None,
        codec: None,
        trusted: false,
        ended_by_sender: false,
        restarting: false,
        return_pid: None,
        return_exe: None,
    });
    Reply::Ok
}

fn kill_share(inner: &Arc<Mutex<Inner>>, events: &broadcast::Sender<Event>) -> Reply {
    let (engine, was_recovering) = {
        let mut g = inner.lock();
        let recovering = g.send_intent.is_some() && g.share.is_none();
        g.send_intent = None;
        g.send_episode = None;
        persist_intent(&g);
        (g.share.take(), recovering)
    };
    match engine {
        Some(engine) => {
            engine.stop();
        }
        None if was_recovering => {}
        None => return Reply::Error { message: "no share is running".into() },
    }
    let state = {
        let mut g = inner.lock();
        g.state.sharing = crate::types::ShareState::Off;
        Box::new(g.state.clone())
    };
    let _ = events.send(Event::ShareStatus {
        sharing: false,
        peer: None,
        message: None,
        trusted: false,
    });
    let _ = events.send(Event::StateChanged { state });
    Reply::Ok
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
                let correction = correction_for(&g, &profile);
                match g.applier.apply(&profile, target.as_ref(), correction.as_deref()) {
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
            Method::RestoreAll => match restore_all(&mut g) {
                Ok(()) => Reply::Ok,
                Err(e) => Reply::Error { message: e.to_string() },
            },
            Method::ListProcesses => {
                Reply::Processes { processes: crate::processes::list_windowed_and_audible() }
            }
            Method::GetUiPrefs => Reply::UiPrefs { prefs: g.prefs.get() },
            Method::SetUiPrefs { prefs } => match g.prefs.set(prefs) {
                Ok(()) => Reply::UiPrefs { prefs: g.prefs.get() },
                Err(e) => Reply::Error { message: format!("{e:#}") },
            },
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
                spawn_share(&self.inner, &self.events, *request, true)
            }
            Method::StopShare => {
                drop(g);
                kill_share(&self.inner, &self.events)
            }
            Method::StartSharePreset { preset, code, peer, peer_id } => {
                let Some(def) = g.presets.get(&preset).cloned() else {
                    return Reply::Error { message: format!("no preset `{preset}`") };
                };
                let game_pid = g.state.foreground.as_ref().map(|f| f.pid);
                let mut req = crate::presets::to_share_request(
                    &def,
                    code,
                    peer,
                    game_pid,
                    &g.presets.recording,
                );
                req.peer_id = peer_id;
                drop(g);
                spawn_share(&self.inner, &self.events, req, true)
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
            Method::SetMixer { side, faders } => {
                drop(g);
                let cmd = crate::share::EngineCmd::Mixer { faders };
                match side {
                    crate::share::MixerSide::Send => engine_command(&self.inner, &cmd),
                    crate::share::MixerSide::Receive => receive_command(&self.inner, &cmd),
                }
            }
            Method::ListAudioDevices => {
                drop(g);
                #[cfg(windows)]
                let devices = crate::hardware::probe_win::list_audio_devices();
                #[cfg(not(windows))]
                let devices = crate::share::AudioDevices::default();
                Reply::AudioDevices { devices }
            }
            Method::SetAudioDevice { side, track, device } => {
                // Saved first, so the pick holds for the next share even when
                // nothing is running now; then sent live if something is.
                match g.prefs.set_device(side, track, device.clone()) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Reply::Error {
                            message: "a receiver has no microphone to choose".into(),
                        }
                    }
                    Err(e) => return Reply::Error { message: e.to_string() },
                }
                let running = match side {
                    crate::share::MixerSide::Send => g.share.is_some(),
                    crate::share::MixerSide::Receive => g.receive.is_some(),
                };
                drop(g);
                if !running {
                    return Reply::Ok;
                }
                let cmd = crate::share::EngineCmd::Device { track, device };
                match side {
                    crate::share::MixerSide::Send => engine_command(&self.inner, &cmd),
                    crate::share::MixerSide::Receive => receive_command(&self.inner, &cmd),
                }
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
                spawn_receive(&self.inner, &self.events, *request, true)
            }
            Method::StopReceive => {
                drop(g);
                kill_receive(&self.inner, &self.events)
            }
            Method::HostReceive { mode, owner } => {
                drop(g);
                info!(?mode, owner, "host command for the receiver");
                if owner != 0 {
                    self.inner.lock().recv_host = Some(owner);
                }
                receive_command(&self.inner, &crate::share::EngineCmd::Host { mode, owner })
            }
            Method::DiscoverReceivers => {
                drop(g);
                match crate::share::discover_receivers(2000) {
                    Ok(receivers) => Reply::Receivers { receivers },
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::ListPeers => {
                drop(g);
                match crate::peers::path() {
                    Ok(p) => Reply::Peers { peers: crate::peers::Store::load(&p).list() },
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::ForgetPeer { id } => {
                drop(g);
                edit_peers(|s| s.forget(&id))
            }
            Method::SetPeerFavourite { id, favourite } => {
                drop(g);
                edit_peers(|s| s.set_favourite(&id, favourite))
            }
            Method::ListHardware => Reply::Hardware {
                headsets: g.library.headsets.clone(),
                monitors: g.library.monitors.clone(),
                interfaces: g.library.interfaces.clone(),
                vendor_controls: crate::hardware::vendor_controls(
                    &g.library.monitors,
                    &g.state.hardware.monitors,
                ),
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
                // Remember what the panels reported: the advertised VCP codes
                // and the EDID colour characteristics, so the library screen
                // can show both without re-probing.
                let mut dirty = false;
                for probed in &report.monitors {
                    if let Some(known) = g.library.monitors.iter_mut().find(|m| m.id == probed.id) {
                        if probed.color.is_some() && known.color != probed.color {
                            known.color = probed.color.clone();
                            dirty = true;
                        }
                    }
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
            Method::SearchCatalog { query } => {
                drop(g);
                let index = match crate::hardware::catalog::index_path() {
                    Ok(p) => p,
                    Err(e) => return Reply::Error { message: format!("{e:#}") },
                };
                match crate::hardware::catalog::search(&index, &query, 40) {
                    Ok(entries) => Reply::Catalog { entries },
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            Method::AddHeadsetFromCatalog { entry, endpoint } => {
                let cache = g.paths.curves_dir();
                drop(g);
                // Fetch outside the lock: this is the one request that can
                // block on the network, and holding the state lock through it
                // would stall every focus change until it returned.
                let (points, fetched) = match crate::hardware::catalog::curve(&entry, &cache) {
                    Ok(v) => v,
                    Err(e) => {
                        return Reply::Error {
                            message: format!("could not get that measurement: {e:#}"),
                        }
                    }
                };
                info!(
                    model = %entry.name, source = %entry.source, points = points.len(), fetched,
                    "headset added from the catalogue"
                );

                let headset = crate::hardware::Headset {
                    id: crate::types::HeadsetId(entry.slug()),
                    name: entry.name.clone(),
                    kind: kind_from_path(&entry.path),
                    curve: Some(points),
                    source: entry.source.clone(),
                    endpoints: endpoint.into_iter().collect(),
                };
                let mut g = self.inner.lock();
                g.library.upsert_headset(headset);
                match g.library.save() {
                    Ok(()) => {
                        drop(g);
                        library_changed(&self.inner, &self.events);
                        Reply::Ok
                    }
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Method::RenderPreview { id, wav } => {
                let Some(profile) = g.store.get(id).cloned() else {
                    return Reply::Error { message: "no such profile".into() };
                };
                let dir = g.previews_dir.clone();
                let correction = correction_for(&g, &profile);
                drop(g);
                let wav = wav.map(std::path::PathBuf::from);
                match crate::audio_bridge::render_preview(
                    &profile.audio,
                    wav.as_deref(),
                    &dir,
                    correction.as_deref(),
                ) {
                    Ok(p) => Reply::Preview {
                        original: p.original.display().to_string(),
                        processed: p.processed.display().to_string(),
                        sample_rate: p.sample_rate,
                        hrtf_applied: p.hrtf_applied,
                    },
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            Method::FirewallStatus => {
                drop(g);
                match crate::firewall::share_program() {
                    Ok(program) => {
                        Reply::Firewall { status: Box::new(crate::firewall::status(&program)) }
                    }
                    Err(e) => Reply::Error { message: format!("{e:#}") },
                }
            }
            Method::ShareCapabilities => {
                drop(g);
                match crate::share::capabilities() {
                    Ok(c) => Reply::Capabilities {
                        can_share: !c.share_codecs.is_empty(),
                        can_receive: !c.receive_codecs.is_empty(),
                        adapters: c.adapters,
                        encoders: c.encoders,
                        decoders: c.decoders,
                        share_codecs: c.share_codecs,
                        receive_codecs: c.receive_codecs,
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
            Method::ElevationPlan { op } => {
                let paths = g.paths.clone();
                drop(g);
                Reply::DryRun { lines: crate::elevate::plan_lines(&paths, op) }
            }
            // Handled ahead of this match (it blocks on a UAC prompt), and
            // only reachable if that dispatch is ever removed.
            #[cfg(windows)]
            Method::RunElevated { op } => {
                let paths = g.paths.clone();
                drop(g);
                self.run_elevated(&paths, op)
            }
            #[cfg(not(windows))]
            Method::RunElevated { .. } => {
                Reply::Error { message: "Elevated installs are Windows-only".into() }
            }
            Method::UninstallPlan { keep_data } => {
                let paths = g.paths.clone();
                drop(g);
                Reply::DryRun { lines: crate::uninstall::plan(&paths, keep_data).lines() }
            }
            // One installer, one uninstaller: Settings hands over to the
            // Windows uninstaller rather than doing its own removal, then
            // stops the core so the files are free.
            Method::LaunchUninstaller => match crate::uninstall::launch_uninstaller() {
                Ok(path) => {
                    drop(g);
                    let _ = self
                        .events
                        .send(Event::Notice { text: format!("Started {}", path.display()) });
                    let _ = self.shutdown.send(CoreEvent::Shutdown);
                    Reply::Ok
                }
                Err(e) => Reply::Error { message: format!("{e:#}") },
            },
            Method::Subscribe => {
                drop(g);
                let last = self.last_recv.lock().unwrap().clone();
                if let Some(ev) = last {
                    let _ = self.events.send(ev);
                }
                Reply::Ok
            }
            // S38: the window closed and the core is staying. Say so where
            // the user can see it, every time, unless they turned it off.
            Method::WindowClosed => {
                let notice = g.prefs.get().close_notice;
                drop(g);
                // Logged either way: the balloon itself leaves no trace, and
                // the two-PC pass could not otherwise tell whether it fired.
                info!(close_notice = notice, "window closed; core keeps running");
                if notice {
                    crate::winloop::balloon(
                        "Relay is still running",
                        "Your profiles keep applying. Right-click the icon by the clock to open or quit Relay.",
                    );
                }
                Reply::Ok
            }
            Method::AckCrash => {
                crate::crash::mark_seen(&crate::crash::dir(&g.paths));
                g.state.last_crash = None;
                Reply::Ok
            }
            Method::Shutdown => {
                let _ = self.shutdown.send(CoreEvent::Shutdown);
                Reply::Ok
            }
        }
    }
}

#[cfg(windows)]
impl IpcHandler {
    /// Ask for administrator rights and run one op in the helper.
    ///
    /// Declining is not an error: the reply carries `declined: true` and the
    /// plainest sentence we have, because the whole point of the prompt is
    /// that saying no must be safe and legible.
    fn run_elevated(&self, paths: &crate::config::Paths, op: crate::elevate::ElevatedOp) -> Reply {
        match crate::elevate::run(paths, &[op]) {
            Ok(response) => {
                let lines = response.lines();
                for line in &lines {
                    let _ = self.events.send(Event::Notice { text: line.clone() });
                }
                Reply::Elevation { declined: false, ok: response.ok(), lines }
            }
            Err(crate::elevate::LaunchError::Declined) => Reply::Elevation {
                declined: true,
                ok: false,
                lines: vec![
                    "Nothing on this PC was changed. You declined the Windows permission prompt."
                        .into(),
                ],
            },
            Err(crate::elevate::LaunchError::Other(e)) => {
                Reply::Error { message: format!("{e:#}") }
            }
        }
    }
}

#[cfg(windows)]
impl crate::ipc::server::Handler for IpcHandler {
    async fn handle(&self, method: Method) -> Reply {
        // The elevated helper blocks on a UAC prompt the user may leave on
        // screen for a minute. The core is a single-threaded runtime, so that
        // wait goes to the blocking pool instead of stopping focus applies,
        // share stats and every other client.
        if let Method::RunElevated { op } = method {
            let paths = self.inner.lock().paths.clone();
            let handler = IpcHandler {
                inner: self.inner.clone(),
                shutdown: self.shutdown.clone(),
                events: self.events.clone(),
                last_recv: self.last_recv.clone(),
            };
            return tokio::task::spawn_blocking(move || handler.run_elevated(&paths, op))
                .await
                .unwrap_or_else(|e| Reply::Error { message: format!("elevation task: {e}") });
        }
        self.handle_sync(method)
    }
}
