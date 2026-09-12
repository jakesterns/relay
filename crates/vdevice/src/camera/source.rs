//! The camera media source the Windows Camera Frame Server hosts.
//!
//! Shape (mirrors the Microsoft VirtualCamera sample, and relay-apo's COM
//! skeleton): the registered CLSID resolves to a class factory that creates
//! an [`Activate`] (an `IMFActivate` whose attribute store the frame server
//! fills — that is how the geometry set on the `IMFVirtualCamera` reaches
//! us). `ActivateObject` builds the [`CamSource`] with one NV12 stream.
//!
//! Frames come from the [`crate::frames`] ring. The ring section is created
//! here on first use (the frame server runs as LOCAL SERVICE, which holds
//! `SeCreateGlobalPrivilege`); the receiver's create call then maps it and
//! writes frames. Until the first frame arrives — and whenever the receiver
//! is gone — the camera serves black, never an error: an app that opened
//! "Relay Camera" must keep getting frames.

// COM method names come from the interfaces, and the COM ABI hands the
// *_Impl trait methods raw pointers behind safe traits; the frame server
// guarantees their validity (same rationale as relay-apo's com.rs).
#![allow(unsafe_code)] // every block carries a SAFETY note
#![allow(non_snake_case)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, IUnknown, Interface, Ref, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_NOTIMPL, E_POINTER, S_FALSE, S_OK,
};
use windows::Win32::Media::KernelStreaming::{KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFAttributes_Impl, IMFGetService,
    IMFGetService_Impl, IMFMediaEvent, IMFMediaEventGenerator_Impl, IMFMediaEventQueue,
    IMFMediaSource, IMFMediaSourceEx, IMFMediaSourceEx_Impl, IMFMediaSource_Impl, IMFMediaStream2,
    IMFMediaStream2_Impl, IMFMediaStream_Impl, IMFPresentationDescriptor, IMFStreamDescriptor,
    MEMediaSample, MENewStream, MESourceStarted, MESourceStopped, MEStreamStarted, MEStreamStopped,
    MFCreateAttributes, MFCreateEventQueue, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreatePresentationDescriptor, MFCreateSample, MFCreateStreamDescriptor, MFGetSystemTime,
    MFMediaType_Video, MFSampleExtension_Token, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MFMEDIASOURCE_IS_LIVE, MF_DEVICESTREAM_STREAM_CATEGORY,
    MF_DEVICESTREAM_STREAM_ID, MF_E_INVALID_STATE_TRANSITION, MF_E_SHUTDOWN,
    MF_E_UNSUPPORTED_SERVICE, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_DEFAULT_STRIDE,
    MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_STREAM_STATE,
    MF_STREAM_STATE_PAUSED, MF_STREAM_STATE_RUNNING, MF_STREAM_STATE_STOPPED,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};

use super::{
    CLSID_RELAY_VCAM, DEFAULT_FPS, DEFAULT_HEIGHT, DEFAULT_WIDTH, RELAY_VCAM_ATTR_FPS,
    RELAY_VCAM_ATTR_HEIGHT, RELAY_VCAM_ATTR_WIDTH,
};
use crate::frames::{section_name_from_env, SharedFrames, SLOT_BYTES};

/// `MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES` (not exported by the crate).
const MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES: GUID =
    GUID::from_u128(0x17145FD1_1B2B_423C_8001_2B6833ED3588);
/// `MFFrameSourceTypes::Color`.
const FRAMESOURCE_COLOR: u32 = 0x0001;

// ---------------------------------------------------------------------------
// Activate
// ---------------------------------------------------------------------------

/// The registered activation object. Its attribute store is what the frame
/// server populates with the `IMFVirtualCamera` attributes (our geometry).
#[implement(IMFActivate)]
pub struct Activate {
    attrs: IMFAttributes,
    source: Mutex<Option<IMFMediaSourceEx>>,
}

impl Activate {
    pub fn create() -> windows::core::Result<IMFActivate> {
        let mut attrs = None;
        // SAFETY: standard attribute-store creation.
        unsafe { MFCreateAttributes(&mut attrs, 4)? };
        let attrs = attrs.expect("MFCreateAttributes out");
        Ok(Activate { attrs, source: Mutex::new(None) }.into())
    }
}

/// Forward one `IMFAttributes` method to the inner store.
macro_rules! fwd {
    ($self:ident . $method:ident ( $($arg:expr),* )) => {{
        // SAFETY: pure delegation; the inner store validates its arguments.
        unsafe { $self.attrs.$method($($arg),*) }
    }};
}

#[allow(clippy::missing_safety_doc)]
impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> windows::core::Result<()> {
        fwd!(self.GetItem(key, (!value.is_null()).then_some(value)))
    }
    fn GetItemType(
        &self,
        key: *const GUID,
    ) -> windows::core::Result<windows::Win32::Media::MediaFoundation::MF_ATTRIBUTE_TYPE> {
        fwd!(self.GetItemType(key))
    }
    fn CompareItem(
        &self,
        key: *const GUID,
        value: *const PROPVARIANT,
    ) -> windows::core::Result<windows::core::BOOL> {
        fwd!(self.CompareItem(key, value))
    }
    fn Compare(
        &self,
        theirs: Ref<IMFAttributes>,
        match_type: windows::Win32::Media::MediaFoundation::MF_ATTRIBUTES_MATCH_TYPE,
    ) -> windows::core::Result<windows::core::BOOL> {
        fwd!(self.Compare(theirs.as_ref(), match_type))
    }
    fn GetUINT32(&self, key: *const GUID) -> windows::core::Result<u32> {
        fwd!(self.GetUINT32(key))
    }
    fn GetUINT64(&self, key: *const GUID) -> windows::core::Result<u64> {
        fwd!(self.GetUINT64(key))
    }
    fn GetDouble(&self, key: *const GUID) -> windows::core::Result<f64> {
        fwd!(self.GetDouble(key))
    }
    fn GetGUID(&self, key: *const GUID) -> windows::core::Result<GUID> {
        fwd!(self.GetGUID(key))
    }
    fn GetStringLength(&self, key: *const GUID) -> windows::core::Result<u32> {
        fwd!(self.GetStringLength(key))
    }
    fn GetString(
        &self,
        key: *const GUID,
        value: windows::core::PWSTR,
        size: u32,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        // SAFETY: delegation; buffer contract is the caller's.
        unsafe {
            self.attrs.GetString(
                key,
                std::slice::from_raw_parts_mut(value.0, size as usize),
                (!length.is_null()).then_some(length),
            )
        }
    }
    fn GetAllocatedString(
        &self,
        key: *const GUID,
        value: *mut windows::core::PWSTR,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        fwd!(self.GetAllocatedString(key, value, length))
    }
    fn GetBlobSize(&self, key: *const GUID) -> windows::core::Result<u32> {
        fwd!(self.GetBlobSize(key))
    }
    fn GetBlob(
        &self,
        key: *const GUID,
        buf: *mut u8,
        size: u32,
        blobsize: *mut u32,
    ) -> windows::core::Result<()> {
        // SAFETY: delegation; buffer contract is the caller's.
        unsafe {
            self.attrs.GetBlob(
                key,
                std::slice::from_raw_parts_mut(buf, size as usize),
                (!blobsize.is_null()).then_some(blobsize),
            )
        }
    }
    fn GetAllocatedBlob(
        &self,
        key: *const GUID,
        buf: *mut *mut u8,
        size: *mut u32,
    ) -> windows::core::Result<()> {
        fwd!(self.GetAllocatedBlob(key, buf, size))
    }
    fn GetUnknown(
        &self,
        key: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        // SAFETY: fetch as IUnknown, then QI to the requested interface.
        unsafe {
            let unk: IUnknown = self.attrs.GetUnknown(key)?;
            unk.query(riid, ppv).ok()
        }
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> windows::core::Result<()> {
        fwd!(self.SetItem(key, value))
    }
    fn DeleteItem(&self, key: *const GUID) -> windows::core::Result<()> {
        fwd!(self.DeleteItem(key))
    }
    fn DeleteAllItems(&self) -> windows::core::Result<()> {
        fwd!(self.DeleteAllItems())
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> windows::core::Result<()> {
        fwd!(self.SetUINT32(key, value))
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> windows::core::Result<()> {
        fwd!(self.SetUINT64(key, value))
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> windows::core::Result<()> {
        fwd!(self.SetDouble(key, value))
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> windows::core::Result<()> {
        fwd!(self.SetGUID(key, value))
    }
    fn SetString(
        &self,
        key: *const GUID,
        value: &windows::core::PCWSTR,
    ) -> windows::core::Result<()> {
        fwd!(self.SetString(key, *value))
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> windows::core::Result<()> {
        // SAFETY: delegation; buffer contract is the caller's.
        unsafe { self.attrs.SetBlob(key, std::slice::from_raw_parts(buf, size as usize)) }
    }
    fn SetUnknown(&self, key: *const GUID, unknown: Ref<IUnknown>) -> windows::core::Result<()> {
        fwd!(self.SetUnknown(key, unknown.as_ref()))
    }
    fn LockStore(&self) -> windows::core::Result<()> {
        fwd!(self.LockStore())
    }
    fn UnlockStore(&self) -> windows::core::Result<()> {
        fwd!(self.UnlockStore())
    }
    fn GetCount(&self) -> windows::core::Result<u32> {
        fwd!(self.GetCount())
    }
    fn GetItemByIndex(
        &self,
        index: u32,
        key: *mut GUID,
        value: *mut PROPVARIANT,
    ) -> windows::core::Result<()> {
        fwd!(self.GetItemByIndex(index, key, (!value.is_null()).then_some(value)))
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> windows::core::Result<()> {
        fwd!(self.CopyAllItems(dest.as_ref()))
    }
}

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(
        &self,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if riid.is_null() || ppv.is_null() {
            return Err(E_POINTER.into());
        }
        let mut guard = self.source.lock().unwrap();
        if guard.is_none() {
            // SAFETY: reads of our own attribute store.
            let (w, h, fps) = unsafe {
                (
                    self.attrs.GetUINT32(&RELAY_VCAM_ATTR_WIDTH).unwrap_or(DEFAULT_WIDTH),
                    self.attrs.GetUINT32(&RELAY_VCAM_ATTR_HEIGHT).unwrap_or(DEFAULT_HEIGHT),
                    self.attrs.GetUINT32(&RELAY_VCAM_ATTR_FPS).unwrap_or(DEFAULT_FPS),
                )
            };
            *guard = Some(CamSource::create(w, h, fps)?);
        }
        let source = guard.as_ref().expect("just set");
        // SAFETY: riid/ppv checked non-null above.
        unsafe { source.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> windows::core::Result<()> {
        if let Some(source) = self.source.lock().unwrap().take() {
            // SAFETY: valid source object.
            unsafe { source.Shutdown()? };
        }
        Ok(())
    }

    fn DetachObject(&self) -> windows::core::Result<()> {
        *self.source.lock().unwrap() = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// State shared between the source and its single video stream.
struct StreamShared {
    queue: IMFMediaEventQueue,
    descriptor: IMFStreamDescriptor,
    attrs: IMFAttributes,
    width: u32,
    height: u32,
    fps: u32,
    /// Back-reference for `GetMediaSource`; cleared on shutdown to break
    /// the cycle.
    parent: Mutex<Option<IMFMediaSource>>,
    state: Mutex<StreamState>,
    /// The frame ring; attached lazily on the first sample request.
    ring: Mutex<RingState>,
    /// Ring sequence of the last frame served (diagnostics only — a stale
    /// ring frame is re-served at the new timestamp, which is exactly the
    /// "hold last frame" behaviour a camera consumer expects).
    last_seq: AtomicU64,
}

#[derive(Clone, Copy, PartialEq)]
enum StreamState {
    Stopped,
    Running,
    Shutdown,
}

#[derive(Default)]
struct RingState {
    section: Option<SharedFrames>,
    scratch: Vec<u8>,
}

// SAFETY: all interior state is Mutex/atomic guarded; the MF interfaces held
// are agile within the frame server's usage.
unsafe impl Send for StreamShared {}
unsafe impl Sync for StreamShared {}

impl StreamShared {
    fn frame_bytes(&self) -> usize {
        (self.width as usize * self.height as usize) * 3 / 2
    }

    /// Copy the newest ring frame into `out` (true) or leave it as black
    /// (false). Never fails: no ring / no receiver / wrong size = black.
    fn fill_from_ring(&self, out: &mut [u8]) -> bool {
        let mut ring = self.ring.lock().unwrap();
        if ring.section.is_none() {
            ring.section = SharedFrames::create(&section_name_from_env()).ok();
        }
        if ring.scratch.len() < SLOT_BYTES {
            ring.scratch.resize(SLOT_BYTES, 0);
        }
        let RingState { section, scratch } = &mut *ring;
        let Some(section) = section.as_ref() else { return false };
        let Some(info) = section.block().read_latest(scratch) else { return false };
        if (info.width, info.height) != (self.width, self.height) {
            return false;
        }
        out.copy_from_slice(&scratch[..out.len()]);
        self.last_seq.store(info.seq, Ordering::Relaxed);
        true
    }
}

#[implement(IMFMediaStream2)]
struct CamStream {
    shared: Arc<StreamShared>,
}

impl CamStream {
    fn create(
        width: u32,
        height: u32,
        fps: u32,
    ) -> windows::core::Result<(IMFMediaStream2, Arc<StreamShared>)> {
        // SAFETY: standard MF object creation; attribute keys are the
        // documented types.
        unsafe {
            let mt = MFCreateMediaType()?;
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            mt.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)?;
            mt.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
            mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
            mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            mt.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            mt.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
            mt.SetUINT32(&MF_MT_SAMPLE_SIZE, (width * height * 3 / 2) as u32)?;

            let descriptor = MFCreateStreamDescriptor(0, &[Some(mt.clone())])?;
            descriptor.GetMediaTypeHandler()?.SetCurrentMediaType(&mt)?;

            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 4)?;
            let attrs = attrs.expect("MFCreateAttributes out");
            attrs.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &pin_category())?;
            attrs.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
            attrs.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, FRAMESOURCE_COLOR)?;

            let shared = Arc::new(StreamShared {
                queue: MFCreateEventQueue()?,
                descriptor,
                attrs,
                width,
                height,
                fps: fps.max(1),
                parent: Mutex::new(None),
                state: Mutex::new(StreamState::Stopped),
                ring: Mutex::new(RingState::default()),
                last_seq: AtomicU64::new(0),
            });
            let stream: IMFMediaStream2 = CamStream { shared: shared.clone() }.into();
            Ok((stream, shared))
        }
    }
}

/// `PINNAME_VIDEO_CAPTURE` as a plain GUID.
fn pin_category() -> GUID {
    // KSIDENTIFIER's Set GUID is the category; the constant is the GUID
    // itself in the KS headers.
    let _: Option<KSIDENTIFIER> = None; // keep the KS import honest
    PINNAME_VIDEO_CAPTURE
}

impl IMFMediaEventGenerator_Impl for CamStream_Impl {
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: delegation to the event queue.
        unsafe { self.shared.queue.GetEvent(flags.0 as u32) }
    }
    fn BeginGetEvent(
        &self,
        callback: Ref<windows::Win32::Media::MediaFoundation::IMFAsyncCallback>,
        state: Ref<IUnknown>,
    ) -> windows::core::Result<()> {
        // SAFETY: delegation to the event queue.
        unsafe { self.shared.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(
        &self,
        result: Ref<windows::Win32::Media::MediaFoundation::IMFAsyncResult>,
    ) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: delegation to the event queue.
        unsafe { self.shared.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        extended_type: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        // SAFETY: delegation to the event queue.
        unsafe { self.shared.queue.QueueEventParamVar(met, extended_type, status, value) }
    }
}

impl IMFMediaStream_Impl for CamStream_Impl {
    fn GetMediaSource(&self) -> windows::core::Result<IMFMediaSource> {
        self.shared
            .parent
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| windows::core::Error::from_hresult(MF_E_SHUTDOWN))
    }

    fn GetStreamDescriptor(&self) -> windows::core::Result<IMFStreamDescriptor> {
        Ok(self.shared.descriptor.clone())
    }

    fn RequestSample(&self, token: Ref<IUnknown>) -> windows::core::Result<()> {
        let state = *self.shared.state.lock().unwrap();
        if state == StreamState::Shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        let bytes = self.shared.frame_bytes();
        // SAFETY: standard sample construction; buffer locked/unlocked in
        // pairs and never escapes.
        unsafe {
            let buffer = MFCreateMemoryBuffer(bytes as u32)?;
            {
                let mut data: *mut u8 = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None)?;
                let out = std::slice::from_raw_parts_mut(data, bytes);
                if !self.shared.fill_from_ring(out) {
                    // Black NV12: luma 0x10, chroma 0x80.
                    let y_len = self.shared.width as usize * self.shared.height as usize;
                    out[..y_len].fill(0x10);
                    out[y_len..].fill(0x80);
                }
                buffer.Unlock()?;
                buffer.SetCurrentLength(bytes as u32)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(10_000_000 / self.shared.fps as i64)?;
            if let Some(token) = token.as_ref() {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            self.shared.queue.QueueEventParamUnk(
                MEMediaSample.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &sample,
            )?;
        }
        Ok(())
    }
}

impl IMFMediaStream2_Impl for CamStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> windows::core::Result<()> {
        let mut state = self.shared.state.lock().unwrap();
        if *state == StreamState::Shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        // SAFETY: event queue delegation.
        unsafe {
            match value {
                MF_STREAM_STATE_RUNNING => {
                    *state = StreamState::Running;
                    self.shared.queue.QueueEventParamVar(
                        MEStreamStarted.0 as u32,
                        &GUID::zeroed(),
                        S_OK,
                        std::ptr::null(),
                    )?;
                }
                MF_STREAM_STATE_STOPPED => {
                    *state = StreamState::Stopped;
                    self.shared.queue.QueueEventParamVar(
                        MEStreamStopped.0 as u32,
                        &GUID::zeroed(),
                        S_OK,
                        std::ptr::null(),
                    )?;
                }
                MF_STREAM_STATE_PAUSED => {
                    return Err(MF_E_INVALID_STATE_TRANSITION.into());
                }
                _ => return Err(windows::core::Error::from_hresult(E_NOTIMPL)),
            }
        }
        Ok(())
    }

    fn GetStreamState(&self) -> windows::core::Result<MF_STREAM_STATE> {
        Ok(match *self.shared.state.lock().unwrap() {
            StreamState::Running => MF_STREAM_STATE_RUNNING,
            _ => MF_STREAM_STATE_STOPPED,
        })
    }
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

struct SourceInner {
    stream: IMFMediaStream2,
    stream_shared: Arc<StreamShared>,
    pd: IMFPresentationDescriptor,
    started: bool,
    shutdown: bool,
}

#[implement(IMFMediaSourceEx, IMFGetService)]
pub struct CamSource {
    queue: IMFMediaEventQueue,
    attrs: IMFAttributes,
    inner: Mutex<SourceInner>,
}

impl CamSource {
    /// Build the source with one selected NV12 stream of the given geometry.
    pub fn create(width: u32, height: u32, fps: u32) -> windows::core::Result<IMFMediaSourceEx> {
        let width = width.clamp(2, crate::frames::MAX_WIDTH) & !1;
        let height = height.clamp(2, crate::frames::MAX_HEIGHT) & !1;
        let (stream, stream_shared) = CamStream::create(width, height, fps)?;
        // SAFETY: standard MF object creation.
        let (queue, attrs, pd) = unsafe {
            let queue = MFCreateEventQueue()?;
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.expect("MFCreateAttributes out");
            let pd =
                MFCreatePresentationDescriptor(Some(&[Some(stream_shared.descriptor.clone())]))?;
            pd.SelectStream(0)?;
            (queue, attrs, pd)
        };
        let object = windows_core::ComObject::new(CamSource {
            queue,
            attrs,
            inner: Mutex::new(SourceInner {
                stream,
                stream_shared,
                pd,
                started: false,
                shutdown: false,
            }),
        });
        let source: IMFMediaSourceEx = object.to_interface();
        // Wire the stream's back-reference now that the source exists (the
        // cycle is broken again in Shutdown).
        let parent: IMFMediaSource = source.cast()?;
        *object.inner.lock().unwrap().stream_shared.parent.lock().unwrap() = Some(parent);
        Ok(source)
    }
}

impl IMFMediaEventGenerator_Impl for CamSource_Impl {
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: delegation to the event queue.
        unsafe { self.queue.GetEvent(flags.0 as u32) }
    }
    fn BeginGetEvent(
        &self,
        callback: Ref<windows::Win32::Media::MediaFoundation::IMFAsyncCallback>,
        state: Ref<IUnknown>,
    ) -> windows::core::Result<()> {
        // SAFETY: delegation to the event queue.
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(
        &self,
        result: Ref<windows::Win32::Media::MediaFoundation::IMFAsyncResult>,
    ) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: delegation to the event queue.
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        extended_type: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        // SAFETY: delegation to the event queue.
        unsafe { self.queue.QueueEventParamVar(met, extended_type, status, value) }
    }
}

impl IMFMediaSource_Impl for CamSource_Impl {
    fn GetCharacteristics(&self) -> windows::core::Result<u32> {
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        let inner = self.inner.lock().unwrap();
        if inner.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        // SAFETY: valid descriptor.
        unsafe { inner.pd.Clone() }
    }

    fn Start(
        &self,
        _pd: Ref<IMFPresentationDescriptor>,
        _time_format: *const GUID,
        start_position: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        let first = !inner.started;
        inner.started = true;
        *inner.stream_shared.state.lock().unwrap() = StreamState::Running;
        // SAFETY: event queue delegation; the stream rides MENewStream as an
        // IUnknown param, per the media source contract.
        unsafe {
            if first {
                self.queue.QueueEventParamUnk(
                    MENewStream.0 as u32,
                    &GUID::zeroed(),
                    S_OK,
                    &inner.stream,
                )?;
            }
            inner.stream_shared.queue.QueueEventParamVar(
                MEStreamStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                start_position,
            )?;
            self.queue.QueueEventParamVar(
                MESourceStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                start_position,
            )?;
        }
        Ok(())
    }

    fn Stop(&self) -> windows::core::Result<()> {
        let inner = self.inner.lock().unwrap();
        if inner.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        *inner.stream_shared.state.lock().unwrap() = StreamState::Stopped;
        // SAFETY: event queue delegation.
        unsafe {
            inner.stream_shared.queue.QueueEventParamVar(
                MEStreamStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )?;
            self.queue.QueueEventParamVar(
                MESourceStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )?;
        }
        Ok(())
    }

    fn Pause(&self) -> windows::core::Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> windows::core::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.shutdown = true;
        *inner.stream_shared.state.lock().unwrap() = StreamState::Shutdown;
        // Break the parent cycle and drop the ring mapping.
        *inner.stream_shared.parent.lock().unwrap() = None;
        inner.stream_shared.ring.lock().unwrap().section = None;
        // SAFETY: queue shutdown is idempotent.
        unsafe {
            let _ = inner.stream_shared.queue.Shutdown();
            let _ = self.queue.Shutdown();
        }
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for CamSource_Impl {
    fn GetSourceAttributes(&self) -> windows::core::Result<IMFAttributes> {
        Ok(self.attrs.clone())
    }

    fn GetStreamAttributes(&self, _stream_id: u32) -> windows::core::Result<IMFAttributes> {
        Ok(self.inner.lock().unwrap().stream_shared.attrs.clone())
    }

    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> windows::core::Result<()> {
        // CPU frames only; the frame server copies as needed.
        Ok(())
    }
}

impl IMFGetService_Impl for CamSource_Impl {
    fn GetService(
        &self,
        _service: *const GUID,
        _riid: *const GUID,
        _ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
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
        let activate = Activate::create()?;
        // SAFETY: iid/object checked non-null above. The frame server asks
        // for IMFActivate (or IUnknown/IMFAttributes, which the activate
        // also answers).
        unsafe { activate.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: windows::core::BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

// The canonical `DllCanUnloadNow` / `DllGetClassObject` names exist only on
// the cdylib: build.rs aliases them with `/EXPORT:name=internal` through
// `cargo:rustc-cdylib-link-arg`. The internal symbols carry unique names so
// the rlib can link into the same binary as relay-apo (which exports the
// same COM entry points) without an LNK2005 collision.

/// The frame server keeps source DLLs loaded; never volunteer to unload.
/// (`pub` so fat LTO keeps the symbol for the cdylib /EXPORT alias.)
#[no_mangle]
pub extern "system" fn RelayVdeviceDllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[no_mangle]
pub extern "system" fn RelayVdeviceDllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null; the caller passes valid GUID pointers.
    unsafe {
        if *rclsid != CLSID_RELAY_VCAM {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        match factory.query(riid, ppv) {
            hr if hr == S_OK => S_OK,
            hr => hr,
        }
    }
}

// Silence the unused import when PCWSTR is only used through macros.
#[allow(unused)]
fn _pcwstr_used(_: PCWSTR) {}
