//! The slice of the NDI® C API Relay uses, declared by hand, and the loader
//! that resolves it from the user-installed runtime. No SDK file is vendored
//! (`docs/dev/ndi-licensing.md`): the layouts below follow the documented C
//! API (the SDK's MIT-licensed `Processing.NDI.*.h` headers), and the tests at
//! the bottom pin every size and offset so a mistake fails here, not inside
//! the runtime. NDI® is a registered trademark of Vizrt NDI AB.
//!
//! Only plain named exports are used (`NDIlib_send_create` and friends), not
//! the `NDIlib_v6_load` function table: a table is one big struct whose layout
//! changes between SDK versions, where a named export either exists or not.

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::{c_char, c_int, c_void};

/// `NDI_LIB_FOURCC(a, b, c, d)`.
pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

/// `NDIlib_FourCC_video_type_NV12`: 4:2:0, a Y plane then an interleaved UV
/// plane at half height, both `line_stride_in_bytes` wide, the UV plane
/// starting straight after `yres` rows of Y.
pub const FOURCC_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');
/// `NDIlib_frame_format_type_progressive`.
pub const FRAME_FORMAT_PROGRESSIVE: c_int = 1;
/// `NDIlib_send_timecode_synthesize`: let the SDK stamp the frame.
pub const TIMECODE_SYNTHESIZE: i64 = i64::MAX;

/// `NDIlib_send_instance_t`.
pub type SendInstance = *mut c_void;

/// `NDIlib_send_create_t`.
#[repr(C)]
pub struct SendCreate {
    pub p_ndi_name: *const c_char,
    /// Null = the default group, which is what everyone else on the LAN sees.
    pub p_groups: *const c_char,
    /// Both off: frames are already paced by the stream, and a clocked send
    /// would block the worker to the SDK's idea of the frame rate.
    pub clock_video: bool,
    pub clock_audio: bool,
}

/// `NDIlib_video_frame_v2_t`.
#[repr(C)]
pub struct VideoFrameV2 {
    pub xres: c_int,
    pub yres: c_int,
    pub four_cc: u32,
    pub frame_rate_N: c_int,
    pub frame_rate_D: c_int,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: c_int,
    pub timecode: i64,
    pub p_data: *const u8,
    /// `line_stride_in_bytes` (a union with `data_size_in_bytes`, which only
    /// compressed FourCCs use).
    pub line_stride_in_bytes: c_int,
    pub p_metadata: *const c_char,
    /// Written by the SDK on receive; ignored on send.
    pub timestamp: i64,
}

/// `NDIlib_audio_frame_v2_t`: 32-bit float, planar.
#[repr(C)]
pub struct AudioFrameV2 {
    pub sample_rate: c_int,
    pub no_channels: c_int,
    pub no_samples: c_int,
    pub timecode: i64,
    pub p_data: *const f32,
    pub channel_stride_in_bytes: c_int,
    pub p_metadata: *const c_char,
    pub timestamp: i64,
}

pub type FnInitialize = unsafe extern "C" fn() -> bool;
pub type FnVersion = unsafe extern "C" fn() -> *const c_char;
pub type FnSendCreate = unsafe extern "C" fn(*const SendCreate) -> SendInstance;
pub type FnSendDestroy = unsafe extern "C" fn(SendInstance);
pub type FnSendVideoV2 = unsafe extern "C" fn(SendInstance, *const VideoFrameV2);
pub type FnSendAudioV2 = unsafe extern "C" fn(SendInstance, *const AudioFrameV2);
pub type FnSendGetNoConnections = unsafe extern "C" fn(SendInstance, u32) -> c_int;

/// Every entry point Relay calls, resolved from one loaded runtime.
#[derive(Clone, Copy)]
pub struct Api {
    pub initialize: FnInitialize,
    pub version: FnVersion,
    pub send_create: FnSendCreate,
    pub send_destroy: FnSendDestroy,
    pub send_video_v2: FnSendVideoV2,
    pub send_audio_v2: FnSendAudioV2,
    pub send_get_no_connections: FnSendGetNoConnections,
}

/// Why NDI output cannot start. `RuntimeMissing` is the ordinary case on a PC
/// without the runtime and is what the UI turns into its install note.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum LoadError {
    #[error("NDI output needs the NDI runtime (looked in: {searched})")]
    RuntimeMissing { searched: String },
    #[error("the NDI runtime at {path} would not load: {reason}")]
    LoadFailed { path: String, reason: String },
    #[error("the NDI runtime at {path} has no {symbol} (too old? NDI 6 is needed)")]
    MissingSymbol { path: String, symbol: &'static str },
    #[error("the NDI runtime refused to start (NDIlib_initialize: this CPU is not supported)")]
    InitFailed,
    #[error("NDI output is only available on Windows")]
    Unsupported,
}

impl LoadError {
    pub fn runtime_missing(&self) -> bool {
        matches!(self, Self::RuntimeMissing { .. })
    }
}

#[cfg(windows)]
pub use win::Runtime;

#[cfg(windows)]
mod win {
    #![allow(unsafe_code)] // LoadLibraryExW + GetProcAddress; SAFETY notes inline

    use super::*;
    use std::sync::OnceLock;
    use windows::core::{PCSTR, PCWSTR};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };

    /// The loaded runtime. Loaded at most once per process and never unloaded:
    /// the SDK keeps threads of its own, and `relay-share` is a per-share
    /// process, so the library goes when the share does.
    pub struct Runtime {
        pub api: Api,
        pub path: String,
        pub version: String,
        _module: HMODULE,
    }

    // SAFETY: the module handle is never freed and the API is a table of
    // function pointers; the SDK documents its send API as thread-safe.
    unsafe impl Send for Runtime {}
    unsafe impl Sync for Runtime {}

    static RUNTIME: OnceLock<Result<Runtime, LoadError>> = OnceLock::new();

    impl Runtime {
        /// Find and load the runtime once. A failure is remembered too: the
        /// user installing the runtime mid-share is picked up by the next
        /// share's process, not by retrying the loader on every toggle.
        pub fn get() -> Result<&'static Runtime, LoadError> {
            RUNTIME.get_or_init(Self::load).as_ref().map_err(Clone::clone)
        }

        fn load() -> Result<Runtime, LoadError> {
            let found = relay_core::ndi::locate_runtime();
            let Some(path) = found.path.clone().filter(|_| found.present) else {
                return Err(LoadError::RuntimeMissing { searched: found.searched.join("; ") });
            };
            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: a NUL-terminated full path. DLL_LOAD_DIR lets the
            // runtime find its own dependencies beside it; DEFAULT_DIRS keeps
            // the rest of the search to System32 and the app folder, never
            // the current directory or PATH.
            let module = unsafe {
                LoadLibraryExW(
                    PCWSTR(wide.as_ptr()),
                    None,
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                )
            }
            .map_err(|e| LoadError::LoadFailed { path: path.clone(), reason: e.to_string() })?;

            macro_rules! sym {
                ($ty:ty, $name:literal) => {{
                    // SAFETY: a NUL-terminated literal on a live module; the
                    // pointer is transmuted to the documented signature.
                    match unsafe { GetProcAddress(module, PCSTR(concat!($name, "\0").as_ptr())) } {
                        Some(f) => unsafe {
                            std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f)
                        },
                        None => {
                            return Err(LoadError::MissingSymbol {
                                path: path.clone(),
                                symbol: $name,
                            })
                        }
                    }
                }};
            }
            let api = Api {
                initialize: sym!(FnInitialize, "NDIlib_initialize"),
                version: sym!(FnVersion, "NDIlib_version"),
                send_create: sym!(FnSendCreate, "NDIlib_send_create"),
                send_destroy: sym!(FnSendDestroy, "NDIlib_send_destroy"),
                send_video_v2: sym!(FnSendVideoV2, "NDIlib_send_send_video_v2"),
                send_audio_v2: sym!(FnSendAudioV2, "NDIlib_send_send_audio_v2"),
                send_get_no_connections: sym!(
                    FnSendGetNoConnections,
                    "NDIlib_send_get_no_connections"
                ),
            };
            // SAFETY: documented entry points with no arguments.
            if !unsafe { (api.initialize)() } {
                return Err(LoadError::InitFailed);
            }
            let version = unsafe {
                let p = (api.version)();
                if p.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            tracing::info!(%path, %version, "NDI runtime loaded");
            Ok(Runtime { api, path, version, _module: module })
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn fourcc_matches_the_sdk_macro() {
        // 'N' 'V' '1' '2' little-endian.
        assert_eq!(FOURCC_NV12, 0x3231_564E);
    }

    #[test]
    fn send_create_layout() {
        assert_eq!(size_of::<SendCreate>(), 24);
        assert_eq!(offset_of!(SendCreate, p_groups), 8);
        assert_eq!(offset_of!(SendCreate, clock_video), 16);
        assert_eq!(offset_of!(SendCreate, clock_audio), 17);
    }

    #[test]
    fn video_frame_layout() {
        assert_eq!(size_of::<VideoFrameV2>(), 72);
        assert_eq!(offset_of!(VideoFrameV2, four_cc), 8);
        assert_eq!(offset_of!(VideoFrameV2, frame_rate_N), 12);
        assert_eq!(offset_of!(VideoFrameV2, frame_rate_D), 16);
        assert_eq!(offset_of!(VideoFrameV2, picture_aspect_ratio), 20);
        assert_eq!(offset_of!(VideoFrameV2, frame_format_type), 24);
        assert_eq!(offset_of!(VideoFrameV2, timecode), 32);
        assert_eq!(offset_of!(VideoFrameV2, p_data), 40);
        assert_eq!(offset_of!(VideoFrameV2, line_stride_in_bytes), 48);
        assert_eq!(offset_of!(VideoFrameV2, p_metadata), 56);
        assert_eq!(offset_of!(VideoFrameV2, timestamp), 64);
    }

    #[test]
    fn audio_frame_layout() {
        assert_eq!(size_of::<AudioFrameV2>(), 56);
        assert_eq!(offset_of!(AudioFrameV2, no_samples), 8);
        assert_eq!(offset_of!(AudioFrameV2, timecode), 16);
        assert_eq!(offset_of!(AudioFrameV2, p_data), 24);
        assert_eq!(offset_of!(AudioFrameV2, channel_stride_in_bytes), 32);
        assert_eq!(offset_of!(AudioFrameV2, p_metadata), 40);
        assert_eq!(offset_of!(AudioFrameV2, timestamp), 48);
    }

    #[test]
    fn only_a_missing_runtime_reads_as_missing() {
        assert!(LoadError::RuntimeMissing { searched: String::new() }.runtime_missing());
        assert!(!LoadError::InitFailed.runtime_missing());
        assert!(LoadError::RuntimeMissing { searched: "x".into() }
            .to_string()
            .starts_with("NDI output needs the NDI runtime"));
    }
}
