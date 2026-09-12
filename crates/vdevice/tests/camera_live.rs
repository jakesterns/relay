//! Live virtual-camera probe — **opt-in only** (`RELAY_VCAM_LIVE=1`), never
//! part of a normal test run. Requires the media source CLSID registered
//! (HKLM, or HKCU for the per-user experiment) with an InprocServer32
//! pointing at a built `relay_vdevice.dll`. Creates "Relay Camera" via the
//! real frame server, then enumerates video capture devices and reads one
//! frame back through an `IMFSourceReader` — the same path Discord / Zoom /
//! Meet use to open a camera.

#![cfg(windows)]
#![allow(unsafe_code)]

use relay_vdevice::camera::control::VirtualCamera;
use relay_vdevice::frames::{section_name_from_env, SharedFrames};
use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaSource, IMFSourceReader, MFCreateAttributes,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFStartup,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

const MF_VERSION: u32 = 0x0002_0070;

#[test]
fn live_camera_appears_and_serves_frames() {
    if std::env::var("RELAY_VCAM_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: set RELAY_VCAM_LIVE=1 (needs the CLSID registered)");
        return;
    }
    let (w, h, fps) = (1280u32, 720u32, 30u32);

    // SAFETY: standard COM/MF init and enumeration on one thread.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        MFStartup(MF_VERSION, 0).expect("MFStartup");

        let cam = VirtualCamera::start(w, h, fps).expect("MFCreateVirtualCamera + Start");
        println!("virtual camera started");

        // Feed the ring like the receiver would (Global\ section created by
        // the media source once the frame server activates it).
        let mut ring = None;
        for _ in 0..50 {
            match SharedFrames::create(&section_name_from_env()) {
                Ok(s) => {
                    ring = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        }
        let frame: Vec<u8> = (0..(w * h * 3 / 2) as usize).map(|i| (i % 200) as u8 + 20).collect();
        if let Some(ring) = &ring {
            let (y, uv) = frame.split_at((w * h) as usize);
            assert!(ring.block().write_frame(w, h, 0, y, w as usize, uv, w as usize));
            println!("ring up, test frame written");
        } else {
            println!("ring not reachable (media source not activated yet) — continuing");
        }

        // Enumerate like a conferencing app.
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1).unwrap();
        let attrs = attrs.unwrap();
        attrs
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .unwrap();
        let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut devices, &mut count).expect("enum devices");
        let list = std::slice::from_raw_parts(devices, count as usize);
        let mut relay: Option<IMFActivate> = None;
        for a in list.iter().flatten() {
            let mut name = windows::core::PWSTR::null();
            let mut len = 0u32;
            if a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut len)
                .is_ok()
            {
                let n = name.to_string().unwrap_or_default();
                println!("camera: {n}");
                if n.contains("Relay Camera") {
                    relay = Some(a.clone());
                }
            }
        }
        let relay = relay.expect("\"Relay Camera\" not in the device list");

        // Open it and read one sample, exactly like an app would.
        let source: IMFMediaSource = relay.ActivateObject().expect("activate device");
        let reader: IMFSourceReader =
            MFCreateSourceReaderFromMediaSource(&source, None).expect("source reader");
        let mut stream_index = 0u32;
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample = None;
        for _ in 0..120 {
            reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    Some(&mut stream_index),
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .expect("ReadSample");
            if sample.is_some() {
                break;
            }
        }
        let sample = sample.expect("no sample from Relay Camera");
        let buffer = sample.ConvertToContiguousBuffer().expect("buffer");
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut data, None, Some(&mut len)).expect("lock");
        let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
        buffer.Unlock().unwrap();
        println!("sample: {} bytes (expect {})", bytes.len(), w * h * 3 / 2);
        assert_eq!(bytes.len() as u32, w * h * 3 / 2);
        if ring.is_some() {
            assert_eq!(&bytes[..64], &frame[..64], "served frame is the ring frame");
        }
        println!("LIVE PASS: Relay Camera enumerated and served frames");
        let _ = PCWSTR::null(); // keep the import used
        drop(cam);
    }
}
