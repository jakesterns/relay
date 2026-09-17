//! Dev harness: mux a raw annex-B HEVC or H.264 elementary stream into a Relay
//! recording file with the production muxer, so real output can be handed to
//! real players/editors. Not part of the shipped product.
//!
//! Usage: cargo run -p relay-capture --example mux_from_annexb -- <in.h265> <out.mp4> <w> <h> [hevc|h264]

use std::fs;

use std::io::{Seek, Write};

use relay_capture::codec::VideoCodec;
use relay_capture::record::annexb;
use relay_capture::record::mkv::MkvMuxer;
use relay_capture::record::mux::{Mp4Muxer, MuxConfig};

/// Either container, so one harness can feed both to a real player.
enum AnyMuxer<W: Write + Seek> {
    Mp4(Mp4Muxer<W>),
    Mkv(MkvMuxer<W>),
}

impl<W: Write + Seek> AnyMuxer<W> {
    fn push_video(&mut self, au: &[u8], pts: i64, key: bool) -> anyhow::Result<()> {
        match self {
            Self::Mp4(m) => m.push_video(au, pts, key),
            Self::Mkv(m) => m.push_video(au, pts, key),
        }
    }
    fn finalize(self) -> anyhow::Result<W> {
        match self {
            Self::Mp4(m) => m.finalize(),
            Self::Mkv(m) => m.finalize(),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: mux_from_annexb <in.h265> <out.mp4|out.mkv> <width> <height>");
        std::process::exit(2);
    }
    let data = fs::read(&args[1])?;
    let out = fs::File::create(&args[2])?;
    let w: u32 = args[3].parse()?;
    let h: u32 = args[4].parse()?;

    let codec = match args.get(5).map(String::as_str) {
        Some("h264") => VideoCodec::H264,
        _ => VideoCodec::Hevc,
    };
    let cfg = MuxConfig::video_only(w, h).with_codec(codec);
    let mut mux = if args[2].ends_with(".mkv") {
        AnyMuxer::Mkv(MkvMuxer::new(out, cfg))
    } else {
        AnyMuxer::Mp4(Mp4Muxer::new(out, cfg))
    };

    // Regroup NALs into access units: a new AU starts at the first VCL NAL
    // after a non-VCL one (good enough for an all-I/P test clip).
    let nalus = annexb::split_nalus(&data);
    let mut au: Vec<u8> = Vec::new();
    let mut au_has_vcl = false;
    let mut idx: i64 = 0;
    let frame_100ns = 10_000_000i64 / 60;
    let flush =
        |au: &mut Vec<u8>, idx: &mut i64, mux: &mut AnyMuxer<fs::File>| -> anyhow::Result<()> {
            if au.is_empty() {
                return Ok(());
            }
            let key = annexb::split_nalus(au).iter().any(|n| match codec {
                VideoCodec::Hevc => matches!(codec.nal_type(n), 16..=21),
                VideoCodec::H264 => codec.nal_type(n) == 5,
            });
            mux.push_video(au, *idx * frame_100ns, key)?;
            *idx += 1;
            au.clear();
            Ok(())
        };

    for n in nalus {
        let t = codec.nal_type(n);
        let (is_vcl, starts_au) = match codec {
            // A VCL NAL with first_slice_segment_in_pic_flag == 1 starts a new AU.
            VideoCodec::Hevc => (t <= 31, t <= 31 && n.len() > 2 && (n[2] & 0x80) != 0),
            // first_mb_in_slice == 0 (ue '1'), or SEI/SPS/PPS/AUD after a picture.
            VideoCodec::H264 => {
                let vcl = (1..=5).contains(&t);
                (vcl, (vcl && n.len() > 1 && (n[1] & 0x80) != 0) || (6..=9).contains(&t))
            }
        };
        if starts_au && au_has_vcl {
            flush(&mut au, &mut idx, &mut mux)?;
            au_has_vcl = false;
        }
        if is_vcl {
            au_has_vcl = true;
        }
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(n);
    }
    flush(&mut au, &mut idx, &mut mux)?;
    mux.finalize()?;
    println!("wrote {} frames to {}", idx, args[2]);
    Ok(())
}
