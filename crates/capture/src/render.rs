//! Receiver presentation: a native D3D11 swapchain window fed by the MF
//! DXVA decoder for the negotiated codec (HEVC or H.264). Decoded NV12 stays on the GPU; the D3D11 video
//! processor converts it straight into the swapchain back buffer. Audio plays
//! out through WASAPI shared mode.
//!
//! The window owns its thread and message pump; access units arrive over a
//! channel from the transport. `capture→present` latency (from the in-band
//! SEI) is reported every 500 ms.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};
use windows::core::{w, Interface};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::decode::mf::MfDecoder;
use crate::transport::receiver::{AccessUnit, RecvStats};
use crate::{probe, signal_now_ns};

/// Receiver output options beyond the window itself.
#[derive(Debug, Default, Clone)]
pub struct RenderOpts {
    /// Start "Relay Camera" and mirror decoded frames into its ring.
    pub vcam: bool,
    /// Render decoded audio to this endpoint id instead of the default
    /// device (the interim virtual-mic route: a VB-Cable / VoiceMeeter
    /// input endpoint).
    pub mic_route: Option<String>,
}

/// Hands a fatal error to the signalling task, which tells the sender and
/// acknowledges on the oneshot once the message is written.
pub type AbortTx = mpsc::Sender<(String, tokio::sync::oneshot::Sender<()>)>;

/// Entry point used by the transport when not headless.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    aus: mpsc::Receiver<AccessUnit>,
    opus: mpsc::Receiver<Vec<u8>>,
    mic: mpsc::Receiver<Vec<u8>>,
    stats: Arc<RecvStats>,
    mut closed: mpsc::Receiver<()>,
    pc: impl webrtc::peer_connection::PeerConnection,
    opts: RenderOpts,
    abort: AbortTx,
) -> Result<()> {
    // Audio playback thread (best-effort; a decode failure must not kill video).
    let (audio_stop_tx, audio_stop_rx) = std::sync::mpsc::channel::<()>();
    let mic_route = opts.mic_route.clone();
    let audio_join =
        std::thread::Builder::new().name("relay-audio-playback".into()).spawn(move || {
            if let Err(e) = crate::playback::run(opus, mic, audio_stop_rx, mic_route) {
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
    let video_join = std::thread::Builder::new().name("relay-render".into()).spawn(move || {
        if let Err(e) = video_thread(aus, stats2, pl, vcam) {
            warn!(error = %e, "render thread stopped");
            *failure2.lock().unwrap() = Some(e.to_string());
            println!(
                "{}",
                serde_json::json!({ "event": "error", "where": "render", "message": e.to_string() })
            );
        }
    })?;

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
                println!("{}", serde_json::json!({
                    "event": "stats",
                    "aus": aus,
                    "presented": presented,
                    "audio_packets": audio,
                    "mic_packets": stats.mic_packets.load(Ordering::Relaxed),
                    "capture_to_present_ms": latency_ms,
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
                let stalled_aus = aus == last_aus;
                let stalled_present = presented == last_presented;
                // Only warn once frames have started: two false alarms fired
                // at connect before the video track existed, which is noise in
                // the one log someone reads when things go wrong.
                if presented > 0 && (stalled_aus || stalled_present) {
                    warn!(
                        aus, presented, audio, latency_ms, pts, slice,
                        arriving = !stalled_aus, presenting = !stalled_present,
                        "receiver stalled"
                    );
                } else if presented > 0 {
                    info!(aus, presented, audio, latency_ms, pts, slice, "receiving");
                }
                last_aus = aus;
                last_presented = presented;
            }
            _ = closed.recv() => { info!("connection closed"); break; }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

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
    let _ = pc.close().await;

    // Bounded joins (B8). The render thread blocks in `aus.blocking_recv()`,
    // and the sending half of that channel lives in `receiver.rs`'s on_track
    // handler, not here — so if closing the peer connection does not drop it,
    // `blocking_recv` never returns and an unconditional join hangs forever.
    // That is exactly what was seen on a real machine: 37 s after "connection
    // closed", relay-share was still resident holding an open render window.
    //
    // This is a mitigation, not the cure: the cure is for the AU sender not to
    // outlive `pc.close()`. Waiting is pure tidiness by this point — the
    // failure reason was taken above, the peer connection is closed, and the
    // process is on its way out — so a thread that will not wake is not worth
    // hanging the exit for.
    join_bounded(video_join, "render");
    join_bounded(audio_join, "audio playback");
    Ok(())
}

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

struct Window {
    hwnd: HWND,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    vp_device: ID3D11VideoDevice,
    vp_context: ID3D11VideoContext,
}

fn video_thread(
    mut aus: mpsc::Receiver<AccessUnit>,
    stats: Arc<RecvStats>,
    present_latency: Arc<AtomicI64>,
    vcam: bool,
) -> Result<()> {
    // SAFETY: COM MTA for MF + free-threaded D3D; balanced on return.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let _mf = probe::MediaFoundation::start()?;

    // Block for the first AU so we can size the window to the stream.
    let first = aus.blocking_recv().context("connection closed before any frame")?;
    let codec = first.codec;
    let (w, h) = crate::decode::probe_dimensions(codec, &first.data).unwrap_or((2560, 1440));
    info!(w, h, codec = codec.label(), "stream dimensions");

    let win = Window::create(w, h)?;
    let mut decoder = MfDecoder::new(&win.device, codec, w, h)?;
    info!(decoder = %decoder.name, "decoder up");
    println!(
        "{}",
        serde_json::json!({
            "event": "render_up",
            "decoder": decoder.name,
            "codec": codec,
            "width": w,
            "height": h,
        })
    );

    let mut vp = VideoPresent::new(&win, w, h)?;

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
                         the share still plays in its own window",
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
    let mut msg = MSG::default();
    // Nothing is decoded until the first keyframe: see the gate in the loop.
    let mut seen_keyframe = false;
    let mut skipped_pre_keyframe: u64 = 0;

    'outer: loop {
        // Pump window messages; quit on close.
        // SAFETY: standard non-blocking message pump on our window thread.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'outer;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        let au = match pending.take() {
            Some(a) => a,
            None => match aus.try_recv() {
                Ok(a) => a,
                Err(mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                Err(_) => break,
            },
        };
        stats.video_aus.fetch_add(1, Ordering::Relaxed);

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

        for frame in decoder.decode(&au.data, au.pts_or_zero())? {
            // Diagnostics for B10, the freeze that logs no stall: `presented`
            // kept climbing at 30/s while the user watched a still picture, so
            // Present() is being called on something that is not changing.
            // These two say which half is stuck. If `pts` stops advancing the
            // decoder is handing back the same picture; if `pts` advances but
            // `slice` never changes, we are being given one surface of the DXVA
            // array repeatedly and presenting whatever is in it.
            stats.last_pts_100ns.store(frame.pts_100ns, Ordering::Relaxed);
            stats.last_subresource.store(frame.subresource as u64, Ordering::Relaxed);
            vp.present(&win, &frame)?;
            stats.video_presented.fetch_add(1, Ordering::Relaxed);
            if let Some(sink) = vcam_sink.as_mut() {
                if let Err(e) = sink.push(&win.device, &win.context, &frame) {
                    warn!(error = %e, "virtual camera sink stopped");
                    vcam_sink = None;
                }
            }
            if let Some(cap_ns) = au.capture_local_ns {
                present_latency.store((signal_now_ns() - cap_ns) / 1_000, Ordering::Relaxed);
            }
        }
    }

    // SAFETY: balances CoInitializeEx.
    unsafe { CoUninitialize() };
    Ok(())
}

impl Window {
    fn create(w: u32, h: u32) -> Result<Self> {
        // SAFETY: standard window-class registration + creation on this thread.
        let hwnd = unsafe {
            let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: w!("RelayReceiver"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&class);
            // Fit the *monitor*, not the stream.
            //
            // This used to clamp to the stream size, so a 2560x1440 share on a
            // 1920x1080 display produced a window bigger than the screen: the
            // title bar and its close button sat off-screen, the taskbar was
            // covered, and the window looked like inescapable borderless
            // fullscreen. On a real receiver the only way out was killing the
            // process from another machine.
            //
            // Scale to fit the work area (which excludes the taskbar), keeping
            // the stream's aspect ratio, and never exceed it.
            let work =
                monitor_work_area().unwrap_or(RECT { left: 0, top: 0, right: 1280, bottom: 720 });
            let avail_w = ((work.right - work.left) as f64 * 0.9).max(320.0);
            let avail_h = ((work.bottom - work.top) as f64 * 0.9).max(180.0);
            let scale = (avail_w / w.max(1) as f64).min(avail_h / h.max(1) as f64).min(1.0);
            let win_w = ((w as f64 * scale).round() as i32).max(320);
            let win_h = ((h as f64 * scale).round() as i32).max(180);

            // Resizable on purpose: a fixed-size window that does not fit is
            // exactly the trap described above.
            let mut rect = RECT { left: 0, top: 0, right: win_w, bottom: win_h };
            let style = WS_OVERLAPPEDWINDOW;
            let _ = AdjustWindowRect(&mut rect, style, false);
            CreateWindowExW(
                Default::default(),
                w!("RelayReceiver"),
                w!("Relay — receiving"),
                style | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?
        };

        // Make this window invisible to screen capture (B9).
        //
        // Without it Relay will happily capture its own output: run a sender
        // and a receiver on one PC and the capture contains the window showing
        // the capture, which contains the window showing the capture. On this
        // project's dev machine that produced an unbounded feedback loop the
        // user described as "an infinite loop of whatever is on my screen, like
        // smearing a painting repeatedly", and it did not stop on its own.
        //
        // WDA_EXCLUDEFROMCAPTURE hides the window from WGC and Desktop
        // Duplication while leaving it fully visible on screen -- unlike
        // WDA_MONITOR, which blacks it out for the user too. Windows 10 2004+.
        // Best-effort: on an older build this fails and the window still works,
        // it is just capturable again, so a share of a share would smear as
        // before rather than the receiver refusing to run.
        //
        // SAFETY: hwnd is the window we just created.
        unsafe {
            if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
                tracing::warn!(
                    "could not exclude the receiver window from capture; \
                     sharing this PC's screen while receiving on it will feed back"
                );
            }
        }

        // Build the device on the primary monitor's adapter — the same GPU the
        // DXVA decoder runs on — so decode and present
        // share one device with no cross-adapter copy.
        let gpu = crate::d3d::device_for_monitor(crate::d3d::primary_monitor())?;
        let device = gpu.device;
        let context = gpu.context;

        let dxgi_device: IDXGIDevice = device.cast()?;
        // SAFETY: live DXGI device.
        let adapter = unsafe { dxgi_device.GetAdapter()? };
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        // SAFETY: valid device, hwnd and desc.
        let swapchain =
            unsafe { factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)? };

        let vp_device: ID3D11VideoDevice = device.cast()?;
        let vp_context: ID3D11VideoContext = context.cast()?;

        Ok(Self { hwnd, device, context, swapchain, vp_device, vp_context })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: destroying our own window.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Owns the video processor and the current swapchain render target.
struct VideoPresent {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

impl VideoPresent {
    fn new(win: &Window, w: u32, h: u32) -> Result<Self> {
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

    fn present(&mut self, win: &Window, frame: &crate::decode::mf::DecodedFrame) -> Result<()> {
        // SAFETY: create views on the decoded texture and the back buffer,
        // blt (NV12→BGRA + scale), present. Views drop at scope end.
        unsafe {
            let back: ID3D11Texture2D = win.swapchain.GetBuffer(0)?;

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
            win.swapchain.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }
}

/// Work area of the primary monitor: the desktop minus the taskbar. Sizing to
/// the full screen rect would put the window under the taskbar, which is half
/// of how the borderless-fullscreen trap looked to a user.
fn monitor_work_area() -> Option<RECT> {
    // SAFETY: SystemParametersInfo writing one RECT we own.
    unsafe {
        let mut r = RECT::default();
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
        Some(r)
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: standard window procedure.
    unsafe {
        match msg {
            WM_CLOSE | WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            // Escape closes it. A receive window that cannot be dismissed is a
            // trap, and the close button is the first thing to go out of reach
            // if the window is ever mis-sized again.
            WM_KEYDOWN
                if wp.0 as u32
                    == windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 as u32 =>
            {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
