//! Receiver presentation: a native D3D11 swapchain window fed by the MF
//! hardware HEVC decoder. Decoded NV12 stays on the GPU; the D3D11 video
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

use crate::decode::mf::MfHevcDecoder;
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

/// Entry point used by the transport when not headless.
pub async fn run(
    aus: mpsc::Receiver<AccessUnit>,
    opus: mpsc::Receiver<Vec<u8>>,
    stats: Arc<RecvStats>,
    mut closed: mpsc::Receiver<()>,
    pc: impl webrtc::peer_connection::PeerConnection,
    opts: RenderOpts,
) -> Result<()> {
    // Audio playback thread (best-effort; a decode failure must not kill video).
    let (audio_stop_tx, audio_stop_rx) = std::sync::mpsc::channel::<()>();
    let mic_route = opts.mic_route.clone();
    let audio_join =
        std::thread::Builder::new().name("relay-audio-playback".into()).spawn(move || {
            if let Err(e) = crate::playback::run(opus, audio_stop_rx, mic_route) {
                warn!(error = %e, "audio playback stopped");
            }
        })?;

    // Video window + decode thread.
    let present_latency = Arc::new(AtomicI64::new(0));
    let pl = present_latency.clone();
    let stats2 = stats.clone();
    let vcam = opts.vcam;
    let video_join = std::thread::Builder::new().name("relay-render".into()).spawn(move || {
        if let Err(e) = video_thread(aus, stats2, pl, vcam) {
            warn!(error = %e, "render thread stopped");
            println!(
                "{}",
                serde_json::json!({ "event": "error", "where": "render", "message": e.to_string() })
            );
        }
    })?;

    // Report present latency until the window or the connection closes.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if video_join.is_finished() { break; }
                println!("{}", serde_json::json!({
                    "event": "stats",
                    "aus": stats.video_aus.load(Ordering::Relaxed),
                    "audio_packets": stats.audio_packets.load(Ordering::Relaxed),
                    "capture_to_present_ms": present_latency.load(Ordering::Relaxed) as f64 / 1e3,
                }));
            }
            _ = closed.recv() => { info!("connection closed"); break; }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    let _ = audio_stop_tx.send(());
    let _ = pc.close().await;
    let _ = video_join.join();
    let _ = audio_join.join();
    Ok(())
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
    let (w, h) = crate::decode::probe_dimensions(&first.data).unwrap_or((2560, 1440));
    info!(w, h, "stream dimensions");

    let win = Window::create(w, h)?;
    let mut decoder = MfHevcDecoder::new(&win.device, w, h)?;
    info!(decoder = %decoder.name, "decoder up");
    println!(
        "{}",
        serde_json::json!({ "event": "render_up", "decoder": decoder.name, "width": w, "height": h })
    );

    let mut vp = VideoPresent::new(&win, w, h)?;

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
        for frame in decoder.decode(&au.data, au.pts_or_zero())? {
            vp.present(&win, &frame)?;
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
            let mut rect = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            let style = WS_OVERLAPPEDWINDOW & !WS_THICKFRAME & !WS_MAXIMIZEBOX;
            let _ = AdjustWindowRect(&mut rect, style, false);
            CreateWindowExW(
                Default::default(),
                w!("RelayReceiver"),
                w!("Relay — receiving"),
                style | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (rect.right - rect.left).min(2560),
                (rect.bottom - rect.top).min(1440),
                None,
                None,
                Some(hinstance.into()),
                None,
            )?
        };

        // Build the device on the primary monitor's adapter — the same GPU the
        // hardware HEVC decoder MFT is registered on — so decode and present
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

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: standard window procedure.
    unsafe {
        match msg {
            WM_CLOSE | WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
