//! The APO COM object audiodg loads: `IAudioProcessingObject`,
//! `IAudioProcessingObjectRT`, `IAudioProcessingObjectConfiguration` and the
//! `IAudioSystemEffects{,2}` markers, hosting `relay_audio::dsp::Chain`.
//!
//! Real-time rules:
//! - `APOProcess` never locks, never allocates, never frees. It reads the
//!   shared-memory bypass word, loads the active chain pointer, and processes
//!   (or copies) — nothing else.
//! - Parameter changes are applied by a control thread: it waits on the
//!   params-changed event, builds a *new* prepared chain off the RT path,
//!   swaps the atomic pointer, waits for the RT side to leave the old chain,
//!   and only then frees it.
//! - Any failure (no shared memory yet, unsupported rate for HRTF, prepare
//!   error) degrades to pass-through, never to an engine error. The APO being
//!   present must be inaudible until the core says otherwise.
//!
//! Format contract: 32-bit float, 2 channels, input rate == output rate
//! (never resample — brief). Everything else is refused at negotiation time
//! with `APOERR_FORMAT_NOT_SUPPORTED`, which makes the engine fall back to
//! running the endpoint without us (pass-through by absence).

// COM plumbing and RT buffer access are inherently unsafe; every block
// carries a SAFETY note.
#![allow(unsafe_code)]
#![allow(non_snake_case)] // COM method names come from the interfaces.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use relay_audio::dsp::Chain;
use relay_audio::shm::{section_name, SharedParams};
use windows::core::{implement, Interface, Ref, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_INVALIDARG, E_POINTER, S_FALSE, S_OK,
    WAIT_OBJECT_0,
};
use windows::Win32::Media::Audio::Apo::{
    APOInitSystemEffects, APOInitSystemEffects2, APOInitSystemEffects3, IAudioMediaType,
    IAudioProcessingObject, IAudioProcessingObjectConfiguration,
    IAudioProcessingObjectConfiguration_Impl, IAudioProcessingObjectRT,
    IAudioProcessingObjectRT_Impl, IAudioProcessingObject_Impl, IAudioSystemEffects,
    IAudioSystemEffects2, IAudioSystemEffects2_Impl, IAudioSystemEffects_Impl,
    APOERR_ALREADY_INITIALIZED, APOERR_ALREADY_UNLOCKED, APOERR_FORMAT_NOT_SUPPORTED,
    APOERR_NOT_INITIALIZED, APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_FLAG_DEFAULT,
    APO_REG_PROPERTIES, UNCOMPRESSEDAUDIOFORMAT,
};
use windows::Win32::Media::Audio::PKEY_AudioEndpoint_GUID;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{CoTaskMemAlloc, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::System::Variant::VT_LPWSTR;

/// CLSID of the Relay endpoint APO — the GUID form of [`crate::ids::APO_CLSID`].
pub const CLSID_RELAY_APO: GUID = GUID::from_u128(0x5A8E9C3B_1F6D_4B0A_9C41_7E2D83A6F0B4);

/// Rebuild-check cadence for the control thread when no event arrives (it
/// also lets the thread notice `stop` without an extra wake object).
const CONTROL_WAIT_MS: u32 = 500;

/// One prepared chain plus the scratch the RT path needs around it. The
/// atomic pointer the RT thread loads points at one of these.
struct RtChain {
    chain: Chain,
    /// Copy-in scratch for the aliasing case (engine hands us the same
    /// buffer for input and output).
    scratch: Vec<f32>,
    /// Frames the chain was prepared for; larger blocks process in chunks.
    max_block: usize,
    latency_hns: i64,
}

/// State shared between the RT path, the control thread and the config path.
/// Everything the RT path touches is an atomic.
struct RtShared {
    /// Null = pass-through. Swapped only by the control thread (or the
    /// config path while the engine guarantees no concurrent APOProcess).
    active: AtomicPtr<RtChain>,
    /// Raw view of the shared-memory block; null until Initialize created
    /// the section. The mapping outlives this pointer: `RelayApo` drops the
    /// `SharedParams` only after the control thread has joined and the
    /// engine has unlocked processing.
    block: AtomicPtr<relay_audio::shm::ParamBlock>,
    /// True while APOProcess is between entry and exit; the control thread
    /// spins on this after a swap before freeing the old chain.
    rt_busy: AtomicBool,
    stop: AtomicBool,
    /// Last shm sequence number the control thread built a chain from.
    built_seq: AtomicU32,
}

impl RtShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicPtr::new(std::ptr::null_mut()),
            block: AtomicPtr::new(std::ptr::null_mut()),
            rt_busy: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            built_seq: AtomicU32::new(0),
        })
    }

    /// Swap in a new chain (or null) and free the old one once the RT side
    /// has provably left it. Never called from the RT thread.
    fn publish(&self, next: *mut RtChain) {
        let old = self.active.swap(next, Ordering::AcqRel);
        if old.is_null() {
            return;
        }
        // The RT thread that loaded `old` sets rt_busy for the duration of
        // its APOProcess call; once we observe it false *after* the swap,
        // no RT reference to `old` can remain.
        while self.rt_busy.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        // SAFETY: `old` came from Box::into_raw in `build_chain` and no
        // other pointer to it exists after the swap + busy handshake.
        drop(unsafe { Box::from_raw(old) });
    }
}

/// Prepare a chain for the current shared-memory parameters. Returns null
/// (pass-through) when there are no parameters yet, the params describe an
/// empty chain, or preparation fails. HRTF quietly drops out at rates the
/// bundled IRs do not cover rather than failing the whole chain.
fn build_chain(shared: &RtShared, rate: u32, max_block: usize) -> *mut RtChain {
    let block = shared.block.load(Ordering::Acquire);
    if block.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: non-null `block` points into the mapping owned by RelayApo,
    // which outlives the control thread (join happens before drop).
    let Some((seq, mut params)) = (unsafe { &*block }).read_params() else {
        return std::ptr::null_mut();
    };
    shared.built_seq.store(seq, Ordering::Release);
    if params.bands.is_empty() && params.limiter.is_none() && !params.hrtf {
        return std::ptr::null_mut();
    }
    if params.hrtf && !matches!(rate, 44100 | 48000 | 96000) {
        params.hrtf = false;
    }
    let mut chain = Chain::new(params);
    if chain.prepare(rate, max_block).is_err() {
        return std::ptr::null_mut();
    }
    let latency_hns = (chain.latency_frames() as i64) * 10_000_000 / rate as i64;
    Box::into_raw(Box::new(RtChain {
        chain,
        scratch: vec![0.0; 2 * max_block],
        max_block,
        latency_hns,
    }))
}

/// Config-path state, valid between LockForProcess and UnlockForProcess.
struct Locked {
    control: Option<std::thread::JoinHandle<()>>,
}

/// Init-path state, valid after Initialize.
struct Initialized {
    /// The mapping (created by us, `Global\` in production). Dropped last.
    shm: Option<SharedParams>,
}

#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration,
    IAudioSystemEffects,
    IAudioSystemEffects2
)]
pub struct RelayApo {
    init: Mutex<Option<Initialized>>,
    locked: Mutex<Option<Locked>>,
    shared: Arc<RtShared>,
    latency_hns: AtomicI64,
}

impl Default for RelayApo {
    fn default() -> Self {
        Self {
            init: Mutex::new(None),
            locked: Mutex::new(None),
            shared: RtShared::new(),
            latency_hns: AtomicI64::new(0),
        }
    }
}

impl RelayApo {
    /// Attach to (create) the endpoint's parameter section. Failure is not
    /// an error: the APO simply stays a wire.
    fn attach_shm(&self, endpoint_guid: &str) -> Option<SharedParams> {
        let instance = std::env::var("RELAY_INSTANCE").unwrap_or_default();
        let name = section_name(endpoint_guid, &instance);
        // Tests run in-session without SeCreateGlobalPrivilege.
        let name = if std::env::var("RELAY_APO_LOCAL_SECTION").is_ok() {
            name.replacen("Global\\", "Local\\", 1)
        } else {
            name
        };
        let r = SharedParams::create(&name);
        crate::diag!(
            "shm create {name}: {}",
            match &r {
                Ok(_) => "S_OK".to_string(),
                Err(e) => format!("{:#010x} {}", e.code().0 as u32, e.message()),
            }
        );
        match r {
            Ok(s) => {
                self.shared.block.store(
                    s.block() as *const _ as *mut relay_audio::shm::ParamBlock,
                    Ordering::Release,
                );
                Some(s)
            }
            Err(_) => None,
        }
    }
}

/// Requested format must be uncompressed 32-bit float stereo; rate is taken
/// as-is (the chain runs at the endpoint rate, whatever it is).
fn format_ok(fmt: &UNCOMPRESSEDAUDIOFORMAT) -> bool {
    fmt.guidFormatType == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        && fmt.dwSamplesPerFrame == 2
        && fmt.dwBytesPerSampleContainer == 4
        && fmt.dwValidBitsPerSample == 32
        && fmt.fFramesPerSecond > 0.0
}

fn uncompressed(mt: &IAudioMediaType) -> windows::core::Result<UNCOMPRESSEDAUDIOFORMAT> {
    let mut f = UNCOMPRESSEDAUDIOFORMAT::default();
    // SAFETY: out-pointer to a live stack struct.
    unsafe { mt.GetUncompressedAudioFormat(&mut f)? };
    Ok(f)
}

/// Shared input/output negotiation: float32 stereo at one unchanging rate.
/// When an opposite-side format exists its rate must match (never resample).
fn negotiate(
    opposite: Option<&IAudioMediaType>,
    requested: Option<&IAudioMediaType>,
) -> windows::core::Result<IAudioMediaType> {
    let req = requested.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
    let f = uncompressed(req)?;
    if !format_ok(&f) {
        return Err(windows::core::Error::from_hresult(APOERR_FORMAT_NOT_SUPPORTED));
    }
    if let Some(opp) = opposite {
        if let Ok(of) = uncompressed(opp) {
            if of.fFramesPerSecond != f.fFramesPerSecond {
                return Err(windows::core::Error::from_hresult(APOERR_FORMAT_NOT_SUPPORTED));
            }
        }
    }
    Ok(req.clone())
}

impl IAudioProcessingObject_Impl for RelayApo_Impl {
    fn Reset(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn GetLatency(&self) -> windows::core::Result<i64> {
        Ok(self.latency_hns.load(Ordering::Acquire))
    }

    fn GetRegistrationProperties(&self) -> windows::core::Result<*mut APO_REG_PROPERTIES> {
        // SAFETY: the caller frees with CoTaskMemFree, per the APO contract.
        let p = unsafe { CoTaskMemAlloc(std::mem::size_of::<APO_REG_PROPERTIES>()) }
            as *mut APO_REG_PROPERTIES;
        if p.is_null() {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        let mut props = APO_REG_PROPERTIES {
            clsid: CLSID_RELAY_APO,
            Flags: APO_FLAG_DEFAULT,
            szFriendlyName: [0; 256],
            szCopyrightInfo: [0; 256],
            u32MajorVersion: crate::ids::APO_MAJOR_VERSION,
            u32MinorVersion: crate::ids::APO_MINOR_VERSION,
            u32MinInputConnections: 1,
            u32MaxInputConnections: 1,
            u32MinOutputConnections: 1,
            u32MaxOutputConnections: 1,
            u32MaxInstances: u32::MAX,
            u32NumAPOInterfaces: 1,
            iidAPOInterfaceList: [IAudioProcessingObject::IID],
        };
        crate::diag!("GetRegistrationProperties flags={:#x}", props.Flags.0);
        for (dst, src) in
            props.szFriendlyName.iter_mut().zip(crate::ids::APO_FRIENDLY_NAME.encode_utf16())
        {
            *dst = src;
        }
        for (dst, src) in
            props.szCopyrightInfo.iter_mut().zip(crate::ids::APO_COPYRIGHT.encode_utf16())
        {
            *dst = src;
        }
        // SAFETY: `p` is a valid allocation of the right size.
        unsafe { p.write(props) };
        Ok(p)
    }

    fn Initialize(&self, cbdatasize: u32, pbydata: *const u8) -> windows::core::Result<()> {
        let mut g = self.init.lock().unwrap();
        if g.is_some() {
            return Err(windows::core::Error::from_hresult(APOERR_ALREADY_INITIALIZED));
        }
        // Which struct the engine hands us depends on the interfaces we
        // expose: APOInitSystemEffects (no IAudioSystemEffects2),
        // APOInitSystemEffects2 (IAudioSystemEffects2), APOInitSystemEffects3
        // (IAudioSystemEffects3, Win11). All three carry the endpoint
        // property store at the same offset. S42c: we used to require the
        // v2 size and silently dropped the endpoint for v1, so the params
        // section was never created.
        let mut endpoint_guid: Option<String> = None;
        let mut discovery_only = false;
        let kind = init_kind(cbdatasize as usize);
        crate::diag!("Initialize cbDataSize={cbdatasize} kind={kind:?} null={}", pbydata.is_null());
        if pbydata.is_null() && cbdatasize != 0 {
            crate::diag!("Initialize -> E_INVALIDARG");
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        if !pbydata.is_null() && kind != InitKind::None {
            // SAFETY: the engine hands us at least cbdatasize readable bytes
            // and `kind` was chosen by size, so each read stays in bounds.
            // Interface fields are borrowed, never dropped here.
            let store = unsafe {
                let base = &*(pbydata as *const APOInitSystemEffects);
                match kind {
                    InitKind::V2 => {
                        let fx = &*(pbydata as *const APOInitSystemEffects2);
                        discovery_only = fx.InitializeForDiscoveryOnly.as_bool();
                    }
                    InitKind::V3 => {
                        let fx = &*(pbydata as *const APOInitSystemEffects3);
                        discovery_only = fx.InitializeForDiscoveryOnly.as_bool();
                    }
                    _ => {}
                }
                base.pAPOEndpointProperties.as_ref()
            };
            if let Some(store) = store {
                // SAFETY: VT_LPWSTR PROPVARIANT out of a live store; cleared
                // after copying the string out.
                unsafe {
                    if let Ok(mut v) = store.GetValue(&PKEY_AudioEndpoint_GUID) {
                        let inner = &v.Anonymous.Anonymous;
                        if inner.vt == VT_LPWSTR {
                            let ws: PCWSTR = PCWSTR(inner.Anonymous.pwszVal.0);
                            if !ws.is_null() {
                                endpoint_guid = ws.to_string().ok();
                            }
                        }
                        let _ = PropVariantClear(&mut v);
                    }
                }
            }
        }
        crate::diag!("Initialize endpoint={endpoint_guid:?} discovery={discovery_only}");

        // Test rig: in-process tests have no audiodg property store to hand
        // us an endpoint; they name one explicitly.
        if endpoint_guid.is_none() {
            endpoint_guid = std::env::var("RELAY_APO_ENDPOINT_OVERRIDE").ok();
        }
        let shm = match (&endpoint_guid, discovery_only) {
            (Some(guid), false) => self.attach_shm(guid),
            _ => None,
        };
        *g = Some(Initialized { shm });
        crate::diag!("Initialize -> S_OK");
        Ok(())
    }

    fn IsInputFormatSupported(
        &self,
        opposite: Ref<IAudioMediaType>,
        requested: Ref<IAudioMediaType>,
    ) -> windows::core::Result<IAudioMediaType> {
        let (opposite, requested) = (opposite.as_ref(), requested.as_ref());
        let r = negotiate(opposite, requested);
        diag_format("IsInputFormatSupported", opposite, requested, &r);
        r
    }

    fn IsOutputFormatSupported(
        &self,
        opposite: Ref<IAudioMediaType>,
        requested: Ref<IAudioMediaType>,
    ) -> windows::core::Result<IAudioMediaType> {
        let (opposite, requested) = (opposite.as_ref(), requested.as_ref());
        let r = negotiate(opposite, requested);
        diag_format("IsOutputFormatSupported", opposite, requested, &r);
        r
    }

    fn GetInputChannelCount(&self) -> windows::core::Result<u32> {
        Ok(2)
    }
}

impl IAudioProcessingObjectConfiguration_Impl for RelayApo_Impl {
    // The COM ABI hands these methods raw pointers behind a safe trait; the
    // engine guarantees their validity (see the SAFETY notes inside).
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn LockForProcess(
        &self,
        num_in: u32,
        in_desc: *const *const APO_CONNECTION_DESCRIPTOR,
        num_out: u32,
        out_desc: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> windows::core::Result<()> {
        if self.init.lock().unwrap().is_none() {
            return Err(windows::core::Error::from_hresult(APOERR_NOT_INITIALIZED));
        }
        if num_in != 1 || num_out != 1 || in_desc.is_null() || out_desc.is_null() {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        // SAFETY: the engine passes num_in/num_out valid descriptor pointers.
        let (rate, max_frames) = unsafe {
            let d = &**in_desc;
            let mt =
                d.pFormat.as_ref().ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            let f = uncompressed(mt)?;
            if !format_ok(&f) {
                return Err(windows::core::Error::from_hresult(APOERR_FORMAT_NOT_SUPPORTED));
            }
            (f.fFramesPerSecond as u32, d.u32MaxFrameCount as usize)
        };
        let max_frames = max_frames.max(1);
        crate::diag!("LockForProcess rate={rate} max_frames={max_frames}");

        // Initial chain, then the control thread for live updates.
        let first = build_chain(&self.shared, rate, max_frames);
        // SAFETY: `first` is exclusively ours until published below.
        self.latency_hns.store(
            if first.is_null() { 0 } else { unsafe { (*first).latency_hns } },
            Ordering::Release,
        );
        self.shared.stop.store(false, Ordering::Release);
        self.shared.publish(first);

        let control = {
            let g = self.init.lock().unwrap();
            let event = g.as_ref().and_then(|i| i.shm.as_ref()).map(|s| s.event_handle());
            event.map(|event| {
                let shared = self.shared.clone();
                // HANDLEs are plain numbers; the event outlives the thread
                // because UnlockForProcess joins before the mapping drops.
                let raw = event.0 as usize;
                std::thread::Builder::new()
                    .name("relay-apo-control".into())
                    .spawn(move || {
                        let event = windows::Win32::Foundation::HANDLE(raw as *mut _);
                        loop {
                            // SAFETY: event stays valid until after join.
                            let w = unsafe { WaitForSingleObject(event, CONTROL_WAIT_MS) };
                            if shared.stop.load(Ordering::Acquire) {
                                break;
                            }
                            let block = shared.block.load(Ordering::Acquire);
                            if block.is_null() {
                                continue;
                            }
                            // SAFETY: block outlives the thread (see RtShared).
                            let seq = unsafe { &*block }.sequence();
                            let stale = seq != shared.built_seq.load(Ordering::Acquire);
                            if w == WAIT_OBJECT_0 || stale {
                                shared.publish(build_chain(&shared, rate, max_frames));
                            }
                        }
                    })
                    .expect("spawn relay-apo-control")
            })
        };
        *self.locked.lock().unwrap() = Some(Locked { control });
        Ok(())
    }

    fn UnlockForProcess(&self) -> windows::core::Result<()> {
        let Some(locked) = self.locked.lock().unwrap().take() else {
            return Err(windows::core::Error::from_hresult(APOERR_ALREADY_UNLOCKED));
        };
        self.shared.stop.store(true, Ordering::Release);
        // Wake the control thread past its wait so it sees `stop` now.
        if let Some(i) = self.init.lock().unwrap().as_ref() {
            if let Some(s) = &i.shm {
                s.notify();
            }
        }
        if let Some(t) = locked.control {
            let _ = t.join();
        }
        // The engine guarantees APOProcess is not running concurrently with
        // UnlockForProcess, so publishing null frees the chain immediately.
        self.shared.publish(std::ptr::null_mut());
        self.latency_hns.store(0, Ordering::Release);
        Ok(())
    }
}

impl IAudioProcessingObjectRT_Impl for RelayApo_Impl {
    // As above: raw pointers behind a safe COM trait, validity guaranteed
    // by the engine.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn APOProcess(
        &self,
        num_in: u32,
        in_conn: *const *const APO_CONNECTION_PROPERTY,
        num_out: u32,
        out_conn: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        if num_in < 1 || num_out < 1 || in_conn.is_null() || out_conn.is_null() {
            return;
        }
        // SAFETY: one valid connection each way, per registration properties;
        // buffers hold ValidFrameCount frames of interleaved f32 stereo (the
        // only format negotiation admits).
        unsafe {
            let input = &**in_conn;
            let output = &mut **out_conn;
            let frames = input.u32ValidFrameCount as usize;
            let n = frames * 2;
            let src = std::slice::from_raw_parts(input.pBuffer as *const f32, n);
            let dst = std::slice::from_raw_parts_mut(output.pBuffer as *mut f32, n);

            self.shared.rt_busy.store(true, Ordering::Release);
            let block = self.shared.block.load(Ordering::Acquire);
            let bypass = block.is_null() || (*block).bypass();
            let chain = self.shared.active.load(Ordering::Acquire);
            if bypass || chain.is_null() {
                if input.pBuffer != output.pBuffer {
                    dst.copy_from_slice(src);
                }
            } else {
                let rt = &mut *chain;
                // Aliased buffers: stage input through the scratch copy.
                let src: &[f32] = if input.pBuffer == output.pBuffer {
                    rt.scratch[..n].copy_from_slice(src);
                    &rt.scratch[..n]
                } else {
                    src
                };
                let step = rt.max_block * 2;
                let mut off = 0;
                while off < n {
                    let end = (off + step).min(n);
                    rt.chain.process(&src[off..end], &mut dst[off..end]);
                    off = end;
                }
            }
            output.u32ValidFrameCount = frames as u32;
            output.u32BufferFlags = input.u32BufferFlags;
            self.shared.rt_busy.store(false, Ordering::Release);
        }
    }

    fn CalcInputFrames(&self, out_frames: u32) -> u32 {
        out_frames
    }

    fn CalcOutputFrames(&self, in_frames: u32) -> u32 {
        in_frames
    }
}

impl IAudioSystemEffects_Impl for RelayApo_Impl {}

impl IAudioSystemEffects2_Impl for RelayApo_Impl {
    #[allow(clippy::not_unsafe_ptr_arg_deref)] // COM out-pointers, checked.
    /// We expose no user-toggleable effects (Relay's UI owns them), so the
    /// list is empty. Implementing the interface is what matters: the engine
    /// hands a mode-aware APO the richer APOInitSystemEffects2.
    fn GetEffectsList(
        &self,
        ids: *mut *mut GUID,
        count: *mut u32,
        _event: windows::Win32::Foundation::HANDLE,
    ) -> windows::core::Result<()> {
        if ids.is_null() || count.is_null() {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        // SAFETY: checked non-null out-pointers.
        unsafe {
            *ids = std::ptr::null_mut();
            *count = 0;
        }
        crate::diag!("GetEffectsList -> 0 effects");
        Ok(())
    }
}

/// Which Initialize payload the engine sent, by size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitKind {
    /// Too small (or empty): no endpoint identity.
    None,
    V1,
    V2,
    V3,
}

/// Classify an Initialize payload size. v3 is smaller than v2 (one pointer
/// fewer), so match it exactly before the >= v2 test.
pub fn init_kind(size: usize) -> InitKind {
    if size == std::mem::size_of::<APOInitSystemEffects3>() {
        InitKind::V3
    } else if size >= std::mem::size_of::<APOInitSystemEffects2>() {
        InitKind::V2
    } else if size >= std::mem::size_of::<APOInitSystemEffects>() {
        InitKind::V1
    } else {
        InitKind::None
    }
}

fn fmt_desc(mt: Option<&IAudioMediaType>) -> String {
    match mt.map(uncompressed) {
        None => "none".into(),
        Some(Err(e)) => format!("err {:#010x}", e.code().0 as u32),
        Some(Ok(f)) => format!(
            "{:?} ch={} bytes={} bits={} rate={}",
            f.guidFormatType,
            f.dwSamplesPerFrame,
            f.dwBytesPerSampleContainer,
            f.dwValidBitsPerSample,
            f.fFramesPerSecond
        ),
    }
}

fn diag_format(
    what: &str,
    opposite: Option<&IAudioMediaType>,
    requested: Option<&IAudioMediaType>,
    r: &windows::core::Result<IAudioMediaType>,
) {
    crate::diag!(
        "{what} opposite=[{}] requested=[{}] -> {}",
        fmt_desc(opposite),
        fmt_desc(requested),
        match r {
            Ok(_) => "S_OK".to_string(),
            Err(e) => format!("{:#010x}", e.code().0 as u32),
        }
    );
}

/// Interfaces the engine may ask for; used for the CreateInstance probe log
/// and the QI-table test.
pub fn expected_iids() -> [(&'static str, GUID); 6] {
    [
        ("IUnknown", windows::core::IUnknown::IID),
        ("IAudioProcessingObject", IAudioProcessingObject::IID),
        ("IAudioProcessingObjectRT", IAudioProcessingObjectRT::IID),
        ("IAudioProcessingObjectConfiguration", IAudioProcessingObjectConfiguration::IID),
        ("IAudioSystemEffects", IAudioSystemEffects::IID),
        ("IAudioSystemEffects2", IAudioSystemEffects2::IID),
    ]
}

/// QI `obj` for `id`; true on S_OK (the reference is released).
pub fn answers(obj: &windows::core::IUnknown, id: &GUID) -> bool {
    let mut p = std::ptr::null_mut();
    // SAFETY: valid object, out-pointer to a local; a returned reference is
    // adopted and dropped (released).
    unsafe {
        let hr = obj.query(id, &mut p);
        if !p.is_null() {
            drop(windows::core::IUnknown::from_raw(p));
        }
        hr == S_OK
    }
}

impl Drop for RelayApo {
    fn drop(&mut self) {
        // Normally UnlockForProcess ran; make release-order explicit anyway:
        // stop the control thread before the shm mapping goes away.
        self.shared.stop.store(true, Ordering::Release);
        if let Ok(mut l) = self.locked.lock() {
            if let Some(locked) = l.take() {
                if let Ok(g) = self.init.lock() {
                    if let Some(i) = g.as_ref() {
                        if let Some(s) = &i.shm {
                            s.notify();
                        }
                    }
                }
                if let Some(t) = locked.control {
                    let _ = t.join();
                }
            }
        }
        self.shared.publish(std::ptr::null_mut());
        self.shared.block.store(std::ptr::null_mut(), Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Class factory + DLL exports
// ---------------------------------------------------------------------------

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<windows::core::IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if object.is_null() || iid.is_null() {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        // SAFETY: iid checked non-null.
        crate::diag!("CreateInstance iid={:?} outer={}", unsafe { *iid }, outer.is_some());
        if outer.is_some() {
            crate::diag!("CreateInstance -> CLASS_E_NOAGGREGATION");
            return Err(windows::core::Error::from_hresult(CLASS_E_NOAGGREGATION));
        }
        let apo: IAudioProcessingObject = RelayApo::default().into();
        if crate::diag::enabled() {
            let unk: windows::core::IUnknown = apo.cast()?;
            for (name, id) in expected_iids() {
                let ok = answers(&unk, &id);
                crate::diag!("  QI {name} -> {}", if ok { "S_OK" } else { "E_NOINTERFACE" });
            }
        }
        // SAFETY: iid/object checked non-null above.
        let hr = unsafe { apo.query(iid, object) };
        crate::diag!("CreateInstance -> {:#010x}", hr.0 as u32);
        hr.ok()
    }

    fn LockServer(&self, _lock: windows::core::BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

/// audiodg keeps APO DLLs loaded for the life of the engine process; we never
/// volunteer to unload (state-free accounting is not worth the risk).
#[no_mangle]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[no_mangle]
extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null; the engine passes valid GUID pointers.
    unsafe {
        crate::diag!("DllGetClassObject clsid={:?} riid={:?}", *rclsid, *riid);
        if *rclsid != CLSID_RELAY_APO {
            crate::diag!("DllGetClassObject -> CLASS_E_CLASSNOTAVAILABLE");
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        let hr = factory.query(riid, ppv);
        crate::diag!("DllGetClassObject -> {:#010x}", hr.0 as u32);
        hr
    }
}

/// Only the attach is logged (proof audiodg mapped us); no other work under
/// the loader lock.
#[no_mangle]
extern "system" fn DllMain(
    _hinst: *mut core::ffi::c_void,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> windows::core::BOOL {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        crate::diag!(
            "DllMain attach exe={:?} TEMP={:?}",
            std::env::current_exe().ok(),
            std::env::var_os("TEMP")
        );
    }
    true.into()
}
