//! The ADL calls themselves: `atiadlxx.dll` resolved by name, one context per
//! session. Windows only; the unit mapping in the parent module is portable.

use std::ffi::c_void;

use anyhow::{bail, Context, Result};
use tracing::debug;

use super::AdlRange;
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

#[cfg(test)]
mod tests {
    use super::*;

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
