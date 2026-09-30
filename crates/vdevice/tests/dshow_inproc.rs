//! In-process proof of the Windows 10 "Relay Camera" DirectShow filter (S43).
//!
//! The filter is created through the DLL's own class-factory entry point —
//! exactly what COM does after `ICreateDevEnum` finds the registration, but
//! with no registration at all — and connected to a test sink pin that
//! plays the app's part (it accepts one subtype and records every sample).
//! Frames come from a `Local\` test ring, so nothing here needs privileges,
//! a filter graph, or any change to the machine.

#![cfg(all(windows, feature = "com"))]
#![allow(unsafe_code, non_snake_case)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use relay_vdevice::camera::dshow::free_media_type;
use relay_vdevice::camera::picture::{self, PixFmt};
use relay_vdevice::camera::source::RelayVdeviceDllGetClassObject;
use relay_vdevice::camera::CLSID_RELAY_DSHOW;
use relay_vdevice::frames::{dshow_section_name_from_env, SharedFrames};
use windows::core::{implement, Interface, Ref, GUID, HRESULT, PWSTR};
use windows::Win32::Foundation::{E_NOTIMPL, E_UNEXPECTED, S_OK};
use windows::Win32::Media::DirectShow::{
    IAMFilterMiscFlags, IAMStreamConfig, IBaseFilter, IEnumMediaTypes, IMediaSample, IMemAllocator,
    IMemInputPin, IMemInputPin_Impl, IPin, IPin_Impl, ALLOCATOR_PROPERTIES,
    AMPROPERTY_PIN_CATEGORY, AM_FILTER_MISC_FLAGS_IS_SOURCE, PINDIR_INPUT, PINDIR_OUTPUT,
    PIN_DIRECTION, PIN_INFO, VFW_E_NO_ALLOCATOR, VFW_E_TYPE_NOT_ACCEPTED, VIDEO_STREAM_CONFIG_CAPS,
};
use windows::Win32::Media::KernelStreaming::IKsPropertySet;
use windows::Win32::Media::MediaFoundation::{
    AMPROPSETID_Pin, AM_MEDIA_TYPE, MEDIASUBTYPE_NV12, MEDIASUBTYPE_RGB24, MEDIASUBTYPE_YUY2,
    PIN_CATEGORY_CAPTURE, VIDEOINFOHEADER,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, IClassFactory, COINIT_MULTITHREADED,
};

// ---------------------------------------------------------------------------
// The app's side: a sink pin that accepts one subtype and records samples.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Log {
    /// (subtype, width, height) of the accepted connection.
    connected: Option<(GUID, i32, i32)>,
    /// (start, end, bytes) per received sample.
    samples: Vec<(i64, i64, Vec<u8>)>,
}

#[implement(IPin, IMemInputPin)]
struct Sink {
    accept: GUID,
    log: Arc<Mutex<Log>>,
    peer: Mutex<Option<IPin>>,
}

impl IPin_Impl for Sink_Impl {
    fn Connect(&self, _: Ref<IPin>, _: *const AM_MEDIA_TYPE) -> windows::core::Result<()> {
        Err(E_UNEXPECTED.into())
    }
    fn ReceiveConnection(
        &self,
        connector: Ref<IPin>,
        pmt: *const AM_MEDIA_TYPE,
    ) -> windows::core::Result<()> {
        // SAFETY: the filter passes a valid media type with a VIDEOINFOHEADER.
        let mt = unsafe { &*pmt };
        if mt.subtype != self.accept {
            return Err(VFW_E_TYPE_NOT_ACCEPTED.into());
        }
        let vih = unsafe { std::ptr::read_unaligned(mt.pbFormat as *const VIDEOINFOHEADER) };
        self.log.lock().unwrap().connected =
            Some((mt.subtype, vih.bmiHeader.biWidth, vih.bmiHeader.biHeight));
        *self.peer.lock().unwrap() = connector.cloned();
        Ok(())
    }
    fn Disconnect(&self) -> windows::core::Result<()> {
        *self.peer.lock().unwrap() = None;
        Ok(())
    }
    fn ConnectedTo(&self) -> windows::core::Result<IPin> {
        self.peer.lock().unwrap().clone().ok_or_else(|| E_UNEXPECTED.into())
    }
    fn ConnectionMediaType(&self, _: *mut AM_MEDIA_TYPE) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn QueryPinInfo(&self, info: *mut PIN_INFO) -> windows::core::Result<()> {
        // SAFETY: caller-owned out struct.
        unsafe { std::ptr::write(info, PIN_INFO { dir: PINDIR_INPUT, ..Default::default() }) };
        Ok(())
    }
    fn QueryDirection(&self) -> windows::core::Result<PIN_DIRECTION> {
        Ok(PINDIR_INPUT)
    }
    fn QueryId(&self) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
    fn QueryAccept(&self, _: *const AM_MEDIA_TYPE) -> HRESULT {
        S_OK
    }
    fn EnumMediaTypes(&self) -> windows::core::Result<IEnumMediaTypes> {
        Err(E_NOTIMPL.into())
    }
    fn QueryInternalConnections(
        &self,
        _: windows::core::OutRef<IPin>,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn EndOfStream(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn BeginFlush(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn EndFlush(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn NewSegment(&self, _: i64, _: i64, _: f64) -> windows::core::Result<()> {
        Ok(())
    }
}

impl IMemInputPin_Impl for Sink_Impl {
    fn GetAllocator(&self) -> windows::core::Result<IMemAllocator> {
        // Make the filter bring its own (CLSID_MemoryAllocator, quartz).
        Err(VFW_E_NO_ALLOCATOR.into())
    }
    fn NotifyAllocator(
        &self,
        _: Ref<IMemAllocator>,
        _: windows::core::BOOL,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn GetAllocatorRequirements(&self) -> windows::core::Result<ALLOCATOR_PROPERTIES> {
        Err(E_NOTIMPL.into())
    }
    fn Receive(&self, sample: Ref<IMediaSample>) -> windows::core::Result<()> {
        let sample = sample.ok()?;
        // SAFETY: a live sample for the duration of the call.
        unsafe {
            let (mut s, mut e) = (0i64, 0i64);
            sample.GetTime(&mut s, &mut e)?;
            let len = sample.GetActualDataLength() as usize;
            let bytes = std::slice::from_raw_parts(sample.GetPointer()?, len).to_vec();
            let mut log = self.log.lock().unwrap();
            if log.samples.len() < 200 {
                log.samples.push((s, e, bytes));
            }
        }
        Ok(())
    }
    fn ReceiveMultiple(
        &self,
        _: *const Option<IMediaSample>,
        _: i32,
    ) -> windows::core::Result<i32> {
        Err(E_NOTIMPL.into())
    }
    fn ReceiveCanBlock(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn com() {
    // SAFETY: per-thread init; an already-initialised thread is fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

fn setup_env() {
    // One instance name for the whole test binary; every test uses it.
    std::env::set_var("RELAY_INSTANCE", format!("dshowtest{}", std::process::id()));
}

/// Create the filter the way COM would: DllGetClassObject → CreateInstance.
fn filter_via_class_factory() -> IBaseFilter {
    // SAFETY: our own exported entry point, called with valid pointers.
    unsafe {
        let mut factory: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr =
            RelayVdeviceDllGetClassObject(&CLSID_RELAY_DSHOW, &IClassFactory::IID, &mut factory);
        assert_eq!(hr, S_OK, "class object for the DirectShow CLSID");
        let factory = IClassFactory::from_raw(factory);
        factory
            .CreateInstance::<Option<&windows::core::IUnknown>, IBaseFilter>(None)
            .expect("CreateInstance")
    }
}

fn output_pin(filter: &IBaseFilter) -> IPin {
    // SAFETY: standard enumeration.
    unsafe {
        let e = filter.EnumPins().expect("EnumPins");
        let mut pins = [None];
        let mut n = 0u32;
        assert_eq!(e.Next(&mut pins, Some(&mut n)), S_OK);
        assert_eq!(n, 1);
        let mut more = [None];
        assert_ne!(e.Next(&mut more, None), S_OK, "exactly one pin");
        pins[0].take().expect("pin")
    }
}

fn run_with_sink(accept: GUID, for_ms: u64, pre: impl FnOnce(&IPin)) -> (Log, IBaseFilter) {
    let filter = filter_via_class_factory();
    let pin = output_pin(&filter);
    pre(&pin);
    let log = Arc::new(Mutex::new(Log::default()));
    let sink: IPin = Sink { accept, log: log.clone(), peer: Mutex::new(None) }.into();
    // SAFETY: standard DirectShow calls on live objects.
    unsafe {
        pin.Connect(&sink, None).expect("Connect");
        filter.Run(0).expect("Run");
        std::thread::sleep(Duration::from_millis(for_ms));
        filter.Stop().expect("Stop");
        pin.Disconnect().expect("Disconnect");
    }
    let log = std::mem::take(&mut *log.lock().unwrap());
    (log, filter)
}

fn test_frame(w: u32, h: u32) -> Vec<u8> {
    (0..picture::nv12_bytes(w, h)).map(|i| (16 + i % 200) as u8).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn filter_answers_the_interfaces_webcam_apps_probe() {
    com();
    setup_env();
    let filter = filter_via_class_factory();
    // SAFETY: standard COM calls on live objects.
    unsafe {
        assert_eq!(filter.GetClassID().unwrap(), CLSID_RELAY_DSHOW);
        let flags: IAMFilterMiscFlags = filter.cast().expect("IAMFilterMiscFlags");
        assert_eq!(flags.GetMiscFlags(), AM_FILTER_MISC_FLAGS_IS_SOURCE.0 as u32);

        let pin = output_pin(&filter);
        assert_eq!(pin.QueryDirection().unwrap(), PINDIR_OUTPUT);
        let id = pin.QueryId().unwrap();
        assert_eq!(id.to_string().unwrap(), "Capture");
        CoTaskMemFree(Some(id.0 as *const _));
        assert!(filter.FindPin(windows::core::w!("Capture")).is_ok());

        // QueryPinInfo hands back the owning filter.
        let mut info = PIN_INFO::default();
        pin.QueryPinInfo(&mut info).unwrap();
        let owner = std::mem::ManuallyDrop::take(&mut info.pFilter).expect("owner");
        assert_eq!(owner.as_raw(), filter.as_raw());
        drop(owner);

        // PIN_CATEGORY_CAPTURE through IKsPropertySet.
        let ks: IKsPropertySet = pin.cast().expect("IKsPropertySet");
        let mut cat = GUID::zeroed();
        let mut got = 0u32;
        ks.Get(
            &AMPROPSETID_Pin,
            AMPROPERTY_PIN_CATEGORY.0 as u32,
            std::ptr::null(),
            0,
            &mut cat as *mut _ as *mut _,
            std::mem::size_of::<GUID>() as u32,
            &mut got,
        )
        .unwrap();
        assert_eq!(cat, PIN_CATEGORY_CAPTURE);
        assert_eq!(got as usize, std::mem::size_of::<GUID>());

        // IAMStreamConfig: 3 formats per offered size, NV12 first.
        let sc: IAMStreamConfig = pin.cast().expect("IAMStreamConfig");
        let (mut count, mut size) = (0i32, 0i32);
        sc.GetNumberOfCapabilities(&mut count, &mut size).unwrap();
        assert!(count >= 9 && count % 3 == 0, "{count}");
        assert_eq!(size as usize, std::mem::size_of::<VIDEO_STREAM_CONFIG_CAPS>());
        let mut subtypes = Vec::new();
        for i in 0..3 {
            let mut caps = VIDEO_STREAM_CONFIG_CAPS::default();
            let mut mt: *mut AM_MEDIA_TYPE = std::ptr::null_mut();
            sc.GetStreamCaps(i, &mut mt, &mut caps as *mut _ as *mut u8).unwrap();
            subtypes.push((*mt).subtype);
            assert_eq!(caps.MaxOutputSize.cx, caps.MinOutputSize.cx);
            free_media_type(mt);
            CoTaskMemFree(Some(mt as *const _));
        }
        assert_eq!(subtypes, vec![MEDIASUBTYPE_NV12, MEDIASUBTYPE_YUY2, MEDIASUBTYPE_RGB24]);

        // A foreign IID from the class factory is refused, not crashed on.
        let mut obj: *mut core::ffi::c_void = std::ptr::null_mut();
        let bogus = GUID::from_u128(0x12345678_0000_0000_0000_000000000001);
        assert_ne!(RelayVdeviceDllGetClassObject(&bogus, &IClassFactory::IID, &mut obj), S_OK);
    }
}

/// One test drives the shared ring through its states in order: no
/// producer (waiting still), NV12 pass-through, YUY2 and RGB24 conversion,
/// SetFormat to another size (scaled), then a stalled producer.
#[test]
fn streams_ring_frames_in_every_format_with_monotonic_timestamps() {
    com();
    setup_env();
    let (w, h) = (320u32, 180u32);
    let ring = SharedFrames::create(&dshow_section_name_from_env()).expect("ring");
    ring.block().set_geometry_hint(w, h, 30);

    // 1. Nothing written yet: the waiting still, at the hinted size.
    let (log, _) = run_with_sink(MEDIASUBTYPE_NV12, 250, |_| {});
    assert_eq!(log.connected, Some((MEDIASUBTYPE_NV12, w as i32, h as i32)));
    assert!(log.samples.len() >= 3, "frames keep coming with no producer: {}", log.samples.len());
    let waiting = picture::waiting_frame_nv12(w, h);
    assert!(log.samples.iter().all(|s| s.2 == waiting), "waiting still served");

    // A producer that keeps writing for the rest of the test.
    let frame = test_frame(w, h);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (frame, stop) = (frame.clone(), stop.clone());
        let name = dshow_section_name_from_env();
        std::thread::spawn(move || {
            let ring = SharedFrames::create(&name).expect("writer ring");
            let (y, uv) = frame.split_at((w * h) as usize);
            let mut pts = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                ring.block().write_frame(w, h, pts, y, w as usize, uv, w as usize);
                pts += 333_333;
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    std::thread::sleep(Duration::from_millis(50));

    // 2. NV12: byte-exact pass-through; timestamps strictly increasing.
    let (log, _) = run_with_sink(MEDIASUBTYPE_NV12, 400, |_| {});
    let live: Vec<_> = log.samples.iter().filter(|s| s.2 == frame).collect();
    assert!(live.len() >= 5, "ring frames delivered: {} of {}", live.len(), log.samples.len());
    for pair in log.samples.windows(2) {
        assert!(
            pair[1].0 > pair[0].0,
            "start times strictly increase: {pair:?}",
            pair = (pair[0].0, pair[1].0)
        );
        assert!(pair[1].0 >= pair[0].1 || pair[0].0 == 0, "no overlap after preroll");
    }
    assert!(log.samples.iter().all(|s| s.1 > s.0), "end after start");
    // Paced near the negotiated 30 fps, not flooding.
    assert!(log.samples.len() <= 20, "{} samples in 400 ms", log.samples.len());

    // 3. YUY2 and RGB24 for apps that insist.
    for (sub, fmt) in [(MEDIASUBTYPE_YUY2, PixFmt::Yuy2), (MEDIASUBTYPE_RGB24, PixFmt::Rgb24)] {
        let (log, _) = run_with_sink(sub, 250, |_| {});
        assert_eq!(log.connected, Some((sub, w as i32, h as i32)));
        let mut want = vec![0u8; fmt.frame_bytes(w, h)];
        picture::convert_nv12(fmt, &frame, w, h, &mut want);
        assert!(log.samples.iter().any(|s| s.2 == want), "{fmt:?} conversion delivered");
    }

    // 4. SetFormat to 640x360 YUY2 before connecting: honoured and scaled.
    let (log, _) = run_with_sink(MEDIASUBTYPE_YUY2, 250, |pin| unsafe {
        let sc: IAMStreamConfig = pin.cast().unwrap();
        let mt = sc.GetFormat().unwrap();
        (*mt).subtype = MEDIASUBTYPE_YUY2;
        let vih = &mut *((*mt).pbFormat as *mut VIDEOINFOHEADER);
        vih.bmiHeader.biWidth = 640;
        vih.bmiHeader.biHeight = 360;
        sc.SetFormat(mt).expect("SetFormat");
        free_media_type(mt);
        CoTaskMemFree(Some(mt as *const _));
    });
    assert_eq!(log.connected, Some((MEDIASUBTYPE_YUY2, 640, 360)));
    let mut scaled = vec![0u8; picture::nv12_bytes(640, 360)];
    picture::scale_nv12(&frame, w, h, &mut scaled, 640, 360);
    let mut want = vec![0u8; PixFmt::Yuy2.frame_bytes(640, 360)];
    picture::convert_nv12(PixFmt::Yuy2, &scaled, 640, 360, &mut want);
    assert!(log.samples.iter().any(|s| s.2 == want), "scaled YUY2 delivered");

    // 5. Producer gone: after the stale window the still comes back rather
    //    than a frozen last frame.
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    let (log, _) = run_with_sink(MEDIASUBTYPE_NV12, 2600, |_| {});
    assert_eq!(
        log.samples.last().map(|s| &s.2),
        Some(&waiting),
        "waiting still after the producer stops"
    );
    drop(ring);
}

#[test]
fn filter_and_pin_are_freed_with_no_cycle() {
    com();
    setup_env();
    let filter = filter_via_class_factory();
    let pin = output_pin(&filter);
    drop(filter);
    // SAFETY: the pin outlives the filter; QueryPinInfo must not crash and
    // simply reports no owner.
    unsafe {
        let mut info = PIN_INFO::default();
        pin.QueryPinInfo(&mut info).unwrap();
        assert!(std::mem::ManuallyDrop::take(&mut info.pFilter).is_none());
    }
}
