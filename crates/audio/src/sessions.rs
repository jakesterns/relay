//! WASAPI render-session inspection (Windows only).
//!
//! Two questions the core asks about the default render endpoint:
//! 1. Which processes have active shared-mode render sessions
//!    (`IAudioSessionManager2` enumeration)?
//! 2. Is the endpoint held in WASAPI-exclusive mode? Exclusive streams do
//!    not appear as enumerable shared sessions, so this is probed directly:
//!    a shared-mode `IAudioClient::Initialize` on a busy endpoint fails
//!    with `AUDCLNT_E_DEVICE_IN_USE`.
//!
//! The core combines the two with its focus watcher: exclusive endpoint +
//! foreground game ⇒ `AudioChainState::ExclusiveBypassed`.
//!
//! Also here: [`hold_exclusive_for_test`], the test helper standing in for
//! "a game known to use WASAPI exclusive mode" (see the M3 plan's DoR).

#![allow(unsafe_code)]

use windows::core::{Interface, HRESULT};
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionStateActive, IAudioClient, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_EXCLUSIVE,
    AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX, WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

/// `AUDCLNT_E_DEVICE_IN_USE`: another client holds the endpoint exclusively.
const DEVICE_IN_USE: HRESULT = HRESULT(0x8889_000Au32 as i32);
/// `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED`: retry with the aligned size.
const BUFFER_NOT_ALIGNED: HRESULT = HRESULT(0x8889_0019u32 as i32);
/// `ERROR_NOT_FOUND` as an HRESULT: no default render endpoint.
const NOT_FOUND: HRESULT = HRESULT(0x8007_0490u32 as i32);
/// `RPC_E_CHANGED_MODE`: COM was already initialised with another model.
const CHANGED_MODE: HRESULT = HRESULT(0x8001_0106u32 as i32);

#[derive(Debug, thiserror::Error)]
pub enum SessionsError {
    /// No default render endpoint — headless CI machine or all devices disabled.
    #[error("no default audio render endpoint")]
    NoDevice,
    /// The device refused every exclusive format we tried (test helper only).
    #[error("endpoint accepts none of the exclusive-mode formats tried")]
    NoExclusiveFormat,
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
}

/// Snapshot of the default render endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderSessions {
    /// Some client holds the endpoint in exclusive mode right now.
    pub exclusive: bool,
    /// PIDs with an *active* shared-mode render session.
    pub active_pids: Vec<u32>,
}

/// Per-call COM guard: MTA init, uninit on drop unless COM was already up
/// in an incompatible mode (then we borrow the existing apartment).
struct ComGuard {
    uninit: bool,
}

impl ComGuard {
    fn new() -> Result<Self, SessionsError> {
        // SAFETY: standard COM initialisation on the calling thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == CHANGED_MODE {
            return Ok(Self { uninit: false });
        }
        hr.ok()?;
        Ok(Self { uninit: true })
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

fn default_render_device() -> Result<windows::Win32::Media::Audio::IMMDevice, SessionsError> {
    // SAFETY: CoCreateInstance of the documented MMDeviceEnumerator CLSID.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
    // SAFETY: enumerator is a valid COM interface.
    match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
        Ok(d) => Ok(d),
        Err(e) if e.code() == NOT_FOUND => Err(SessionsError::NoDevice),
        Err(e) => Err(e.into()),
    }
}

/// The default render endpoint's GUID — the `{...}` MMDevices registry key
/// name and the suffix of the shared-memory section the APO serves. Device
/// ids look like `{0.0.0.00000000}.{f8ae226b-…}`; the last braced group is
/// the endpoint GUID.
pub fn default_render_endpoint_guid() -> Result<String, SessionsError> {
    let _com = ComGuard::new()?;
    let device = default_render_device()?;
    // SAFETY: device is valid; the returned PWSTR is CoTaskMem-owned and
    // freed below after copying out.
    let id = unsafe {
        let p = device.GetId()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        s
    };
    match id.rfind('{') {
        Some(i) => Ok(id[i..].to_string()),
        None => Ok(id),
    }
}

/// Probe the default render endpoint: active session PIDs + exclusive flag.
///
/// Costs one COM activation and one shared-mode `Initialize` attempt on a
/// throwaway `IAudioClient`; a few hundred microseconds in practice. Safe to
/// call at 1 Hz while a game profile is active.
pub fn probe_default_render() -> Result<RenderSessions, SessionsError> {
    let _com = ComGuard::new()?;
    let device = default_render_device()?;

    // 1. Shared-mode sessions → active PIDs.
    let mut active_pids = Vec::new();
    // SAFETY: activating a documented control-plane interface on the device.
    let manager: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None) }?;
    // SAFETY: manager is valid; the enumerator lives on the returned interface.
    let sessions = unsafe { manager.GetSessionEnumerator() }?;
    // SAFETY: sessions is valid.
    let count = unsafe { sessions.GetCount() }?;
    for i in 0..count {
        // SAFETY: index is within [0, count).
        let Ok(control) = (unsafe { sessions.GetSession(i) }) else { continue };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
        // SAFETY: control2 is a valid session control.
        let state = match unsafe { control2.GetState() } {
            Ok(s) => s,
            Err(_) => continue,
        };
        if state != AudioSessionStateActive {
            continue;
        }
        // SAFETY: control2 is valid; system sessions fail here and are skipped.
        if let Ok(pid) = unsafe { control2.GetProcessId() } {
            if pid != 0 {
                active_pids.push(pid);
            }
        }
    }

    // 2. Exclusive probe: a shared-mode Initialize fails with
    //    AUDCLNT_E_DEVICE_IN_USE iff someone holds the endpoint exclusively.
    // SAFETY: activating IAudioClient on the device.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
    // SAFETY: client is valid; the returned format is freed below.
    let mix = unsafe { client.GetMixFormat() }?;
    // SAFETY: mix points at a WAVEFORMATEX the OS just allocated; a zero
    // buffer duration asks for the minimum shared-mode buffer.
    let init = unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 0, 0, mix, None) };
    // SAFETY: freeing the CoTaskMem allocation from GetMixFormat.
    unsafe { CoTaskMemFree(Some(mix as *const _)) };
    let exclusive = match init {
        Ok(()) => false,
        Err(e) if e.code() == DEVICE_IN_USE => true,
        // Any other failure tells us nothing about exclusivity.
        Err(_) => false,
    };
    // The throwaway client is released on drop; it was never started.

    Ok(RenderSessions { exclusive, active_pids })
}

/// Holds the default render endpoint in WASAPI-exclusive mode until dropped.
/// This is the "game known to use exclusive mode" for the automated test.
pub struct ExclusiveHold {
    client: IAudioClient,
    _com: ComGuard,
}

impl Drop for ExclusiveHold {
    fn drop(&mut self) {
        // SAFETY: stopping a client we started; failure just means it never ran.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Open an exclusive-mode stream on the default render endpoint. Tries
/// 16-bit PCM at 48 k then 44.1 k (the formats consumer endpoints accept
/// in exclusive mode).
pub fn hold_exclusive_for_test() -> Result<ExclusiveHold, SessionsError> {
    let com = ComGuard::new()?;
    let device = default_render_device()?;
    let mut last: Option<windows::core::Error> = None;
    for rate in [48_000u32, 44_100] {
        let block_align = 2 * 16 / 8;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block_align as u32,
            nBlockAlign: block_align as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // SAFETY: activating IAudioClient on a valid device.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
        let mut default_period = 0i64;
        let mut min_period = 0i64;
        // SAFETY: out-params are valid stack slots.
        unsafe { client.GetDevicePeriod(Some(&mut default_period), Some(&mut min_period)) }?;
        // SAFETY: format is a complete PCM WAVEFORMATEX on the stack.
        let init = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                0,
                default_period,
                default_period,
                &format,
                None,
            )
        };
        let init = match init {
            Err(e) if e.code() == BUFFER_NOT_ALIGNED => {
                // Recompute an aligned duration from the granted buffer size,
                // on a fresh client as the API requires.
                // SAFETY: querying the size the failed Initialize computed.
                let frames = unsafe { client.GetBufferSize() }?;
                let aligned = (frames as i64 * 10_000_000 + (rate as i64 / 2)) / rate as i64;
                // SAFETY: same activation as above.
                let client2: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
                // SAFETY: same contract as the first Initialize.
                let r = unsafe {
                    client2.Initialize(
                        AUDCLNT_SHAREMODE_EXCLUSIVE,
                        0,
                        aligned,
                        aligned,
                        &format,
                        None,
                    )
                };
                r.map(|()| client2)
            }
            Err(e) => Err(e),
            Ok(()) => Ok(client),
        };
        match init {
            Ok(client) => {
                // SAFETY: starting the stream we just initialised; it renders
                // silence (no render client filled) which is fine for a probe.
                unsafe { client.Start() }?;
                return Ok(ExclusiveHold { client, _com: com });
            }
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) if e.code() == DEVICE_IN_USE => Err(e.into()),
        _ => Err(SessionsError::NoExclusiveFormat),
    }
}
