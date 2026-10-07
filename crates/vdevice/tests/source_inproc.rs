//! In-process proof of the camera media source, mirroring relay-apo's
//! in-process COM test: activate the source the way the Windows Camera
//! Frame Server would (attributes → `ActivateObject` → `Start` →
//! `RequestSample`) and assert the served NV12 bytes are exactly what the
//! receiver wrote into the frame ring. No registration, no frame server —
//! the COM object itself is exercised end to end.

#![cfg(all(windows, feature = "com"))]
#![allow(unsafe_code)]

use relay_vdevice::camera::picture::{self, testpat, PixFmt};
use relay_vdevice::camera::source::Activate;
use relay_vdevice::camera::{RELAY_VCAM_ATTR_FPS, RELAY_VCAM_ATTR_HEIGHT, RELAY_VCAM_ATTR_WIDTH};
use relay_vdevice::frames::{section_name_from_env, SharedFrames};
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaSourceEx, IMFMediaStream2, IMFSample, MEMediaSample, MENewStream, MFStartup,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MF_MT_FRAME_SIZE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Variant::VT_UNKNOWN;

const NO_WAIT: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS = MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(1);
const MF_VERSION: u32 = 0x0002_0070;

fn test_frame(w: u32, h: u32) -> Vec<u8> {
    (0..(w as usize * h as usize) * 3 / 2).map(|i| (i % 251) as u8).collect()
}

/// Pull the IUnknown payload out of a media event.
unsafe fn event_unknown<T: Interface>(
    ev: &windows::Win32::Media::MediaFoundation::IMFMediaEvent,
) -> T {
    // SAFETY: VT_UNKNOWN PROPVARIANT out of a live event; cleared after the
    // interface is cloned out.
    unsafe {
        let mut v = ev.GetValue().expect("event value");
        let inner = &v.Anonymous.Anonymous;
        assert_eq!(inner.vt, VT_UNKNOWN, "event payload is IUnknown");
        let unk = (*inner.Anonymous.punkVal).clone().expect("non-null punkVal");
        let out: T = unk.cast().expect("cast event payload");
        let _ = PropVariantClear(&mut v);
        out
    }
}

#[test]
fn activate_start_request_sample_serves_ring_frames() {
    // Local\ section + unique instance: no privileges, no cross-test clash.
    std::env::set_var("RELAY_VCAM_LOCAL_SECTION", "1");
    std::env::set_var("RELAY_INSTANCE", format!("vcamtest{}", std::process::id()));

    let (w, h, fps) = (128u32, 72u32, 30u32);
    let frame = test_frame(w, h);

    // SAFETY: standard MF + COM usage on one thread; objects dropped before
    // exit.
    unsafe {
        MFStartup(MF_VERSION, 0).expect("MFStartup");

        // Receiver side: create the ring and publish one frame.
        let ring = SharedFrames::create(&section_name_from_env()).expect("create ring");
        let (y, uv) = frame.split_at((w * h) as usize);
        assert!(ring.block().write_frame(w, h, 777, y, w as usize, uv, w as usize));

        // Frame-server side: activate with the geometry attributes.
        let activate = Activate::create().expect("activate");
        activate.SetUINT32(&RELAY_VCAM_ATTR_WIDTH, w).unwrap();
        activate.SetUINT32(&RELAY_VCAM_ATTR_HEIGHT, h).unwrap();
        activate.SetUINT32(&RELAY_VCAM_ATTR_FPS, fps).unwrap();
        let source: IMFMediaSourceEx = activate.ActivateObject().expect("ActivateObject");

        // Same object comes back on a second activation.
        let again: IMFMediaSourceEx = activate.ActivateObject().expect("ActivateObject 2");
        assert_eq!(source.as_raw(), again.as_raw());

        let pd = source.CreatePresentationDescriptor().expect("pd");
        source.Start(&pd, std::ptr::null(), std::ptr::null()).expect("start");

        let ev = source.GetEvent(NO_WAIT).expect("source event");
        assert_eq!(ev.GetType().expect("type"), MENewStream.0 as u32);
        let stream: IMFMediaStream2 = event_unknown(&ev);

        // Descriptor advertises our NV12 geometry.
        let sd = stream.GetStreamDescriptor().expect("sd");
        let mt = sd.GetMediaTypeHandler().unwrap().GetCurrentMediaType().unwrap();
        let size = mt
            .GetUINT64(&windows::Win32::Media::MediaFoundation::MF_MT_FRAME_SIZE)
            .expect("frame size");
        assert_eq!(((size >> 32) as u32, size as u32), (w, h));

        // Drain MEStreamStarted, then request a sample.
        let _ = stream.GetEvent(NO_WAIT).expect("stream started event");
        stream.RequestSample(None).expect("request");
        let ev = stream.GetEvent(NO_WAIT).expect("sample event");
        assert_eq!(ev.GetType().expect("type"), MEMediaSample.0 as u32);
        let sample: IMFSample = event_unknown(&ev);
        let buffer = sample.ConvertToContiguousBuffer().expect("buffer");
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut data, None, Some(&mut len)).expect("lock");
        assert_eq!(len as usize, frame.len());
        let served = std::slice::from_raw_parts(data, len as usize).to_vec();
        buffer.Unlock().expect("unlock");
        assert_eq!(served, frame, "served NV12 must be the ring frame, byte for byte");
        assert!(sample.GetSampleDuration().expect("duration") > 0);

        // The source asked the producer for its negotiated size.
        assert_eq!(ring.block().requested_size(), Some((w, h)));

        // A receiver frame of another size is scaled into the negotiated
        // one (r54), not replaced by black.
        let small = test_frame(64, 36);
        let (y2, uv2) = small.split_at(64 * 36);
        assert!(ring.block().write_frame(64, 36, 778, y2, 64, uv2, 64));
        let served = pull(&stream);
        let mut want = vec![0u8; picture::nv12_bytes(w, h)];
        picture::scale_nv12(&small, 64, 36, &mut want, w, h);
        assert_eq!(served, want, "a 64x36 frame is scaled to the negotiated 128x72");

        // Shutdown tears down cleanly, gives the producer its size back, and
        // further requests fail.
        source.Shutdown().expect("shutdown");
        assert!(stream.RequestSample(None).is_err());
        assert_eq!(ring.block().requested_size(), None);

        // ---- r54: a 1440p stream, every offered size, orientation ----
        let (sw, sh) = (2560u32, 1440u32);
        let activate = Activate::create().expect("activate 1440p");
        activate.SetUINT32(&RELAY_VCAM_ATTR_WIDTH, sw).unwrap();
        activate.SetUINT32(&RELAY_VCAM_ATTR_HEIGHT, sh).unwrap();
        activate.SetUINT32(&RELAY_VCAM_ATTR_FPS, 60).unwrap();
        let source: IMFMediaSourceEx = activate.ActivateObject().expect("ActivateObject 1440p");
        let pd = source.CreatePresentationDescriptor().expect("pd");
        source.Start(&pd, std::ptr::null(), std::ptr::null()).expect("start");
        let ev = source.GetEvent(NO_WAIT).expect("source event");
        let stream: IMFMediaStream2 = event_unknown(&ev);
        let _ = stream.GetEvent(NO_WAIT).expect("stream started event");
        let handler = stream.GetStreamDescriptor().unwrap().GetMediaTypeHandler().unwrap();
        let mut sizes = Vec::new();
        for i in 0..handler.GetMediaTypeCount().unwrap() {
            let size =
                handler.GetMediaTypeByIndex(i).unwrap().GetUINT64(&MF_MT_FRAME_SIZE).unwrap();
            sizes.push(((size >> 32) as u32, size as u32));
        }
        assert_eq!(sizes, [(1920, 1080), (2560, 1440), (1280, 720)], "1080p first");

        let marker = testpat::marker_nv12(sw, sh);
        let (my, muv) = marker.split_at((sw * sh) as usize);
        for i in 0..sizes.len() as u32 {
            let mt = handler.GetMediaTypeByIndex(i).unwrap();
            handler.SetCurrentMediaType(&mt).unwrap();
            let (tw, th) = sizes[i as usize];
            assert!(ring.block().write_frame(
                sw,
                sh,
                900 + i as i64,
                my,
                sw as usize,
                muv,
                sw as usize
            ));
            let served = pull(&stream);
            assert_eq!(served.len(), picture::nv12_bytes(tw, th), "{tw}x{th} sample size");
            testpat::assert_oriented(PixFmt::Nv12, &served, tw, th, "frame server");
            assert_eq!(ring.block().requested_size(), Some((tw, th)), "asked for {tw}x{th}");
        }
        source.Shutdown().expect("shutdown 1440p");
    }
}

/// Request one sample and copy its bytes out.
unsafe fn pull(stream: &IMFMediaStream2) -> Vec<u8> {
    // SAFETY: a live stream; the buffer is locked and unlocked in a pair.
    unsafe {
        stream.RequestSample(None).expect("request");
        let ev = stream.GetEvent(NO_WAIT).expect("sample event");
        assert_eq!(ev.GetType().expect("type"), MEMediaSample.0 as u32);
        let sample: IMFSample = event_unknown(&ev);
        let buffer = sample.ConvertToContiguousBuffer().expect("buffer");
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut data, None, Some(&mut len)).expect("lock");
        let out = std::slice::from_raw_parts(data, len as usize).to_vec();
        buffer.Unlock().expect("unlock");
        out
    }
}
