//! Receiver presentation: a native D3D11 swapchain window fed by the MF
//! DXVA decoder for the negotiated codec (HEVC or H.264). Decoded NV12 stays
//! on the GPU; the D3D11 video processor converts it straight into the
//! swapchain back buffer. Audio plays out through WASAPI shared mode.
//!
//! Two threads, on purpose (S29). The *window* thread creates the HWND and
//! does nothing but pump its messages; the *render* thread decodes and
//! presents. They used to be one thread, and a message pump that also
//! decodes stalls the picture for the whole of any modal loop — a title-bar
//! drag, a resize — which was tolerable for a window of its own and would be
//! reachable from ordinary app resizing once the stream lives inside the app.
//! The two never wait on each other while both are alive: the window thread
//! sets a flag and keeps pumping; the render thread releases its D3D objects
//! and *then* posts the window a shutdown message.
//!
//! Hosting (`host` module): with `--host <hwnd>` the window is created as a
//! frameless popup *owned by* the app window, hidden until the app positions
//! it over its video area, and the app can pop it out into an ordinary
//! top-level window and back. It is a popup, not a `WS_CHILD`, because
//! `SetWindowDisplayAffinity` — the thing that stops Relay capturing its own
//! output (B9) — applies only to top-level windows of the calling process.
//! The engine therefore keeps ownership of every style change and reasserts
//! the affinity after each one; the app only moves the window about.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};
use windows::core::Interface;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::command::{self, EngineCmd, HostMode};
use crate::decode::mf::MfDecoder;
use crate::transport::receiver::{AccessUnit, RecvStats};
use crate::{probe, signal_now_ns};

pub mod host;

/// Receiver output options beyond the window itself.
#[derive(Debug, Default, Clone)]
pub struct RenderOpts {
    /// Start "Relay Camera" and mirror decoded frames into its ring.
    pub vcam: bool,
    /// Render decoded audio to this endpoint id instead of the default
    /// device (the interim virtual-mic route: a VB-Cable / VoiceMeeter
    /// input endpoint).
    pub mic_route: Option<String>,
    /// The app window to embed the stream window in. `None` = a window of
    /// its own, as when run from a console.
    pub host: Option<u64>,
    /// The output endpoint received audio plays on (S40), changed live by a
    /// `device` command. Empty = the System default, followed if it moves.
    pub output: Arc<crate::devices::DeviceSlot>,
}

/// Apply a `device` command on the receiver. Only `Output` means anything
/// here, and not while the audio is routed into a virtual mic: that route is
/// the call's input, and a speaker pick must not silently undo it.
pub fn apply_device(
    output: &crate::devices::DeviceSlot,
    mic_routed: bool,
    track: crate::command::DeviceTrack,
    device: Option<String>,
) {
    use crate::command::DeviceTrack;
    match track {
        DeviceTrack::Output if mic_routed => {
            info!("output pick ignored: received audio is routed to the virtual mic")
        }
        DeviceTrack::Output => {
            let changed = output.set(device.clone());
            info!(
                device = device.as_deref().unwrap_or("System default"),
                changed, "receiver output device set"
            );
        }
        DeviceTrack::Mic => tracing::debug!("mic device is a sender command; ignored"),
    }
}

/// Hands a fatal error to the signalling task, which tells the sender and
/// acknowledges on the oneshot once the message is written.
pub type AbortTx = mpsc::Sender<(String, tokio::sync::oneshot::Sender<()>)>;

/// The transport's link to the window thread: the HWND once it exists, and
/// a host command that arrived before it did.
pub struct HostLink {
    hwnd: AtomicIsize,
    pending: Mutex<Option<(HostMode, u64)>>,
    /// Bumped by the window thread after every hosting-mode change. The
    /// render thread recreates its swapchain when it sees a new value: a
    /// flip-model swapchain does not reliably keep presenting into a window
    /// whose frame and owner just changed (the second PC showed black in
    /// the app after every pop-in), and a fresh one costs nothing visible.
    surface_gen: AtomicU64,
}

impl HostLink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            hwnd: AtomicIsize::new(0),
            pending: Mutex::new(None),
            surface_gen: AtomicU64::new(0),
        })
    }

    /// The window changed mode; the swapchain should be rebuilt.
    pub(crate) fn bump_surface(&self) {
        self.surface_gen.fetch_add(1, Ordering::AcqRel);
    }

    fn surface_gen(&self) -> u64 {
        self.surface_gen.load(Ordering::Acquire)
    }

    /// Ask the window thread to change hosting mode. Before the window
    /// exists the request is parked and applied at creation, so a command
    /// that races the first frame is not lost.
    pub fn post(&self, mode: HostMode, owner: u64) {
        let hwnd = self.hwnd.load(Ordering::Acquire);
        if hwnd == 0 {
            *self.pending.lock().unwrap() = Some((mode, owner));
            return;
        }
        host::post_mode(HWND(hwnd as *mut _), mode, owner);
    }

    fn take_pending(&self) -> Option<(HostMode, u64)> {
        self.pending.lock().unwrap().take()
    }

    fn set_hwnd(&self, hwnd: HWND) {
        self.hwnd.store(hwnd.0 as isize, Ordering::Release);
    }
}

/// The engine's one reader of the core's commands. The receiver opens it
/// before it starts waiting for a sender and hands it on here, so a `stop`
/// is heard while waiting too and no line is lost between two readers.
pub type StdinLines = tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>;

/// Open the command reader.
pub fn stdin_lines() -> StdinLines {
    use tokio::io::AsyncBufReadExt;
    tokio::io::BufReader::new(tokio::io::stdin()).lines()
}

/// Entry point used by the transport when not headless.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    aus: mpsc::Receiver<AccessUnit>,
    opus: mpsc::Receiver<Vec<u8>>,
    mic: mpsc::Receiver<Vec<u8>>,
    rest: mpsc::Receiver<Vec<u8>>,
    stats: Arc<RecvStats>,
    mut closed: mpsc::Receiver<()>,
    pc: impl webrtc::peer_connection::PeerConnection,
    opts: RenderOpts,
    abort: AbortTx,
    stdin_lines: StdinLines,
) -> Result<()> {
    // Audio playback thread (best-effort; a decode failure must not kill video).
    let (audio_stop_tx, audio_stop_rx) = std::sync::mpsc::channel::<()>();
    let output = opts.output.clone();
    let mic_routed = opts.mic_route.is_some();
    let output2 = output.clone();
    let audio_stats = Arc::new(crate::playback::PlaybackStats::default());
    let audio_stats2 = audio_stats.clone();
    // Per-track gain and mute (S37): set from stdin here, read by playback.
    let faders = crate::mixer::Faders::shared();
    let faders2 = faders.clone();
    let audio_join =
        std::thread::Builder::new().name("relay-audio-playback".into()).spawn(move || {
            use crate::mixer::Track;
            if let Err(e) = crate::playback::run(
                vec![(opus, Track::App), (mic, Track::Mic), (rest, Track::Rest)],
                audio_stop_rx,
                output2,
                audio_stats2,
                faders2,
            ) {
                warn!(error = %e, "audio playback stopped");
            }
        })?;

    // Video window + decode thread.
    let present_latency = Arc::new(AtomicI64::new(0));
    let pl = present_latency.clone();
    let stats2 = stats.clone();
    let vcam = opts.vcam;
    let failure: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    let failure2 = failure.clone();
    let quit = Arc::new(AtomicBool::new(false));
    let quit2 = quit.clone();
    let link = HostLink::new();
    let link2 = link.clone();
    let host_owner = opts.host;
    let video_join = std::thread::Builder::new().name("relay-render".into()).spawn(move || {
        if let Err(e) = video_thread(aus, stats2, pl, vcam, quit2, link2, host_owner) {
            // ERROR, and in share.log: the one line someone reads when a
            // receive dies (the dead-host fatal used to reach core.log only).
            tracing::error!(error = %e, "receiver failed; telling the sender why");
            *failure2.lock().unwrap() = Some(e.to_string());
            println!(
                "{}",
                serde_json::json!({ "event": "error", "where": "render", "message": e.to_string() })
            );
        }
    })?;

    // Commands from the core on stdin: `stop`, and `host` to move the window
    // between the app and a window of its own. The receiver never read stdin
    // before S29, so a stop had to wait out the core's 3 s grace and a kill.
    let mut stdin_lines = stdin_lines;
    let spawned_by_core = std::env::var("RELAY_SPAWNED").is_ok();
    let mut stdin_open = true;

    // Report present latency until the window or the connection closes.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
    // Previous sample, so a stall shows up as "no change" rather than needing
    // two log lines compared by eye.
    let mut last_aus: u64 = 0;
    let mut last_presented: u64 = 0;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if video_join.is_finished() { break; }
                let aus = stats.video_aus.load(Ordering::Relaxed);
                let presented = stats.video_presented.load(Ordering::Relaxed);
                let audio = stats.audio_packets.load(Ordering::Relaxed);
                let latency_ms = present_latency.load(Ordering::Relaxed) as f64 / 1e3;
                // null until the warm-up frames have passed (see WARMUP_FRAMES).
                let latency = (presented > 60).then_some(latency_ms);
                // The log says "warming up" rather than a 0.0 that reads as a
                // real measurement.
                let latency_log = latency.map_or_else(|| "warming up".to_string(), |v| format!("{v:.3}"));
                let gaps = stats.video_gaps.load(Ordering::Relaxed);
                let lost = stats.video_lost_packets.load(Ordering::Relaxed);
                let recovered = stats.video_recovered.load(Ordering::Relaxed);
                let keyframe_requests = stats.keyframe_requests.load(Ordering::Relaxed);
                let withheld = stats.video_aus_dropped.load(Ordering::Relaxed);
                println!("{}", serde_json::json!({
                    "event": "stats",
                    "aus": aus,
                    "presented": presented,
                    "audio_packets": audio,
                    "mic_packets": stats.mic_packets.load(Ordering::Relaxed),
                    "rest_packets": stats.rest_packets.load(Ordering::Relaxed),
                    "return_packets": stats.return_packets.load(Ordering::Relaxed),
                    "return_peak": stats.return_peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
                    "capture_to_present_ms": latency,
                    "rtp_gaps": gaps,
                    "rtp_lost": lost,
                    "rtp_recovered": recovered,
                    "keyframe_requests": keyframe_requests,
                    "frames_withheld": withheld,
                    "audio": audio_stats.json(),
                }));

                // Also to the log. A receiver that freezes mid-share leaves
                // nothing behind otherwise: on a real machine the picture
                // stopped after a few seconds and share.log had no line at all
                // between "decoder up" and the disconnect 33 s later, so there
                // was no way to tell whether frames stopped arriving, stopped
                // decoding, or stopped being presented. These three counters
                // separate those cases.
                let pts = stats.last_pts_100ns.load(Ordering::Relaxed);
                let slice = stats.last_subresource.load(Ordering::Relaxed);
                let audio_ms = audio_stats.buffered_ms();
                let stalled_aus = aus == last_aus;
                let stalled_present = presented == last_presented;
                // Only warn once frames have started: two false alarms fired
                // at connect before the video track existed, which is noise in
                // the one log someone reads when things go wrong.
                if presented > 0 && (stalled_aus || stalled_present) {
                    warn!(
                        aus, presented, audio, audio_ms, latency_ms = %latency_log, pts, slice, gaps, lost,
                        arriving = !stalled_aus, presenting = !stalled_present,
                        "receiver stalled"
                    );
                } else if presented > 0 {
                    info!(
                        aus, presented, audio, audio_ms, latency_ms = %latency_log, pts, slice, gaps, lost,
                        recovered, keyframe_requests, withheld, "receiving"
                    );
                }
                last_aus = aus;
                last_presented = presented;
            }
            line = stdin_lines.next_line(), if stdin_open => {
                match line {
                    Ok(Some(l)) => match command::parse_line(&l) {
                        Some(EngineCmd::Stop) => { info!("stop command received"); break; }
                        Some(EngineCmd::Host { mode, owner }) => {
                            info!(?mode, owner, "host command received on stdin");
                            link.post(mode, owner)
                        }
                        Some(EngineCmd::Mixer { faders: set }) => {
                            faders.apply(&set);
                            tracing::debug!(?set, "mixer set");
                        }
                        Some(EngineCmd::Device { track, device }) => {
                            apply_device(&output, mic_routed, track, device);
                        }
                        Some(other) => tracing::debug!(?other, "command not for a receiver"),
                        None => tracing::debug!(line = %l, "unrecognised stdin line ignored"),
                    },
                    _ if spawned_by_core => { info!("stdin closed; core went away"); break; }
                    _ => stdin_open = false,
                }
            }
            _ = closed.recv() => { info!("connection closed"); break; }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    // Whatever ended the loop, the render thread must stop now (B8): it polls
    // this flag between frames and no longer blocks on a channel that the
    // peer connection may never close.
    quit.store(true, Ordering::Release);

    // And the process must end, whatever the teardown below does. On the
    // second PC a share that ended by the sender stopping left relay-share
    // resident after the render thread had finished; the core reports the
    // end only when this process exits, so the app never said the share was
    // over and the last window stayed on screen. Everything after this line
    // is tidiness — the peer is gone or going, the picture has stopped — so
    // it gets a deadline, and the exit says so. The core reads `stopped` off
    // stdout before the pipe closes.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(3));
        warn!("teardown did not finish within 3 s; exiting now");
        println!("{}", serde_json::json!({ "event": "stopped", "forced": true }));
        std::process::exit(0);
    });

    // Tell the sender why before closing, or it only learns "connection lost"
    // seconds later when ICE gives up (B3). Bounded: never hang the exit.
    let reason = failure.lock().unwrap().take();
    if let Some(reason) = reason {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        if abort.send((reason, done_tx)).await.is_ok() {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), done_rx).await;
        }
    }

    let _ = audio_stop_tx.send(());
    let teardown = std::time::Instant::now();
    crate::transport::close_bounded(&pc, "receiver").await;

    // Bounded joins (B8): a thread that will not wake is not worth hanging
    // the exit for — the failure reason was taken above, the peer connection
    // is closed, and the process is on its way out.
    join_bounded(video_join, "render");
    join_bounded(audio_join, "audio playback");
    info!(ms = teardown.elapsed().as_secs_f64() * 1e3, "teardown finished");
    Ok(())
}

/// How long the picture may stand still before we call the share over.
///
/// Generous on purpose: a frame is ~33 ms at 30 fps and a bad LAN moment can
/// swallow a second, so this only fires on a genuine end-of-stream. The cost of
/// being wrong is asymmetric — closing a live share early is far worse than
/// holding a dead one for another second.
const AU_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Wait briefly for a worker, then give up and say so. A thread still parked
/// on a channel at process exit costs nothing; a process that never exits
/// costs the user a stuck window and a stray capture.
fn join_bounded<T: Send + 'static>(handle: std::thread::JoinHandle<T>, what: &str) {
    const GRACE: std::time::Duration = std::time::Duration::from_millis(750);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = handle.join();
        let _ = tx.send(r.is_ok());
    });
    match rx.recv_timeout(GRACE) {
        Ok(_) => {}
        Err(_) => warn!(thread = what, "did not stop within {GRACE:?}; exiting anyway"),
    }
}

/// The D3D side of the window: device, swapchain and video-processor
/// interfaces. Lives on the render thread; the HWND it targets belongs to
/// the window thread.
pub struct Surface {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// `None` only between releasing an old chain and binding a new one; a
    /// failed rebuild leaves it `None` until the retry succeeds.
    swapchain: Option<IDXGISwapChain1>,
    vp_device: ID3D11VideoDevice,
    vp_context: ID3D11VideoContext,
    hwnd: HWND,
    size: (u32, u32),
}

impl Surface {
    /// Replace the swapchain with a new one on the same window and device.
    /// Called after the window's hosting mode changed; see `HostLink`.
    fn recreate_swapchain(&mut self) -> Result<()> {
        let (w, h) = self.size;
        let dxgi_device: IDXGIDevice = self.device.cast()?;
        // SAFETY: live device. The old chain must be *gone* before the new
        // one is created: a window carries one swapchain, and creating a
        // second while the first is alive fails with E_ACCESSDENIED — which
        // is what r8 did on the second PC, seven times out of seven, leaving
        // the picture black after every pop-in. No view on the old back
        // buffer is alive between presents; ClearState + Flush drops the
        // context's own references.
        unsafe {
            drop(self.swapchain.take());
            self.context.ClearState();
            self.context.Flush();
            let adapter = dxgi_device.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let desc = Self::desc(w, h);
            let fresh = factory
                .CreateSwapChainForHwnd(&self.device, self.hwnd, &desc, None, None)
                .context("CreateSwapChainForHwnd after a hosting change")?;
            let _ = factory.MakeWindowAssociation(
                self.hwnd,
                DXGI_MWA_NO_WINDOW_CHANGES | DXGI_MWA_NO_ALT_ENTER,
            );
            self.swapchain = Some(fresh);
        }
        info!("swapchain recreated after a hosting change");
        Ok(())
    }

    fn swapchain(&self) -> Result<&IDXGISwapChain1> {
        self.swapchain.as_ref().context("no swapchain: the last rebuild failed")
    }

    fn desc(w: u32, h: u32) -> DXGI_SWAP_CHAIN_DESC1 {
        DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            // The window is whatever size the app or the user makes it; the
            // back buffer stays at stream size and DWM scales it.
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        }
    }

    fn new(hwnd: HWND, w: u32, h: u32) -> Result<Self> {
        // Build the device on the primary monitor's adapter — the same GPU the
        // DXVA decoder runs on — so decode and present share one device with
        // no cross-adapter copy.
        let gpu = crate::d3d::device_for_monitor(crate::d3d::primary_monitor())?;
        let device = gpu.device;
        let context = gpu.context;

        let dxgi_device: IDXGIDevice = device.cast()?;
        // SAFETY: live DXGI device.
        let adapter = unsafe { dxgi_device.GetAdapter()? };
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        let desc = Self::desc(w, h);
        // SAFETY: valid device, hwnd and desc.
        let swapchain =
            unsafe { factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)? };
        // The window is pumped on another thread. DXGI would otherwise hook
        // its message procedure to watch for Alt+Enter and window changes,
        // and that hook is the classic cross-thread deadlock: the pump waits
        // on DXGI while DXGI waits on Present. Tell it to leave the window
        // alone; fullscreen switching is not something a receiver does.
        // SAFETY: factory and hwnd are live.
        unsafe {
            let _ = factory
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_WINDOW_CHANGES | DXGI_MWA_NO_ALT_ENTER);
        }

        let vp_device: ID3D11VideoDevice = device.cast()?;
        let vp_context: ID3D11VideoContext = context.cast()?;

        Ok(Self {
            device,
            context,
            swapchain: Some(swapchain),
            vp_device,
            vp_context,
            hwnd,
            size: (w, h),
        })
    }
}

/// Decode + present. Owns the D3D objects; the window is on its own thread.
fn video_thread(
    mut aus: mpsc::Receiver<AccessUnit>,
    stats: Arc<RecvStats>,
    present_latency: Arc<AtomicI64>,
    vcam: bool,
    quit: Arc<AtomicBool>,
    link: Arc<HostLink>,
    host_owner: Option<u64>,
) -> Result<()> {
    // SAFETY: COM MTA for MF + free-threaded D3D; balanced on return.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let _mf = probe::MediaFoundation::start()?;

    // Wait for the first AU so we can size the window to the stream. The
    // core can stop us meanwhile (a stop before any frame), so poll rather
    // than block: a blocked thread here is the B8 hang.
    let first = loop {
        if quit.load(Ordering::Acquire) {
            return Ok(());
        }
        match aus.try_recv() {
            Ok(au) => break au,
            Err(mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(2))
            }
            Err(_) => anyhow::bail!("connection closed before any frame"),
        }
    };
    let codec = first.codec;
    let (w, h) = crate::decode::probe_dimensions(codec, &first.data).unwrap_or((2560, 1440));
    info!(w, h, codec = codec.label(), "stream dimensions");

    let win = host::WindowThread::start(w, h, host_owner, link.clone(), quit.clone())?;
    let mut surface = Surface::new(win.hwnd, w, h)?;
    let mut surface_gen = link.surface_gen();
    let mut decoder = MfDecoder::new(&surface.device, codec, w, h)?;
    info!(decoder = %decoder.name, "decoder up");
    println!(
        "{}",
        serde_json::json!({
            "event": "render_up",
            "decoder": decoder.name,
            "codec": codec,
            "width": w,
            "height": h,
            "hwnd": win.hwnd.0 as isize as u64,
            "host": win.mode_label(),
            "excluded_from_capture": win.excluded_from_capture,
        })
    );

    let mut vp = VideoPresent::new(&surface, w, h)?;

    // Refuse below Windows 11 22H2 *before* touching the API. mfsensorgroup.dll
    // is delay-loaded (see build.rs), so a missing MFCreateVirtualCamera raises
    // a structured exception on first call rather than returning an error —
    // this check, not the `Err` arm below, is what keeps Windows 10 safe.
    let vcam = vcam && {
        let ok = relay_vdevice::detect::frameserver_supported();
        if !ok {
            let build = relay_vdevice::detect::windows_build();
            warn!(?build, "MFCreateVirtualCamera not present; continuing without a camera");
            println!(
                "{}",
                serde_json::json!({
                    "event": "vcam_error",
                    "message": format!(
                        "this PC has no virtual camera API (Windows build {}); \
                         the share still plays in Relay",
                        build.map(|b| b.to_string()).unwrap_or_else(|| "unknown".into()),
                    ),
                })
            );
        }
        ok
    };

    // Virtual camera (opt-in): best-effort — a missing registration or an
    // unsupported build reports once and the window carries on alone.
    let mut vcam_sink = if vcam {
        match crate::vcam_sink::VcamSink::start(w, h, 60) {
            Ok(sink) => {
                println!("{}", serde_json::json!({ "event": "vcam_up", "width": w, "height": h }));
                Some(sink)
            }
            Err(e) => {
                warn!(error = %e, "virtual camera unavailable");
                println!(
                    "{}",
                    serde_json::json!({ "event": "vcam_error", "message": e.to_string() })
                );
                None
            }
        }
    } else {
        None
    };

    let mut pending: Option<AccessUnit> = Some(first);
    // Nothing is decoded until the first keyframe: see the gate in the loop.
    let mut seen_keyframe = false;
    let mut skipped_pre_keyframe: u64 = 0;
    let mut last_au_at = std::time::Instant::now();
    // Test hook (B3): `RELAY_TEST_FAIL_RENDER=<secs>` fails this thread that
    // many seconds after the first keyframe, the way a lost GPU would -- the
    // only clean way to prove a mid-share fatal reaches the sender.
    let fail_after = std::env::var("RELAY_TEST_FAIL_RENDER")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|s| *s >= 0.0)
        .map(std::time::Duration::from_secs_f64);
    if let Some(d) = fail_after {
        warn!(after = ?d, "TEST: the render thread will fail after the first keyframe");
    }
    let mut decoding_since: Option<std::time::Instant> = None;

    let result: Result<()> = 'outer: loop {
        if let (Some(d), Some(t)) = (fail_after, decoding_since) {
            if t.elapsed() >= d {
                break Err(anyhow::anyhow!("TEST: render failure injected (RELAY_TEST_FAIL_RENDER)"));
            }
        }
        // The window thread asks us to stop (Esc, close, the owner window
        // going away) and the transport does too (stop, connection closed).
        if quit.load(Ordering::Acquire) {
            break Ok(());
        }
        let gen = link.surface_gen();
        if gen != surface_gen {
            surface_gen = gen;
            if let Err(e) = surface.recreate_swapchain() {
                // Retry next frame rather than present into nothing.
                warn!(error = %e, "could not recreate the swapchain; retrying");
                surface_gen = gen.wrapping_sub(1);
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
        }

        let au = match pending.take() {
            Some(a) => a,
            None => match aus.try_recv() {
                Ok(a) => a,
                Err(mpsc::error::TryRecvError::Empty) => {
                    // The sender stopping does NOT close this channel.
                    //
                    // `au_tx` is held for the lifetime of the receiver task, so
                    // `Err(Disconnected)` below effectively never fires: when a
                    // share ended, this loop kept spinning on `Empty` and the
                    // window sat on its last decoded frame forever. A user
                    // watching that reported it as the receiver "freezing and
                    // staying frozen" — the picture was simply the final frame
                    // of a share that had already finished, with nothing on
                    // screen saying so.
                    //
                    // So treat a long gap as the end of the stream. It is also
                    // the right answer when the network drops: ICE takes
                    // seconds to notice, and a still picture with no
                    // explanation is the worst thing to show meanwhile.
                    if last_au_at.elapsed() >= AU_IDLE_TIMEOUT {
                        info!(
                            after = ?last_au_at.elapsed(),
                            presented = stats.video_presented.load(Ordering::Relaxed),
                            "no access units; treating the share as ended"
                        );
                        break 'outer Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                Err(_) => break Ok(()),
            },
        };
        last_au_at = std::time::Instant::now();
        // NB: `video_aus` is incremented by the depay loop in
        // `transport::receiver` as each access unit is assembled. Counting it
        // again here double-counted every frame and made `aus` exactly 2x
        // `presented`, which read as though half of everything was being
        // dropped. Nothing was being dropped.

        // Wait for a keyframe before decoding anything.
        //
        // Joining mid-GOP means the first access units reference frames we
        // never had, so the decoder predicts from nothing and paints garbage.
        // A user watching described it as "smeared paint" for the first moment
        // of a share. Showing nothing until there is something real to show is
        // the honest behaviour, and the sender sends an IDR on connect anyway,
        // so the wait is short.
        if !seen_keyframe {
            if au.codec.is_keyframe(&au.data) {
                seen_keyframe = true;
                decoding_since = Some(std::time::Instant::now());
                info!(
                    skipped = skipped_pre_keyframe,
                    codec = au.codec.label(),
                    "first keyframe; decoding starts here"
                );
            } else {
                skipped_pre_keyframe += 1;
                continue;
            }
        }

        let frames = match decoder.decode(&au.data, au.pts_or_zero()) {
            Ok(f) => f,
            Err(e) => break Err(e),
        };
        for frame in frames {
            // Diagnostics for B10, the freeze that logs no stall: `presented`
            // kept climbing at 30/s while the user watched a still picture, so
            // Present() is being called on something that is not changing.
            // These two say which half is stuck. If `pts` stops advancing the
            // decoder is handing back the same picture; if `pts` advances but
            // `slice` never changes, we are being given one surface of the DXVA
            // array repeatedly and presenting whatever is in it.
            stats.last_pts_100ns.store(frame.pts_100ns, Ordering::Relaxed);
            stats.last_subresource.store(frame.subresource as u64, Ordering::Relaxed);
            if let Err(e) = vp.present(&surface, &frame) {
                // A Present that fails because the window is already gone is
                // the window thread stopping us, not a fault.
                if quit.load(Ordering::Acquire) {
                    break 'outer Ok(());
                }
                break 'outer Err(e);
            }
            stats.video_presented.fetch_add(1, Ordering::Relaxed);
            if let Some(sink) = vcam_sink.as_mut() {
                if let Err(e) = sink.push(&surface.device, &surface.context, &frame) {
                    warn!(error = %e, "virtual camera sink stopped");
                    vcam_sink = None;
                }
            }
            // The first second of frames after a connect carries decoder
            // start-up and the wait for the first keyframe: the two-PC matrix
            // saw 100-290 ms there, then single digits. Reporting it put a
            // scary number on the health card for the first sample of every
            // share, so latency is reported from the second second on.
            const WARMUP_FRAMES: u64 = 60;
            if let Some(cap_ns) = au
                .capture_local_ns
                .filter(|_| stats.video_presented.load(Ordering::Relaxed) > WARMUP_FRAMES)
            {
                present_latency.store((signal_now_ns() - cap_ns) / 1_000, Ordering::Relaxed);
            }
        }
    };

    // Release everything that targets the window *before* asking the window
    // thread to destroy it: a swapchain must not outlive its HWND, and DXGI
    // may need the window's thread to answer a message during release, so
    // that thread has to still be pumping here.
    drop(vcam_sink);
    drop(vp);
    drop(decoder);
    drop(surface);
    win.shutdown();

    // SAFETY: balances CoInitializeEx.
    unsafe { CoUninitialize() };
    result
}

/// Owns the video processor and the current swapchain render target.
struct VideoPresent {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

impl VideoPresent {
    fn new(win: &Surface, w: u32, h: u32) -> Result<Self> {
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputWidth: w,
            InputHeight: h,
            OutputWidth: w,
            OutputHeight: h,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            ..Default::default()
        };
        // SAFETY: object creation with a valid descriptor.
        let (enumerator, processor) = unsafe {
            let e = win.vp_device.CreateVideoProcessorEnumerator(&desc)?;
            let p = win.vp_device.CreateVideoProcessor(&e, 0)?;
            (e, p)
        };
        Ok(Self { enumerator, processor })
    }

    fn present(&mut self, win: &Surface, frame: &crate::decode::mf::DecodedFrame) -> Result<()> {
        // SAFETY: create views on the decoded texture and the back buffer,
        // blt (NV12→BGRA + scale), present. Views drop at scope end.
        unsafe {
            let swapchain = win.swapchain()?;
            let back: ID3D11Texture2D = swapchain.GetBuffer(0)?;

            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: frame.subresource },
                },
            };
            let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
            win.vp_device.CreateVideoProcessorInputView(
                &frame.texture,
                &self.enumerator,
                &in_desc,
                Some(&mut in_view),
            )?;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
            win.vp_device.CreateVideoProcessorOutputView(
                &back,
                &self.enumerator,
                &out_desc,
                Some(&mut out_view),
            )?;
            // H.264 codes 1080 rows as 1088 and crops; the decoded texture can
            // be the coded size, so blit only the picture, never the padding.
            let src =
                RECT { left: 0, top: 0, right: frame.width as i32, bottom: frame.height as i32 };
            win.vp_context.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&src));
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(in_view),
                ..Default::default()
            };
            let res = win.vp_context.VideoProcessorBlt(
                &self.processor,
                out_view.as_ref().unwrap(),
                0,
                std::slice::from_ref(&stream),
            );
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            res?;
            let _ = win.context;
            // Present with vsync off for lowest latency.
            swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }
}

/// `relay-share host-stub [--host <hwnd>]`: the receiver's window, events and
/// host commands with a moving colour wash in place of a decoded stream.
///
/// Exists because the real thing cannot be tried on one PC — a receiver on
/// the sending PC captures itself (B9) — and the hosting mechanics (owner
/// changes, affinity, z-order, the app positioning the window) are the part
/// most likely to go wrong. Nothing here captures anything.
pub async fn run_stub(host: Option<u64>) -> Result<()> {
    println!(
        "{}",
        serde_json::json!({ "event": "waiting", "name": "stub", "port": 0, "code": "000000" })
    );
    println!("{}", serde_json::json!({ "event": "paired", "sender": "host-stub" }));
    println!("{}", serde_json::json!({ "event": "codec", "codec": "h264" }));

    let quit = Arc::new(AtomicBool::new(false));
    let link = HostLink::new();
    let presented = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (q2, l2, p2) = (quit.clone(), link.clone(), presented.clone());
    let video_join = std::thread::Builder::new().name("relay-render".into()).spawn(move || {
        if let Err(e) = stub_thread(q2, l2, p2, host) {
            warn!(error = %e, "stub render thread stopped");
            println!(
                "{}",
                serde_json::json!({ "event": "error", "where": "render", "message": e.to_string() })
            );
        }
    })?;

    let mut stdin_lines = {
        use tokio::io::AsyncBufReadExt;
        tokio::io::BufReader::new(tokio::io::stdin()).lines()
    };
    let spawned_by_core = std::env::var("RELAY_SPAWNED").is_ok();
    let mut stdin_open = true;
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if video_join.is_finished() { break; }
                let p = presented.load(Ordering::Relaxed);
                println!("{}", serde_json::json!({
                    "event": "stats", "aus": p, "presented": p,
                    "audio_packets": 0, "mic_packets": 0, "capture_to_present_ms": 0.0,
                }));
                info!(presented = p, "stub presenting");
            }
            line = stdin_lines.next_line(), if stdin_open => {
                match line {
                    Ok(Some(l)) => match command::parse_line(&l) {
                        Some(EngineCmd::Stop) => break,
                        Some(EngineCmd::Host { mode, owner }) => link.post(mode, owner),
                        _ => {}
                    },
                    _ if spawned_by_core => break,
                    _ => stdin_open = false,
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    quit.store(true, Ordering::Release);
    join_bounded(video_join, "stub render");
    println!("{}", serde_json::json!({ "event": "stopped" }));
    Ok(())
}

fn stub_thread(
    quit: Arc<AtomicBool>,
    link: Arc<HostLink>,
    presented: Arc<std::sync::atomic::AtomicU64>,
    host: Option<u64>,
) -> Result<()> {
    // SAFETY: balanced below.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let (w, h) = (1920u32, 1080u32);
    let win = host::WindowThread::start(w, h, host, link.clone(), quit.clone())?;
    let mut surface = Surface::new(win.hwnd, w, h)?;
    let mut surface_gen = link.surface_gen();
    // `ClearView` (a rect-bounded clear) is on the 11.1 context.
    let ctx1: ID3D11DeviceContext1 = surface.context.cast()?;
    println!(
        "{}",
        serde_json::json!({
            "event": "render_up", "decoder": "stub", "codec": "h264",
            "width": w, "height": h,
            "hwnd": win.hwnd.0 as isize as u64, "host": win.mode_label(),
            "excluded_from_capture": win.excluded_from_capture,
        })
    );
    let started = std::time::Instant::now();
    while !quit.load(Ordering::Acquire) {
        let gen = link.surface_gen();
        if gen != surface_gen {
            surface_gen = gen;
            if let Err(e) = surface.recreate_swapchain() {
                warn!(error = %e, "could not recreate the swapchain; retrying");
                surface_gen = gen.wrapping_sub(1);
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
        }
        // A slow hue sweep with a bright bar marching across it: a stalled
        // picture is unmistakable, and so is a torn or stretched one.
        let t = started.elapsed().as_secs_f32();
        let (r, g, b) = hue(t * 0.1);
        // SAFETY: back buffer of our own swapchain; the view drops at scope end.
        let presented_ok = unsafe {
            let swapchain = surface.swapchain()?;
            let back: ID3D11Texture2D = swapchain.GetBuffer(0)?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            surface.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            let rtv = rtv.context("render target view")?;
            surface.context.ClearRenderTargetView(&rtv, &[r * 0.25, g * 0.25, b * 0.25, 1.0]);
            let x = ((t * 0.5).fract() * w as f32) as i32;
            let bar = RECT { left: x, top: 0, right: (x + 24).min(w as i32), bottom: h as i32 };
            ctx1.ClearView(&rtv, &[r, g, b, 1.0], Some(std::slice::from_ref(&bar)));
            let band = RECT { left: 0, top: 0, right: w as i32, bottom: 12 };
            ctx1.ClearView(&rtv, &[0.79, 0.66, 0.42, 1.0], Some(std::slice::from_ref(&band)));
            swapchain.Present(1, DXGI_PRESENT(0)).ok().is_ok()
        };
        if !presented_ok && quit.load(Ordering::Acquire) {
            break;
        }
        presented.fetch_add(1, Ordering::Relaxed);
    }
    drop(ctx1);
    drop(surface);
    win.shutdown();
    // SAFETY: balances CoInitializeEx.
    unsafe { CoUninitialize() };
    Ok(())
}

/// A saturated colour at `t` turns round the hue circle.
fn hue(t: f32) -> (f32, f32, f32) {
    let x = t.fract() * 6.0;
    let f = x.fract();
    match x as u32 {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    }
}
