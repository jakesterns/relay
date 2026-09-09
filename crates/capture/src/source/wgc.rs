//! Windows.Graphics.Capture of one monitor.
//!
//! Free-threaded `Direct3D11CaptureFramePool` with 2 buffers; the FrameArrived
//! callback forwards the frame (texture + QPC timestamp) through a bounded
//! channel. When the consumer is busy the callback closes the frame instead —
//! dropping to target fps costs nothing and copies nothing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, warn};
use windows::core::{Interface, Result as WinResult};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::{CapturedFrame, FrameSource};
use crate::d3d::Gpu;

pub struct WgcCapture {
    rx: Receiver<CapturedFrame>,
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    size: (u32, u32),
    dropped: Arc<AtomicU64>,
}

impl WgcCapture {
    /// Start capturing `hmonitor` on `gpu`'s device.
    pub fn monitor(gpu: &Gpu, hmonitor: HMONITOR, cursor: bool) -> Result<Self> {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        // SAFETY: hmonitor is a live monitor handle.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(hmonitor) }
            .context("GraphicsCaptureItem::CreateForMonitor")?;

        let dxgi: IDXGIDevice = gpu.device.cast()?;
        // SAFETY: dxgi is a valid DXGI device.
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?;
        let d3d_device: IDirect3DDevice = inspectable.cast()?;

        let item_size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            item_size,
        )
        .context("CreateFreeThreaded frame pool")?;
        let session = pool.CreateCaptureSession(&item)?;
        session.SetIsCursorCaptureEnabled(cursor)?;
        // Win11 can hide the yellow capture border; ignore failure elsewhere.
        if let Err(e) = session.SetIsBorderRequired(false) {
            debug!(error = %e, "capture border suppression unavailable");
        }

        let (tx, rx) = sync_channel::<CapturedFrame>(2);
        let dropped = Arc::new(AtomicU64::new(0));
        pool.FrameArrived(&TypedEventHandler::new({
            let tx: SyncSender<CapturedFrame> = tx;
            let dropped = dropped.clone();
            move |pool: windows::core::Ref<'_, Direct3D11CaptureFramePool>, _| -> WinResult<()> {
                let Some(pool) = pool.as_ref() else { return Ok(()) };
                while let Ok(frame) = pool.TryGetNextFrame() {
                    let surface = frame.Surface()?;
                    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
                    // SAFETY: the surface wraps a D3D11 texture on our device.
                    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
                    let size = frame.ContentSize()?;
                    let captured = CapturedFrame {
                        texture,
                        width: size.Width as u32,
                        height: size.Height as u32,
                        qpc_100ns: frame.SystemRelativeTime()?.Duration,
                        wgc_frame: Some(frame),
                    };
                    match tx.try_send(captured) {
                        Ok(()) => {}
                        Err(TrySendError::Full(f)) => {
                            // Consumer busy: close the frame (drop), no copy.
                            dropped.fetch_add(1, Ordering::Relaxed);
                            drop(f);
                        }
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
                Ok(())
            }
        }))?;

        session.StartCapture().context("StartCapture")?;
        Ok(Self {
            rx,
            session,
            pool,
            size: (item_size.Width as u32, item_size.Height as u32),
            dropped,
        })
    }
}

impl FrameSource for WgcCapture {
    fn next(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        match self.rx.recv_timeout(timeout) {
            Ok(f) => Ok(Some(f)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("capture callback stopped")
            }
        }
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for WgcCapture {
    fn drop(&mut self) {
        if let Err(e) = self.session.Close() {
            warn!(error = %e, "closing capture session");
        }
        if let Err(e) = self.pool.Close() {
            warn!(error = %e, "closing frame pool");
        }
    }
}

// SAFETY: the session/pool are free-threaded WinRT objects; WgcCapture is
// only driven from one thread at a time.
unsafe impl Send for WgcCapture {}
