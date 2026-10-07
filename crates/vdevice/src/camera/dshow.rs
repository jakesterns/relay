//! "Relay Camera" for Windows 10 (and any build without the frame-server
//! virtual camera): a user-mode DirectShow video source filter, the same
//! approach as OBS VirtualCam. No driver, no service: the app that opens the
//! camera (Zoom, Discord, Teams, Chrome) loads `relay_vdevice.dll` into its
//! own process through `ICreateDevEnum` and pulls frames from the filter's
//! one output pin.
//!
//! Shape:
//! - [`Filter`] — `IBaseFilter` (+ `IMediaFilter`, `IPersist`) and
//!   `IAMFilterMiscFlags` (it is a source).
//! - [`Pin`] — the output pin: `IPin`, `IAMStreamConfig` (formats and sizes
//!   for the app's picker), `IKsPropertySet` (answers
//!   `AMPROPERTY_PIN_CATEGORY` with `PIN_CATEGORY_CAPTURE`, which is what
//!   `ICaptureGraphBuilder2` and most webcam code look for).
//! - A worker thread per run: allocator buffer → newest ring frame (or the
//!   waiting still) → converted to the negotiated format → `Receive`. The
//!   app's own threads never touch the ring and never wait on it.
//!
//! Formats: NV12 (the ring's own), YUY2 and RGB24 for apps that insist, at
//! the size the producer announced in the ring header (fallback 720p) plus
//! 1080p / 720p / 360p; a stream larger than 1080p offers 1080p first (r54).
//! The pin asks the producer for frames of the negotiated size (the ring's
//! size request), which the share engine scales on the GPU; anything else
//! that arrives is area-scaled here (`picture::Scaler`), aspect kept.
//! Orientation per spec: NV12 and YUY2 top-down, RGB24 a bottom-up DIB
//! (positive `biHeight`). Timestamps are stream time from the run's start,
//! strictly increasing. The negotiated type and every live/still switch go
//! to the camera log (`camera::diag`).
//!
//! Ring: `Local\Relay.Cam` (see `frames::dshow_section_name_from_env`): the
//! filter runs in the user's session like the receiver, so no global
//! namespace and no privilege is involved.
//!
//! Lifetime: the filter holds its pin; the pin holds only a weak reference
//! back (for `QueryPinInfo`), so there is no reference cycle.

#![allow(unsafe_code)] // COM ABI; every block carries a SAFETY note
#![allow(non_snake_case)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{
    implement, IUnknown, IUnknownImpl, Interface, Ref, GUID, HRESULT, PCWSTR, PWSTR,
};
use windows::Win32::Foundation::{
    CLASS_E_NOAGGREGATION, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER, E_UNEXPECTED, S_FALSE, S_OK,
};
use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
use windows::Win32::Media::DirectShow::{
    IAMFilterMiscFlags, IAMFilterMiscFlags_Impl, IAMStreamConfig, IAMStreamConfig_Impl,
    IBaseFilter, IBaseFilter_Impl, IEnumMediaTypes, IEnumMediaTypes_Impl, IEnumPins,
    IEnumPins_Impl, IFilterGraph, IMediaFilter_Impl, IMediaSample, IMemAllocator, IMemInputPin,
    IPin, IPin_Impl, State_Paused, State_Running, State_Stopped, ALLOCATOR_PROPERTIES,
    AMPROPERTY_PIN_CATEGORY, AM_FILTER_MISC_FLAGS_IS_SOURCE, E_PROP_ID_UNSUPPORTED,
    E_PROP_SET_UNSUPPORTED, FILTER_INFO, FILTER_STATE, PINDIR_OUTPUT, PIN_DIRECTION, PIN_INFO,
    VFW_E_ALREADY_CONNECTED, VFW_E_NOT_CONNECTED, VFW_E_NOT_STOPPED, VFW_E_NO_ACCEPTABLE_TYPES,
    VFW_E_TYPE_NOT_ACCEPTED, VIDEO_STREAM_CONFIG_CAPS,
};
use windows::Win32::Media::IReferenceClock;
use windows::Win32::Media::KernelStreaming::{IKsPropertySet, IKsPropertySet_Impl};
use windows::Win32::Media::MediaFoundation::{
    AMPROPSETID_Pin, CLSID_MemoryAllocator, FORMAT_VideoInfo, MEDIATYPE_Video, AM_MEDIA_TYPE,
    MEDIASUBTYPE_NV12, MEDIASUBTYPE_RGB24, MEDIASUBTYPE_YUY2, PIN_CATEGORY_CAPTURE,
    VIDEOINFOHEADER,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CoTaskMemFree, CoUninitialize, IClassFactory,
    IClassFactory_Impl, IPersist_Impl, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

use super::picture::{self, PixFmt};
use super::{diag, CLSID_RELAY_DSHOW};
use crate::frames::{dshow_section_name_from_env, SharedFrames, MAX_HEIGHT, MAX_WIDTH, SLOT_BYTES};

/// `KSPROPERTY_SUPPORT_GET`.
const KSPROPERTY_SUPPORT_GET: u32 = 1;
/// Pin name / id apps see.
const PIN_NAME: &str = "Capture";
/// Size when no producer has announced one yet: 720p, whose NV12 frame
/// (1.4 MB) fits the default buffers of clients that take the first type
/// (ffmpeg dropped frames at 1080p on the Win10 pass, S43b).
const FALLBACK: (u32, u32, u32) = (1280, 720, 30);
/// The ring is considered gone when its frame counter has not moved for
/// this long; the app then gets the waiting still instead of a frozen frame.
const STALE_AFTER: Duration = Duration::from_secs(2);
/// How often to retry mapping the ring while nobody is producing.
const RING_RETRY: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------

/// One negotiated (or offered) output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub fmt: PixFmt,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Format {
    pub fn bytes(&self) -> usize {
        self.fmt.frame_bytes(self.width, self.height)
    }
    fn period_100ns(&self) -> i64 {
        10_000_000 / self.fps.max(1) as i64
    }
}

fn subtype(fmt: PixFmt) -> GUID {
    match fmt {
        PixFmt::Nv12 => MEDIASUBTYPE_NV12,
        PixFmt::Yuy2 => MEDIASUBTYPE_YUY2,
        PixFmt::Rgb24 => MEDIASUBTYPE_RGB24,
    }
}

fn fourcc(fmt: PixFmt) -> u32 {
    match fmt {
        PixFmt::Nv12 => u32::from_le_bytes(*b"NV12"),
        PixFmt::Yuy2 => u32::from_le_bytes(*b"YUY2"),
        PixFmt::Rgb24 => 0, // BI_RGB
    }
}

/// Everything the pin offers, preferred first: for each size, NV12 then
/// YUY2 then RGB24. `hint` is the producer's announced geometry.
///
/// A stream larger than 1080p (PC2's r54 ring was 2560x1440) puts 1080p
/// first: an app that takes the first type gets a size every call service
/// sends, scaled once on the producer's GPU, instead of a 1440p frame it
/// then shrinks badly itself. The stream's own size stays offered second.
pub fn offered_formats(hint: Option<(u32, u32, u32)>) -> Vec<Format> {
    let (w, h, fps) = hint.unwrap_or(FALLBACK);
    let fps = fps.clamp(5, 60);
    let larger_than_1080p = u64::from(w) * u64::from(h) > 1920 * 1080;
    let mut sizes = if larger_than_1080p { vec![(1920, 1080), (w, h)] } else { vec![(w, h)] };
    for s in [(1920, 1080), (1280, 720), (640, 360)] {
        if !sizes.contains(&s) {
            sizes.push(s);
        }
    }
    sizes
        .into_iter()
        .flat_map(|(width, height)| {
            PixFmt::ALL.into_iter().map(move |fmt| Format { fmt, width, height, fps })
        })
        .collect()
}

fn video_info(f: &Format) -> VIDEOINFOHEADER {
    VIDEOINFOHEADER {
        dwBitRate: (f.bytes() as u64 * 8 * f.fps as u64).min(u32::MAX as u64) as u32,
        AvgTimePerFrame: f.period_100ns(),
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: f.width as i32,
            biHeight: f.height as i32,
            biPlanes: 1,
            biBitCount: f.fmt.bits_per_pixel(),
            biCompression: fourcc(f.fmt),
            biSizeImage: f.bytes() as u32,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Fill `mt` with `f`; `pbFormat` is `CoTaskMemAlloc`ed (the caller frees
/// it the DirectShow way — `free_media_type`, or the app's `DeleteMediaType`).
unsafe fn fill_media_type(mt: *mut AM_MEDIA_TYPE, f: &Format) -> windows::core::Result<()> {
    let vih = video_info(f);
    let size = std::mem::size_of::<VIDEOINFOHEADER>();
    // SAFETY: allocation checked; the header is POD copied into it.
    unsafe {
        let pb = CoTaskMemAlloc(size) as *mut u8;
        if pb.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(&vih as *const _ as *const u8, pb, size);
        std::ptr::write(
            mt,
            AM_MEDIA_TYPE {
                majortype: MEDIATYPE_Video,
                subtype: subtype(f.fmt),
                bFixedSizeSamples: true.into(),
                bTemporalCompression: false.into(),
                lSampleSize: f.bytes() as u32,
                formattype: FORMAT_VideoInfo,
                pUnk: std::mem::ManuallyDrop::new(None),
                cbFormat: size as u32,
                pbFormat: pb,
            },
        );
    }
    Ok(())
}

/// A whole `CoTaskMemAlloc`ed media type (enumerators, `GetFormat`).
unsafe fn alloc_media_type(f: &Format) -> windows::core::Result<*mut AM_MEDIA_TYPE> {
    // SAFETY: allocation checked, then fully initialised by fill.
    unsafe {
        let mt = CoTaskMemAlloc(std::mem::size_of::<AM_MEDIA_TYPE>()) as *mut AM_MEDIA_TYPE;
        if mt.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        if let Err(e) = fill_media_type(mt, f) {
            CoTaskMemFree(Some(mt as *const _));
            return Err(e);
        }
        Ok(mt)
    }
}

/// Free the format block and `pUnk` of a media type (not the struct).
///
/// # Safety
/// `mt` must be a valid media type whose `pbFormat` came from
/// `CoTaskMemAlloc` (or is null).
pub unsafe fn free_media_type(mt: *mut AM_MEDIA_TYPE) {
    // SAFETY: per the contract above.
    unsafe {
        let mt = &mut *mt;
        if !mt.pbFormat.is_null() {
            CoTaskMemFree(Some(mt.pbFormat as *const _));
            mt.pbFormat = std::ptr::null_mut();
        }
        mt.cbFormat = 0;
        std::mem::ManuallyDrop::drop(&mut mt.pUnk);
        mt.pUnk = std::mem::ManuallyDrop::new(None);
    }
}

/// What a caller-supplied media type pins down. `None` fields are
/// wildcards (a partial type in `Connect`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Wanted {
    fmt: Option<PixFmt>,
    size: Option<(u32, u32)>,
    fps: Option<u32>,
}

/// Parse a media type. `Err` = not something this pin can ever produce.
unsafe fn parse_media_type(mt: *const AM_MEDIA_TYPE) -> Result<Wanted, ()> {
    if mt.is_null() {
        return Ok(Wanted::default());
    }
    // SAFETY: caller-owned, valid for the call.
    let mt = unsafe { &*mt };
    if mt.majortype != GUID::zeroed() && mt.majortype != MEDIATYPE_Video {
        return Err(());
    }
    let fmt = if mt.subtype == GUID::zeroed() {
        None
    } else {
        Some(PixFmt::ALL.into_iter().find(|&f| subtype(f) == mt.subtype).ok_or(())?)
    };
    let mut want = Wanted { fmt, ..Default::default() };
    if mt.formattype == FORMAT_VideoInfo
        && !mt.pbFormat.is_null()
        && mt.cbFormat as usize >= std::mem::size_of::<VIDEOINFOHEADER>()
    {
        // SAFETY: size checked; VIDEOINFOHEADER may be unaligned in the
        // caller's buffer, so read it unaligned.
        let vih = unsafe { std::ptr::read_unaligned(mt.pbFormat as *const VIDEOINFOHEADER) };
        let (w, mut h) = (vih.bmiHeader.biWidth, vih.bmiHeader.biHeight);
        // YUV is top-down whatever the sign of biHeight, and the DirectShow
        // docs ask filters to accept either sign for it. For RGB a negative
        // height is a top-down DIB, which this pin does not write: refuse it
        // rather than deliver an upside-down call (and a wildcard subtype
        // with a negative height could resolve to RGB24, so refuse that too).
        if h < 0 && matches!(want.fmt, Some(PixFmt::Nv12 | PixFmt::Yuy2)) {
            h = -h;
        }
        if w != 0 || h != 0 {
            if w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 {
                return Err(());
            }
            if w as u32 > MAX_WIDTH || h as u32 > MAX_HEIGHT {
                return Err(());
            }
            want.size = Some((w as u32, h as u32));
        }
        if vih.AvgTimePerFrame > 0 {
            want.fps = Some(((10_000_000 + vih.AvgTimePerFrame / 2) / vih.AvgTimePerFrame) as u32);
        }
    } else if mt.formattype != GUID::zeroed() && mt.formattype != FORMAT_VideoInfo {
        return Err(());
    }
    Ok(want)
}

/// Resolve a wanted type against a base format: anything unspecified comes
/// from `base`, fps is clamped to what the pin can pace.
fn resolve(want: Wanted, base: Format) -> Format {
    Format {
        fmt: want.fmt.unwrap_or(base.fmt),
        width: want.size.map_or(base.width, |s| s.0),
        height: want.size.map_or(base.height, |s| s.1),
        fps: want.fps.unwrap_or(base.fps).clamp(1, 60),
    }
}

// ---------------------------------------------------------------------------
// Frame source (worker side only)
// ---------------------------------------------------------------------------

/// Pulls the newest ring frame and turns it into the negotiated format.
/// Only the worker thread owns one; the app's threads never read the ring.
pub struct FrameSource {
    f: Format,
    ring_name: String,
    ring: Option<SharedFrames>,
    last_try: Option<Instant>,
    last_count: u32,
    last_change: Instant,
    scratch: Vec<u8>,
    scaled: Vec<u8>,
    scaler: picture::Scaler,
    waiting: Vec<u8>,
    /// Whether the last frame was live (`Some(true)`) or the still, for the
    /// one log line per switch.
    was_live: Option<bool>,
    /// Size of the last ring frame, so a change (the producer starting to
    /// honour the size request) is logged once.
    last_ring_size: (u32, u32),
    /// This source asked the producer for its negotiated size.
    requested: bool,
}

impl FrameSource {
    pub fn new(f: Format, ring_name: String) -> Self {
        let waiting_nv12 = picture::waiting_frame_nv12(f.width, f.height);
        let mut waiting = vec![0u8; f.bytes()];
        picture::convert_nv12(f.fmt, &waiting_nv12, f.width, f.height, &mut waiting);
        Self {
            f,
            ring_name,
            ring: None,
            last_try: None,
            last_count: 0,
            last_change: Instant::now(),
            scratch: Vec::new(),
            scaled: Vec::new(),
            scaler: picture::Scaler::new(),
            waiting,
            was_live: None,
            last_ring_size: (0, 0),
            requested: false,
        }
    }

    /// Write one frame into `out` (exactly `f.bytes()` long). `true` = a
    /// live ring frame, `false` = the waiting still. Never blocks.
    pub fn render(&mut self, out: &mut [u8]) -> bool {
        let live = self.live_frame(out);
        if !live {
            out.copy_from_slice(&self.waiting);
        }
        if self.was_live != Some(live) {
            let f = self.f;
            diag::line(&match (self.was_live, live) {
                (_, true) => format!(
                    "ring up: {}x{} frames → {} {}x{}",
                    self.last_ring_size.0,
                    self.last_ring_size.1,
                    f.fmt.name(),
                    f.width,
                    f.height
                ),
                (Some(true), false) => "vcam ring down → showing the waiting still".to_string(),
                (_, false) => "no stream yet → showing the waiting still".to_string(),
            });
            self.was_live = Some(live);
        }
        live
    }

    fn live_frame(&mut self, out: &mut [u8]) -> bool {
        if self.ring.is_none() {
            if self.last_try.is_some_and(|t| t.elapsed() < RING_RETRY) {
                return false;
            }
            self.last_try = Some(Instant::now());
            // `create` maps an existing section or makes the (empty) one the
            // producer will then map — whichever side comes first.
            self.ring = SharedFrames::create(&self.ring_name).ok();
        }
        let Some(ring) = self.ring.as_ref() else { return false };
        let block = ring.block();
        if !self.requested {
            // Ask the producer for frames of the negotiated size: it scales
            // on the GPU, once, and this thread just copies.
            block.request_size(self.f.width, self.f.height);
            self.requested = true;
        }
        let count = block.frame_count();
        if count != self.last_count {
            self.last_count = count;
            self.last_change = Instant::now();
        } else if count == 0 || self.last_change.elapsed() > STALE_AFTER {
            return false;
        }
        if self.scratch.len() < SLOT_BYTES {
            self.scratch.resize(SLOT_BYTES, 0);
        }
        let Some(info) = block.read_latest(&mut self.scratch) else { return false };
        let f = self.f;
        if (info.width, info.height) != self.last_ring_size {
            if self.was_live == Some(true) {
                diag::line(&format!(
                    "ring frames now {}x{} (was {}x{}) → {} {}x{}",
                    info.width,
                    info.height,
                    self.last_ring_size.0,
                    self.last_ring_size.1,
                    f.fmt.name(),
                    f.width,
                    f.height
                ));
            }
            self.last_ring_size = (info.width, info.height);
        }
        let nv12: &[u8] = if (info.width, info.height) == (f.width, f.height) {
            &self.scratch
        } else {
            self.scaled.resize(picture::nv12_bytes(f.width, f.height), 0);
            self.scaler.scale(
                &self.scratch,
                info.width,
                info.height,
                &mut self.scaled,
                f.width,
                f.height,
            );
            &self.scaled
        };
        picture::convert_nv12(f.fmt, nv12, f.width, f.height, out);
        true
    }
}

impl Drop for FrameSource {
    fn drop(&mut self) {
        // Hand the producer back its own size, unless another camera has
        // asked for something else since.
        if let (true, Some(ring)) = (self.requested, self.ring.as_ref()) {
            ring.block().withdraw_request(self.f.width, self.f.height);
        }
    }
}

/// The producer's announced geometry, read once when the filter is made.
fn ring_hint() -> Option<(u32, u32, u32)> {
    SharedFrames::open(&dshow_section_name_from_env()).ok()?.block().geometry_hint()
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct Conn {
    peer: IPin,
    input: IMemInputPin,
    alloc: IMemAllocator,
}

struct St {
    state: FILTER_STATE,
    /// The graph, *not* AddRef'd (the graph owns the filter; holding a
    /// reference would be a cycle — DirectShow's documented rule).
    graph: *mut core::ffi::c_void,
    name: Vec<u16>,
    clock: Option<IReferenceClock>,
    /// Current format: the connection's, or the last `SetFormat`.
    format: Format,
    conn: Option<Conn>,
}

struct Shared {
    st: Mutex<St>,
    offered: Vec<Format>,
    stop: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
    ring_name: String,
}

// SAFETY: the COM pointers inside are only used under the mutex or handed
// to the worker thread, and DirectShow pins/allocators are free-threaded
// (the base classes and quartz's allocator lock internally).
unsafe impl Send for St {}
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

struct SendPtr<T>(T);
// SAFETY: see `Shared` — allocator and input pin are free-threaded.
unsafe impl<T> Send for SendPtr<T> {}

impl Shared {
    fn new(hint: Option<(u32, u32, u32)>) -> Arc<Self> {
        let offered = offered_formats(hint);
        Arc::new(Self {
            st: Mutex::new(St {
                state: State_Stopped,
                graph: std::ptr::null_mut(),
                name: Vec::new(),
                clock: None,
                format: offered[0],
                conn: None,
            }),
            offered,
            stop: AtomicBool::new(false),
            worker: Mutex::new(None),
            ring_name: dshow_section_name_from_env(),
        })
    }

    /// Commit the allocator and start the delivery thread (Stopped → Paused).
    fn start_streaming(self: &Arc<Self>) -> windows::core::Result<()> {
        let st = self.st.lock().unwrap();
        let Some(conn) = st.conn.as_ref() else { return Ok(()) };
        // SAFETY: valid allocator.
        unsafe { conn.alloc.Commit()? };
        let io = SendPtr((conn.input.clone(), conn.alloc.clone()));
        let f = st.format;
        drop(st);
        self.stop.store(false, Ordering::Release);
        let me = self.clone();
        let handle = std::thread::Builder::new()
            .name("relay-dshow-cam".into())
            .spawn(move || {
                let io = io;
                deliver(&me, &io.0 .0, &io.0 .1, f)
            })
            .map_err(|_| windows::core::Error::from_hresult(E_FAIL))?;
        *self.worker.lock().unwrap() = Some(handle);
        Ok(())
    }

    /// Stop the delivery thread: flag, decommit (unblocks `GetBuffer`), join.
    fn stop_streaming(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(conn) = self.st.lock().unwrap().conn.as_ref() {
            // SAFETY: valid allocator; decommit is idempotent.
            let _ = unsafe { conn.alloc.Decommit() };
        }
        if let Some(h) = self.worker.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

/// The delivery loop (worker thread). Paused: one preroll frame so a
/// renderer can finish its pause, then wait. Running: one frame per period,
/// stamped with stream time since the run began.
fn deliver(shared: &Shared, input: &IMemInputPin, alloc: &IMemAllocator, f: Format) {
    // SAFETY: balanced with CoUninitialize at the end of this thread.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut source = FrameSource::new(f, shared.ring_name.clone());
    let period = f.period_100ns();
    let mut clock0: Option<Instant> = None;
    let mut next_start: i64 = 0;
    let mut prerolled = false;
    let mut first = true;
    while !shared.stop.load(Ordering::Acquire) {
        let state = shared.st.lock().unwrap().state;
        let running = state == State_Running;
        if !running && (prerolled || state != State_Paused) {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        // SAFETY: allocator contract — GetBuffer blocks until a buffer is
        // free or the allocator is decommitted (our stop path).
        let sample = unsafe {
            let mut s: Option<IMediaSample> = None;
            if alloc.GetBuffer(&mut s, None, None, 0).is_err() {
                break;
            }
            match s {
                Some(s) => s,
                None => break,
            }
        };
        let bytes = f.bytes();
        // SAFETY: the buffer is ours until released (dropped); size checked.
        let ok = unsafe {
            match (sample.GetPointer(), sample.GetSize()) {
                (Ok(p), size) if !p.is_null() && size as usize >= bytes => {
                    source.render(std::slice::from_raw_parts_mut(p, bytes));
                    true
                }
                _ => false,
            }
        };
        if !ok {
            break;
        }
        // Stream time: 0 at the first running frame, strictly increasing.
        let start = if running {
            let t0 = *clock0.get_or_insert_with(Instant::now);
            let now = (t0.elapsed().as_nanos() / 100) as i64;
            now.max(next_start)
        } else {
            0
        };
        let end = start + period;
        // SAFETY: valid sample; pointers to locals live for the call.
        let delivered = unsafe {
            let _ = sample.SetTime(Some(&start), Some(&end));
            let _ = sample.SetSyncPoint(true);
            let _ = sample.SetDiscontinuity(first);
            let _ = sample.SetActualDataLength(bytes as i32);
            input.Receive(&sample)
        };
        drop(sample);
        first = false;
        if delivered.is_err() {
            break; // downstream refused: stop delivering, as CSourceStream does
        }
        next_start = end;
        if !running {
            prerolled = true;
            continue;
        }
        // Pace to the frame period on our own clock.
        let t0 = clock0.expect("set above");
        let due = Duration::from_nanos(end as u64 * 100);
        if let Some(wait) = due.checked_sub(t0.elapsed()) {
            std::thread::sleep(wait.min(Duration::from_millis(200)));
        }
    }
    if com {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

#[implement(IBaseFilter, IAMFilterMiscFlags)]
pub struct Filter {
    shared: Arc<Shared>,
    pin: Mutex<Option<IPin>>,
}

impl Filter {
    /// A new filter with its pin; geometry from the ring hint if a producer
    /// has announced one.
    pub fn create() -> windows::core::Result<IBaseFilter> {
        Self::create_with_hint(ring_hint())
    }

    /// Test seam: a filter with an explicit geometry hint.
    pub fn create_with_hint(hint: Option<(u32, u32, u32)>) -> windows::core::Result<IBaseFilter> {
        let shared = Shared::new(hint);
        let object =
            windows_core::ComObject::new(Filter { shared: shared.clone(), pin: Mutex::new(None) });
        let filter: IBaseFilter = object.to_interface();
        // The filter holds the pin strongly; the pin holds a weak reference
        // back, so dropping the app's last filter reference frees both.
        let weak = filter.downgrade()?;
        let pin: IPin = Pin { shared, filter: weak }.into();
        object.pin.lock().unwrap().replace(pin);
        Ok(filter)
    }

    fn pin(&self) -> Option<IPin> {
        self.pin.lock().unwrap().clone()
    }
}

impl Drop for Filter {
    fn drop(&mut self) {
        self.shared.stop_streaming();
    }
}

impl IPersist_Impl for Filter_Impl {
    fn GetClassID(&self) -> windows::core::Result<GUID> {
        Ok(CLSID_RELAY_DSHOW)
    }
}

impl IMediaFilter_Impl for Filter_Impl {
    fn Stop(&self) -> windows::core::Result<()> {
        self.shared.stop_streaming();
        self.shared.st.lock().unwrap().state = State_Stopped;
        Ok(())
    }

    fn Pause(&self) -> windows::core::Result<()> {
        let was = self.shared.st.lock().unwrap().state;
        if was == State_Stopped {
            self.shared.st.lock().unwrap().state = State_Paused;
            self.shared.start_streaming()?;
        } else {
            self.shared.st.lock().unwrap().state = State_Paused;
        }
        Ok(())
    }

    fn Run(&self, _tstart: i64) -> windows::core::Result<()> {
        if self.shared.st.lock().unwrap().state == State_Stopped {
            IMediaFilter_Impl::Pause(self)?;
        }
        self.shared.st.lock().unwrap().state = State_Running;
        Ok(())
    }

    fn GetState(&self, _timeout: u32) -> windows::core::Result<FILTER_STATE> {
        Ok(self.shared.st.lock().unwrap().state)
    }

    fn SetSyncSource(&self, clock: Ref<IReferenceClock>) -> windows::core::Result<()> {
        self.shared.st.lock().unwrap().clock = clock.cloned();
        Ok(())
    }

    fn GetSyncSource(&self) -> windows::core::Result<IReferenceClock> {
        // `Err(S_FALSE)`-free contract: no clock is reported as a null out
        // pointer with S_OK by the shim when we return an error; callers
        // treat both as "no clock".
        self.shared
            .st
            .lock()
            .unwrap()
            .clock
            .clone()
            .ok_or_else(|| windows::core::Error::from_hresult(S_FALSE))
    }
}

impl IBaseFilter_Impl for Filter_Impl {
    fn EnumPins(&self) -> windows::core::Result<IEnumPins> {
        Ok(PinEnum { pin: self.pin(), pos: Mutex::new(0) }.into())
    }

    fn FindPin(&self, id: &PCWSTR) -> windows::core::Result<IPin> {
        // SAFETY: caller's NUL-terminated string.
        let id = unsafe { id.to_string() }.map_err(|_| E_POINTER)?;
        if id == PIN_NAME {
            self.pin().ok_or_else(|| windows::core::Error::from_hresult(E_UNEXPECTED))
        } else {
            Err(windows::Win32::Media::DirectShow::VFW_E_NOT_FOUND.into())
        }
    }

    fn QueryFilterInfo(&self, info: *mut FILTER_INFO) -> windows::core::Result<()> {
        if info.is_null() {
            return Err(E_POINTER.into());
        }
        let st = self.shared.st.lock().unwrap();
        let mut name = [0u16; 128];
        let n = st.name.len().min(127);
        name[..n].copy_from_slice(&st.name[..n]);
        // SAFETY: the graph pointer is live while we are in it (it removes
        // us before it dies); the copy we hand out is AddRef'd, per the
        // QueryFilterInfo contract.
        let graph = unsafe { IFilterGraph::from_raw_borrowed(&st.graph).cloned() };
        // SAFETY: caller-owned out struct.
        unsafe {
            std::ptr::write(
                info,
                FILTER_INFO { achName: name, pGraph: std::mem::ManuallyDrop::new(graph) },
            )
        };
        Ok(())
    }

    fn JoinFilterGraph(
        &self,
        graph: Ref<IFilterGraph>,
        name: &PCWSTR,
    ) -> windows::core::Result<()> {
        let mut st = self.shared.st.lock().unwrap();
        st.graph = graph.as_ref().map_or(std::ptr::null_mut(), |g| g.as_raw());
        st.name = if name.is_null() {
            Vec::new()
        } else {
            // SAFETY: caller's NUL-terminated string.
            unsafe { name.as_wide() }.to_vec()
        };
        Ok(())
    }

    fn QueryVendorInfo(&self) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
}

impl IAMFilterMiscFlags_Impl for Filter_Impl {
    fn GetMiscFlags(&self) -> u32 {
        AM_FILTER_MISC_FLAGS_IS_SOURCE.0 as u32
    }
}

// ---------------------------------------------------------------------------
// Output pin
// ---------------------------------------------------------------------------

#[implement(IPin, IAMStreamConfig, IKsPropertySet)]
pub struct Pin {
    shared: Arc<Shared>,
    filter: windows::core::Weak<IBaseFilter>,
}

impl Pin_Impl {
    /// Try one concrete format against the receiving pin, then settle the
    /// allocator. On success the connection is recorded.
    fn try_connect(&self, receiver: &IPin, f: &Format) -> windows::core::Result<()> {
        let me: IPin = self.to_interface();
        let mut mt = AM_MEDIA_TYPE::default();
        // SAFETY: local media type, freed below; the receiver copies it.
        unsafe {
            fill_media_type(&mut mt, f)?;
            let r = receiver.ReceiveConnection(&me, &mt);
            free_media_type(&mut mt);
            r?;
        }
        match self.settle_allocator(receiver, f) {
            Ok(conn) => {
                let mut st = self.shared.st.lock().unwrap();
                st.format = *f;
                st.conn = Some(conn);
                drop(st);
                // Once per connection: what the app picked, for testers.
                let first = self.shared.offered[0];
                diag::line(&format!(
                    "connected: {} {}x{} @ {} fps (first offered {} {}x{})",
                    f.fmt.name(),
                    f.width,
                    f.height,
                    f.fps,
                    first.fmt.name(),
                    first.width,
                    first.height
                ));
                Ok(())
            }
            Err(e) => {
                // SAFETY: undo the half-made connection on the receiver.
                let _ = unsafe { receiver.Disconnect() };
                Err(e)
            }
        }
    }

    fn settle_allocator(&self, receiver: &IPin, f: &Format) -> windows::core::Result<Conn> {
        // SAFETY: standard IMemInputPin negotiation.
        unsafe {
            let input: IMemInputPin = receiver.cast()?;
            let req = input.GetAllocatorRequirements().unwrap_or_default();
            let alloc = match input.GetAllocator() {
                Ok(a) => a,
                Err(_) => CoCreateInstance(&CLSID_MemoryAllocator, None, CLSCTX_INPROC_SERVER)?,
            };
            let want = ALLOCATOR_PROPERTIES {
                cBuffers: req.cBuffers.max(3),
                cbBuffer: f.bytes() as i32,
                cbAlign: req.cbAlign.max(1),
                cbPrefix: req.cbPrefix.max(0),
            };
            let got = alloc.SetProperties(&want)?;
            if (got.cbBuffer as usize) < f.bytes() || got.cBuffers < 1 {
                return Err(E_FAIL.into());
            }
            input.NotifyAllocator(&alloc, false)?;
            Ok(Conn { peer: receiver.clone(), input, alloc })
        }
    }

    fn candidates(&self, want: Wanted) -> Vec<Format> {
        let current = self.shared.st.lock().unwrap().format;
        let mut out = Vec::new();
        if want.size.is_some() || want.fps.is_some() {
            out.push(resolve(want, current));
        }
        // The current (possibly SetFormat'd) format first, then the offers.
        for f in std::iter::once(current).chain(self.shared.offered.iter().copied()) {
            let f = Format { fps: want.fps.unwrap_or(f.fps).clamp(1, 60), ..f };
            if want.fmt.is_none_or(|w| w == f.fmt) && !out.contains(&f) {
                out.push(f);
            }
        }
        out
    }
}

impl IPin_Impl for Pin_Impl {
    fn Connect(&self, receiver: Ref<IPin>, pmt: *const AM_MEDIA_TYPE) -> windows::core::Result<()> {
        let receiver = receiver.ok()?;
        {
            let st = self.shared.st.lock().unwrap();
            if st.conn.is_some() {
                return Err(VFW_E_ALREADY_CONNECTED.into());
            }
            if st.state != State_Stopped {
                return Err(VFW_E_NOT_STOPPED.into());
            }
        }
        // SAFETY: caller's media type, valid for the call (may be null).
        let want = unsafe { parse_media_type(pmt) }
            .map_err(|()| windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED))?;
        for f in self.candidates(want) {
            if self.try_connect(receiver, &f).is_ok() {
                return Ok(());
            }
        }
        Err(VFW_E_NO_ACCEPTABLE_TYPES.into())
    }

    fn ReceiveConnection(
        &self,
        _connector: Ref<IPin>,
        _pmt: *const AM_MEDIA_TYPE,
    ) -> windows::core::Result<()> {
        Err(E_UNEXPECTED.into()) // output pin
    }

    fn Disconnect(&self) -> windows::core::Result<()> {
        let mut st = self.shared.st.lock().unwrap();
        if st.state != State_Stopped {
            return Err(VFW_E_NOT_STOPPED.into());
        }
        match st.conn.take() {
            Some(conn) => {
                // SAFETY: valid allocator.
                let _ = unsafe { conn.alloc.Decommit() };
                Ok(())
            }
            None => Err(windows::core::Error::from_hresult(S_FALSE)),
        }
    }

    fn ConnectedTo(&self) -> windows::core::Result<IPin> {
        self.shared
            .st
            .lock()
            .unwrap()
            .conn
            .as_ref()
            .map(|c| c.peer.clone())
            .ok_or_else(|| windows::core::Error::from_hresult(VFW_E_NOT_CONNECTED))
    }

    fn ConnectionMediaType(&self, pmt: *mut AM_MEDIA_TYPE) -> windows::core::Result<()> {
        if pmt.is_null() {
            return Err(E_POINTER.into());
        }
        let st = self.shared.st.lock().unwrap();
        // SAFETY: caller-owned out struct.
        unsafe {
            if st.conn.is_none() {
                std::ptr::write(pmt, AM_MEDIA_TYPE::default());
                return Err(VFW_E_NOT_CONNECTED.into());
            }
            fill_media_type(pmt, &st.format)
        }
    }

    fn QueryPinInfo(&self, info: *mut PIN_INFO) -> windows::core::Result<()> {
        if info.is_null() {
            return Err(E_POINTER.into());
        }
        let mut name = [0u16; 128];
        for (d, s) in name.iter_mut().zip(PIN_NAME.encode_utf16()) {
            *d = s;
        }
        // SAFETY: caller-owned out struct; the filter reference is AddRef'd
        // (upgrade), per the contract.
        unsafe {
            std::ptr::write(
                info,
                PIN_INFO {
                    pFilter: std::mem::ManuallyDrop::new(self.filter.upgrade()),
                    dir: PINDIR_OUTPUT,
                    achName: name,
                },
            )
        };
        Ok(())
    }

    fn QueryDirection(&self) -> windows::core::Result<PIN_DIRECTION> {
        Ok(PINDIR_OUTPUT)
    }

    fn QueryId(&self) -> windows::core::Result<PWSTR> {
        co_task_string(PIN_NAME)
    }

    fn QueryAccept(&self, pmt: *const AM_MEDIA_TYPE) -> HRESULT {
        // SAFETY: caller's media type.
        match unsafe { parse_media_type(pmt) } {
            Ok(w) if w.fmt.is_some() => S_OK,
            _ => S_FALSE,
        }
    }

    fn EnumMediaTypes(&self) -> windows::core::Result<IEnumMediaTypes> {
        Ok(TypeEnum { list: self.candidates(Wanted::default()), pos: Mutex::new(0) }.into())
    }

    fn QueryInternalConnections(
        &self,
        _pins: windows::core::OutRef<IPin>,
        _n: *mut u32,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EndOfStream(&self) -> windows::core::Result<()> {
        Err(E_UNEXPECTED.into())
    }
    fn BeginFlush(&self) -> windows::core::Result<()> {
        Err(E_UNEXPECTED.into())
    }
    fn EndFlush(&self) -> windows::core::Result<()> {
        Err(E_UNEXPECTED.into())
    }
    fn NewSegment(&self, _start: i64, _stop: i64, _rate: f64) -> windows::core::Result<()> {
        Ok(())
    }
}

impl IAMStreamConfig_Impl for Pin_Impl {
    fn SetFormat(&self, pmt: *const AM_MEDIA_TYPE) -> windows::core::Result<()> {
        if pmt.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: caller's media type.
        let want = unsafe { parse_media_type(pmt) }
            .map_err(|()| windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED))?;
        let Some(_) = want.fmt else { return Err(VFW_E_TYPE_NOT_ACCEPTED.into()) };
        let mut st = self.shared.st.lock().unwrap();
        let f = resolve(want, st.format);
        if st.conn.is_some() {
            // Reconnecting with another type is the graph's job; only the
            // same type is a no-op while connected.
            return if f == st.format { Ok(()) } else { Err(VFW_E_ALREADY_CONNECTED.into()) };
        }
        st.format = f;
        Ok(())
    }

    fn GetFormat(&self) -> windows::core::Result<*mut AM_MEDIA_TYPE> {
        let f = self.shared.st.lock().unwrap().format;
        // SAFETY: returns a CoTaskMem media type the caller frees.
        unsafe { alloc_media_type(&f) }
    }

    fn GetNumberOfCapabilities(
        &self,
        count: *mut i32,
        size: *mut i32,
    ) -> windows::core::Result<()> {
        if count.is_null() || size.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: caller-owned outs, checked.
        unsafe {
            *count = self.shared.offered.len() as i32;
            *size = std::mem::size_of::<VIDEO_STREAM_CONFIG_CAPS>() as i32;
        }
        Ok(())
    }

    fn GetStreamCaps(
        &self,
        index: i32,
        ppmt: *mut *mut AM_MEDIA_TYPE,
        pscc: *mut u8,
    ) -> windows::core::Result<()> {
        if ppmt.is_null() || pscc.is_null() {
            return Err(E_POINTER.into());
        }
        let Some(f) = usize::try_from(index).ok().and_then(|i| self.shared.offered.get(i)) else {
            return Err(windows::Win32::Foundation::S_FALSE.into());
        };
        let size = windows::Win32::Foundation::SIZE { cx: f.width as i32, cy: f.height as i32 };
        let caps = VIDEO_STREAM_CONFIG_CAPS {
            guid: FORMAT_VideoInfo,
            InputSize: size,
            MinCroppingSize: size,
            MaxCroppingSize: size,
            MinOutputSize: size,
            MaxOutputSize: size,
            MinFrameInterval: 10_000_000 / 60,
            MaxFrameInterval: 10_000_000 / 5,
            MinBitsPerSecond: (f.fmt.frame_bytes(f.width, f.height) * 8 * 5) as i32,
            MaxBitsPerSecond: (f.bytes() as u64 * 8 * 60).min(i32::MAX as u64) as i32,
            ..Default::default()
        };
        // SAFETY: caller supplies a VIDEO_STREAM_CONFIG_CAPS-sized buffer
        // (GetNumberOfCapabilities told it the size); may be unaligned.
        unsafe {
            std::ptr::write_unaligned(pscc as *mut VIDEO_STREAM_CONFIG_CAPS, caps);
            *ppmt = alloc_media_type(f)?;
        }
        Ok(())
    }
}

impl IKsPropertySet_Impl for Pin_Impl {
    fn Set(
        &self,
        _set: *const GUID,
        _id: u32,
        _inst: *const core::ffi::c_void,
        _inst_len: u32,
        _data: *const core::ffi::c_void,
        _len: u32,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Get(
        &self,
        set: *const GUID,
        id: u32,
        _inst: *const core::ffi::c_void,
        _inst_len: u32,
        data: *mut core::ffi::c_void,
        len: u32,
        returned: *mut u32,
    ) -> windows::core::Result<()> {
        // SAFETY: caller's GUID pointer.
        if set.is_null() || unsafe { *set } != AMPROPSETID_Pin {
            return Err(E_PROP_SET_UNSUPPORTED.into());
        }
        if id != AMPROPERTY_PIN_CATEGORY.0 as u32 {
            return Err(E_PROP_ID_UNSUPPORTED.into());
        }
        if !returned.is_null() {
            // SAFETY: caller-owned out.
            unsafe { *returned = std::mem::size_of::<GUID>() as u32 };
        }
        if data.is_null() {
            return Ok(()); // size query
        }
        if (len as usize) < std::mem::size_of::<GUID>() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: size checked; may be unaligned.
        unsafe { std::ptr::write_unaligned(data as *mut GUID, PIN_CATEGORY_CAPTURE) };
        Ok(())
    }

    fn QuerySupported(&self, set: *const GUID, id: u32) -> windows::core::Result<u32> {
        // SAFETY: caller's GUID pointer.
        if set.is_null() || unsafe { *set } != AMPROPSETID_Pin {
            return Err(E_PROP_SET_UNSUPPORTED.into());
        }
        if id != AMPROPERTY_PIN_CATEGORY.0 as u32 {
            return Err(E_PROP_ID_UNSUPPORTED.into());
        }
        Ok(KSPROPERTY_SUPPORT_GET)
    }
}

fn co_task_string(s: &str) -> windows::core::Result<PWSTR> {
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: allocation checked; the caller frees with CoTaskMemFree.
    unsafe {
        let p = CoTaskMemAlloc(w.len() * 2) as *mut u16;
        if p.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        Ok(PWSTR(p))
    }
}

// ---------------------------------------------------------------------------
// Enumerators
// ---------------------------------------------------------------------------

#[implement(IEnumPins)]
struct PinEnum {
    pin: Option<IPin>,
    pos: Mutex<usize>,
}

impl IEnumPins_Impl for PinEnum_Impl {
    fn Next(&self, count: u32, pins: *mut Option<IPin>, fetched: *mut u32) -> HRESULT {
        if pins.is_null() || (count > 1 && fetched.is_null()) {
            return E_POINTER;
        }
        let mut pos = self.pos.lock().unwrap();
        let total = usize::from(self.pin.is_some());
        let mut n = 0u32;
        while n < count && *pos < total {
            // SAFETY: caller's array of at least `count` slots.
            unsafe { std::ptr::write(pins.add(n as usize), self.pin.clone()) };
            n += 1;
            *pos += 1;
        }
        if !fetched.is_null() {
            // SAFETY: caller-owned out.
            unsafe { *fetched = n };
        }
        if n == count {
            S_OK
        } else {
            S_FALSE
        }
    }
    fn Skip(&self, count: u32) -> windows::core::Result<()> {
        let mut pos = self.pos.lock().unwrap();
        *pos += count as usize;
        if *pos <= usize::from(self.pin.is_some()) {
            Ok(())
        } else {
            Err(windows::core::Error::from_hresult(S_FALSE))
        }
    }
    fn Reset(&self) -> windows::core::Result<()> {
        *self.pos.lock().unwrap() = 0;
        Ok(())
    }
    fn Clone(&self) -> windows::core::Result<IEnumPins> {
        Ok(PinEnum { pin: self.pin.clone(), pos: Mutex::new(*self.pos.lock().unwrap()) }.into())
    }
}

#[implement(IEnumMediaTypes)]
struct TypeEnum {
    list: Vec<Format>,
    pos: Mutex<usize>,
}

impl IEnumMediaTypes_Impl for TypeEnum_Impl {
    fn Next(&self, count: u32, out: *mut *mut AM_MEDIA_TYPE, fetched: *mut u32) -> HRESULT {
        if out.is_null() || (count > 1 && fetched.is_null()) {
            return E_POINTER;
        }
        let mut pos = self.pos.lock().unwrap();
        let mut n = 0u32;
        while n < count && *pos < self.list.len() {
            // SAFETY: caller's array of at least `count` slots; each entry a
            // CoTaskMem media type the caller deletes.
            match unsafe { alloc_media_type(&self.list[*pos]) } {
                Ok(mt) => unsafe { *out.add(n as usize) = mt },
                Err(e) => return e.code(),
            }
            n += 1;
            *pos += 1;
        }
        if !fetched.is_null() {
            // SAFETY: caller-owned out.
            unsafe { *fetched = n };
        }
        if n == count {
            S_OK
        } else {
            S_FALSE
        }
    }
    fn Skip(&self, count: u32) -> windows::core::Result<()> {
        let mut pos = self.pos.lock().unwrap();
        *pos += count as usize;
        if *pos <= self.list.len() {
            Ok(())
        } else {
            Err(windows::core::Error::from_hresult(S_FALSE))
        }
    }
    fn Reset(&self) -> windows::core::Result<()> {
        *self.pos.lock().unwrap() = 0;
        Ok(())
    }
    fn Clone(&self) -> windows::core::Result<IEnumMediaTypes> {
        Ok(TypeEnum { list: self.list.clone(), pos: Mutex::new(*self.pos.lock().unwrap()) }.into())
    }
}

// ---------------------------------------------------------------------------
// Class factory
// ---------------------------------------------------------------------------

#[implement(IClassFactory)]
pub struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if object.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let filter = Filter::create()?;
        // SAFETY: iid/object checked non-null above.
        unsafe { filter.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: windows::core::BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_three_formats_per_size_hint_first() {
        let offers = offered_formats(Some((2560, 1440, 60)));
        assert_eq!(offers.len(), 12, "4 sizes x 3 formats");
        // r54: larger than 1080p → 1080p first, the stream's size second,
        // 720p still offered.
        assert_eq!(offers[0], Format { fmt: PixFmt::Nv12, width: 1920, height: 1080, fps: 60 });
        assert_eq!(offers[1].fmt, PixFmt::Yuy2);
        assert_eq!(offers[2].fmt, PixFmt::Rgb24);
        assert_eq!(offers[3], Format { fmt: PixFmt::Nv12, width: 2560, height: 1440, fps: 60 });
        assert!(offers.iter().any(|f| (f.width, f.height) == (1280, 720)));
        // 1080p or smaller: the stream's own size stays first.
        assert_eq!(offered_formats(Some((1920, 1080, 60)))[0].width, 1920);
        assert_eq!(offered_formats(Some((1600, 900, 60)))[0].width, 1600);
        // Hint equal to a standard size is not listed twice.
        assert_eq!(offered_formats(Some((1280, 720, 30))).len(), 9);
        // No producer yet: 720p30, then 1080p and 360p.
        assert_eq!(offered_formats(None)[0].width, 1280);
        assert_eq!(offered_formats(None)[0].height, 720);
        assert_eq!(offered_formats(None)[0].fps, 30);
    }

    #[test]
    fn media_type_round_trips_through_the_parser() {
        for f in offered_formats(Some((1280, 720, 60))) {
            let mut mt = AM_MEDIA_TYPE::default();
            // SAFETY: local type, freed below.
            unsafe {
                fill_media_type(&mut mt, &f).unwrap();
                assert_eq!(mt.lSampleSize as usize, f.bytes());
                let want = parse_media_type(&mt).expect("our own type parses");
                assert_eq!(resolve(want, offered_formats(None)[0]), f);
                free_media_type(&mut mt);
            }
        }
    }

    #[test]
    fn parser_refuses_foreign_and_top_down_types() {
        let f = Format { fmt: PixFmt::Rgb24, width: 640, height: 360, fps: 30 };
        let mut mt = AM_MEDIA_TYPE::default();
        // SAFETY: local types, freed below.
        unsafe {
            fill_media_type(&mut mt, &f).unwrap();
            let vih = &mut *(mt.pbFormat as *mut VIDEOINFOHEADER);
            vih.bmiHeader.biHeight = -360;
            assert!(parse_media_type(&mt).is_err(), "top-down RGB refused");
            // YUV is top-down whatever the sign: accepted either way.
            mt.subtype = MEDIASUBTYPE_YUY2;
            let w = parse_media_type(&mt).expect("negative-height YUY2 accepted");
            assert_eq!((w.fmt, w.size), (Some(PixFmt::Yuy2), Some((640, 360))));
            mt.subtype = MEDIASUBTYPE_NV12;
            assert!(parse_media_type(&mt).is_ok());
            mt.subtype = GUID::zeroed();
            assert!(parse_media_type(&mt).is_err(), "wildcard + negative could be RGB: refused");
            mt.subtype = MEDIASUBTYPE_RGB24;
            vih.bmiHeader.biHeight = 360;
            mt.subtype = GUID::from_u128(0x30323449_0000_0010_8000_00aa00389b71); // I420
            assert!(parse_media_type(&mt).is_err(), "I420 not offered");
            mt.subtype = GUID::zeroed();
            let w = parse_media_type(&mt).expect("wildcard subtype");
            assert_eq!((w.fmt, w.size), (None, Some((640, 360))));
            free_media_type(&mut mt);
        }
        assert_eq!(unsafe { parse_media_type(std::ptr::null()) }, Ok(Wanted::default()));
    }
}
