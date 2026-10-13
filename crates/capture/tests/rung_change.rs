//! S49: a resolution step end to end through the real GPU pieces, headless.
//!
//! The sender rebuilds its hardware encoder at the new rung; the receiver
//! sees a keyframe of a new size, rebuilds its decoder, and scales the
//! decoded NV12 back to the size its window and Relay Camera were made for.
//! This drives exactly those calls — `MfEncoder::new` at two sizes,
//! `probe_dimensions`, `MfDecoder::new`, `Converter::new_on` +
//! `convert_slice` from a decoder's texture array — and checks the sizes
//! that come out. No window, no capture of the screen (the synthetic noise
//! source), so it is safe on the PC being used. Skips without a GPU, as CI
//! runners have none.

#![cfg(windows)]

use relay_capture::codec::VideoCodec;
use relay_capture::decode::mf::MfDecoder;
use relay_capture::encode::convert::Converter;
use relay_capture::encode::mf::{EncoderConfig, EncoderEvent, MfEncoder};
use relay_capture::source::pattern::NoiseSource;
use relay_capture::source::FrameSource;
use windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC;

/// Encode `frames` frames of the noise source at `size`; the access units.
fn encode(
    gpu: &relay_capture::d3d::Gpu,
    codec: VideoCodec,
    size: (u32, u32),
    frames: usize,
) -> Vec<(Vec<u8>, bool)> {
    let mut src = NoiseSource::new(gpu, size).expect("noise source");
    let mut conv = Converter::new(gpu, size, size).expect("converter");
    let enc = MfEncoder::new(
        gpu,
        &EncoderConfig { codec, width: size.0, height: size.1, fps: 60, bitrate_bps: 20_000_000 },
    )
    .expect("encoder");
    let mut out = Vec::new();
    let mut submitted = 0;
    let mut pts = 0i64;
    while out.len() < frames {
        match enc.next_event().expect("encoder event") {
            EncoderEvent::NeedInput if submitted < frames + 8 => {
                let f = src.next(std::time::Duration::from_millis(100)).unwrap().expect("a frame");
                let nv12 = conv.convert(&f.texture).unwrap();
                enc.submit(&nv12, pts).unwrap();
                pts += 166_667;
                submitted += 1;
            }
            EncoderEvent::NeedInput => break,
            EncoderEvent::Output(o) => out.push((o.data, o.keyframe)),
        }
    }
    out
}

fn tex_size(t: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D) -> (u32, u32) {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: a live texture and an out-structure.
    unsafe { t.GetDesc(&mut d) };
    (d.Width, d.Height)
}

#[test]
fn a_rung_change_decodes_and_scales_back_to_the_stream_size() {
    let gpu = match relay_capture::d3d::device_for_monitor(relay_capture::d3d::primary_monitor()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("skipped: no D3D11 device ({e:#})");
            return;
        }
    };
    let _mf = relay_capture::probe::MediaFoundation::start().expect("MF");
    let codec = VideoCodec::H264;
    if !relay_capture::encode::mf::hardware_encoder_available(codec, gpu.adapter_luid) {
        eprintln!("skipped: no hardware H.264 encoder");
        return;
    }
    let (top, step) = ((1920u32, 1080u32), (1280u32, 720u32));

    // The share starts at the top rung; the window is made for it.
    let first = encode(&gpu, codec, top, 10);
    assert!(first[0].1, "a fresh encoder starts with a keyframe");
    assert_eq!(relay_capture::decode::probe_dimensions(codec, &first[0].0), Some(top));
    let mut dec = MfDecoder::new(&gpu.device, codec, top.0, top.1).expect("decoder");
    let mut decoded = 0;
    for (i, (au, _)) in first.iter().enumerate() {
        for f in dec.decode(au, i as i64 * 166_667).expect("decode top") {
            assert_eq!((f.width, f.height), top);
            decoded += 1;
        }
    }
    assert!(decoded > 0);

    // A step down: a new encoder at the lower rung, whose first unit is a
    // keyframe of the new size — what the receiver's render loop reacts to.
    let second = encode(&gpu, codec, step, 10);
    assert!(second[0].1);
    let size = relay_capture::decode::probe_dimensions(codec, &second[0].0);
    assert_eq!(size, Some(step), "the receiver can read the new size from the keyframe");
    let mut dec =
        MfDecoder::new(&gpu.device, codec, step.0, step.1).expect("decoder at the new size");
    let mut scaler = Converter::new_on(&gpu.device, &gpu.context, step, top).expect("scaler");
    scaler.set_source_rect(Some((0, 0, step.0, step.1)));
    let mut scaled = 0;
    for (i, (au, _)) in second.iter().enumerate() {
        for f in dec.decode(au, (10 + i) as i64 * 166_667).expect("decode step") {
            assert_eq!((f.width, f.height), step);
            let out = scaler.convert_slice(&f.texture, f.subresource).expect("scale");
            assert_eq!(tex_size(&out), top, "downstream still sees the window's size");
            scaled += 1;
        }
    }
    assert!(scaled > 0, "frames after the step were decoded and scaled");
}
