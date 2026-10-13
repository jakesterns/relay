//! Test clips for S48: synthetic gameplay-like video and audio, encoded with
//! Windows' own software H.264 encoder MFT (present on every install, no
//! GPU needed) and Opus, written through Relay's own fMP4 / MKV muxers —
//! the same files a Relay recording produces, apart from the codec.
//!
//! Picture: three scenes (night, mid, daylight) a few seconds each, textured
//! and moving so they classify as gameplay. Sound: a noise bed with bright
//! footstep-like ticks and low booms.

use std::fs::File;
use std::io::BufWriter;
use std::mem::ManuallyDrop;
use std::path::Path;

use anyhow::{Context, Result};
use relay_core::share::RecordingContainer;
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};

use crate::codec::VideoCodec;
use crate::record::mkv::MkvMuxer;
use crate::record::mux::{Mp4Muxer, MuxConfig};

fn variant_u32(v: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: v },
            }),
        },
    }
}

fn pack(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }
    fn unit(&mut self) -> f32 {
        (self.next() & 0xffff) as f32 / 65535.0
    }
}

/// One NV12 frame of the synthetic game at frame `i`.
fn frame(i: u32, fps: u32, w: u32, h: u32, rng: &mut Lcg) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut buf = vec![0u8; w * h * 3 / 2];
    let scene = (i / (fps * 3)) % 3; // 3 s per scene
    let (base, spread, sat) = match scene {
        0 => (20.0, 30.0, 6.0),   // night: deep shadows with texture
        1 => (90.0, 60.0, 18.0),  // mid
        _ => (160.0, 50.0, 30.0), // daylight
    };
    let shift = (i * 3) as usize;
    for y in 0..h {
        for x in 0..w {
            let tex = (((x + shift) / 8 + y / 8) % 5) as f32 / 4.0;
            let n = rng.unit() * 6.0;
            buf[y * w + x] = (16.0 + base + tex * spread + n).clamp(16.0, 235.0) as u8;
        }
    }
    let uv = &mut buf[w * h..];
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let p = (y * w) + x * 2;
            let s = ((x + y + shift) % 16) as f32 / 16.0 * sat;
            uv[p] = (128.0 + s) as u8;
            uv[p + 1] = (128.0 - s) as u8;
        }
    }
    buf
}

/// 48 kHz stereo audio for second-fraction `t0..t0+n`.
fn audio(n: usize, start: usize, rng: &mut Lcg) -> Vec<f32> {
    let mut out = Vec::with_capacity(n * 2);
    let mut prev = 0.0f32;
    for k in 0..n {
        let t = start + k;
        let bed = (rng.unit() - 0.5) * 0.01;
        // A tick every 1.5 s (bright, 30 ms) and a boom every 6 s (low, 400 ms).
        let tick_phase = t % 72_000;
        let white = rng.unit() - 0.5;
        let tick = if tick_phase < 1_440 {
            (white - prev) * 0.6 * (1.0 - tick_phase as f32 / 1_440.0)
        } else {
            0.0
        };
        prev = white;
        let boom_phase = t % 288_000;
        let boom = if boom_phase < 19_200 {
            (boom_phase as f32 * 2.0 * std::f32::consts::PI * 55.0 / 48_000.0).sin()
                * 0.7
                * (1.0 - boom_phase as f32 / 19_200.0)
        } else {
            0.0
        };
        let s = bed + tick + boom;
        out.push(s);
        out.push(s);
    }
    out
}

/// Encoded H.264 access units: (Annex B bytes, pts 100 ns, keyframe).
fn encode_h264(secs: u32, w: u32, h: u32, fps: u32) -> Result<Vec<(Vec<u8>, i64, bool)>> {
    // SAFETY: a sync software MFT driven with caller-allocated output, every
    // COM object live for the calls that use it.
    unsafe {
        let mft: IMFTransform =
            CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)
                .context("the Microsoft H.264 encoder MFT")?;
        if let Ok(api) = mft.cast::<ICodecAPI>() {
            let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(fps));
            let _ = api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0));
        }
        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, 6_000_000)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        mft.SetOutputType(0, &out, 0).context("encoder output type")?;
        let inp = MFCreateMediaType()?;
        inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        inp.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        inp.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        mft.SetInputType(0, &inp, 0).context("encoder input type")?;
        let header = mft
            .GetOutputCurrentType(0)
            .ok()
            .and_then(|t| {
                let mut size = 0u32;
                let mut p = std::ptr::null_mut();
                t.GetAllocatedBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut p, &mut size).ok()?;
                let v = std::slice::from_raw_parts(p, size as usize).to_vec();
                windows::Win32::System::Com::CoTaskMemFree(Some(p as _));
                Some(v)
            })
            .unwrap_or_default();
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        let mut aus = Vec::new();
        let mut rng = Lcg(48);
        let dur = 10_000_000 / fps as i64;
        let drain = |aus: &mut Vec<(Vec<u8>, i64, bool)>| -> Result<()> {
            loop {
                let info = mft.GetOutputStreamInfo(0)?;
                let buf = MFCreateMemoryBuffer(info.cbSize.max(w * h * 2))?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buf)?;
                let mut odb = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(sample.clone())),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let r = mft.ProcessOutput(0, &mut odb, &mut status);
                ManuallyDrop::drop(&mut odb[0].pSample);
                ManuallyDrop::drop(&mut odb[0].pEvents);
                match r {
                    Ok(()) => {
                        let pts = sample.GetSampleTime().unwrap_or(0);
                        let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
                        let c = sample.ConvertToContiguousBuffer()?;
                        let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                        c.Lock(&mut p, None, Some(&mut len))?;
                        let data = std::slice::from_raw_parts(p, len as usize).to_vec();
                        c.Unlock()?;
                        aus.push((data, pts, key));
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) => return Err(e).context("encoder ProcessOutput"),
                }
            }
        };
        for i in 0..secs * fps {
            let nv12 = frame(i, fps, w, h, &mut rng);
            let buf = MFCreateMemoryBuffer(nv12.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), p, nv12.len());
            buf.SetCurrentLength(nv12.len() as u32)?;
            buf.Unlock()?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(i as i64 * dur)?;
            s.SetSampleDuration(dur)?;
            mft.ProcessInput(0, &s, 0).context("encoder ProcessInput")?;
            drain(&mut aus)?;
        }
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
        mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        drain(&mut aus)?;
        // The first keyframe must carry SPS/PPS for the muxer.
        if let Some(first) = aus.iter_mut().find(|a| a.2) {
            let has_sps =
                crate::record::annexb::extract_param_sets_for(VideoCodec::H264, &first.0).is_some();
            if !has_sps && !header.is_empty() {
                let mut v = header.clone();
                v.extend_from_slice(&first.0);
                first.0 = v;
            }
        }
        Ok(aus)
    }
}

/// Write a `secs`-long synthetic clip to `path`.
pub fn write_clip(
    path: &Path,
    container: RecordingContainer,
    secs: u32,
    w: u32,
    h: u32,
    fps: u32,
) -> Result<()> {
    let _mf = crate::probe::MediaFoundation::start()?;
    let video = {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        // SAFETY: COM for this thread (the MFT is a COM object).
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        encode_h264(secs, w, h, fps)?
    };
    let mut enc = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)?;
    let mut rng = Lcg(7);
    let mut packets = Vec::new();
    let frame = 960; // 20 ms
    let mut out = vec![0u8; 4000];
    for k in 0..(secs as usize * 48_000 / frame) {
        let pcm = audio(frame, k * frame, &mut rng);
        let n = enc.encode_float(&pcm, &mut out)?;
        packets.push((out[..n].to_vec(), k as i64 * 200_000));
    }
    let cfg = MuxConfig::with_opus(w, h).with_codec(VideoCodec::H264);
    let file = BufWriter::new(File::create(path)?);
    // Interleave by time, video first at equal stamps.
    let mut a = packets.iter().peekable();
    macro_rules! mux {
        ($m:expr) => {{
            let mut m = $m;
            for (au, pts, key) in &video {
                while let Some((p, ap)) = a.peek() {
                    if *ap > *pts {
                        break;
                    }
                    m.push_audio(0, p, *ap, 200_000)?;
                    a.next();
                }
                m.push_video(au, *pts, *key)?;
            }
            for (p, ap) in a {
                m.push_audio(0, p, *ap, 200_000)?;
            }
            m.finalize()?;
        }};
    }
    match container {
        RecordingContainer::Mp4 => mux!(Mp4Muxer::new(file, cfg)),
        RecordingContainer::Mkv => mux!(MkvMuxer::new(file, cfg)),
    }
    Ok(())
}
