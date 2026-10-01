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
//!
//! Child APO (S44b). When the installer took an MFX slot that held another
//! APO (Microsoft's "WM audio GFX APO", Realtek's RtkAPO MFX, ...), it
//! recorded that CLSID at [`crate::ids::PKEY_RELAY_CHILD_MFX`] in the FX
//! property store. Initialize reads it from `pAPOSystemEffectsProperties`,
//! CoCreates the child in-process and forwards Initialize (same payload,
//! `APOInit.clsid` patched to the child's own). Format negotiation must
//! satisfy both; LockForProcess locks the child with its input re-pointed
//! at a Relay-owned mid buffer; GetEffectsList is the child's (Relay lists
//! none); GetLatency is the sum.
//!
//! Order: **Relay's EQ first, then the child.** The vendor/Windows effect is
//! typically loudness, room correction or a limiter - an EQ boost belongs
//! in front of the limiter that protects the output, never behind it.
//! Bypass (and "no chain built") = the child alone, straight from the
//! engine's buffers, zero allocations. A child that fails to load,
//! initialize or lock is dropped and Relay runs alone (logged via diag).

// COM plumbing and RT buffer access are inherently unsafe; every block
// carries a SAFETY note.
#![allow(unsafe_code)]
#![allow(non_snake_case)] // COM method names come from the interfaces.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use relay_audio::dsp::Chain;
use relay_audio::shm::{section_name, SharedParams};
use windows::core::{implement, Interface, Ref, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
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
    APOERR_NOT_INITIALIZED, APO_CONNECTION_BUFFER_TYPE_EXTERNAL, APO_CONNECTION_DESCRIPTOR,
    APO_CONNECTION_PROPERTY, APO_FLAG_DEFAULT, APO_REG_PROPERTIES, UNCOMPRESSEDAUDIOFORMAT,
};
use windows::Win32::Media::Audio::PKEY_AudioEndpoint_GUID;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Com::{CoTaskMemAlloc, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::System::Variant::VT_LPWSTR;

/// CLSID of the Relay endpoint APO — the GUID form of [`crate::ids::APO_CLSID`].
pub const CLSID_RELAY_APO: GUID = GUID::from_u128(0x5A8E9C3B_1F6D_4B0A_9C41_7E2D83A6F0B4);

/// [`crate::ids::PKEY_RELAY_CHILD_MFX`] as a property key.
pub const PKEY_RELAY_CHILD_MFX: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x7c3f2a91_5e4d_4b8a_a1f6_3d92c0e4b7a5), pid: 1 };

/// Parse a braced CLSID string (`{8-4-4-4-12}`).
pub fn parse_clsid(s: &str) -> Option<GUID> {
    let s = s.trim();
    if !crate::ids::is_braced_guid(s) {
        return None;
    }
    let hex: String = s[1..37].chars().filter(|c| *c != '-').collect();
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

/// The hosted child APO and the interfaces Relay forwards to.
struct Child {
    apo: IAudioProcessingObject,
    rt: IAudioProcessingObjectRT,
    cfg: IAudioProcessingObjectConfiguration,
    fx2: Option<IAudioSystemEffects2>,
}

impl Child {
    fn new(apo: IAudioProcessingObject) -> windows::core::Result<Self> {
        Ok(Self { rt: apo.cast()?, cfg: apo.cast()?, fx2: apo.cast().ok(), apo })
    }
}

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
    /// True while the child APO is locked for processing (S44b).
    child_live: AtomicBool,
    /// Mid buffer between Relay's chain and the child (owned by `Locked`),
    /// and its length in samples. Null when there is no live child.
    mid: AtomicPtr<f32>,
    mid_len: AtomicUsize,
}

impl RtShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicPtr::new(std::ptr::null_mut()),
            block: AtomicPtr::new(std::ptr::null_mut()),
            rt_busy: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            built_seq: AtomicU32::new(0),
            child_live: AtomicBool::new(false),
            mid: AtomicPtr::new(std::ptr::null_mut()),
            mid_len: AtomicUsize::new(0),
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
    /// The mid buffer the RT path hands the child (S44b); `RtShared::mid`
    /// points into it while the child is live.
    _mid: Option<Box<[f32]>>,
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
    /// Child latency, added to ours while the child is locked.
    child_latency_hns: AtomicI64,
    /// A child handed in before Initialize (tests); otherwise Initialize
    /// creates one from the FX store's record.
    pending_child: Mutex<Option<IAudioProcessingObject>>,
    /// The initialized child. Set once at Initialize; read lock-free by the
    /// RT path.
    child: OnceLock<Child>,
}

impl Default for RelayApo {
    fn default() -> Self {
        Self {
            init: Mutex::new(None),
            locked: Mutex::new(None),
            shared: RtShared::new(),
            latency_hns: AtomicI64::new(0),
            child_latency_hns: AtomicI64::new(0),
            pending_child: Mutex::new(None),
            child: OnceLock::new(),
        }
    }
}

impl RelayApo {
    /// An APO that will host `child` (instead of the CLSID recorded in the
    /// FX store). Initialize still forwards to it; if that fails Relay runs
    /// alone. The in-process tests' way to drive the chain.
    pub fn with_child(child: IAudioProcessingObject) -> Self {
        let apo = Self::default();
        *apo.pending_child.lock().unwrap() = Some(child);
        apo
    }

    /// Create (if needed) and initialize the child. `payload` is the
    /// engine's Initialize bytes; the copy the child gets carries its own
    /// CLSID in `APOInit.clsid` when it is known.
    fn init_child(&self, clsid: Option<GUID>, payload: &[u8]) {
        let pending = self.pending_child.lock().unwrap().take();
        let apo = match (pending, clsid) {
            (Some(apo), _) => apo,
            (None, Some(c)) if c != CLSID_RELAY_APO => {
                // SAFETY: plain COM activation; audiodg's thread has COM up.
                match unsafe {
                    CoCreateInstance::<_, IAudioProcessingObject>(&c, None, CLSCTX_INPROC_SERVER)
                } {
                    Ok(a) => a,
                    Err(e) => {
                        crate::diag!(
                            "child {c:?} CoCreateInstance -> {:#010x}; Relay runs alone",
                            e.code().0 as u32
                        );
                        return;
                    }
                }
            }
            _ => return,
        };
        // Same payload, 8-byte aligned, with the child's own CLSID.
        let mut words = vec![0u64; payload.len().div_ceil(8)];
        // SAFETY: `words` holds at least payload.len() bytes.
        let bytes = unsafe {
            std::ptr::copy_nonoverlapping(
                payload.as_ptr(),
                words.as_mut_ptr() as *mut u8,
                payload.len(),
            );
            std::slice::from_raw_parts(words.as_ptr() as *const u8, payload.len())
        };
        if let Some(c) = clsid {
            // APOInitBaseStruct: cbSize (u32) then clsid (GUID) at offset 4.
            if payload.len() >= 4 + std::mem::size_of::<GUID>() {
                // SAFETY: in bounds (checked); unaligned write of a POD.
                unsafe { (words.as_mut_ptr() as *mut u8).add(4).cast::<GUID>().write_unaligned(c) };
            }
        }
        // SAFETY: the payload's interface pointers are borrowed for the
        // call exactly as the engine lent them to us.
        let r = unsafe { apo.Initialize(bytes) }.and_then(|()| Child::new(apo));
        match r {
            Ok(child) => {
                let _ = self.child.set(child);
                crate::diag!("child {clsid:?} initialized; chained after Relay");
            }
            Err(e) => {
                crate::diag!(
                    "child {clsid:?} Initialize -> {:#010x}; Relay runs alone",
                    e.code().0 as u32
                )
            }
        }
    }

    /// Unlock a live child and forget the mid buffer (the caller drops the
    /// `Locked` that owns it afterwards). Not on the RT path.
    fn unlock_child(&self) {
        if self.shared.child_live.swap(false, Ordering::AcqRel) {
            if let Some(c) = self.child.get() {
                // SAFETY: plain COM call; the engine is not processing.
                let _ = unsafe { c.cfg.UnlockForProcess() };
            }
        }
        self.shared.mid.store(std::ptr::null_mut(), Ordering::Release);
        self.shared.mid_len.store(0, Ordering::Release);
        self.child_latency_hns.store(0, Ordering::Release);
    }

    /// Ask the child whether it takes `requested` unchanged. Relay's format
    /// is 1:1, so the child sees the same format on both sides.
    fn child_agrees(
        &self,
        output: bool,
        opposite: Option<&IAudioMediaType>,
        requested: &IAudioMediaType,
    ) -> windows::core::Result<()> {
        let Some(c) = self.child.get() else { return Ok(()) };
        // SAFETY: valid interface pointers.
        let r = unsafe {
            if output {
                c.apo.IsOutputFormatSupported(opposite, requested)
            } else {
                c.apo.IsInputFormatSupported(opposite, requested)
            }
        };
        let refuse = || windows::core::Error::from_hresult(APOERR_FORMAT_NOT_SUPPORTED);
        let got = uncompressed(&r.map_err(|_| refuse())?)?;
        let want = uncompressed(requested)?;
        let same = got.guidFormatType == want.guidFormatType
            && got.dwSamplesPerFrame == want.dwSamplesPerFrame
            && got.dwBytesPerSampleContainer == want.dwBytesPerSampleContainer
            && got.dwValidBitsPerSample == want.dwValidBitsPerSample
            && got.fFramesPerSecond == want.fFramesPerSecond;
        if same {
            Ok(())
        } else {
            Err(refuse())
        }
    }

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
        if let Some(c) = self.child.get() {
            // SAFETY: plain COM call.
            let _ = unsafe { c.apo.Reset() };
        }
        Ok(())
    }

    fn GetLatency(&self) -> windows::core::Result<i64> {
        Ok(self.latency_hns.load(Ordering::Acquire)
            + self.child_latency_hns.load(Ordering::Acquire))
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

    // COM ABI: raw payload pointer behind a safe trait; the engine
    // guarantees cbdatasize readable bytes.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
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
        let mut child_clsid: Option<GUID> = None;
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
            let (store, fx_store) = unsafe {
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
                (base.pAPOEndpointProperties.as_ref(), base.pAPOSystemEffectsProperties.as_ref())
            };
            // S44b: the FX store (this endpoint's FxProperties) records the
            // APO Relay displaced, if any.
            if let Some(fx) = fx_store {
                // SAFETY: as below - a PROPVARIANT read out and cleared.
                unsafe {
                    if let Ok(mut v) = fx.GetValue(&PKEY_RELAY_CHILD_MFX) {
                        let inner = &v.Anonymous.Anonymous;
                        if inner.vt == VT_LPWSTR {
                            let ws: PCWSTR = PCWSTR(inner.Anonymous.pwszVal.0);
                            if !ws.is_null() {
                                child_clsid = ws.to_string().ok().and_then(|s| parse_clsid(&s));
                            }
                        }
                        let _ = PropVariantClear(&mut v);
                    }
                }
            }
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
        crate::diag!("Initialize child={child_clsid:?}");
        let payload: &[u8] = if pbydata.is_null() {
            &[]
        } else {
            // SAFETY: the engine hands us cbdatasize readable bytes.
            unsafe { std::slice::from_raw_parts(pbydata, cbdatasize as usize) }
        };
        self.init_child(child_clsid, payload);
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
        let r = negotiate(opposite, requested)
            .and_then(|m| self.child_agrees(false, opposite, &m).map(|()| m));
        diag_format("IsInputFormatSupported", opposite, requested, &r);
        r
    }

    fn IsOutputFormatSupported(
        &self,
        opposite: Ref<IAudioMediaType>,
        requested: Ref<IAudioMediaType>,
    ) -> windows::core::Result<IAudioMediaType> {
        let (opposite, requested) = (opposite.as_ref(), requested.as_ref());
        let r = negotiate(opposite, requested)
            .and_then(|m| self.child_agrees(true, opposite, &m).map(|()| m));
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

        // S44b: lock the child with its input re-pointed at our mid buffer
        // and the engine's output descriptor as its own.
        let mut mid_buf: Option<Box<[f32]>> = None;
        self.shared.child_live.store(false, Ordering::Release);
        self.child_latency_hns.store(0, Ordering::Release);
        if let Some(c) = self.child.get() {
            let mut buf = vec![0.0f32; 2 * max_frames].into_boxed_slice();
            // SAFETY: bitwise copy of the engine's descriptor; its media
            // type is ManuallyDrop, so no reference is taken or released.
            let mut d: APO_CONNECTION_DESCRIPTOR = unsafe { std::ptr::read(*in_desc) };
            d.Type = APO_CONNECTION_BUFFER_TYPE_EXTERNAL;
            d.pBuffer = buf.as_mut_ptr() as usize;
            d.u32MaxFrameCount = max_frames as u32;
            let dp: *const APO_CONNECTION_DESCRIPTOR = &d;
            // SAFETY: one input descriptor (ours, alive for the call) and
            // the engine's output descriptor array.
            let r = unsafe {
                c.cfg.LockForProcess(&[dp], std::slice::from_raw_parts(out_desc, num_out as usize))
            };
            match r {
                Ok(()) => {
                    // SAFETY: plain COM call.
                    let lat = unsafe { c.apo.GetLatency() }.unwrap_or(0);
                    self.child_latency_hns.store(lat, Ordering::Release);
                    self.shared.mid.store(buf.as_mut_ptr(), Ordering::Release);
                    self.shared.mid_len.store(buf.len(), Ordering::Release);
                    self.shared.child_live.store(true, Ordering::Release);
                    mid_buf = Some(buf);
                    crate::diag!("child LockForProcess -> S_OK latency={lat}");
                }
                Err(e) => {
                    crate::diag!(
                        "child LockForProcess -> {:#010x}; Relay runs alone",
                        e.code().0 as u32
                    );
                }
            }
        }

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
        *self.locked.lock().unwrap() = Some(Locked { control, _mid: mid_buf });
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
        // The mid buffer (in `locked`) drops at scope end, after this.
        self.unlock_child();
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
            let child = if self.shared.child_live.load(Ordering::Acquire) {
                self.child.get()
            } else {
                None
            };
            let mid = self.shared.mid.load(Ordering::Acquire);
            let mid_ok = !mid.is_null() && n <= self.shared.mid_len.load(Ordering::Acquire);
            if let (Some(c), true) = (child, bypass || chain.is_null()) {
                // Bypass: the child alone, on the engine's own buffers.
                c.rt.APOProcess(num_in, in_conn, num_out, out_conn);
            } else if let (Some(c), true) = (child, mid_ok) {
                // Relay's EQ into the mid buffer, then the child to output.
                let rt = &mut *chain;
                let mid_s = std::slice::from_raw_parts_mut(mid, n);
                let step = rt.max_block * 2;
                let mut off = 0;
                while off < n {
                    let end = (off + step).min(n);
                    rt.chain.process(&src[off..end], &mut mid_s[off..end]);
                    off = end;
                }
                let mid_c = APO_CONNECTION_PROPERTY {
                    pBuffer: mid as usize,
                    u32ValidFrameCount: frames as u32,
                    u32BufferFlags: input.u32BufferFlags,
                    u32Signature: input.u32Signature,
                };
                let mid_p: *const APO_CONNECTION_PROPERTY = &mid_c;
                c.rt.APOProcess(1, &mid_p, num_out, out_conn);
            } else if let Some(c) = child {
                // A block larger than the lock promised: keep the child's
                // effect rather than ours.
                c.rt.APOProcess(num_in, in_conn, num_out, out_conn);
            } else if bypass || chain.is_null() {
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
            if child.is_none() {
                // (A child sets the output connection itself.)
                output.u32ValidFrameCount = frames as u32;
                output.u32BufferFlags = input.u32BufferFlags;
            }
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
        // S44b: Relay lists none of its own, so the merged list is the
        // child's (its effects stay visible to Windows' UI).
        if let Some(fx2) = self.child.get().and_then(|c| c.fx2.as_ref()) {
            // SAFETY: the caller's checked out-pointers, passed straight on.
            let r = unsafe { fx2.GetEffectsList(ids, count, _event) };
            crate::diag!("GetEffectsList -> child's ({})", r.is_ok());
            return r;
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
        self.unlock_child();
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
