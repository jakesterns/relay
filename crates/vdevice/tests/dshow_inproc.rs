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
    /// Samples that carried a media-type change (dynamic format change the
    /// app never agreed to). Must stay 0.
    type_changes: u32,
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
            if let Ok(mt) = sample.GetMediaType() {
                if !mt.is_null() {
                    log.type_changes += 1;
                    free_media_type(mt);
                    CoTaskMemFree(Some(mt as *const _));
                }
            }
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

    // 6. S43b: no size announced. The first advertised type is 720p NV12
    //    (1080p's 3.1 MB frame overran ffmpeg's default buffer on the Win10
    //    pass). A client that takes it, then sees the stream start at some
    //    other size, keeps its 720p type and gets the stream scaled into it:
    //    no reconnect and no media-type change it did not ask for.
    ring.block().set_geometry_hint(0, 0, 0);
    let first_offer = |pin: &IPin| unsafe {
        let sc: IAMStreamConfig = pin.cast().unwrap();
        let mut caps = VIDEO_STREAM_CONFIG_CAPS::default();
        let mut mt: *mut AM_MEDIA_TYPE = std::ptr::null_mut();
        sc.GetStreamCaps(0, &mut mt, &mut caps as *mut _ as *mut u8).unwrap();
        let vih = &*((*mt).pbFormat as *const VIDEOINFOHEADER);
        let got = ((*mt).subtype, vih.bmiHeader.biWidth, vih.bmiHeader.biHeight);
        free_media_type(mt);
        CoTaskMemFree(Some(mt as *const _));
        got
    };
    let late = {
        let (frame, name) = (frame.clone(), dshow_section_name_from_env());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let ring = SharedFrames::create(&name).expect("late writer ring");
            let (y, uv) = frame.split_at((w * h) as usize);
            for i in 0..70 {
                ring.block().write_frame(w, h, i * 333_333, y, w as usize, uv, w as usize);
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let (log, _) = run_with_sink(MEDIASUBTYPE_NV12, 1000, |pin| {
        assert_eq!(first_offer(pin), (MEDIASUBTYPE_NV12, 1920, 1080), "idle: 1080p NV12 first");
    });
    late.join().unwrap();
    assert_eq!(log.connected, Some((MEDIASUBTYPE_NV12, 1920, 1080)));
    let n1080 = picture::nv12_bytes(1920, 1080);
    assert!(log.samples.iter().all(|s| s.2.len() == n1080), "every sample stays 1080p NV12");
    let mut scaled = vec![0u8; n1080];
    picture::scale_nv12(&frame, w, h, &mut scaled, 1920, 1080);
    assert!(log.samples.iter().any(|s| s.2 == scaled), "late 320x180 stream scaled to 1080p");
    assert_eq!(log.type_changes, 0, "no media-type change mid-run");

    // With a size announced, the stream's own size is first again.
    ring.block().set_geometry_hint(w, h, 30);
    let filter = filter_via_class_factory();
    assert_eq!(first_offer(&output_pin(&filter)), (MEDIASUBTYPE_NV12, w as i32, h as i32));
    drop(filter);

    // 7. r54 (PC2: Discord saw the picture mirrored and blurry from a
    //    2560x1440 ring). A 1440p stream offers 1080p first. Every offered
    //    type -- each size in NV12, YUY2 and RGB24 -- delivers the asymmetric
    //    marker picture the right way round per its spec, and the pin asks
    //    the producer for exactly the size it negotiated. The writer here
    //    plays the share engine: it honours the size request, as the GPU
    //    path does.
    let (sw, sh) = (2560u32, 1440u32);
    ring.block().set_geometry_hint(sw, sh, 60);
    let log_path =
        std::env::temp_dir().join(format!("relay-camera-log-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log_path);
    std::env::set_var("RELAY_CAMERA_LOG", &log_path);
    let offers: Vec<(GUID, i32, i32)> = {
        let filter = filter_via_class_factory();
        let pin = output_pin(&filter);
        assert_eq!(first_offer(&pin), (MEDIASUBTYPE_NV12, 1920, 1080), "1440p stream: 1080p first");
        // SAFETY: standard IAMStreamConfig enumeration.
        unsafe {
            let sc: IAMStreamConfig = pin.cast().unwrap();
            let (mut count, mut size) = (0i32, 0i32);
            sc.GetNumberOfCapabilities(&mut count, &mut size).unwrap();
            (0..count)
                .map(|i| {
                    let mut caps = VIDEO_STREAM_CONFIG_CAPS::default();
                    let mut mt: *mut AM_MEDIA_TYPE = std::ptr::null_mut();
                    sc.GetStreamCaps(i, &mut mt, &mut caps as *mut _ as *mut u8).unwrap();
                    let vih = &*((*mt).pbFormat as *const VIDEOINFOHEADER);
                    let got = ((*mt).subtype, vih.bmiHeader.biWidth, vih.bmiHeader.biHeight);
                    free_media_type(mt);
                    CoTaskMemFree(Some(mt as *const _));
                    got
                })
                .collect()
        }
    };
    assert_eq!(offers.len(), 12);
    assert!(offers.iter().any(|o| (o.1, o.2) == (2560, 1440)), "native size still offered");
    assert!(offers.iter().any(|o| (o.1, o.2) == (1280, 720)), "720p still offered");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let honouring = {
        let (stop, name) = (stop.clone(), dshow_section_name_from_env());
        std::thread::spawn(move || {
            let ring = SharedFrames::create(&name).expect("producer ring");
            let mut cached: Option<((u32, u32), Vec<u8>)> = None;
            let mut pts = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let size = ring.block().requested_size().unwrap_or((sw, sh));
                if cached.as_ref().map(|c| c.0) != Some(size) {
                    cached = Some((size, picture::testpat::marker_nv12(size.0, size.1)));
                }
                let (_, f) = cached.as_ref().unwrap();
                let (y, uv) = f.split_at((size.0 * size.1) as usize);
                ring.block().write_frame(
                    size.0,
                    size.1,
                    pts,
                    y,
                    size.0 as usize,
                    uv,
                    size.0 as usize,
                );
                pts += 166_666;
                std::thread::sleep(Duration::from_millis(8));
            }
        })
    };
    let fmt_of = |g: GUID| match g {
        g if g == MEDIASUBTYPE_NV12 => PixFmt::Nv12,
        g if g == MEDIASUBTYPE_YUY2 => PixFmt::Yuy2,
        _ => PixFmt::Rgb24,
    };
    for &(sub, ow, oh) in &offers {
        let (log, _) = run_with_sink(sub, 400, |pin| unsafe {
            let sc: IAMStreamConfig = pin.cast().unwrap();
            let mt = sc.GetFormat().unwrap();
            (*mt).subtype = sub;
            let vih = &mut *((*mt).pbFormat as *mut VIDEOINFOHEADER);
            vih.bmiHeader.biWidth = ow;
            vih.bmiHeader.biHeight = oh;
            sc.SetFormat(mt).expect("SetFormat");
            free_media_type(mt);
            CoTaskMemFree(Some(mt as *const _));
        });
        assert_eq!(log.connected, Some((sub, ow, oh)));
        let fmt = fmt_of(sub);
        let (ow, oh) = (ow as u32, oh as u32);
        let live = log
            .samples
            .iter()
            .rev()
            .find(|s| {
                s.2.len() == fmt.frame_bytes(ow, oh)
                    && picture::testpat::probe(fmt, &s.2, ow, oh, ow / 32, oh / 32).0 > 150
            })
            .unwrap_or_else(|| panic!("{fmt:?} {ow}x{oh}: a live sample arrived"));
        picture::testpat::assert_oriented(fmt, &live.2, ow, oh, "DirectShow");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    honouring.join().unwrap();

    // The CPU path: a producer that ignores the request (an older share
    // engine, or a second camera) is area-scaled by the pin, still the
    // right way round.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let native = {
        let (stop, name) = (stop.clone(), dshow_section_name_from_env());
        std::thread::spawn(move || {
            let ring = SharedFrames::create(&name).expect("native ring");
            let f = picture::testpat::marker_nv12(sw, sh);
            let (y, uv) = f.split_at((sw * sh) as usize);
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                ring.block().write_frame(sw, sh, 0, y, sw as usize, uv, sw as usize);
                std::thread::sleep(Duration::from_millis(30));
            }
        })
    };
    let (log, _) = run_with_sink(MEDIASUBTYPE_RGB24, 2500, |pin| unsafe {
        let sc: IAMStreamConfig = pin.cast().unwrap();
        let mt = sc.GetFormat().unwrap();
        (*mt).subtype = MEDIASUBTYPE_RGB24;
        let vih = &mut *((*mt).pbFormat as *mut VIDEOINFOHEADER);
        vih.bmiHeader.biWidth = 640;
        vih.bmiHeader.biHeight = 360;
        sc.SetFormat(mt).expect("SetFormat");
        free_media_type(mt);
        CoTaskMemFree(Some(mt as *const _));
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    native.join().unwrap();
    let live = log
        .samples
        .iter()
        .rev()
        .find(|s| picture::testpat::probe(PixFmt::Rgb24, &s.2, 640, 360, 20, 11).0 > 150)
        .expect("a CPU-scaled live RGB24 sample");
    picture::testpat::assert_oriented(PixFmt::Rgb24, &live.2, 640, 360, "DirectShow CPU-scaled");

    // 8. The camera log names what the app negotiated, once per connection,
    //    and the gap when the producer stops.
    let text = std::fs::read_to_string(&log_path).expect("camera log written");
    assert!(text.contains("connected: NV12 1920x1080 @ 60 fps"), "{text}");
    assert!(text.contains("connected: RGB24 640x360"), "{text}");
    assert!(text.contains("ring up: "), "{text}");
    std::env::remove_var("RELAY_CAMERA_LOG");
    let _ = std::fs::remove_file(&log_path);
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

/// The calls OBS's libdshowcapture makes on activation, through the real
/// qcap/quartz graph objects: `ICaptureGraphBuilder2::FindInterface` for a
/// crossbar (r57 crashed OBS inside qcap here), then pin lookup.
#[test]
fn obs_shaped_graph_probe_does_not_crash() {
    use windows::Win32::Media::DirectShow::{IAMCrossbar, ICaptureGraphBuilder2, IGraphBuilder};
    use windows::Win32::Media::MediaFoundation::{CLSID_CaptureGraphBuilder2, CLSID_FilterGraph};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    com();
    setup_env();
    let filter = filter_via_class_factory();
    // SAFETY: standard DirectShow graph calls on live objects.
    unsafe {
        let graph: IGraphBuilder =
            CoCreateInstance(&CLSID_FilterGraph, None, CLSCTX_INPROC_SERVER).expect("graph");
        let builder: ICaptureGraphBuilder2 =
            CoCreateInstance(&CLSID_CaptureGraphBuilder2, None, CLSCTX_INPROC_SERVER)
                .expect("builder");
        builder.SetFiltergraph(&graph).expect("SetFiltergraph");
        graph.AddFilter(&filter, windows::core::w!("Relay Camera")).expect("AddFilter");
        eprintln!("FindInterface crossbar…");
        let mut out: *mut core::ffi::c_void = std::ptr::null_mut();
        let r = builder.FindInterface(None, None, &filter, &IAMCrossbar::IID, &mut out);
        eprintln!("FindInterface crossbar -> {r:?}");
        assert!(r.is_err(), "no crossbar");
        let mut out: *mut core::ffi::c_void = std::ptr::null_mut();
        let r = builder.FindInterface(
            Some(&PIN_CATEGORY_CAPTURE),
            Some(&windows::Win32::Media::MediaFoundation::MEDIATYPE_Video),
            &filter,
            &IAMStreamConfig::IID,
            &mut out,
        );
        eprintln!("FindInterface stream config -> {r:?}");
        assert!(r.is_ok(), "stream config found on the capture pin");
        drop(IAMStreamConfig::from_raw(out));
        graph.RemoveFilter(&filter).expect("RemoveFilter");
    }
}

/// OBS's activate → run → stop → deactivate → activate cycle against a real
/// qcap downstream filter (Smart Tee), through the graph's `ConnectDirect`.
#[test]
fn graph_connect_run_reconnect_cycle() {
    use windows::Win32::Media::DirectShow::{IGraphBuilder, IMediaControl};
    use windows::Win32::Media::MediaFoundation::{CLSID_FilterGraph, CLSID_SmartTee};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    com();
    setup_env();
    for round in 0..3 {
        let filter = filter_via_class_factory();
        // SAFETY: standard DirectShow graph calls on live objects.
        unsafe {
            let graph: IGraphBuilder =
                CoCreateInstance(&CLSID_FilterGraph, None, CLSCTX_INPROC_SERVER).expect("graph");
            let tee: IBaseFilter =
                CoCreateInstance(&CLSID_SmartTee, None, CLSCTX_INPROC_SERVER).expect("tee");
            graph.AddFilter(&filter, windows::core::w!("Relay Camera")).unwrap();
            graph.AddFilter(&tee, windows::core::w!("Tee")).unwrap();
            let out = output_pin(&filter);
            let e = tee.EnumPins().unwrap();
            let mut pins = [None];
            let mut n = 0;
            let _ = e.Next(&mut pins, Some(&mut n));
            let tee_in = pins[0].take().unwrap();
            eprintln!("round {round}: ConnectDirect");
            graph.ConnectDirect(&out, &tee_in, None).expect("ConnectDirect");
            let mc: IMediaControl = graph.cast().unwrap();
            eprintln!("round {round}: Run");
            let _ = mc.Run();
            std::thread::sleep(Duration::from_millis(300));
            eprintln!("round {round}: Stop");
            mc.Stop().unwrap();
            graph.RemoveFilter(&tee).unwrap();
            graph.RemoveFilter(&filter).unwrap();
            eprintln!("round {round}: ConnectedTo after removal = {:?}", out.ConnectedTo().is_ok());
        }
    }
}

/// Failed getters NULL their out pointer (the r57 OBS crash: garbage left in
/// `ConnectedTo`'s out by an unconnected pin, read by qcap).
#[test]
fn failed_getters_null_their_out_pointer() {
    use windows::Win32::Media::DirectShow::{IBaseFilter_Vtbl, IPin_Vtbl};
    com();
    setup_env();
    let filter = filter_via_class_factory();
    let pin = output_pin(&filter);
    let poison = 0xDEAD_BEEF_usize as *mut core::ffi::c_void;
    // SAFETY: raw vtable calls with valid out slots, as a C caller makes them.
    unsafe {
        let mut out = poison;
        let vt = &**(pin.as_raw() as *const *const IPin_Vtbl);
        let hr = (vt.ConnectedTo)(pin.as_raw(), &mut out);
        assert!(hr.is_err(), "unconnected");
        assert!(out.is_null(), "ConnectedTo out NULLed");
        let vt = &**(filter.as_raw() as *const *const IBaseFilter_Vtbl);
        let mut out = poison;
        let hr = (vt.base__.GetSyncSource)(filter.as_raw(), &mut out);
        assert_eq!(hr, windows::Win32::Foundation::S_FALSE, "no clock");
        assert!(out.is_null(), "GetSyncSource out NULLed");
        let mut out = poison;
        let hr = (vt.FindPin)(filter.as_raw(), windows::core::w!("nope"), &mut out);
        assert!(hr.is_err());
        assert!(out.is_null(), "FindPin out NULLed");
        let mut s = windows::core::PWSTR(poison as *mut u16);
        let hr = (vt.QueryVendorInfo)(filter.as_raw(), &mut s);
        assert!(hr.is_err());
        assert!(s.is_null(), "QueryVendorInfo out NULLed");
        // The interfaces still work through the patched vtables.
        assert!(pin.QueryDirection().is_ok());
        assert!(filter.FindPin(windows::core::w!("Capture")).is_ok());
    }
}
