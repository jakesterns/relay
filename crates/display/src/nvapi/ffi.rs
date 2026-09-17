//! The NvAPI calls themselves: `nvapi64.dll` loaded per operation through
//! `nvapi_QueryInterface`. Windows only; the unit mapping in the parent
//! module is portable.

use std::ffi::c_void;

use anyhow::{bail, Result};
use tracing::debug;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const ID_INITIALIZE: u32 = 0x0150E828;
const ID_UNLOAD: u32 = 0xD22BDD7E;
const ID_ENUM_DISPLAY_HANDLE: u32 = 0x9ABDD40D;
const ID_GET_DISPLAY_NAME: u32 = 0x22A78B05;
const ID_GET_DVC_INFO: u32 = 0x4085DE45;
const ID_SET_DVC_LEVEL: u32 = 0x172409B4;
const ID_GET_HUE_INFO: u32 = 0x95B64341;
const ID_SET_HUE_ANGLE: u32 = 0xF5A0F22C;

const NVAPI_END_ENUMERATION: i32 = -7;

type NvStatus = i32;
type NvDisplayHandle = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct DvcInfoRaw {
    version: u32,
    current: i32,
    min: i32,
    max: i32,
}

#[repr(C)]
#[derive(Default)]
struct HueInfoRaw {
    version: u32,
    current: i32,
    default_: i32,
}

const fn nv_version<T>(ver: u32) -> u32 {
    (std::mem::size_of::<T>() as u32) | (ver << 16)
}

type FnInitialize = unsafe extern "C" fn() -> NvStatus;
type FnUnload = unsafe extern "C" fn() -> NvStatus;
type FnEnumDisplay = unsafe extern "C" fn(u32, *mut NvDisplayHandle) -> NvStatus;
type FnDisplayName = unsafe extern "C" fn(NvDisplayHandle, *mut [u8; 64]) -> NvStatus;
type FnGetDvc = unsafe extern "C" fn(NvDisplayHandle, u32, *mut DvcInfoRaw) -> NvStatus;
type FnSetDvc = unsafe extern "C" fn(NvDisplayHandle, u32, i32) -> NvStatus;
type FnGetHue = unsafe extern "C" fn(NvDisplayHandle, u32, *mut HueInfoRaw) -> NvStatus;
type FnSetHue = unsafe extern "C" fn(NvDisplayHandle, u32, i32) -> NvStatus;

/// Digital-vibrance state of one display, in raw NvAPI units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dvc {
    pub current: i32,
    pub min: i32,
    pub max: i32,
}

/// One NVIDIA-driven display: opaque handle + the GDI name that keys it to
/// the probe's monitors (`\\.\DISPLAY1`).
pub struct NvDisplay {
    handle: NvDisplayHandle,
    pub gdi_name: String,
}

/// A loaded NvAPI session. Dropping unloads the library.
pub struct NvApi {
    module: HMODULE,
    unload: FnUnload,
    enum_display: FnEnumDisplay,
    display_name: FnDisplayName,
    get_dvc: FnGetDvc,
    set_dvc: FnSetDvc,
    get_hue: FnGetHue,
    set_hue: FnSetHue,
}

impl NvApi {
    /// `None` when there is no NVIDIA driver (or NvAPI refuses to start) —
    /// callers fall back to the gamma-ramp path.
    pub fn load() -> Option<Self> {
        // SAFETY: standard dynamic load; the interface pointers are used only
        // while the module stays loaded (held by Self).
        unsafe {
            let module = LoadLibraryW(PCWSTR(windows::core::w!("nvapi64.dll").as_ptr())).ok()?;
            type RawFn = unsafe extern "system" fn() -> isize;
            type QueryFn = unsafe extern "C" fn(u32) -> *mut c_void;
            let query = std::mem::transmute::<RawFn, QueryFn>(GetProcAddress(
                module,
                windows::core::s!("nvapi_QueryInterface"),
            )?);
            let get = |id: u32| -> Option<*mut c_void> {
                let p = query(id);
                (!p.is_null()).then_some(p)
            };
            type P = *mut c_void;
            let init = std::mem::transmute::<P, FnInitialize>(get(ID_INITIALIZE)?);
            let api = NvApi {
                module,
                unload: std::mem::transmute::<P, FnUnload>(get(ID_UNLOAD)?),
                enum_display: std::mem::transmute::<P, FnEnumDisplay>(get(ID_ENUM_DISPLAY_HANDLE)?),
                display_name: std::mem::transmute::<P, FnDisplayName>(get(ID_GET_DISPLAY_NAME)?),
                get_dvc: std::mem::transmute::<P, FnGetDvc>(get(ID_GET_DVC_INFO)?),
                set_dvc: std::mem::transmute::<P, FnSetDvc>(get(ID_SET_DVC_LEVEL)?),
                get_hue: std::mem::transmute::<P, FnGetHue>(get(ID_GET_HUE_INFO)?),
                set_hue: std::mem::transmute::<P, FnSetHue>(get(ID_SET_HUE_ANGLE)?),
            };
            let rc = init();
            if rc != 0 {
                debug!(rc, "NvAPI_Initialize failed");
                let _ = FreeLibrary(module);
                return None;
            }
            Some(api)
        }
    }

    /// All NVIDIA-driven displays with their GDI names.
    pub fn displays(&self) -> Vec<NvDisplay> {
        let mut out = Vec::new();
        for i in 0..16u32 {
            let mut handle: NvDisplayHandle = std::ptr::null_mut();
            // SAFETY: enum fills the handle; END_ENUMERATION terminates.
            let rc = unsafe { (self.enum_display)(i, &mut handle) };
            if rc == NVAPI_END_ENUMERATION {
                break;
            }
            if rc != 0 || handle.is_null() {
                continue;
            }
            let mut name = [0u8; 64];
            // SAFETY: 64-byte NvAPI_ShortString out buffer per the contract.
            let rc = unsafe { (self.display_name)(handle, &mut name) };
            if rc != 0 {
                continue;
            }
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            out.push(NvDisplay {
                handle,
                gdi_name: String::from_utf8_lossy(&name[..end]).into_owned(),
            });
        }
        out
    }

    pub fn get_vibrance(&self, d: &NvDisplay) -> Result<Dvc> {
        let mut info = DvcInfoRaw { version: nv_version::<DvcInfoRaw>(1), ..Default::default() };
        // SAFETY: versioned struct out-param per the contract.
        let rc = unsafe { (self.get_dvc)(d.handle, 0, &mut info) };
        if rc != 0 {
            bail!("NvAPI_GetDVCInfo failed with {rc} on {}", d.gdi_name);
        }
        Ok(Dvc { current: info.current, min: info.min, max: info.max })
    }

    pub fn set_vibrance(&self, d: &NvDisplay, level: i32) -> Result<()> {
        // SAFETY: plain call.
        let rc = unsafe { (self.set_dvc)(d.handle, 0, level) };
        if rc != 0 {
            bail!("NvAPI_SetDVCLevel({level}) failed with {rc} on {}", d.gdi_name);
        }
        Ok(())
    }

    /// (current angle, default angle) in degrees.
    pub fn get_hue(&self, d: &NvDisplay) -> Result<(i32, i32)> {
        let mut info = HueInfoRaw { version: nv_version::<HueInfoRaw>(1), ..Default::default() };
        // SAFETY: versioned struct out-param per the contract.
        let rc = unsafe { (self.get_hue)(d.handle, 0, &mut info) };
        if rc != 0 {
            bail!("NvAPI_GetHUEInfo failed with {rc} on {}", d.gdi_name);
        }
        Ok((info.current, info.default_))
    }

    pub fn set_hue(&self, d: &NvDisplay, angle: i32) -> Result<()> {
        // SAFETY: plain call.
        let rc = unsafe { (self.set_hue)(d.handle, 0, angle) };
        if rc != 0 {
            bail!("NvAPI_SetHUEAngle({angle}) failed with {rc} on {}", d.gdi_name);
        }
        Ok(())
    }

    /// The display whose GDI name matches, if NVIDIA drives it.
    pub fn display_by_gdi_name(&self, gdi_name: &str) -> Option<NvDisplay> {
        self.displays().into_iter().find(|d| d.gdi_name.eq_ignore_ascii_case(gdi_name))
    }
}

impl Drop for NvApi {
    fn drop(&mut self) {
        // SAFETY: balances load(); nothing in this process uses NvAPI after us.
        unsafe {
            let _ = (self.unload)();
            let _ = FreeLibrary(self.module);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_versions_encode_size_and_version() {
        assert_eq!(nv_version::<DvcInfoRaw>(1), 16 | (1 << 16));
        assert_eq!(nv_version::<HueInfoRaw>(1), 12 | (1 << 16));
    }
}
