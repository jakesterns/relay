//! Headless proof of the receiver→camera pixel path: a real D3D11 NV12
//! texture (the shape the HEVC decoder outputs) goes through
//! `RingWriter::push` — staging copy, map, strided row copy — and comes out
//! of the shared frame ring byte-for-byte. Together with relay-vdevice's
//! in-process media source test this covers the whole chain except the
//! frame-server hosting itself (live runbook in the M5 plan).

#![cfg(windows)]
#![allow(unsafe_code)]

use relay_capture::decode::mf::DecodedFrame;
use relay_capture::vcam_sink::RingWriter;
use relay_vdevice::frames::{SharedFrames, SLOT_BYTES};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
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

/// An NV12 texture the video processor accepts as input (render target as
/// well, like the decoder's own output), holding `data`.
fn input_frame(gpu: &relay_capture::d3d::Gpu, data: &[u8], w: u32, h: u32) -> DecodedFrame {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr() as *const _,
        SysMemPitch: w,
        SysMemSlicePitch: 0,
    };
    let mut tex = None;
    // SAFETY: valid desc + tightly packed NV12 initial data (pitch = width).
    unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)).expect("texture") };
    DecodedFrame {
        texture: tex.expect("texture out"),
        subresource: 0,
        pts_100ns: 1,
        width: w,
        height: h,
    }
}

/// Michelson contrast above black (16 reads as zero).
fn contrast(a: &[u8]) -> f64 {
    let (lo, hi) = a.iter().fold((255u8, 0u8), |(l, h), &v| (l.min(v), h.max(v)));
    let (lo, hi) = (lo.saturating_sub(16) as f64, hi.saturating_sub(16) as f64);
    if hi + lo == 0.0 {
        0.0
    } else {
        (hi - lo) / (hi + lo)
    }
}

fn mean_sd(a: &[u8]) -> (f64, f64) {
    let m = a.iter().map(|&v| v as f64).sum::<f64>() / a.len() as f64;
    (m, (a.iter().map(|&v| (v as f64 - m).powi(2)).sum::<f64>() / a.len() as f64).sqrt())
}

/// r54: the camera asked for a smaller size; the writer scales on the GPU
/// and writes that size, the right way round. Quality against the old
/// nearest-neighbour scale, on gratings: strokes finer than the target can
/// show must turn grey (no moire, no wrong level), and bold strokes must
/// keep their contrast (legible).
#[test]
fn ring_writer_scales_on_the_gpu_to_the_requested_size() {
    use relay_vdevice::camera::picture::{self, testpat, PixFmt};
    let (w, h) = (2560u32, 1440u32);
    let gpu = match relay_capture::d3d::device_for_monitor(relay_capture::d3d::primary_monitor()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipped: no D3D11 device on this machine ({e:#})");
            return;
        }
    };
    let marker = testpat::marker_nv12(w, h);
    // Left half: 1 px on / 1 px off (finer than both targets). Right half:
    // 4 px on / 4 px off (a bold stroke both targets can show).
    let mut gratings = vec![128u8; picture::nv12_bytes(w, h)];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let on = if x < w as usize / 2 { x % 2 == 0 } else { (x / 4) % 2 == 0 };
            gratings[y * w as usize + x] = if on { 235 } else { 16 };
        }
    }
    let marker_frame = input_frame(&gpu, &marker, w, h);
    let grating_frame = input_frame(&gpu, &gratings, w, h);

    let name = format!("Local\\Relay.Cam.gpuscale.{}", std::process::id());
    let section = SharedFrames::create(&name).expect("create ring");
    let reader = SharedFrames::open(&name).expect("open ring");
    let mut writer = RingWriter::with_ring(section);
    let mut out = vec![0u8; SLOT_BYTES];

    for (tw, th) in [(1920u32, 1080u32), (1280, 720)] {
        reader.block().request_size(tw, th);
        let n = picture::nv12_bytes(tw, th);

        // Orientation.
        assert!(writer.push(&gpu.device, &gpu.context, &marker_frame).expect("push"));
        let info = reader.block().read_latest(&mut out).expect("frame");
        assert_eq!((info.width, info.height), (tw, th), "written at the camera's size");
        testpat::assert_oriented(PixFmt::Nv12, &out[..n], tw, th, "GPU-scaled ring frame");

        // Quality, one row from the middle of each half.
        assert!(writer.push(&gpu.device, &gpu.context, &grating_frame).expect("push"));
        reader.block().read_latest(&mut out).expect("grating frame");
        let mut near = vec![0u8; n];
        picture::scale_nv12_nearest(&gratings, w, h, &mut near, tw, th);
        let row = |buf: &[u8], left: bool| -> Vec<u8> {
            let (tw, r) = (tw as usize, th as usize / 2);
            let (a, b) = if left { (tw / 16, tw * 7 / 16) } else { (tw * 9 / 16, tw * 15 / 16) };
            buf[r * tw + a..r * tw + b].to_vec()
        };
        let mid = (235.0 + 16.0) / 2.0;
        let (gm, gs) = mean_sd(&row(&out, true));
        let (nm, ns) = mean_sd(&row(&near, true));
        let (gc, nc) = (contrast(&row(&out, false)), contrast(&row(&near, false)));
        eprintln!(
            "{tw}x{th}: fine strokes GPU mean err {:.1} sd {gs:.1}, nearest mean err {:.1} sd {ns:.1}; bold-stroke contrast GPU {gc:.2}, nearest {nc:.2}",
            (gm - mid).abs(),
            (nm - mid).abs()
        );
        // No aliasing: the fine strokes come out an even grey at the right
        // level, where point sampling gives moire or a wrong solid level.
        let g_alias = (gm - mid).abs() + gs;
        let n_alias = (nm - mid).abs() + ns;
        assert!(
            g_alias * 2.0 < n_alias,
            "{tw}x{th}: aliasing GPU {g_alias:.1} vs nearest {n_alias:.1}"
        );
        // Legible: bold strokes keep most of their contrast.
        assert!(gc > 0.8, "{tw}x{th}: bold-stroke contrast {gc:.2}");
    }

    // No request (or one for the stream's own size): native frames again.
    reader.block().withdraw_request(1280, 720);
    assert!(writer.push(&gpu.device, &gpu.context, &marker_frame).expect("push native"));
    let info = reader.block().read_latest(&mut out).expect("native frame");
    assert_eq!((info.width, info.height), (w, h));
    assert_eq!(&out[..marker.len()], &marker[..], "native pass-through unchanged");
}
