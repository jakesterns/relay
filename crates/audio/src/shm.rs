//! The APO parameter block: a named shared-memory section the core writes and
//! the APO reads lock-free on the audio engine's real-time thread.
//!
//! Layout contract (brief: "bypass flag is the first word"):
//! - word 0: `bypass` — 1 = pass-through. Read directly every block, so it
//!   takes effect on the next `APOProcess` call with no handshake.
//! - words 1–3: magic / layout version / seqlock sequence.
//! - the rest: [`PodParams`], a fixed-size POD image of [`ChainParams`],
//!   guarded by the seqlock (`seq` odd = write in progress; reader retries,
//!   and keeps its previous parameters when a writer is mid-update).
//!
//! A named auto-reset event (section name + [`EVENT_SUFFIX`]) is signalled
//! after every parameter write; the APO's control thread waits on it and
//! rebuilds the DSP chain off the real-time path.
//!
//! Namespace note: audiodg.exe hosts APOs in session 0, so a section shared
//! with the user's core must live under `Global\`. LOCAL SERVICE holds
//! `SeCreateGlobalPrivilege`, the interactive core does not — therefore the
//! *APO* creates the section (granting the interactive group read/write) and
//! the core opens it, retrying until the endpoint's APO first runs. Tests use
//! `Local\` names inside one session.

// Raw shared memory and seqlock publication require pointer volatility and
// atomics over foreign-owned memory; every unsafe block carries a SAFETY note.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};

use crate::params::{BandParams, ChainParams, FilterKind, LimiterParams, MAX_BANDS};

/// "RAPO" little-endian.
pub const MAGIC: u32 = u32::from_le_bytes(*b"RAPO");
/// Bump on any layout change; readers refuse other versions.
pub const VERSION: u32 = 1;
/// Appended to the section name to form the params-changed event name.
pub const EVENT_SUFFIX: &str = ".evt";

/// Section name for one endpoint, shared between core and APO.
/// `instance` is the `RELAY_INSTANCE` suffix ("" in production).
pub fn section_name(endpoint_guid: &str, instance: &str) -> String {
    // The GUID is registry-derived: braces/hex/dashes only, safe in an object name.
    if instance.is_empty() {
        format!("Global\\Relay.APO.{endpoint_guid}")
    } else {
        format!("Global\\Relay.APO.{instance}.{endpoint_guid}")
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PodBand {
    /// 0 Peaking, 1 LowShelf, 2 HighShelf, 3 LowPass, 4 HighPass.
    pub kind: u32,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
    pub enabled: u32,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PodLimiter {
    pub present: u32,
    pub below_hz: f32,
    pub threshold_db: f32,
    pub lookahead_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
}

/// Fixed-size POD image of [`ChainParams`]. No pointers, no padding surprises:
/// every field is 4 bytes.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PodParams {
    pub n_bands: u32,
    pub bands: [PodBand; MAX_BANDS],
    pub limiter: PodLimiter,
    pub hrtf: u32,
}

impl Default for PodParams {
    fn default() -> Self {
        encode(&ChainParams::default())
    }
}

/// The block at offset 0 of the section.
#[repr(C)]
pub struct ParamBlock {
    /// First word of the section: 1 = pass-through, effective next block.
    bypass: AtomicU32,
    magic: AtomicU32,
    version: AtomicU32,
    /// Seqlock over `params`: odd while a writer is inside.
    seq: AtomicU32,
    params: std::cell::UnsafeCell<PodParams>,
}

// SAFETY: cross-thread access to `params` is mediated by the seqlock; all
// other fields are atomics.
unsafe impl Sync for ParamBlock {}

pub const BLOCK_SIZE: usize = std::mem::size_of::<ParamBlock>();

/// [`ChainParams`] → POD. Bands beyond [`MAX_BANDS`] are dropped (the DSP
/// would refuse them anyway).
pub fn encode(p: &ChainParams) -> PodParams {
    let mut bands =
        [PodBand { kind: 0, freq_hz: 0.0, gain_db: 0.0, q: 0.0, enabled: 0 }; MAX_BANDS];
    let n = p.bands.len().min(MAX_BANDS);
    for (dst, src) in bands.iter_mut().zip(p.bands.iter().take(n)) {
        *dst = PodBand {
            kind: match src.kind {
                FilterKind::Peaking => 0,
                FilterKind::LowShelf => 1,
                FilterKind::HighShelf => 2,
                FilterKind::LowPass => 3,
                FilterKind::HighPass => 4,
            },
            freq_hz: src.freq_hz,
            gain_db: src.gain_db,
            q: src.q,
            enabled: src.enabled as u32,
        };
    }
    let limiter = match &p.limiter {
        Some(l) => PodLimiter {
            present: 1,
            below_hz: l.below_hz,
            threshold_db: l.threshold_db,
            lookahead_ms: l.lookahead_ms,
            release_ms: l.release_ms,
            knee_db: l.knee_db,
        },
        None => PodLimiter {
            present: 0,
            below_hz: 0.0,
            threshold_db: 0.0,
            lookahead_ms: 0.0,
            release_ms: 0.0,
            knee_db: 0.0,
        },
    };
    PodParams { n_bands: n as u32, bands, limiter, hrtf: p.hrtf as u32 }
}

/// POD → [`ChainParams`]. Unknown filter kinds decode as disabled peaking
/// bands rather than failing — a newer writer must not brick an older APO.
pub fn decode(p: &PodParams) -> ChainParams {
    let n = (p.n_bands as usize).min(MAX_BANDS);
    let bands = p.bands[..n]
        .iter()
        .map(|b| BandParams {
            kind: match b.kind {
                1 => FilterKind::LowShelf,
                2 => FilterKind::HighShelf,
                3 => FilterKind::LowPass,
                4 => FilterKind::HighPass,
                _ => FilterKind::Peaking,
            },
            freq_hz: b.freq_hz,
            gain_db: b.gain_db,
            q: b.q,
            enabled: b.enabled != 0 && b.kind <= 4,
        })
        .collect();
    let limiter = (p.limiter.present != 0).then(|| LimiterParams {
        below_hz: p.limiter.below_hz,
        threshold_db: p.limiter.threshold_db,
        lookahead_ms: p.limiter.lookahead_ms,
        release_ms: p.limiter.release_ms,
        knee_db: p.limiter.knee_db,
    });
    ChainParams { bands, limiter, hrtf: p.hrtf != 0 }
}

impl ParamBlock {
    /// Initialise a freshly mapped (zeroed) block. Bypass starts *on*: until
    /// the core writes real parameters the APO must be a straight wire.
    pub fn init(&self) {
        self.bypass.store(1, Ordering::Release);
        // SAFETY: called once by the section creator before the name is
        // published to any reader.
        unsafe { *self.params.get() = PodParams::default() };
        self.seq.store(0, Ordering::Release);
        self.version.store(VERSION, Ordering::Release);
        // Magic last: a reader that sees it can trust the rest.
        self.magic.store(MAGIC, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        self.magic.load(Ordering::Acquire) == MAGIC
            && self.version.load(Ordering::Acquire) == VERSION
    }

    pub fn bypass(&self) -> bool {
        self.bypass.load(Ordering::Acquire) != 0
    }

    pub fn set_bypass(&self, on: bool) {
        self.bypass.store(on as u32, Ordering::Release);
    }

    /// Publish new parameters (single-writer: the core).
    pub fn write_params(&self, p: &ChainParams) {
        let pod = encode(p);
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Release); // odd: in progress
                                                              // SAFETY: single writer; readers seeing an odd/changed seq discard.
        unsafe { std::ptr::write_volatile(self.params.get(), pod) };
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// Seqlock read. Returns `None` when a writer is mid-update after a few
    /// retries (real-time caller keeps its previous parameters) or when the
    /// block is not valid.
    pub fn read_params(&self) -> Option<(u32, ChainParams)> {
        if !self.is_valid() {
            return None;
        }
        for _ in 0..4 {
            let s0 = self.seq.load(Ordering::Acquire);
            if s0 & 1 != 0 {
                continue;
            }
            // SAFETY: torn reads are possible and are detected by the second
            // sequence load below; a torn PodParams is discarded unread.
            let pod = unsafe { std::ptr::read_volatile(self.params.get()) };
            if self.seq.load(Ordering::Acquire) == s0 {
                return Some((s0, decode(&pod)));
            }
        }
        None
    }

    /// The sequence number of the last published write (even), without
    /// reading the body. Lets the RT path skip work when nothing changed.
    pub fn sequence(&self) -> u32 {
        self.seq.load(Ordering::Acquire)
    }
}

#[cfg(windows)]
pub use win::SharedParams;

#[cfg(windows)]
mod win {
    //! The named section + event. The creator (normally the APO inside
    //! audiodg) grants the interactive group read/write so the user's core
    //! can steer it; everyone else gets nothing.

    use windows::core::{Owned, PCWSTR};
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ,
        FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, OpenEventW, SetEvent, EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
    };

    use super::{ParamBlock, BLOCK_SIZE, EVENT_SUFFIX};

    /// SYSTEM, service accounts (audiodg runs as LOCAL SERVICE) and the
    /// interactive group get full access; nobody else is in the DACL. The
    /// section only ever holds EQ parameters.
    const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;IU)";

    pub struct SharedParams {
        // Order matters: the view must unmap before the mapping handle closes.
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        _mapping: Owned<windows::Win32::Foundation::HANDLE>,
        event: Owned<windows::Win32::Foundation::HANDLE>,
        /// True when this side created (and initialised) the section.
        pub created: bool,
    }

    // SAFETY: the view is only dereferenced through &ParamBlock, whose
    // cross-thread rules the seqlock enforces; handles are thread-safe.
    unsafe impl Send for SharedParams {}
    unsafe impl Sync for SharedParams {}

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl SharedParams {
        /// Create the section + event (APO side, or tests). Initialises the
        /// block when the section is new.
        pub fn create(name: &str) -> windows::core::Result<Self> {
            // SAFETY: SDDL string is static and valid; the descriptor is
            // freed by LocalFree via Owned semantics — we leak it instead
            // (one small allocation per process lifetime) to keep this
            // dependency-free.
            let sd = unsafe {
                let mut psd = PSECURITY_DESCRIPTOR::default();
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(wide(SDDL).as_ptr()),
                    SDDL_REVISION_1,
                    &mut psd,
                    None,
                )?;
                psd
            };
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            let wname = wide(name);
            // SAFETY: valid security attributes and a NUL-terminated name.
            let (mapping, created) = unsafe {
                let h = CreateFileMappingW(
                    windows::Win32::Foundation::INVALID_HANDLE_VALUE,
                    Some(&sa),
                    PAGE_READWRITE,
                    0,
                    BLOCK_SIZE as u32,
                    PCWSTR(wname.as_ptr()),
                )?;
                (Owned::new(h), GetLastError() != ERROR_ALREADY_EXISTS)
            };
            // SAFETY: mapping is a valid section handle sized BLOCK_SIZE.
            let view = unsafe {
                MapViewOfFile(*mapping, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, BLOCK_SIZE)
            };
            if view.Value.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            let wevt = wide(&format!("{name}{EVENT_SUFFIX}"));
            // SAFETY: same security attributes; auto-reset event.
            let event = unsafe {
                Owned::new(CreateEventW(Some(&sa), false, false, PCWSTR(wevt.as_ptr()))?)
            };
            let me = Self { view, _mapping: mapping, event, created };
            if created {
                me.block().init();
            }
            Ok(me)
        }

        /// Open an existing section + event (core side). Fails until the
        /// APO has created it.
        pub fn open(name: &str) -> windows::core::Result<Self> {
            let wname = wide(name);
            // SAFETY: NUL-terminated name; access checked against the DACL.
            let mapping = unsafe {
                Owned::new(OpenFileMappingW(
                    (FILE_MAP_READ | FILE_MAP_WRITE).0,
                    false,
                    PCWSTR(wname.as_ptr()),
                )?)
            };
            // SAFETY: as in `create`.
            let view = unsafe {
                MapViewOfFile(*mapping, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, BLOCK_SIZE)
            };
            if view.Value.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            let wevt = wide(&format!("{name}{EVENT_SUFFIX}"));
            // SAFETY: NUL-terminated name.
            let event = unsafe {
                Owned::new(OpenEventW(
                    EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
                    false,
                    PCWSTR(wevt.as_ptr()),
                )?)
            };
            Ok(Self { view, _mapping: mapping, event, created: false })
        }

        pub fn block(&self) -> &ParamBlock {
            // SAFETY: the view is at least BLOCK_SIZE, page-aligned (so
            // aligned for ParamBlock), and lives as long as `self`.
            unsafe { &*(self.view.Value as *const ParamBlock) }
        }

        /// Signal the params-changed event after a write.
        pub fn notify(&self) {
            // SAFETY: `event` is a valid event handle.
            let _ = unsafe { SetEvent(*self.event) };
        }

        /// The raw event handle, for the APO's control-thread wait.
        pub fn event_handle(&self) -> windows::Win32::Foundation::HANDLE {
            *self.event
        }
    }

    impl Drop for SharedParams {
        fn drop(&mut self) {
            // SAFETY: view was returned by MapViewOfFile and not yet unmapped.
            let _ = unsafe { UnmapViewOfFile(self.view) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{BandParams, ChainParams, LimiterParams};

    fn sample_params() -> ChainParams {
        ChainParams {
            bands: vec![
                BandParams::peaking(1000.0, 3.5, 1.4),
                BandParams::peaking(80.0, -2.0, 0.7),
            ],
            limiter: Some(LimiterParams::new(150.0, -12.0)),
            hrtf: true,
        }
    }

    #[test]
    fn encode_decode_round_trip() {
        let p = sample_params();
        assert_eq!(decode(&encode(&p)), p);
        let empty = ChainParams::default();
        assert_eq!(decode(&encode(&empty)), empty);
    }

    #[test]
    fn block_layout_bypass_is_first_word_and_pod_sized() {
        assert_eq!(std::mem::offset_of!(ParamBlock, bypass), 0);
        // Everything is u32/f32: no hidden padding in the POD image.
        assert_eq!(std::mem::size_of::<PodParams>(), 4 * (1 + 5 * MAX_BANDS + 6 + 1),);
    }

    #[test]
    fn seqlock_write_then_read() {
        let block = ParamBlock {
            bypass: AtomicU32::new(0),
            magic: AtomicU32::new(0),
            version: AtomicU32::new(0),
            seq: AtomicU32::new(0),
            params: std::cell::UnsafeCell::new(PodParams::default()),
        };
        block.init();
        assert!(block.bypass(), "bypass must start on");
        assert!(block.is_valid());

        let p = sample_params();
        block.write_params(&p);
        let (seq, read) = block.read_params().expect("read after write");
        assert_eq!(read, p);
        assert_eq!(seq, 2);
        block.set_bypass(false);
        assert!(!block.bypass());
    }

    #[test]
    fn reader_rejects_invalid_block() {
        let block = ParamBlock {
            bypass: AtomicU32::new(0),
            magic: AtomicU32::new(0), // never initialised
            version: AtomicU32::new(0),
            seq: AtomicU32::new(0),
            params: std::cell::UnsafeCell::new(PodParams::default()),
        };
        assert!(block.read_params().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn shared_section_create_open_and_signal() {
        // Local\ namespace: same-session test does not need
        // SeCreateGlobalPrivilege.
        let name = format!("Local\\Relay.APO.test.{}", std::process::id());
        let creator = SharedParams::create(&name).expect("create");
        assert!(creator.created);
        assert!(creator.block().bypass());

        let opener = SharedParams::open(&name).expect("open");
        let p = sample_params();
        opener.block().write_params(&p);
        opener.block().set_bypass(false);
        opener.notify();

        let (_, seen) = creator.block().read_params().expect("read");
        assert_eq!(seen, p);
        assert!(!creator.block().bypass());
    }
}
