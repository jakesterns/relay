//! `relay-share` — the per-share engine process, spawned by the core.
//!
//! ```text
//! relay-share probe                capability report (MFTEnumEx, WGC) as JSON
//! relay-share bench-capture [SECS] capture-only latency benchmark
//! relay-share bench-encode [SECS] [WxH|4k|native] [hevc|h264]
//!                                  capture → NV12 → hardware encode benchmark
//! relay-share bench-codec IN.nv12 W H hevc|h264 MBPS OUT
//!                                  encode a raw clip, for codec quality comparisons
//! relay-share send                 share the primary monitor to a paired peer
//! relay-share recv                 receive and render to a window
//! ```
//!
//! Stats and lifecycle messages go to stdout as NDJSON; the core relays them
//! to the UI. `stop\n` on stdin asks for a graceful teardown.

use anyhow::{bail, Context as _, Result};

/// A rotating `share.log` next to the core's own, or `None` if it cannot be
/// opened. Honours `--data-dir`'s environment equivalent the same way the core
/// does, so a test instance does not scribble into the real log.
fn share_log_writer() -> Option<relay_core::logging::SharedWriter> {
    let paths = relay_core::config::Paths::default_for_user().ok()?;
    let _ = std::fs::create_dir_all(paths.log_dir());
    relay_core::logging::SharedWriter::open(
        paths.log_dir().join("share.log"),
        relay_core::logging::MAX_BYTES,
        relay_core::logging::KEEP,
    )
    .ok()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    let level = if std::env::var("RELAY_LOG").is_ok() {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    // Log to a file as well as stderr.
    //
    // The core spawns this process with stdout as a pipe it reads for NDJSON
    // and stderr inherited, so in the installed app nothing here is kept. When
    // the receiver died on a Windows 10 machine there was no log, no WER entry
    // and no event-log record; the cause was only found by re-running the
    // binary by hand in a console. The component doing the hardest work was
    // the one that left nothing behind.
    //
    // Same rotating writer the core uses, so the format and the 1 MB x 3
    // budget match, in the same folder the uninstaller already removes. A
    // failure to open it is not worth refusing to run over -- stderr still
    // works, and a share that will not start because of its own log file
    // would be a worse bug than the one this fixes.
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    match share_log_writer() {
        Some(writer) => tracing_subscriber::fmt()
            .with_writer(writer.and(std::io::stderr))
            .with_ansi(false)
            .with_max_level(level)
            .init(),
        None => tracing_subscriber::fmt().with_writer(std::io::stderr).with_max_level(level).init(),
    }
    relay_capture::transport::netio::forward_log_crate(level == tracing::Level::DEBUG);

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
                Some("native") => None,
                Some(s) => {
                    let (w, h) = s.split_once('x').context("size must be WxH or `4k`")?;
                    Some((w.parse()?, h.parse()?))
                }
                None => None,
            };
            let bitrate: u32 =
                std::env::var("RELAY_BITRATE_MBPS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
            let codec = parse_codec(args.get(3).map(String::as_str).unwrap_or("hevc"))?;
            bench_encode(secs, out_size, bitrate * 1_000_000, codec)
        }
        #[cfg(windows)]
        "bench-codec" => {
            let [input, w, h, codec, mbps, out] = &args[1..] else {
                bail!("bench-codec IN.nv12 W H hevc|h264 MBPS OUT");
            };
            bench_codec(
                std::path::Path::new(input),
                (w.parse()?, h.parse()?),
                parse_codec(codec)?,
                mbps.parse::<u32>()? * 1_000_000,
                std::path::Path::new(out),
            )
        }
        #[cfg(windows)]
        "bench-audio" => {
            let secs: u64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(5);
            match args.get(2).map(String::as_str) {
                Some("dual") => bench_audio_dual(secs),
                Some("mic") => bench_audio(secs, relay_capture::audio::AudioSource::Microphone),
                Some(pid) => bench_audio(
                    secs,
                    relay_capture::audio::AudioSource::Process { pid: pid.parse()? },
                ),
                None => bench_audio(secs, relay_capture::audio::AudioSource::Desktop),
            }
        }
        #[cfg(windows)]
        "bench-capture" => {
            let secs: u64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(10);
            bench_capture(secs)
        }
        #[cfg(windows)]
        "send" => {
            let opts = parse_send_args(&args[1..])?;
            run_async(relay_capture::transport::sender::run(opts))
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
        // A stand-in receiver for exercising the in-app hosting on one PC:
        // same events, same window, same host commands, but it paints a
        // moving pattern instead of decoding a stream, so nothing is captured
        // and nothing can feed back (B9). The core spawns it in place of
        // `recv` when RELAY_RECEIVE_STUB is set.
        #[cfg(windows)]
        "host-stub" => {
            let opts = parse_recv_args(&args[1..])?;
            run_async(relay_capture::render::run_stub(opts.host))
        }
        #[cfg(windows)]
        "recv" => {
            let opts = parse_recv_args(&args[1..])?;
            run_async(relay_capture::transport::receiver::run(opts))
        }
        // B8 regression check, not a feature: start a read on stdin the way
        // `send`/`recv` do, finish 200 ms later without stdin ever yielding,
        // and let the caller time the exit. See `tests/stdin_shutdown.rs`.
        "stdin-exit-check" => run_async(async {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
            tokio::select! {
                _ = lines.next_line() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
            }
            Ok(())
        }),
        "" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown command `{other}`\n{USAGE}"),
    }
}

/// Run one share on its own runtime, and do not let the runtime outlive it.
///
/// `send` and `recv` read commands with `tokio::io::stdin()`, which parks a
/// blocking-pool thread in `ReadFile`. Dropping a runtime waits for that
/// thread, and the read only returns when the core writes a line or closes
/// the pipe — so a share that ended any *other* way (the sender stopped, the
/// connection dropped) finished its teardown in milliseconds and then sat in
/// `Runtime::drop` until the 3 s deadline killed it. That was B8. A `stop`
/// from the core never showed it, because that line is what the read was
/// waiting for. `RELAY_RUNTIME_DROP=1` brings the old exit back for the test.
fn run_async<F: std::future::Future<Output = Result<()>>>(fut: F) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let result = rt.block_on(fut);
    if std::env::var_os("RELAY_RUNTIME_DROP").is_none() {
        rt.shutdown_timeout(std::time::Duration::from_millis(100));
    }
    result
}

/// `hevc` / `h264` for the benches. Benches only: a share negotiates.
#[cfg(windows)]
fn parse_codec(s: &str) -> Result<relay_capture::codec::VideoCodec> {
    use relay_capture::codec::VideoCodec;
    match s.to_ascii_lowercase().as_str() {
        "hevc" | "h265" => Ok(VideoCodec::Hevc),
        "h264" | "avc" => Ok(VideoCodec::H264),
        other => bail!("unknown codec `{other}` (expected hevc or h264)"),
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
        size: None,
        record_dir: None,
        record: false,
        replay_secs: 0,
        preview_fps: 0,
        container: relay_core::share::RecordingContainer::Mp4,
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
            // Additive since S2: this adds a second Opus track rather than
            // replacing the program mix. Mic-only is `--no-audio --audio-mic`,
            // which is what the core emits for a legacy `mic` preset.
            "--audio-mic" => opts.mic = true,
            "--no-cursor" => opts.cursor = false,
            "--preview-fps" => opts.preview_fps = it.next().context("--preview-fps N")?.parse()?,
            "--size" => {
                let s = it.next().context("--size WxH")?;
                let (w, h) = s.split_once('x').context("--size must be WxH")?;
                opts.size = Some((w.parse()?, h.parse()?));
            }
            "--record-dir" => {
                opts.record_dir = Some(it.next().context("--record-dir PATH")?.into())
            }
            "--record" => opts.record = true,
            "--container" => {
                opts.container = match it.next().context("--container mp4|mkv")?.as_str() {
                    "mp4" => relay_core::share::RecordingContainer::Mp4,
                    "mkv" => relay_core::share::RecordingContainer::Mkv,
                    other => bail!("unknown container `{other}` (expected mp4 or mkv)"),
                }
            }
            "--replay-secs" => opts.replay_secs = it.next().context("--replay-secs N")?.parse()?,
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
    let mut opts = relay_capture::transport::receiver::RecvOpts {
        name: None,
        headless: false,
        code: None,
        vcam: false,
        mic_route: None,
        host: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => opts.name = it.next().cloned(),
            "--headless" => opts.headless = true,
            "--code" => opts.code = it.next().cloned(),
            "--vcam" => opts.vcam = true,
            "--mic-route" => opts.mic_route = it.next().cloned(),
            "--host" => {
                opts.host = Some(it.next().context("--host <hwnd>")?.parse().context("--host")?)
            }
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
    eprintln!("audio source: {source:?}, {secs}s");
    let run = run_audio_source(source, secs)?;
    println!("{}", serde_json::json!({ "stage": "audio", "program": run.report() }));
    Ok(())
}

/// One source run: counters plus the packetization latency distribution
/// (WASAPI block arrival → Opus packet encoded).
#[cfg(windows)]
struct AudioRun {
    packets: u64,
    bytes: u64,
    peak: f32,
    elapsed: f64,
    lat: relay_capture::Percentiles,
    /// The endpoint's own format, before folding and rate conversion. Per
    /// track, because the program mix and the microphone are different
    /// endpoints and are routinely at different rates.
    endpoint_rate: u32,
    endpoint_channels: u16,
    conversion: Option<String>,
}

#[cfg(windows)]
impl AudioRun {
    fn report(&self) -> serde_json::Value {
        let (p50, p99, max) = self.lat.summary().unwrap_or((0.0, 0.0, 0.0));
        serde_json::json!({
            "endpoint_rate": self.endpoint_rate,
            "endpoint_channels": self.endpoint_channels,
            "conversion": self.conversion,
            "packets": self.packets,
            "expected_packets": (self.elapsed * 100.0) as u64,
            "kbps": self.bytes as f64 * 8.0 / self.elapsed / 1e3,
            "peak": self.peak,
            "packetize_ms": { "p50": p50, "p99": p99, "max": max },
        })
    }
}

#[cfg(windows)]
fn run_audio_source(source: relay_capture::audio::AudioSource, secs: u64) -> Result<AudioRun> {
    use relay_capture::audio::{AudioSource, OpusProfile, OpusStream};
    use relay_capture::{time, Percentiles};
    use std::time::{Duration, Instant};

    // Match what a real share uses for this source, or the bench is measuring
    // an encoder nobody runs.
    let profile = match source {
        AudioSource::Microphone => OpusProfile::voice(),
        _ => OpusProfile::program(),
    };
    let mut stream = OpusStream::new(source, profile)?;
    let mut run = AudioRun {
        packets: 0,
        bytes: 0,
        peak: 0.0,
        elapsed: 0.0,
        lat: Percentiles::default(),
        endpoint_rate: stream.endpoint_rate(),
        endpoint_channels: stream.endpoint_channels(),
        conversion: stream.conversion.clone(),
    };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if let Some(p) = stream.next(Duration::from_millis(200))? {
            run.packets += 1;
            run.bytes += p.data.len() as u64;
            run.peak = run.peak.max(stream.peak);
            run.lat.push_ms(time::ticks_to_ms(p.qpc_100ns - p.captured_qpc_100ns));
        }
    }
    run.elapsed = start.elapsed().as_secs_f64();
    Ok(run)
}

/// Both audio sources at once, the way a share with `--audio-mic` runs them:
/// two WASAPI clients, two Opus encoders, two threads. The number that
/// matters is whether the program mix packetizes any slower with the mic
/// alongside it than it does alone.
#[cfg(windows)]
fn bench_audio_dual(secs: u64) -> Result<()> {
    use relay_capture::audio::AudioSource;

    eprintln!("audio sources: desktop + microphone, {secs}s");
    let mic = std::thread::Builder::new()
        .name("bench-mic".into())
        .spawn(move || run_audio_source(AudioSource::Microphone, secs))?;
    let program = run_audio_source(AudioSource::Desktop, secs)?;
    let mic = mic.join().map_err(|_| anyhow::anyhow!("mic bench thread panicked"))??;
    let fp = relay_core::footprint::FootprintMeter::new().sample();
    println!(
        "{}",
        serde_json::json!({
            "stage": "audio-dual",
            "program": program.report(),
            "mic": mic.report(),
            "process_cpu_percent": fp.cpu_percent,
            "process_rss_mb": fp.rss_bytes as f64 / 1e6,
        })
    );
    Ok(())
}

/// Capture → NV12 (optionally scaled) → hardware encode for `secs`.
/// The decision-gate number is `capture_to_encoder_input` p99 + `encode` p99.
#[cfg(windows)]
fn bench_encode(
    secs: u64,
    out_size: Option<(u32, u32)>,
    bitrate_bps: u32,
    codec: relay_capture::codec::VideoCodec,
) -> Result<()> {
    use relay_capture::encode::convert::Converter;
    use relay_capture::encode::mf::{EncoderConfig, EncoderEvent, InflightClock, MfEncoder};
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
        MfEncoder::new(&gpu, &EncoderConfig { codec, width: w, height: h, fps: 60, bitrate_bps })?;
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
            "codec": codec,
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

/// Encode a raw NV12 clip (as written by `ffmpeg -pix_fmt nv12 -f rawvideo`)
/// with the share's exact encoder and tuning, paced at 60 fps like a live
/// capture, and write the Annex B bitstream to `out`. Same frames in, so two
/// codecs can be compared for quality at a given bitrate by decoding `out`
/// against the source. Frames are uploaded through a staging texture; the
/// upload is outside the timed encode window.
#[cfg(windows)]
fn bench_codec(
    input: &std::path::Path,
    (w, h): (u32, u32),
    codec: relay_capture::codec::VideoCodec,
    bitrate_bps: u32,
    out: &std::path::Path,
) -> Result<()> {
    use relay_capture::encode::mf::{EncoderConfig, EncoderEvent, InflightClock, MfEncoder};
    use relay_capture::{d3d, probe, time, Percentiles};
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

    const FPS: u32 = 60;
    const RING: usize = 8;
    let _mf = probe::MediaFoundation::start()?;
    let gpu = d3d::device_for_monitor(d3d::primary_monitor())?;
    let enc =
        MfEncoder::new(&gpu, &EncoderConfig { codec, width: w, height: h, fps: FPS, bitrate_bps })?;
    eprintln!(
        "bench-codec: {} ({}) {w}x{h}@{FPS} {} Mb/s",
        enc.name,
        codec.label(),
        bitrate_bps / 1_000_000
    );

    let desc = |usage, bind: u32, cpu: u32| D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let mut staging = None;
    let mut ring = Vec::with_capacity(RING);
    // SAFETY: valid descriptors; out pointers are ours.
    unsafe {
        gpu.device.CreateTexture2D(
            &desc(D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_WRITE.0 as u32),
            None,
            Some(&mut staging),
        )?;
        for _ in 0..RING {
            let mut t = None;
            gpu.device.CreateTexture2D(
                &desc(D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0),
                None,
                Some(&mut t),
            )?;
            ring.push(t.unwrap());
        }
    }
    let staging = staging.unwrap();

    let frame_len = (w * h * 3 / 2) as usize;
    let mut reader = std::io::BufReader::with_capacity(frame_len * 2, std::fs::File::open(input)?);
    let mut writer = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut buf = vec![0u8; frame_len];
    let mut clock = InflightClock::default();
    let mut encode_time = Percentiles::default();
    let (mut submitted, mut outputs, mut bytes, mut keyframes) = (0u64, 0u64, 0u64, 0u64);
    let mut eof = false;
    let start = Instant::now();
    let frame_100ns = 10_000_000 / FPS as i64;

    loop {
        match enc.next_event()? {
            EncoderEvent::NeedInput => {
                if eof || reader.read_exact(&mut buf).is_err() {
                    eof = true;
                    if clock.in_flight() == 0 {
                        break;
                    }
                    continue;
                }
                // Pace like a live 60 fps capture: rate control and the
                // encoder's queueing both behave differently when flooded.
                let due = start + Duration::from_micros(submitted * 1_000_000 / FPS as u64);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                let tex = &ring[submitted as usize % RING];
                // SAFETY: map the CPU staging texture, copy both NV12 planes
                // honouring the row pitch, unmap, then copy to the GPU texture.
                unsafe {
                    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                    gpu.context.Map(&staging, 0, D3D11_MAP_WRITE, 0, Some(&mut m))?;
                    let pitch = m.RowPitch as usize;
                    let dst = m.pData as *mut u8;
                    let (wu, hu) = (w as usize, h as usize);
                    for row in 0..hu {
                        std::ptr::copy_nonoverlapping(
                            buf.as_ptr().add(row * wu),
                            dst.add(row * pitch),
                            wu,
                        );
                    }
                    for row in 0..hu / 2 {
                        std::ptr::copy_nonoverlapping(
                            buf.as_ptr().add(wu * hu + row * wu),
                            dst.add(hu * pitch + row * pitch),
                            wu,
                        );
                    }
                    gpu.context.Unmap(&staging, 0);
                    gpu.context.CopyResource(tex, &staging);
                }
                let pts = submitted as i64 * frame_100ns;
                clock.submitted(pts, time::qpc_now_100ns());
                enc.submit(tex, pts)?;
                submitted += 1;
            }
            EncoderEvent::Output(frame) => {
                if let Some(dt) = clock.completed(frame.pts_100ns, time::qpc_now_100ns()) {
                    encode_time.push_ms(time::ticks_to_ms(dt));
                }
                outputs += 1;
                keyframes += u64::from(frame.keyframe);
                bytes += frame.data.len() as u64;
                writer.write_all(&frame.data)?;
                if eof && clock.in_flight() == 0 {
                    break;
                }
            }
        }
    }
    writer.flush()?;
    let clip_secs = submitted as f64 / FPS as f64;
    let (e50, e99, emax) = encode_time.summary().unwrap_or((0.0, 0.0, 0.0));
    println!(
        "{}",
        serde_json::json!({
            "stage": "codec",
            "codec": codec,
            "encoder": enc.name,
            "size": format!("{w}x{h}@{FPS}"),
            "target_mbps": bitrate_bps as f64 / 1e6,
            "frames_in": submitted,
            "frames_out": outputs,
            "keyframes": keyframes,
            "produced_mbps": bytes as f64 * 8.0 / clip_secs.max(1e-9) / 1e6,
            "encode_ms": { "p50": e50, "p99": e99, "max": emax },
        })
    );
    Ok(())
}

const USAGE: &str = "\
relay-share [probe|bench-capture [SECS]|bench-encode [SECS] [WxH|4k]|send|recv]

  probe          print the capability report (hardware HEVC MFTs, WGC) as JSON
  bench-audio    capture audio (desktop | mic | pid N), Opus-encode, report packet flow
  bench-capture  measure capture latency on the primary monitor
  bench-encode   measure capture -> NV12 -> hardware encode latency (hevc | h264)
  bench-codec    encode a raw NV12 clip to an Annex B file (codec comparisons)
  bench-audio    measure Opus packetization latency for one source, or for
                 the program mix and the microphone together (`dual`)
  send           share to a paired peer (spawned by relay-core)
                 (--audio-pid <pid> narrows the program mix to one process;
                  --audio-mic *adds* a second microphone track;
                  --no-audio --audio-mic sends the microphone alone)
  recv           receive a share and render it to a window
                 (--vcam mirrors into the Relay virtual camera;
                  --mic-route <endpoint-id> renders audio to that endpoint)
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
        assert_eq!(o.record_dir, None);
        assert!(!o.record);
        assert_eq!(o.replay_secs, 0);
        assert_eq!(o.size, None);
    }

    /// `--audio-mic` *adds* the microphone: the program mix keeps its own
    /// track, which is the whole point of S2.
    #[test]
    fn send_size_and_mic_flags() {
        let o =
            parse_send_args(&s(&["--code", "1", "--size", "2560x1440", "--audio-mic"])).unwrap();
        assert_eq!(o.size, Some((2560, 1440)));
        assert!(o.mic);
        assert!(matches!(o.audio, Some(AudioSource::Desktop)), "desktop mix is still sent");
        assert!(parse_send_args(&s(&["--code", "1", "--size", "huge"])).is_err());
    }

    /// Game audio and the mic together: two tracks, neither displacing the
    /// other.
    #[test]
    fn game_audio_and_mic_coexist() {
        let o =
            parse_send_args(&s(&["--code", "1", "--audio-pid", "4321", "--audio-mic"])).unwrap();
        assert!(matches!(o.audio, Some(AudioSource::Process { pid: 4321 })));
        assert!(o.mic);
    }

    /// Mic alone is still expressible, and is what the core emits for a
    /// legacy `mic` preset — so those presets behave exactly as before.
    #[test]
    fn mic_only_is_no_audio_plus_audio_mic() {
        let o = parse_send_args(&s(&["--code", "1", "--no-audio", "--audio-mic"])).unwrap();
        assert!(o.audio.is_none(), "no program track");
        assert!(o.mic);
    }

    #[test]
    fn send_defaults_to_no_mic_track() {
        assert!(!parse_send_args(&s(&["--code", "1"])).unwrap().mic);
    }

    #[test]
    fn send_recording_flags() {
        let o = parse_send_args(&s(&[
            "--code",
            "1",
            "--record-dir",
            r"C:\Users\jake\Videos\Relay",
            "--record",
            "--replay-secs",
            "90",
        ]))
        .unwrap();
        assert_eq!(
            o.record_dir.as_deref(),
            Some(std::path::Path::new(r"C:\Users\jake\Videos\Relay"))
        );
        assert!(o.record);
        assert_eq!(o.replay_secs, 90);
        assert!(parse_send_args(&s(&["--code", "1", "--replay-secs", "soon"])).is_err());
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

        let o = parse_recv_args(&s(&["--host", "133742"])).unwrap();
        assert_eq!(o.host, Some(133742));
        assert!(parse_recv_args(&s(&["--host", "nope"])).is_err());

        assert!(parse_recv_args(&s(&["--wat"])).is_err());
    }
}
