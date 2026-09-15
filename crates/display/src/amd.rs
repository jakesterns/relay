//! Per-display colour on AMD: saturation and hue via the AMD Display Library.
//!
//! This is the AMD half of the `DisplayIo` GPU-colour seam; [`nvapi`] is the
//! other. Same contract: loaded dynamically per operation, nothing resident
//! between applies, and a machine without the driver simply returns `None`
//! from [`Adl::load`] so the caller can fall back to the (vendor-neutral)
//! gamma-ramp path.
//!
//! [`nvapi`]: crate::nvapi
//!
//! # Why ADL and not the ADLX vtables
//!
//! AMD ships two user-mode interfaces to the same driver feature — the
//! "Custom Color" block in Radeon Software:
//!
//! - **ADLX** (`amdadlx64.dll`), the modern one, is a COM-like C++ interface.
//!   Its C binding is vtable-based: you reach `IADLXDisplayServices` ⇒
//!   `IADLXDisplay` ⇒ custom colour by indexing function-pointer tables whose
//!   layout is fixed by the SDK headers of the version you compiled against.
//!   A wrong or shifted slot does not fail — it calls *a different method*.
//! - **ADL** (`atiadlxx.dll`), the long-standing one, is a flat C API whose
//!   entry points are resolved by name with `GetProcAddress`.
//!
//! We have no AMD-driven display on the development machine (see
//! `docs/plans/M2-display.md`), so a mis-ordered vtable slot could not be
//! caught by running it. With the flat API, a name that is missing or
//! renamed fails loudly at load time and we report "unavailable" instead of
//! writing something nobody asked for. That is the same rule the VCP quirks
//! table follows: never issue a command you cannot prove the meaning of.
//!
//! ADL is still shipped by current drivers (`atiadlxx.dll` 7.25.10.1590 on
//! this machine, alongside ADLX 1.4.0.121) and `ADL2_Display_Color_Set` is
//! the exact call Radeon Software's own slider makes. If ADLX becomes the
//! only transport, it replaces this module's *inside* — the seam above it
//! (`DisplayIo::gpu_*`) does not move.
//!
//! x86-64 only, which is all Relay targets: with one calling convention on
//! that ABI the `extern "C"` declarations below cannot be mismatched.

use std::ffi::c_void;

use anyhow::{bail, Context, Result};
use tracing::debug;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GetProcessHeap, HeapAlloc, HeapFree, HEAP_ZERO_MEMORY};

const ADL_OK: i32 = 0;
const ADL_MAX_PATH: usize = 256;

/// `ADL_VENDOR_ID` (`adl_defines.h`) — decimal 1002, not the hex PCI id.
/// ADL enumerates *every* adapter the OS has, not only AMD's: on the
/// development machine it lists the RTX 3090 four times with `iVendorID` 10.
/// Every colour call is addressed by adapter index, so a missing vendor check
/// would mean sending AMD commands at an NVIDIA adapter.
const ADL_VENDOR_ID_AMD: i32 = 1002;

/// `ADL_DISPLAY_COLOR_*` selectors (`adl_defines.h`).
const ADL_DISPLAY_COLOR_SATURATION: i32 = 1 << 2;
const ADL_DISPLAY_COLOR_HUE: i32 = 1 << 3;

/// `ADL_DISPLAY_DISPLAYINFO_*` bits of `ADLDisplayInfo::iDisplayInfoValue`.
const DISPLAYINFO_CONNECTED: i32 = 0x0000_0001;
const DISPLAYINFO_MAPPED: i32 = 0x0000_0002;

type AdlContext = *mut c_void;

/// `AdapterInfo` from `adl_structures.h` (Windows layout: `iExist` present).
#[repr(C)]
#[derive(Clone, Copy)]
struct AdapterInfo {
    size: i32,
    adapter_index: i32,
    udid: [u8; ADL_MAX_PATH],
    bus_number: i32,
    device_number: i32,
    function_number: i32,
    vendor_id: i32,
    adapter_name: [u8; ADL_MAX_PATH],
    display_name: [u8; ADL_MAX_PATH],
    present: i32,
    exist: i32,
    driver_path: [u8; ADL_MAX_PATH],
    driver_path_ext: [u8; ADL_MAX_PATH],
    pnp_string: [u8; ADL_MAX_PATH],
    os_display_index: i32,
}

impl Default for AdapterInfo {
    fn default() -> Self {
        // SAFETY: every field is a plain integer or byte array; all-zero is a
        // valid value for each.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdlDisplayId {
    logical_index: i32,
    physical_index: i32,
    logical_adapter_index: i32,
    physical_adapter_index: i32,
}

/// `ADLDisplayInfo` from `adl_structures.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct AdlDisplayInfo {
    display_id: AdlDisplayId,
    controller_index: i32,
    display_name: [u8; ADL_MAX_PATH],
    manufacturer_name: [u8; ADL_MAX_PATH],
    display_type: i32,
    output_type: i32,
    connector: i32,
    info_mask: i32,
    info_value: i32,
}

type MallocCallback = unsafe extern "C" fn(i32) -> *mut c_void;

type FnMainControlCreate = unsafe extern "C" fn(MallocCallback, i32, *mut AdlContext) -> i32;
type FnMainControlDestroy = unsafe extern "C" fn(AdlContext) -> i32;
type FnNumberOfAdapters = unsafe extern "C" fn(AdlContext, *mut i32) -> i32;
type FnAdapterInfoGet = unsafe extern "C" fn(AdlContext, *mut AdapterInfo, i32) -> i32;
type FnDisplayInfoGet =
    unsafe extern "C" fn(AdlContext, i32, *mut i32, *mut *mut AdlDisplayInfo, i32) -> i32;
type FnColorGet = unsafe extern "C" fn(
    AdlContext,
    i32,
    i32,
    i32,
    *mut i32,
    *mut i32,
    *mut i32,
    *mut i32,
    *mut i32,
) -> i32;
type FnColorSet = unsafe extern "C" fn(AdlContext, i32, i32, i32, i32) -> i32;

/// ADL's allocator callback. Process heap so [`adl_free`] can release it
/// without tracking layouts.
unsafe extern "C" fn adl_malloc(size: i32) -> *mut c_void {
    if size <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: process heap is always valid; zeroed so a short write from ADL
    // cannot leave us reading uninitialised bytes.
    unsafe { HeapAlloc(GetProcessHeap().unwrap_or_default(), HEAP_ZERO_MEMORY, size as usize) }
}

/// Frees a block handed back by ADL (allocated through [`adl_malloc`]).
///
/// # Safety
/// `p` must be a pointer ADL produced via our callback, freed once.
unsafe fn adl_free(p: *mut c_void) {
    if !p.is_null() {
        // SAFETY: caller's contract — the block came from `adl_malloc`.
        let _ =
            unsafe { HeapFree(GetProcessHeap().unwrap_or_default(), Default::default(), Some(p)) };
    }
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// One AMD-driven display: the (adapter, display) index pair ADL addresses
/// colour by, plus the GDI name that keys it to the probe's monitors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdlDisplay {
    pub adapter_index: i32,
    pub display_index: i32,
    pub gdi_name: String,
    /// Marketing name of the adapter driving it, for logs.
    pub adapter_name: String,
}

/// A driver-reported control range. ADL exposes `default` separately from
/// `min`, and neutral is `default` — unlike NvAPI's DVC, where neutral *is*
/// the minimum. That difference is the whole reason the mapping below is not
/// the NVIDIA one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdlRange {
    pub current: i32,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    pub step: i32,
}

/// A loaded ADL session. Dropping destroys the context and unloads the DLL.
pub struct Adl {
    module: HMODULE,
    context: AdlContext,
    destroy: FnMainControlDestroy,
    number_of_adapters: FnNumberOfAdapters,
    adapter_info: FnAdapterInfoGet,
    display_info: FnDisplayInfoGet,
    color_get: FnColorGet,
    color_set: FnColorSet,
}

impl Adl {
    /// `None` when there is no AMD driver, or ADL refuses to start.
    pub fn load() -> Option<Self> {
        // SAFETY: standard dynamic load; the resolved pointers are only used
        // while the module stays loaded (held by Self).
        unsafe {
            let module = LoadLibraryW(PCWSTR(windows::core::w!("atiadlxx.dll").as_ptr())).ok()?;
            type RawFn = unsafe extern "system" fn() -> isize;
            let sym =
                |name: windows::core::PCSTR| -> Option<RawFn> { GetProcAddress(module, name) };
            // Every entry point is resolved *before* the context is created,
            // so a driver missing any of them costs one `FreeLibrary` and no
            // half-started session.
            let resolve = || -> Option<(
                FnMainControlCreate,
                FnMainControlDestroy,
                FnNumberOfAdapters,
                FnAdapterInfoGet,
                FnDisplayInfoGet,
                FnColorGet,
                FnColorSet,
            )> {
                Some((
                    std::mem::transmute::<RawFn, FnMainControlCreate>(sym(windows::core::s!(
                        "ADL2_Main_Control_Create"
                    ))?),
                    std::mem::transmute::<RawFn, FnMainControlDestroy>(sym(windows::core::s!(
                        "ADL2_Main_Control_Destroy"
                    ))?),
                    std::mem::transmute::<RawFn, FnNumberOfAdapters>(sym(windows::core::s!(
                        "ADL2_Adapter_NumberOfAdapters_Get"
                    ))?),
                    std::mem::transmute::<RawFn, FnAdapterInfoGet>(sym(windows::core::s!(
                        "ADL2_Adapter_AdapterInfo_Get"
                    ))?),
                    std::mem::transmute::<RawFn, FnDisplayInfoGet>(sym(windows::core::s!(
                        "ADL2_Display_DisplayInfo_Get"
                    ))?),
                    std::mem::transmute::<RawFn, FnColorGet>(sym(windows::core::s!(
                        "ADL2_Display_Color_Get"
                    ))?),
                    std::mem::transmute::<RawFn, FnColorSet>(sym(windows::core::s!(
                        "ADL2_Display_Color_Set"
                    ))?),
                ))
            };
            let Some((
                create,
                destroy,
                number_of_adapters,
                adapter_info,
                display_info,
                color_get,
                color_set,
            )) = resolve()
            else {
                debug!("atiadlxx.dll is missing an ADL2 entry point");
                let _ = FreeLibrary(module);
                return None;
            };

            let mut context: AdlContext = std::ptr::null_mut();
            // 1 = enumerate connected adapters only.
            let rc = create(adl_malloc, 1, &mut context);
            if rc != ADL_OK || context.is_null() {
                debug!(rc, "ADL2_Main_Control_Create failed");
                let _ = FreeLibrary(module);
                return None;
            }
            Some(Adl {
                module,
                context,
                destroy,
                number_of_adapters,
                adapter_info,
                display_info,
                color_get,
                color_set,
            })
        }
    }

    fn adapters(&self) -> Vec<AdapterInfo> {
        let mut count = 0i32;
        // SAFETY: out-param per the contract.
        let rc = unsafe { (self.number_of_adapters)(self.context, &mut count) };
        if rc != ADL_OK || count <= 0 {
            debug!(rc, count, "no ADL adapters");
            return Vec::new();
        }
        let mut infos = vec![AdapterInfo::default(); count as usize];
        let bytes = count * std::mem::size_of::<AdapterInfo>() as i32;
        // SAFETY: the buffer is exactly `count` structs and we say so.
        let rc = unsafe { (self.adapter_info)(self.context, infos.as_mut_ptr(), bytes) };
        if rc != ADL_OK {
            debug!(rc, "ADL2_Adapter_AdapterInfo_Get failed");
            return Vec::new();
        }
        infos
    }

    /// Every AMD-driven display that is connected *and* mapped into the
    /// desktop, with its GDI name. Unmapped and disconnected outputs are
    /// skipped: a profile only ever targets a monitor the user can see.
    pub fn displays(&self) -> Vec<AdlDisplay> {
        let mut out: Vec<AdlDisplay> = Vec::new();
        for a in self.adapters() {
            if a.present == 0 || a.vendor_id != ADL_VENDOR_ID_AMD {
                continue;
            }
            let gdi_name = c_str(&a.display_name);
            if gdi_name.is_empty() {
                continue;
            }
            let adapter_name = c_str(&a.adapter_name);
            let mut count = 0i32;
            let mut list: *mut AdlDisplayInfo = std::ptr::null_mut();
            // SAFETY: ADL allocates `list` through our callback; freed below.
            // 0 = do not force a detect (that can flash the panel).
            let rc = unsafe {
                (self.display_info)(self.context, a.adapter_index, &mut count, &mut list, 0)
            };
            if rc != ADL_OK || list.is_null() {
                debug!(rc, adapter = a.adapter_index, "ADL2_Display_DisplayInfo_Get failed");
                continue;
            }
            // SAFETY: ADL reports `count` entries in the block it allocated.
            let displays = unsafe { std::slice::from_raw_parts(list, count.max(0) as usize) };
            for d in displays {
                let usable = d.info_value & (DISPLAYINFO_CONNECTED | DISPLAYINFO_MAPPED)
                    == (DISPLAYINFO_CONNECTED | DISPLAYINFO_MAPPED);
                if !usable || d.display_id.logical_adapter_index != a.adapter_index {
                    continue;
                }
                if out.iter().any(|e| e.gdi_name.eq_ignore_ascii_case(&gdi_name)) {
                    continue;
                }
                out.push(AdlDisplay {
                    adapter_index: a.adapter_index,
                    display_index: d.display_id.logical_index,
                    gdi_name: gdi_name.clone(),
                    adapter_name: adapter_name.clone(),
                });
            }
            // SAFETY: the block came from our `adl_malloc`, freed once here.
            unsafe { adl_free(list as *mut c_void) };
        }
        out
    }

    /// One line per ADL adapter, for logs and the live probe. Cheap, and
    /// the fastest way to tell "no AMD driver" from "AMD driver, nothing
    /// plugged into it" when a user reports colour doing nothing.
    pub fn adapter_summary(&self) -> Vec<String> {
        self.adapters()
            .iter()
            .map(|a| {
                format!(
                    "adapter {} vendor={} present={} exist={} \"{}\" on \"{}\"",
                    a.adapter_index,
                    a.vendor_id,
                    a.present,
                    a.exist,
                    c_str(&a.adapter_name),
                    c_str(&a.display_name),
                )
            })
            .collect()
    }

    /// The AMD-driven display with this GDI name, if AMD drives it.
    pub fn display_by_gdi_name(&self, gdi_name: &str) -> Option<AdlDisplay> {
        self.displays().into_iter().find(|d| d.gdi_name.eq_ignore_ascii_case(gdi_name))
    }

    fn color(&self, d: &AdlDisplay, kind: i32) -> Result<AdlRange> {
        let (mut current, mut default, mut min, mut max, mut step) = (0, 0, 0, 0, 0);
        // SAFETY: five `int*` out-params per the contract.
        let rc = unsafe {
            (self.color_get)(
                self.context,
                d.adapter_index,
                d.display_index,
                kind,
                &mut current,
                &mut default,
                &mut min,
                &mut max,
                &mut step,
            )
        };
        if rc != ADL_OK {
            bail!("ADL2_Display_Color_Get(kind={kind}) failed with {rc} on {}", d.gdi_name);
        }
        Ok(AdlRange { current, default, min, max, step: step.max(1) })
    }

    fn set_color(&self, d: &AdlDisplay, kind: i32, value: i32) -> Result<()> {
        // SAFETY: plain call.
        let rc = unsafe {
            (self.color_set)(self.context, d.adapter_index, d.display_index, kind, value)
        };
        if rc != ADL_OK {
            bail!(
                "ADL2_Display_Color_Set(kind={kind}, {value}) failed with {rc} on {}",
                d.gdi_name
            );
        }
        Ok(())
    }

    pub fn get_saturation(&self, d: &AdlDisplay) -> Result<AdlRange> {
        self.color(d, ADL_DISPLAY_COLOR_SATURATION).context("reading AMD saturation")
    }

    pub fn set_saturation(&self, d: &AdlDisplay, value: i32) -> Result<()> {
        self.set_color(d, ADL_DISPLAY_COLOR_SATURATION, value)
    }

    pub fn get_hue(&self, d: &AdlDisplay) -> Result<AdlRange> {
        self.color(d, ADL_DISPLAY_COLOR_HUE).context("reading AMD hue")
    }

    pub fn set_hue(&self, d: &AdlDisplay, value: i32) -> Result<()> {
        self.set_color(d, ADL_DISPLAY_COLOR_HUE, value)
    }
}

impl Drop for Adl {
    fn drop(&mut self) {
        // SAFETY: balances load(); nothing in this process uses ADL after us.
        unsafe {
            if !self.context.is_null() {
                let _ = (self.destroy)(self.context);
            }
            let _ = FreeLibrary(self.module);
        }
    }
}

// SAFETY: the context is only ever used through `&self` methods, ADL is
// documented as thread-safe in its ADL2 (context-carrying) form, and the
// handle is not tied to the creating thread.
unsafe impl Send for Adl {}
unsafe impl Sync for Adl {}

// ---------------------------------------------------------------------------
// Pure unit mapping: profile units ⇄ ADL raw units.
//
// See the module docs in `nvapi` for the NVIDIA side. The two are *not* the
// same curve, because the two drivers do not expose the same control:
//
//   NvAPI DVC        range [min..max] (0..63), neutral == min
//   ADL saturation   range [min..max] (0..200), neutral == driver default (100)
//
// So NVIDIA can only add vibrance — the bottom half of the profile slider
// (0..50, "less colour than normal") has nowhere to go and clamps to
// neutral. AMD *can* desaturate, and the mapping below honours it: pinning
// the AMD half to neutral to match NVIDIA would throw away a control the
// hardware has, and the point of the vendor seam is that each GPU does the
// best it can with the same profile, not that both do the worst.
// ---------------------------------------------------------------------------

/// Round to the nearest representable value: inside `[min, max]` and on the
/// driver's `step` grid measured from `min`. ADL rejects off-grid values on
/// some controls, and silently snaps on others — neither is a good surprise.
fn snap(value: i32, r: &AdlRange) -> i32 {
    let step = r.step.max(1);
    let v = value.clamp(r.min, r.max);
    let snapped = r.min + ((v - r.min) as f64 / step as f64).round() as i32 * step;
    snapped.clamp(r.min, r.max)
}

/// Profile vibrance (0..=100, 50 = neutral) → raw ADL saturation.
///
/// Piecewise-linear with the knee at the driver's own default:
///
/// ```text
///   0 ──────────── 50 ──────────── 100      profile
///  min ─────── default ─────────── max      ADL
/// ```
///
/// Two straight segments rather than one line across `[min, max]`, because
/// the default is not necessarily the midpoint of the range, and 50 must mean
/// "leave the colour alone" exactly — a profile at 50 has to be a no-op or
/// every game would shift the desktop's colour a little.
pub fn vibrance_percent_to_saturation(percent: i32, r: &AdlRange) -> i32 {
    let p = percent.clamp(0, 100);
    if r.max <= r.min {
        return r.default;
    }
    let raw = if p >= 50 {
        r.default as f64 + (p - 50) as f64 / 50.0 * (r.max - r.default) as f64
    } else {
        r.default as f64 - (50 - p) as f64 / 50.0 * (r.default - r.min) as f64
    };
    snap(raw.round() as i32, r)
}

/// Inverse of [`vibrance_percent_to_saturation`], for reporting.
pub fn saturation_to_vibrance_percent(raw: i32, r: &AdlRange) -> i32 {
    if r.max <= r.min {
        return 50;
    }
    let raw = raw.clamp(r.min, r.max);
    let pct = if raw >= r.default {
        let span = (r.max - r.default) as f64;
        if span <= 0.0 {
            50.0
        } else {
            50.0 + (raw - r.default) as f64 / span * 50.0
        }
    } else {
        let span = (r.default - r.min) as f64;
        if span <= 0.0 {
            50.0
        } else {
            50.0 - (r.default - raw) as f64 / span * 50.0
        }
    };
    (pct.round() as i32).clamp(0, 100)
}

/// Profile hue (degrees, 0 = neutral, wraps) → raw ADL hue, plus whether the
/// driver's range forced a clamp.
///
/// Degrees stay degrees. NvAPI takes a full 0..359 rotation; ADL's hue is a
/// narrow signed trim (commonly ±30°). The profile angle is first folded into
/// `(-180, 180]` — +350° and −10° are the same rotation — and then *clamped*,
/// not rescaled. Rescaling would make "10°" mean one thing on NVIDIA and
/// another on AMD for the same profile; clamping keeps the unit honest and
/// tells the caller the request was trimmed, which surfaces in the UI's
/// "applied via" line instead of silently under-delivering.
pub fn hue_deg_to_adl(hue_deg: i32, r: &AdlRange) -> (i32, bool) {
    let wrapped = ((hue_deg % 360) + 540) % 360 - 180; // (-180, 180]
    let wrapped = if wrapped == -180 { 180 } else { wrapped };
    if r.max <= r.min {
        return (r.default, wrapped != 0);
    }
    let snapped = snap(wrapped, r);
    (snapped, snapped != wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The range Radeon Software reports for saturation on current drivers.
    fn saturation() -> AdlRange {
        AdlRange { current: 100, default: 100, min: 0, max: 200, step: 1 }
    }

    /// ADL hue is a narrow signed trim.
    fn hue() -> AdlRange {
        AdlRange { current: 0, default: 0, min: -30, max: 30, step: 1 }
    }

    #[test]
    fn neutral_vibrance_is_exactly_the_driver_default() {
        assert_eq!(vibrance_percent_to_saturation(50, &saturation()), 100);
        // And with an off-centre default, still exactly the default.
        let odd = AdlRange { current: 80, default: 80, min: 0, max: 200, step: 1 };
        assert_eq!(vibrance_percent_to_saturation(50, &odd), 80);
    }

    #[test]
    fn vibrance_is_piecewise_linear_through_the_default() {
        let r = saturation();
        assert_eq!(vibrance_percent_to_saturation(100, &r), 200);
        assert_eq!(vibrance_percent_to_saturation(75, &r), 150);
        assert_eq!(vibrance_percent_to_saturation(0, &r), 0);
        assert_eq!(vibrance_percent_to_saturation(25, &r), 50);
        // Off-centre default: each side scales to its own span.
        let odd = AdlRange { current: 80, default: 80, min: 0, max: 200, step: 1 };
        assert_eq!(vibrance_percent_to_saturation(75, &odd), 140, "half of 80..200");
        assert_eq!(vibrance_percent_to_saturation(25, &odd), 40, "half of 0..80");
    }

    #[test]
    fn amd_honours_desaturation_where_nvidia_clamps() {
        // The documented divergence, pinned so it cannot drift unnoticed.
        let r = saturation();
        assert_eq!(vibrance_percent_to_saturation(20, &r), 40, "AMD goes below neutral");
        assert_eq!(
            crate::nvapi::vibrance_percent_to_dvc(20, 0, 63),
            0,
            "NVIDIA has no sub-neutral DVC and stays neutral"
        );
    }

    #[test]
    fn vibrance_round_trips_within_one_percent() {
        let r = saturation();
        for p in [0, 10, 25, 40, 50, 60, 75, 90, 100] {
            let raw = vibrance_percent_to_saturation(p, &r);
            assert!(
                (saturation_to_vibrance_percent(raw, &r) - p).abs() <= 1,
                "p={p} raw={raw} back={}",
                saturation_to_vibrance_percent(raw, &r)
            );
        }
    }

    #[test]
    fn coarse_steps_snap_onto_the_driver_grid() {
        let r = AdlRange { current: 100, default: 100, min: 0, max: 200, step: 10 };
        assert_eq!(vibrance_percent_to_saturation(51, &r), 100, "nearest grid point");
        assert_eq!(vibrance_percent_to_saturation(52, &r), 100, "104 rounds down onto 100");
        assert_eq!(vibrance_percent_to_saturation(57, &r), 110);
        assert_eq!(vibrance_percent_to_saturation(100, &r), 200);
        // Never off the ends, whatever the grid.
        let ragged = AdlRange { current: 0, default: 0, min: -30, max: 30, step: 7 };
        for p in 0..=100 {
            let v = vibrance_percent_to_saturation(p, &ragged);
            assert!((-30..=30).contains(&v), "p={p} -> {v}");
        }
    }

    #[test]
    fn degenerate_ranges_report_neutral() {
        let dead = AdlRange { current: 5, default: 5, min: 5, max: 5, step: 1 };
        assert_eq!(vibrance_percent_to_saturation(90, &dead), 5);
        assert_eq!(saturation_to_vibrance_percent(5, &dead), 50);
        assert_eq!(hue_deg_to_adl(20, &dead), (5, true));
        assert_eq!(hue_deg_to_adl(0, &dead), (5, false));
    }

    #[test]
    fn hue_wraps_to_a_signed_trim_then_clamps() {
        let r = hue();
        assert_eq!(hue_deg_to_adl(0, &r), (0, false));
        assert_eq!(hue_deg_to_adl(15, &r), (15, false));
        assert_eq!(hue_deg_to_adl(350, &r), (-10, false), "350 deg == -10 deg");
        assert_eq!(hue_deg_to_adl(-10, &r), (-10, false));
        assert_eq!(hue_deg_to_adl(90, &r), (30, true), "clamped, and says so");
        assert_eq!(hue_deg_to_adl(270, &r), (-30, true));
        assert_eq!(hue_deg_to_adl(360, &r), (0, false), "a full turn is neutral");
    }

    #[test]
    fn hue_stays_in_range_for_every_angle() {
        let r = hue();
        for deg in -720..=720 {
            let (v, _) = hue_deg_to_adl(deg, &r);
            assert!((r.min..=r.max).contains(&v), "deg={deg} -> {v}");
        }
    }

    #[test]
    fn adapter_info_matches_the_adl_header_layout() {
        // 9 ints + 6 × ADL_MAX_PATH bytes. If this ever fails, the struct no
        // longer matches `adl_structures.h` and every field read is garbage.
        assert_eq!(std::mem::size_of::<AdapterInfo>(), 9 * 4 + 6 * ADL_MAX_PATH);
        assert_eq!(std::mem::size_of::<AdlDisplayInfo>(), 10 * 4 + 2 * ADL_MAX_PATH);
        assert_eq!(std::mem::align_of::<AdapterInfo>(), 4);
    }

    #[test]
    fn amd_vendor_id_is_the_decimal_adl_constant() {
        // Observed live: ADL reports 1002 for Radeon and 10 for GeForce.
        // Writing 0x1002 here would filter out every real AMD adapter.
        assert_eq!(ADL_VENDOR_ID_AMD, 1002);
        assert_ne!(ADL_VENDOR_ID_AMD, 0x1002);
    }

    #[test]
    fn c_str_stops_at_the_nul() {
        let mut buf = [0u8; 8];
        buf[..5].copy_from_slice(b"AMD\0X");
        assert_eq!(c_str(&buf), "AMD");
        assert_eq!(c_str(b"no nul"), "no nul");
    }
}
