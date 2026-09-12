//! The camera frame ring: a named shared-memory section the receiver writes
//! (one NV12 frame per slot) and the camera media source reads lock-free.
//!
//! Layout contract:
//! - header: magic / layout version / `latest` (slot index of the newest
//!   complete frame, [`NO_FRAME`] until the first write) / `write_idx`
//!   (single-writer rotation state) / `frames` (total written, diagnostics).
//! - [`SLOTS`] slots, each `{ seq, width, height, pts_100ns, data }` with a
//!   per-slot seqlock (`seq` odd = write in progress; a reader that sees the
//!   sequence change mid-copy discards and retries on the then-latest slot).
//! - slot data is tightly packed NV12 (Y plane then interleaved UV), sized
//!   for 4K; smaller frames use a prefix.
//!
//! Namespace note (same as `relay_audio::shm`): the Frame Server service
//! hosts the media source as LOCAL SERVICE, which holds
//! `SeCreateGlobalPrivilege`; the interactive receiver does not. Whichever
//! side arrives first *tries* to create the section — in production that
//! only succeeds for the media source, and the receiver's create call opens
//! the existing section instead (or fails and retries until the camera is
//! up). Tests use `Local\` names inside one session.

// Raw shared memory and seqlock publication require pointer volatility and
// atomics over foreign-owned memory; every unsafe block carries a SAFETY
// note.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};

/// "RCAM" little-endian.
pub const MAGIC: u32 = u32::from_le_bytes(*b"RCAM");
/// Bump on any layout change; readers refuse other versions.
pub const VERSION: u32 = 1;
/// Slots in the ring: the writer never touches `latest`, so a reader always
/// has a stable slot plus one in flight plus one spare.
pub const SLOTS: usize = 3;
/// Largest frame the ring carries (4K NV12).
pub const MAX_WIDTH: u32 = 3840;
pub const MAX_HEIGHT: u32 = 2160;
/// Tightly packed NV12 bytes for a full 4K frame.
pub const SLOT_BYTES: usize = (MAX_WIDTH as usize * MAX_HEIGHT as usize) * 3 / 2;
/// `latest` value before the first frame.
pub const NO_FRAME: u32 = u32::MAX;

/// Section name shared between the receiver and the media source.
/// `instance` is the `RELAY_INSTANCE` suffix ("" in production).
pub fn section_name(instance: &str) -> String {
    if instance.is_empty() {
        "Global\\Relay.Cam".to_owned()
    } else {
        format!("Global\\Relay.Cam.{instance}")
    }
}

/// The production section name after applying the `RELAY_INSTANCE` and
/// `RELAY_VCAM_LOCAL_SECTION` (tests: `Local\` needs no privilege)
/// environment overrides. Both sides call this so the names always agree.
pub fn section_name_from_env() -> String {
    let instance = std::env::var("RELAY_INSTANCE").unwrap_or_default();
    let name = section_name(&instance);
    if std::env::var("RELAY_VCAM_LOCAL_SECTION").is_ok() {
        name.replacen("Global\\", "Local\\", 1)
    } else {
        name
    }
}

/// Dimensions and timestamp of one frame in the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub pts_100ns: i64,
    /// The slot sequence the frame was read at; compare to skip duplicates.
    pub seq: u64,
}

#[repr(C)]
struct Slot {
    /// Seqlock: odd while the writer is inside.
    seq: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
    _pad: u32,
    pts_100ns: std::cell::UnsafeCell<i64>,
    data: std::cell::UnsafeCell<[u8; SLOT_BYTES]>,
}

/// The block at offset 0 of the section.
#[repr(C)]
pub struct FrameBlock {
    magic: AtomicU32,
    version: AtomicU32,
    /// Slot index of the newest complete frame; [`NO_FRAME`] until then.
    latest: AtomicU32,
    /// Single-writer rotation cursor (next slot to write).
    write_idx: AtomicU32,
    /// Total frames written (diagnostics / liveness).
    frames: AtomicU32,
    _pad: [u32; 3],
    slots: [Slot; SLOTS],
}

// SAFETY: cross-process access to each slot body is mediated by its seqlock;
// all other fields are atomics.
unsafe impl Sync for FrameBlock {}

pub const BLOCK_SIZE: usize = std::mem::size_of::<FrameBlock>();

impl FrameBlock {
    /// Initialise a freshly mapped (zeroed) block. Magic is stored last so a
    /// reader that sees it can trust the rest.
    pub fn init(&self) {
        self.latest.store(NO_FRAME, Ordering::Release);
        self.write_idx.store(0, Ordering::Release);
        self.frames.store(0, Ordering::Release);
        self.version.store(VERSION, Ordering::Release);
        self.magic.store(MAGIC, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        self.magic.load(Ordering::Acquire) == MAGIC
            && self.version.load(Ordering::Acquire) == VERSION
    }

    /// Total frames written so far (0 until the receiver produces one).
    pub fn frame_count(&self) -> u32 {
        self.frames.load(Ordering::Acquire)
    }

    /// Write one tightly-packing NV12 frame from strided planes (as mapped
    /// from a D3D11 staging texture). `y` must cover `y_stride * height`
    /// bytes and `uv` must cover `uv_stride * height / 2`. Frames larger
    /// than 4K or with odd dimensions are rejected.
    pub fn write_frame(
        &self,
        width: u32,
        height: u32,
        pts_100ns: i64,
        y: &[u8],
        y_stride: usize,
        uv: &[u8],
        uv_stride: usize,
    ) -> bool {
        if width == 0
            || height == 0
            || width > MAX_WIDTH
            || height > MAX_HEIGHT
            || width % 2 != 0
            || height % 2 != 0
            || y_stride < width as usize
            || uv_stride < width as usize
            || y.len() < y_stride * height as usize
            || uv.len() < uv_stride * (height as usize / 2)
        {
            return false;
        }
        let idx = self.write_idx.load(Ordering::Relaxed) as usize % SLOTS;
        // Never overwrite the slot a reader may be copying from.
        let idx = if idx as u32 == self.latest.load(Ordering::Acquire) {
            (idx + 1) % SLOTS
        } else {
            idx
        };
        let slot = &self.slots[idx];
        let s = slot.seq.load(Ordering::Relaxed);
        slot.seq.store(s.wrapping_add(1), Ordering::Release); // odd: in progress
        slot.width.store(width, Ordering::Relaxed);
        slot.height.store(height, Ordering::Relaxed);
        // SAFETY: single writer; readers seeing an odd/changed seq discard.
        unsafe {
            *slot.pts_100ns.get() = pts_100ns;
            let dst = (*slot.data.get()).as_mut_ptr();
            let w = width as usize;
            for row in 0..height as usize {
                std::ptr::copy_nonoverlapping(
                    y.as_ptr().add(row * y_stride),
                    dst.add(row * w),
                    w,
                );
            }
            let uv_base = w * height as usize;
            for row in 0..height as usize / 2 {
                std::ptr::copy_nonoverlapping(
                    uv.as_ptr().add(row * uv_stride),
                    dst.add(uv_base + row * w),
                    w,
                );
            }
        }
        slot.seq.store(s.wrapping_add(2), Ordering::Release);
        self.write_idx.store((idx as u32 + 1) % SLOTS as u32, Ordering::Relaxed);
        self.latest.store(idx as u32, Ordering::Release);
        self.frames.fetch_add(1, Ordering::AcqRel);
        true
    }

    /// Copy the newest complete frame into `out` (which must hold
    /// [`SLOT_BYTES`]; only `width * height * 3 / 2` bytes are meaningful).
    /// Returns `None` when there is no frame yet, the block is invalid, or a
    /// writer kept lapping us for four attempts.
    pub fn read_latest(&self, out: &mut [u8]) -> Option<FrameInfo> {
        if !self.is_valid() || out.len() < SLOT_BYTES {
            return None;
        }
        for _ in 0..4 {
            let idx = self.latest.load(Ordering::Acquire);
            if idx == NO_FRAME {
                return None;
            }
            let slot = &self.slots[idx as usize % SLOTS];
            let s0 = slot.seq.load(Ordering::Acquire);
            if s0 & 1 != 0 {
                continue;
            }
            let width = slot.width.load(Ordering::Relaxed);
            let height = slot.height.load(Ordering::Relaxed);
            if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
                return None;
            }
            let bytes = (width as usize * height as usize) * 3 / 2;
            // SAFETY: torn reads are possible and are detected by the second
            // sequence load below; a torn frame is discarded.
            let pts = unsafe {
                std::ptr::copy_nonoverlapping(
                    (*slot.data.get()).as_ptr(),
                    out.as_mut_ptr(),
                    bytes,
                );
                std::ptr::read_volatile(slot.pts_100ns.get())
            };
            if slot.seq.load(Ordering::Acquire) == s0 {
                return Some(FrameInfo {
                    width,
                    height,
                    pts_100ns: pts,
                    seq: ((idx as u64) << 32) | s0 as u64,
                });
            }
        }
        None
    }
}

#[cfg(windows)]
pub use win::SharedFrames;

#[cfg(windows)]
mod win {
    //! The named section. SYSTEM, service accounts (the Frame Server runs as
    //! LOCAL SERVICE) and the interactive group get full access; nobody else
    //! is in the DACL. The section only ever holds video frames.

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

    use super::{FrameBlock, BLOCK_SIZE};

    const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;IU)";

    pub struct SharedFrames {
        // Order matters: the view must unmap before the mapping handle closes.
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        _mapping: Owned<windows::Win32::Foundation::HANDLE>,
        /// True when this side created (and initialised) the section.
        pub created: bool,
    }

    // SAFETY: the view is only dereferenced through &FrameBlock, whose
    // cross-process rules the seqlocks enforce; handles are thread-safe.
    unsafe impl Send for SharedFrames {}
    unsafe impl Sync for SharedFrames {}

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    impl SharedFrames {
        /// Create the section, or map it if it already exists (the create
        /// call opens an existing section of the same name). Initialises the
        /// block only when the section is new. In production only LOCAL
        /// SERVICE can *create* the `Global\` name; the receiver's create
        /// fails until the media source is up — callers retry.
        pub fn create(name: &str) -> windows::core::Result<Self> {
            // SAFETY: SDDL string is static and valid; the descriptor is
            // intentionally leaked (one small allocation per process).
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
            Self::map(mapping, created)
        }

        /// Open an existing section (never creates). Fails until the other
        /// side has created it.
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
            Self::map(mapping, false)
        }

        fn map(
            mapping: Owned<windows::Win32::Foundation::HANDLE>,
            created: bool,
        ) -> windows::core::Result<Self> {
            // SAFETY: mapping is a valid section handle sized BLOCK_SIZE.
            let view = unsafe {
                MapViewOfFile(*mapping, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, BLOCK_SIZE)
            };
            if view.Value.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            let me = Self { view, _mapping: mapping, created };
            if created {
                me.block().init();
            }
            Ok(me)
        }

        pub fn block(&self) -> &FrameBlock {
            // SAFETY: the view is at least BLOCK_SIZE, page-aligned (so
            // aligned for FrameBlock), and lives as long as `self`.
            unsafe { &*(self.view.Value as *const FrameBlock) }
        }
    }

    impl Drop for SharedFrames {
        fn drop(&mut self) {
            // SAFETY: view was returned by MapViewOfFile, not yet unmapped.
            let _ = unsafe { UnmapViewOfFile(self.view) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_frame(w: u32, h: u32, tag: u8) -> Vec<u8> {
        let mut v = vec![0u8; (w as usize * h as usize) * 3 / 2];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(tag);
        }
        v
    }

    // Heap-backed block for pure-layout tests (too big for the stack).
    fn heap_block() -> Box<FrameBlock> {
        // SAFETY: FrameBlock is all atomics/UnsafeCell over POD; the zeroed
        // image is exactly the fresh-section state init() expects.
        let b: Box<FrameBlock> = unsafe {
            Box::from_raw(std::alloc::alloc_zeroed(std::alloc::Layout::new::<FrameBlock>())
                as *mut FrameBlock)
        };
        b.init();
        b
    }

    #[test]
    fn header_layout_magic_first() {
        assert_eq!(std::mem::offset_of!(FrameBlock, magic), 0);
        assert!(BLOCK_SIZE > SLOTS * SLOT_BYTES);
    }

    #[test]
    fn write_then_read_round_trip() {
        let block = heap_block();
        assert!(block.is_valid());
        let mut out = vec![0u8; SLOT_BYTES];
        assert!(block.read_latest(&mut out).is_none(), "no frame yet");

        let (w, h) = (128, 72);
        let frame = test_frame(w, h, 7);
        let y = &frame[..(w * h) as usize];
        let uv = &frame[(w * h) as usize..];
        assert!(block.write_frame(w, h, 1234, y, w as usize, uv, w as usize));

        let info = block.read_latest(&mut out).expect("frame after write");
        assert_eq!((info.width, info.height, info.pts_100ns), (w, h, 1234));
        assert_eq!(&out[..frame.len()], &frame[..]);
    }

    #[test]
    fn strided_write_packs_tightly() {
        let block = heap_block();
        let (w, h) = (4u32, 2u32);
        let stride = 8usize;
        // Y rows: [row0 pad][row1 pad]; UV row: one row of h/2.
        let y = [1, 2, 3, 4, 99, 99, 99, 99, 5, 6, 7, 8, 99, 99, 99, 99];
        let uv = [9, 10, 11, 12, 99, 99, 99, 99];
        assert!(block.write_frame(w, h, 0, &y, stride, &uv, stride));
        let mut out = vec![0u8; SLOT_BYTES];
        block.read_latest(&mut out).expect("frame");
        assert_eq!(&out[..12], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn writer_never_overwrites_latest_slot() {
        let block = heap_block();
        let (w, h) = (2u32, 2u32);
        let f = test_frame(w, h, 0);
        let (y, uv) = f.split_at((w * h) as usize);
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![0u8; SLOT_BYTES];
        for _ in 0..10 {
            assert!(block.write_frame(w, h, 0, y, w as usize, uv, w as usize));
            let info = block.read_latest(&mut out).expect("frame");
            seen.insert((info.seq >> 32) as u32);
        }
        // Rotation actually rotates and stays in range.
        assert!(seen.len() >= 2 && seen.iter().all(|&i| (i as usize) < SLOTS));
    }

    #[test]
    fn rejects_bad_dimensions() {
        let block = heap_block();
        let f = test_frame(4, 2, 0);
        let (y, uv) = f.split_at(8);
        assert!(!block.write_frame(3, 2, 0, y, 4, uv, 4), "odd width");
        assert!(!block.write_frame(MAX_WIDTH + 2, 2, 0, y, 4, uv, 4), "too wide");
        assert!(!block.write_frame(4, 2, 0, &y[..4], 4, uv, 4), "short plane");
    }

    #[cfg(windows)]
    #[test]
    fn shared_section_create_open_round_trip() {
        // Local\ namespace: same-session test does not need
        // SeCreateGlobalPrivilege.
        let name = format!("Local\\Relay.Cam.test.{}", std::process::id());
        let creator = SharedFrames::create(&name).expect("create");
        assert!(creator.created);
        assert!(creator.block().is_valid());

        // Second create maps the existing section without re-initialising.
        let writer = SharedFrames::create(&name).expect("create-existing");
        assert!(!writer.created);

        let (w, h) = (64u32, 36u32);
        let f = test_frame(w, h, 3);
        let (y, uv) = f.split_at((w * h) as usize);
        assert!(writer.block().write_frame(w, h, 42, y, w as usize, uv, w as usize));

        let reader = SharedFrames::open(&name).expect("open");
        let mut out = vec![0u8; SLOT_BYTES];
        let info = reader.block().read_latest(&mut out).expect("read");
        assert_eq!((info.width, info.height, info.pts_100ns), (w, h, 42));
        assert_eq!(&out[..f.len()], &f[..]);
    }
}
