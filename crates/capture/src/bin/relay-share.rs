//! `relay-share` — the per-share engine process, spawned by the core.
//!
//! ```text
//! relay-share probe                capability report (MFTEnumEx, WGC) as JSON
//! relay-share bench-capture [SECS] capture-only latency benchmark
//! relay-share bench-encode [SECS]  capture → NV12 → HEVC encode benchmark
//! relay-share send                 share the primary monitor to a paired peer
//! relay-share recv                 receive and render to a window
//! ```
//!
//! Stats and lifecycle messages go to stdout as NDJSON; the core relays them
//! to the UI. `stop\n` on stdin asks for a graceful teardown.

use anyhow::{bail, Context as _, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(if std::env::var("RELAY_LOG").is_ok() {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();

    match cmd {
        #[cfg(windows)]
        "probe" => {
            let _mf = relay_capture::probe::MediaFoundation::start()?;
            let report = relay_capture::probe::report()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        #[cfg(windows)]
        "bench-encode" => {
            let secs: u64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(60);
            let out_size = match args.get(2).map(String::as_str) {
                Some("4k") => Some((3840u32, 2160u32)),
                Some(s) => {
                    let (w, h) = s.split_once('x').context("size must be WxH or `4k`")?;
                    Some((w.parse()?, h.parse()?))
                }
                None => None,
            };
            let bitrate: u32 =
                std::env::var("RELAY_BITRATE_MBPS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
            bench_encode(secs, out_size, bitrate * 1_000_000)
        }
        #[cfg(windows)]
        "bench-audio" => {
            let secs: u64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(5);
            let source = match args.get(2).map(String::as_str) {
                Some("mic") => relay_capture::audio::AudioSource::Microphone,
                Some(pid) => relay_capture::audio::AudioSource::Process { pid: pid.parse()? },
                None => relay_capture::audio::AudioSource::Desktop,
            };
            bench_audio(secs, source)
        }
        #[cfg(windows)]
        "bench-capture" => {
            let secs: u64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(10);
            bench_capture(secs)
        }
        #[cfg(windows)]
        "send" => {
            let opts = parse_send_args(&args[1..])?;
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?
                .block_on(relay_capture::transport::sender::run(opts))
        }
        #[cfg(windows)]
        "discover" => {
            let mut timeout_ms = 2000u64;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--timeout-ms" {
                    timeout_ms = it.next().context("--timeout-ms N")?.parse()?;
                }
            }
            let found = relay_capture::transport::discovery::browse(
                std::time::Duration::from_millis(timeout_ms),
            )?;
            println!("{}", serde_json::to_string(&found)?);
            Ok(())
        }
        #[cfg(windows)]
        "recv" => {
            let opts = parse_recv_args(&args[1..])?;
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?
                .block_on(relay_capture::transport::receiver::run(opts))
        }
        "" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown command `{other}`\n{USAGE}"),
    }
}

/// Parse `relay-share send` flags into [`SendOpts`].
#[cfg(windows)]
fn parse_send_args(args: &[String]) -> Result<relay_capture::transport::sender::SendOpts> {
    let mut opts = relay_capture::transport::sender::SendOpts {
        peer: None,
        code: String::new(),
        bitrate_bps: 60_000_000,
        fps: 60,
        audio: Some(relay_capture::audio::AudioSource::Desktop),
        mic: false,
        cursor: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--peer" => opts.peer = it.next().cloned(),
            "--code" => opts.code = it.next().cloned().unwrap_or_default(),
            "--bitrate" => {
                opts.bitrate_bps = it.next().context("--bitrate Mb/s")?.parse::<u32>()? * 1_000_000
            }
            "--fps" => opts.fps = it.next().context("--fps N")?.parse()?,
            "--no-audio" => opts.audio = None,
            "--audio-pid" => {
                opts.audio = Some(relay_capture::audio::AudioSource::Process {
                    pid: it.next().context("--audio-pid PID")?.parse()?,
                })
            }
            "--no-cursor" => opts.cursor = false,
            other => bail!("unknown send flag `{other}`"),
        }
    }
    if opts.code.is_empty() {
        bail!("send needs --code <six digits from the receiver>");
    }
    Ok(opts)
}

/// Parse `relay-share recv` flags into [`RecvOpts`].
#[cfg(windows)]
fn parse_recv_args(args: &[String]) -> Result<relay_capture::transport::receiver::RecvOpts> {
    let mut opts =
        relay_capture::transport::receiver::RecvOpts { name: None, headless: false, code: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => opts.name = it.next().cloned(),
            "--headless" => opts.headless = true,
            "--code" => opts.code = it.next().cloned(),
            other => bail!("unknown recv flag `{other}`"),
        }
    }
    Ok(opts)
}

/// Capture the primary monitor for `secs` and report present→received
/// latency percentiles, delivered fps and source-side drops.
#[cfg(windows)]
fn bench_capture(secs: u64) -> Result<()> {
    use relay_capture::{d3d, source, time, Percentiles};
    use std::time::{Duration, Instant};

    let hmon = d3d::primary_monitor();
    let gpu = d3d::device_for_monitor(hmon)?;
    eprintln!("capturing primary monitor on adapter: {}", gpu.adapter_name);
    let mut src = source::create(&gpu, hmon, true)?;
    let (w, h) = src.size();
    eprintln!("capture size {w}x{h}, running {secs}s");

    let mut lat = Percentiles::default();
    let mut frames = 0u64;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        let Some(frame) = src.next(Duration::from_millis(500))? else {
            continue;
        };
        let delta = time::qpc_now_100ns() - frame.qpc_100ns;
        lat.push_ms(time::ticks_to_ms(delta));
        frames += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    let (p50, p99, max) = lat.summary().unwrap_or((0.0, 0.0, 0.0));
    println!(
        "{}",
        serde_json::json!({
            "stage": "capture",
            "size": format!("{w}x{h}"),
            "adapter": gpu.adapter_name,
            "frames": frames,
            "fps": frames as f64 / elapsed,
            "present_to_receive_ms": { "p50": p50, "p99": p99, "max": max },
            "dropped_by_source": src.dropped(),
        })
    );
    Ok(())
}

/// Capture audio for `secs`, Opus-encode 10 ms frames, report packet flow.
#[cfg(windows)]
fn bench_audio(secs: u64, source: relay_capture::audio::AudioSource) -> Result<()> {
    use relay_capture::audio::OpusStream;
    use std::time::{Duration, Instant};

    eprintln!("audio source: {source:?}, {secs}s");
    let mut stream = OpusStream::new(source, 160_000)?;
    let mut packets = 0u64;
    let mut bytes = 0u64;
    let mut peak = 0.0f32;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if let Some(p) = stream.next(Duration::from_millis(200))? {
            packets += 1;
            bytes += p.data.len() as u64;
            peak = peak.max(stream.peak);
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({
            "stage": "audio",
            "packets": packets,
            "expected_packets": (elapsed * 100.0) as u64,
            "kbps": bytes as f64 * 8.0 / elapsed / 1e3,
            "peak": peak,
        })
    );
    Ok(())
}

/// Capture → NV12 (optionally scaled) → HEVC hardware encode for `secs`.
/// The decision-gate number is `capture_to_encoder_input` p99 + `encode` p99.
#[cfg(windows)]
fn bench_encode(secs: u64, out_size: Option<(u32, u32)>, bitrate_bps: u32) -> Result<()> {
    use relay_capture::encode::convert::Converter;
    use relay_capture::encode::mf::{EncoderConfig, EncoderEvent, InflightClock, MfHevcEncoder};
    use relay_capture::source::{wgc::WgcCapture, FrameSource};
    use relay_capture::{d3d, probe, time, Percentiles};
    use std::time::{Duration, Instant};

    let _mf = probe::MediaFoundation::start()?;
    let hmon = d3d::primary_monitor();
    let gpu = d3d::device_for_monitor(hmon)?;
    let mut src = WgcCapture::monitor(&gpu, hmon, true)?;
    let in_size = src.size();
    let (w, h) = out_size.unwrap_or(in_size);
    let mut conv = Converter::new(&gpu, in_size, (w, h))?;
    let enc =
        MfHevcEncoder::new(&gpu, &EncoderConfig { width: w, height: h, fps: 60, bitrate_bps })?;
    eprintln!(
        "encoder: {} on {} | {}x{} -> {}x{} @60, {} Mb/s CBR, {secs}s",
        enc.name,
        gpu.adapter_name,
        in_size.0,
        in_size.1,
        w,
        h,
        bitrate_bps / 1_000_000
    );

    let mut meter = relay_core::footprint::FootprintMeter::new();
    meter.sample();
    let mut cap_to_input = Percentiles::default();
    let mut encode_time = Percentiles::default();
    let mut clock = InflightClock::default();
    let mut frames_out = 0u64;
    let mut keyframes = 0u64;
    let mut bytes = 0u64;
    let start = Instant::now();
    let deadline = Duration::from_secs(secs);

    while (start.elapsed() < deadline || clock.in_flight() > 0)
        && start.elapsed() < deadline + Duration::from_secs(2)
    {
        match enc.next_event()? {
            EncoderEvent::NeedInput => {
                if start.elapsed() >= deadline {
                    // Stop feeding; drain what is in flight via a small wait.
                    if clock.in_flight() == 0 {
                        break;
                    }
                    continue;
                }
                let Some(frame) = src.next(Duration::from_millis(500))? else { continue };
                let nv12 = conv.convert(&frame.texture)?;
                let now = time::qpc_now_100ns();
                cap_to_input.push_ms(time::ticks_to_ms(now - frame.qpc_100ns));
                enc.submit(&nv12, frame.qpc_100ns)?;
                clock.submitted(frame.qpc_100ns, time::qpc_now_100ns());
            }
            EncoderEvent::Output(out) => {
                let now = time::qpc_now_100ns();
                if let Some(dt) = clock.completed(out.pts_100ns, now) {
                    encode_time.push_ms(time::ticks_to_ms(dt));
                }
                frames_out += 1;
                keyframes += u64::from(out.keyframe);
                bytes += out.data.len() as u64;
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let fp = meter.sample();
    let (c50, c99, cmax) = cap_to_input.summary().unwrap_or((0.0, 0.0, 0.0));
    let (e50, e99, emax) = encode_time.summary().unwrap_or((0.0, 0.0, 0.0));
    println!(
        "{}",
        serde_json::json!({
            "stage": "encode",
            "encoder": enc.name,
            "input": format!("{}x{}", in_size.0, in_size.1),
            "output": format!("{w}x{h}@60"),
            "seconds": elapsed,
            "frames_encoded": frames_out,
            "fps": frames_out as f64 / elapsed,
            "keyframes": keyframes,
            "produced_mbps": bytes as f64 * 8.0 / elapsed / 1e6,
            "capture_to_encoder_input_ms": { "p50": c50, "p99": c99, "max": cmax },
            "encode_ms": { "p50": e50, "p99": e99, "max": emax },
            "dropped_by_source": src.dropped(),
            "process_cpu_percent": fp.cpu_percent,
            "process_rss_mb": fp.rss_bytes as f64 / 1e6,
        })
    );
    Ok(())
}

const USAGE: &str = "\
relay-share [probe|bench-capture [SECS]|bench-encode [SECS] [WxH|4k]|send|recv]

  probe          print the capability report (hardware HEVC MFTs, WGC) as JSON
  bench-capture  measure capture latency on the primary monitor
  bench-encode   measure capture -> NV12 -> HEVC hardware encode latency
  send           share to a paired peer (spawned by relay-core)
  recv           receive a share and render it to a window
";

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use relay_capture::audio::AudioSource;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn send_defaults() {
        let o = parse_send_args(&s(&["--code", "123456"])).unwrap();
        assert_eq!(o.code, "123456");
        assert_eq!(o.peer, None);
        assert_eq!(o.bitrate_bps, 60_000_000);
        assert_eq!(o.fps, 60);
        assert!(matches!(o.audio, Some(AudioSource::Desktop)));
        assert!(o.cursor);
    }

    #[test]
    fn send_all_flags() {
        let o = parse_send_args(&s(&[
            "--code",
            "000042",
            "--peer",
            "den-pc",
            "--bitrate",
            "80",
            "--fps",
            "30",
            "--audio-pid",
            "4321",
            "--no-cursor",
        ]))
        .unwrap();
        assert_eq!(o.peer.as_deref(), Some("den-pc"));
        assert_eq!(o.bitrate_bps, 80_000_000, "--bitrate is Mb/s");
        assert_eq!(o.fps, 30);
        assert!(matches!(o.audio, Some(AudioSource::Process { pid: 4321 })));
        assert!(!o.cursor);
    }

    #[test]
    fn send_no_audio_wins_over_default() {
        let o = parse_send_args(&s(&["--code", "1", "--no-audio"])).unwrap();
        assert!(o.audio.is_none());
    }

    #[test]
    fn send_requires_code_and_rejects_unknown_flags() {
        assert!(parse_send_args(&s(&[])).unwrap_err().to_string().contains("--code"));
        assert!(parse_send_args(&s(&["--code", "1", "--nope"]))
            .unwrap_err()
            .to_string()
            .contains("--nope"));
        assert!(parse_send_args(&s(&["--code", "1", "--bitrate", "lots"])).is_err());
    }

    #[test]
    fn recv_flags() {
        let o = parse_recv_args(&s(&[])).unwrap();
        assert_eq!(o.name, None);
        assert!(!o.headless);
        assert_eq!(o.code, None);

        let o =
            parse_recv_args(&s(&["--name", "den-pc", "--headless", "--code", "555555"])).unwrap();
        assert_eq!(o.name.as_deref(), Some("den-pc"));
        assert!(o.headless);
        assert_eq!(o.code.as_deref(), Some("555555"));

        assert!(parse_recv_args(&s(&["--wat"])).is_err());
    }
}
