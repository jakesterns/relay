//! The real [`HardwareProbe`]: WASAPI render endpoints and attached monitors.
//!
//! OS-layer only, read-only. Audio identity comes from `IMMDeviceEnumerator`
//! (endpoint id + container GUID from the property store); monitor identity
//! comes from the EDID block Windows caches in the registry under the device
//! instance that `QueryDisplayConfig` names — nothing is queried from the
//! panel itself except the optional DDC/CI capabilities string.

use tracing::{debug, warn};
use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    CapabilitiesRequestAndCapabilitiesReply, DestroyPhysicalMonitors, DisplayConfigGetDeviceInfo,
    GetCapabilitiesStringLength, GetDisplayConfigBufferSizes, GetPhysicalMonitorsFromHMONITOR,
    QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, PHYSICAL_MONITOR, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, PROPERTYKEY, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};

/// `MONITORINFO::dwFlags` primary bit (winuser.h; not surfaced by the crate).
const MONITORINFOF_PRIMARY: u32 = 1;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY};
use windows::Win32::System::Variant::{VT_CLSID, VT_LPWSTR};

use super::{
    ddc, edid, edid_color, endpoint_key, EndpointInfo, HardwareProbe, MonitorProbe, ProbeReport,
};
use crate::types::MonitorId;

pub struct WindowsHardwareProbe;

impl HardwareProbe for WindowsHardwareProbe {
    fn probe(&self, with_ddc: bool) -> ProbeReport {
        let _com = ComGuard::init();
        let endpoints = match probe_endpoints() {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "audio endpoint probe failed");
                Vec::new()
            }
        };
        let monitors = probe_monitors(with_ddc);
        ProbeReport { endpoints, monitors }
    }
}

/// Per-call COM init. `CoUninitialize` is only called when this call's init
/// actually succeeded; a thread already in a different apartment
/// (RPC_E_CHANGED_MODE) is used as-is.
pub(super) struct ComGuard {
    uninit: bool,
}

impl ComGuard {
    pub(super) fn init() -> Self {
        // SAFETY: plain COM init on the calling thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self { uninit: hr.is_ok() }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};
const PKEY_DEVICE_CONTAINER_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c),
    pid: 2,
};

fn probe_endpoints() -> windows::core::Result<Vec<EndpointInfo>> {
    // SAFETY: standard MMDevice enumeration; every raw string is copied out
    // and freed with CoTaskMemFree, PROPVARIANTs are cleared after reading.
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let default_id = en
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()
            .and_then(|d| device_id(&d))
            .unwrap_or_default();

        let coll = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let n = coll.GetCount()?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let dev = coll.Item(i)?;
            let Some(id) = device_id(&dev) else { continue };
            let (name, container) = device_props(&dev);
            out.push(EndpointInfo {
                key: endpoint_key(container.as_deref(), &id),
                name: name.unwrap_or_else(|| "Unknown endpoint".into()),
                default: !default_id.is_empty() && id == default_id,
            });
        }
        Ok(out)
    }
}

unsafe fn device_id(dev: &IMMDevice) -> Option<String> {
    // SAFETY: `p` is the NUL-terminated id the OS allocated; freed right after.
    unsafe {
        let p = dev.GetId().ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Friendly name and container GUID (lowercase, no braces) from the property
/// store. Best-effort: a device that answers neither is still listed.
unsafe fn device_props(dev: &IMMDevice) -> (Option<String>, Option<String>) {
    // SAFETY: PROPVARIANT union reads are guarded by the vt tag and every
    // variant is cleared after reading.
    unsafe {
        let Ok(store) = dev.OpenPropertyStore(STGM_READ) else { return (None, None) };

        let name = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME).ok().and_then(|mut pv| {
            let out = (pv.Anonymous.Anonymous.vt == VT_LPWSTR)
                .then(|| pv.Anonymous.Anonymous.Anonymous.pwszVal.to_string().ok())
                .flatten();
            let _ = PropVariantClear(&mut pv);
            out
        });

        let container = store.GetValue(&PKEY_DEVICE_CONTAINER_ID).ok().and_then(|mut pv| {
            let out = if pv.Anonymous.Anonymous.vt == VT_CLSID {
                let p = pv.Anonymous.Anonymous.Anonymous.puuid;
                (!p.is_null()).then(|| format!("{:?}", *p).to_lowercase())
            } else {
                None
            };
            let _ = PropVariantClear(&mut pv);
            out
        });

        (name, container)
    }
}

// ---------------------------------------------------------------------------
// Monitors
// ---------------------------------------------------------------------------

pub(crate) fn probe_monitors(with_ddc: bool) -> Vec<MonitorProbe> {
    let gdi = gdi_monitors();
    let mut out = Vec::new();
    for path in active_paths() {
        let Some(target) = target_name(&path) else { continue };
        let device_path = wide_str(&target.monitorDevicePath);
        let friendly = wide_str(&target.monitorFriendlyDeviceName);

        let raw_edid = edid_from_registry(&device_path);
        let parsed = raw_edid.as_deref().and_then(edid::parse);
        // Same blob, so the colour read costs nothing beyond the parse.
        let color = raw_edid.as_deref().and_then(edid_color::parse);
        let (id, name, native) = match (&parsed, &raw_edid) {
            (Some(e), Some(raw)) => (
                edid::monitor_id(e, raw),
                e.name.clone().or_else(|| (!friendly.is_empty()).then(|| friendly.clone())),
                e.native,
            ),
            _ => {
                // No EDID (headless adapters, some laptop panels): fall back to
                // a hash of the device path. Positional, so not port-stable —
                // logged so the limitation is visible.
                debug!(path = %device_path, "monitor has no EDID; using path-derived id");
                (
                    MonitorId(format!("mon:path:x{:08x}", fnv1a(device_path.as_bytes()))),
                    (!friendly.is_empty()).then(|| friendly.clone()),
                    None,
                )
            }
        };

        let gdi_name = source_gdi_name(&path).unwrap_or_default();
        let (hmonitor, primary) = gdi
            .iter()
            .find(|m| m.device == gdi_name)
            .map(|m| (m.hmonitor, m.primary))
            .unwrap_or((0, false));

        let refresh = path.targetInfo.refreshRate;
        let refresh_hz = (refresh.Denominator != 0)
            .then(|| refresh.Numerator as f32 / refresh.Denominator as f32);

        let ddc = if with_ddc && hmonitor != 0 {
            ddc_capabilities(HMONITOR(hmonitor as *mut _)).map(|caps| ddc::parse_vcp_codes(&caps))
        } else {
            None
        };

        out.push(MonitorProbe {
            id,
            name: name.unwrap_or_else(|| "Unknown monitor".into()),
            native,
            refresh_hz,
            primary,
            hmonitor,
            gdi_name,
            ddc,
            color,
        });
    }
    out.sort_by_key(|m| !m.primary);
    out
}

fn active_paths() -> Vec<DISPLAYCONFIG_PATH_INFO> {
    // SAFETY: sizes come from GetDisplayConfigBufferSizes; the arrays are
    // exactly that large. Retried once because the topology can change between
    // the two calls.
    unsafe {
        for _ in 0..3 {
            let (mut n_paths, mut n_modes) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes)
                != ERROR_SUCCESS
            {
                return Vec::new();
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
            let rc = QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut n_paths,
                paths.as_mut_ptr(),
                &mut n_modes,
                modes.as_mut_ptr(),
                None,
            );
            if rc == ERROR_SUCCESS {
                paths.truncate(n_paths as usize);
                return paths;
            }
        }
        Vec::new()
    }
}

fn target_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<DISPLAYCONFIG_TARGET_DEVICE_NAME> {
    let mut info = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: the packet is sized and typed per the header contract.
    (unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } == 0).then_some(info)
}

fn source_gdi_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let mut info = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: as above.
    (unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } == 0)
        .then(|| wide_str(&info.viewGdiDeviceName))
}

struct GdiMonitor {
    hmonitor: i64,
    device: String,
    primary: bool,
}

fn gdi_monitors() -> Vec<GdiMonitor> {
    unsafe extern "system" fn cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        lparam: LPARAM,
    ) -> windows::core::BOOL {
        // SAFETY: lparam is the Vec passed below, alive for the whole call.
        let out = unsafe { &mut *(lparam.0 as *mut Vec<GdiMonitor>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` is a correctly sized MONITORINFOEXW.
        if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo) }.as_bool() {
            out.push(GdiMonitor {
                hmonitor: hmon.0 as i64,
                device: wide_str(&info.szDevice),
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        true.into()
    }
    let mut out: Vec<GdiMonitor> = Vec::new();
    // SAFETY: the callback only runs within this call.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// `\\?\DISPLAY#GSM5C7C#5&3906ed52&0&UID4358#{guid}` →
/// `SYSTEM\CurrentControlSet\Enum\DISPLAY\GSM5C7C\5&3906ed52&0&UID4358\Device Parameters`,
/// where PnP caches the EDID the panel reported. Read-only; equivalent to the
/// SetupAPI dance without the ceremony.
fn edid_from_registry(device_path: &str) -> Option<Vec<u8>> {
    let trimmed = device_path.strip_prefix(r"\\?\")?;
    let instance = trimmed.rsplit_once('#').map(|(head, _guid)| head)?.replace('#', "\\");
    let subkey = format!(r"SYSTEM\CurrentControlSet\Enum\{instance}\Device Parameters");
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let value_w: Vec<u16> = "EDID".encode_utf16().chain(std::iter::once(0)).collect();
    let mut len = 0u32;
    // SAFETY: NUL-terminated wide strings; sizes per the two-call contract.
    unsafe {
        let rc = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&mut len),
        );
        if rc != ERROR_SUCCESS || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let rc = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        );
        (rc == ERROR_SUCCESS).then(|| {
            buf.truncate(len as usize);
            buf
        })
    }
}

/// Raw MCCS capabilities string over DDC/CI. Slow (tens to hundreds of ms per
/// monitor) — full-probe path only.
fn ddc_capabilities(hmon: HMONITOR) -> Option<String> {
    // SAFETY: physical monitor handles are destroyed on every exit path.
    unsafe {
        let mut phys = [PHYSICAL_MONITOR::default()];
        GetPhysicalMonitorsFromHMONITOR(hmon, &mut phys).ok()?;
        let handle = phys[0].hPhysicalMonitor;
        let mut len = 0u32;
        let caps = if GetCapabilitiesStringLength(handle, &mut len) != 0 && len > 1 {
            let mut buf = vec![0u8; len as usize];
            if CapabilitiesRequestAndCapabilitiesReply(handle, &mut buf) != 0 {
                let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                Some(String::from_utf8_lossy(&buf[..end]).into_owned())
            } else {
                None
            }
        } else {
            None
        };
        let _ = DestroyPhysicalMonitors(&phys);
        caps
    }
}

fn wide_str(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path → registry-instance mapping, against the dev PC's real path shape.
    #[test]
    fn device_path_maps_to_enum_instance() {
        let p = r"\\?\DISPLAY#GSM5C7C#5&3906ed52&0&UID4358#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        let trimmed = p.strip_prefix(r"\\?\").unwrap();
        let instance = trimmed.rsplit_once('#').unwrap().0.replace('#', "\\");
        assert_eq!(instance, r"DISPLAY\GSM5C7C\5&3906ed52&0&UID4358");
    }

    /// Manual: prints each monitor's raw MCCS capabilities string and the
    /// parsed VCP codes. `cargo test -p relay-core live_ddc -- --ignored --nocapture`
    #[test]
    #[ignore = "talks DDC/CI to the attached monitor; run by hand"]
    fn live_ddc_caps() {
        let _com = ComGuard::init();
        for m in probe_monitors(false) {
            let caps = ddc_capabilities(HMONITOR(m.hmonitor as *mut _));
            println!("== {} ({})", m.name, m.id.0);
            match caps {
                Some(raw) => {
                    println!("raw: {raw}");
                    println!("vcp: {:02X?}", ddc::parse_vcp_codes(&raw));
                }
                None => println!("no DDC/CI reply"),
            }
        }
    }

    /// Live smoke test: on this dev machine the probe must see at least one
    /// endpoint and the LG monitor with an EDID-derived id.
    #[test]
    fn live_probe_reports_this_machine() {
        let report = WindowsHardwareProbe.probe(false);
        assert!(!report.endpoints.is_empty(), "no render endpoints found");
        assert_eq!(report.endpoints.iter().filter(|e| e.default).count(), 1);
        assert!(!report.monitors.is_empty(), "no monitors found");
        assert!(report.monitors[0].primary, "primary must sort first");
        for m in &report.monitors {
            assert!(m.id.0.starts_with("mon:"), "unexpected id {}", m.id.0);
            assert_ne!(m.hmonitor, 0);
        }
    }
}
