//! D3D11 device creation bound to the adapter that owns the captured monitor,
//! so capture, colour conversion and encode all live on one GPU with no
//! cross-adapter copies.

use anyhow::{bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput,
};
use windows::Win32::Graphics::Gdi::HMONITOR;

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_name: String,
    /// Adapter LUID — used to bind MFT enumeration to this GPU.
    pub adapter_luid: windows::Win32::Foundation::LUID,
}

/// The DXGI adapter and output for `hmonitor`, plus the adapter's name.
fn adapter_for_monitor(hmonitor: HMONITOR) -> Result<(IDXGIAdapter1, IDXGIOutput, String)> {
    // SAFETY: standard DXGI enumeration; interfaces are ref-counted.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            ai += 1;
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                oi += 1;
                let desc = output.GetDesc()?;
                if desc.Monitor == hmonitor {
                    let ad = adapter.GetDesc1()?;
                    let name = String::from_utf16_lossy(
                        &ad.Description[..ad.Description.iter().position(|c| *c == 0).unwrap_or(0)],
                    );
                    return Ok((adapter, output, name));
                }
            }
        }
    }
    bail!("no DXGI output matches the requested monitor")
}

/// LUID and description of the adapter driving `hmonitor`, without creating
/// a device — enough to ask Media Foundation which encoders that GPU has.
pub fn adapter_luid_for_monitor(
    hmonitor: HMONITOR,
) -> Result<(windows::Win32::Foundation::LUID, String)> {
    let (adapter, _output, name) = adapter_for_monitor(hmonitor)?;
    // SAFETY: adapter is live.
    let luid = unsafe { adapter.GetDesc1()? }.AdapterLuid;
    Ok((luid, name))
}

/// The DXGI output for `hmonitor` (Desktop Duplication needs it).
pub fn output_for_monitor(hmonitor: HMONITOR) -> Result<IDXGIOutput> {
    let (_adapter, output, _name) = adapter_for_monitor(hmonitor)?;
    Ok(output)
}

/// D3D11 device on the monitor's adapter with BGRA + video support (the video
/// flag enables the D3D11 video processor used for BGRA→NV12 on the GPU).
pub fn device_for_monitor(hmonitor: HMONITOR) -> Result<Gpu> {
    let (adapter, _output, adapter_name) = adapter_for_monitor(hmonitor)?;
    // SAFETY: adapter is live.
    let adapter_luid = unsafe { adapter.GetDesc1()? }.AdapterLuid;
    let mut device = None;
    let mut context = None;
    // SAFETY: standard device creation; out params filled on success.
    unsafe {
        D3D11CreateDevice(
            &adapter.cast::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>()?,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .context("D3D11CreateDevice")?;
    let device = device.context("no device")?;
    let context = context.context("no context")?;
    // The WGC callback thread and the encoder both touch this device.
    let mt: windows::Win32::Graphics::Direct3D11::ID3D11Multithread = context.cast()?;
    // SAFETY: enabling the context's internal lock.
    unsafe {
        let _ = mt.SetMultithreadProtected(true);
    }
    Ok(Gpu { device, context, adapter_name, adapter_luid })
}

/// A monitor's size in pixels.
pub fn monitor_size(hmonitor: HMONITOR) -> Option<(u32, u32)> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    let mut mi =
        MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: a sized out-structure for a monitor handle.
    unsafe { GetMonitorInfoW(hmonitor, &mut mi) }.as_bool().then(|| {
        let r = mi.rcMonitor;
        ((r.right - r.left) as u32, (r.bottom - r.top) as u32)
    })
}

/// Primary monitor handle.
pub fn primary_monitor() -> HMONITOR {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
    // SAFETY: always returns a monitor with DEFAULTTOPRIMARY.
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

/// All monitors in enumeration order, primary first — the index the
/// `SourceTarget::Display`/`Region` commands refer to.
pub fn monitors() -> Vec<HMONITOR> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC};

    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        // SAFETY: `out` is the Vec passed below, valid for the whole call.
        unsafe { &mut *(out.0 as *mut Vec<HMONITOR>) }.push(m);
        true.into()
    }
    let mut list: Vec<HMONITOR> = Vec::new();
    // SAFETY: callback only runs during this call; `list` outlives it.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut list as *mut _ as isize));
    }
    let primary = primary_monitor();
    if let Some(pos) = list.iter().position(|m| *m == primary) {
        list.swap(0, pos);
    }
    list
}
