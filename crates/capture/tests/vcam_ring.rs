//! Headless proof of the receiver→camera pixel path: a real D3D11 NV12
//! texture (the shape the HEVC decoder outputs) goes through
//! `RingWriter::push` — staging copy, map, strided row copy — and comes out
//! of the shared frame ring byte-for-byte. Together with relay-vdevice's
//! in-process media source test this covers the whole chain except the
//! frame-server hosting itself (live runbook in the M5 plan).

#![cfg(windows)]
#![allow(unsafe_code)]

use relay_capture::vcam_sink::RingWriter;
use relay_vdevice::frames::{SharedFrames, SLOT_BYTES};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

fn nv12_pattern(w: u32, h: u32) -> Vec<u8> {
    (0..(w as usize * h as usize) * 3 / 2).map(|i| (i * 7 % 251) as u8).collect()
}

#[test]
fn ring_writer_round_trips_an_nv12_texture() {
    let (w, h) = (320u32, 180u32);
    let frame_bytes = nv12_pattern(w, h);

    // CI runners have no GPU (D3D11CreateDevice fails there); this test is
    // about the copy path, so it needs real hardware and skips without it.
    let gpu = match relay_capture::d3d::device_for_monitor(relay_capture::d3d::primary_monitor()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipped: no D3D11 device on this machine ({e:#})");
            return;
        }
    };

    // An NV12 texture with the pattern as initial data, standing in for the
    // decoder output (subresource 0 of a 1-element array).
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: frame_bytes.as_ptr() as *const _,
        SysMemPitch: w,
        SysMemSlicePitch: 0,
    };
    let mut tex = None;
    // SAFETY: valid desc + tightly packed NV12 initial data (pitch = width).
    unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)).expect("texture") };
    let frame = relay_capture::decode::mf::DecodedFrame {
        texture: tex.expect("texture out"),
        subresource: 0,
        pts_100ns: 4242,
        width: w,
        height: h,
    };

    // Local ring created by the test (the media source's role in production).
    let name = format!("Local\\Relay.Cam.ringtest.{}", std::process::id());
    let section = SharedFrames::create(&name).expect("create ring");
    let reader = SharedFrames::open(&name).expect("open ring");

    let mut writer = RingWriter::with_ring(section);
    let wrote = writer.push(&gpu.device, &gpu.context, &frame).expect("push");
    assert!(wrote, "frame accepted by the ring");
    assert_eq!(writer.frames_written(), 1);

    let mut out = vec![0u8; SLOT_BYTES];
    let info = reader.block().read_latest(&mut out).expect("frame in ring");
    assert_eq!((info.width, info.height, info.pts_100ns), (w, h, 4242));
    assert_eq!(&out[..frame_bytes.len()], &frame_bytes[..], "pixels round-trip byte-for-byte");
}
