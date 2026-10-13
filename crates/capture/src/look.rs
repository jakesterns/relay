//! S47: the look sampler (`relay-share look`).
//!
//! Spawned by the core only while a game with "Learn this game's look" on
//! has focus, killed on blur. Once per interval it takes the latest frame of
//! the game's monitor through DXGI Desktop Duplication, scales it to
//! 480×270 on the GPU (the preview tap's video processor), reads that back,
//! analyses it with `relay_display::learn` and prints one line of numbers.
//! The frame itself is dropped straight away: nothing is written to disk and
//! no pixels leave this process.
//!
//! Why Desktop Duplication and not Windows.Graphics.Capture: duplication is
//! output-level (it never names or touches the game's window or process),
//! it coalesces updates on its own so sampling at 1 fps costs nothing in
//! between, and it draws no capture border. WGC would need a capture item
//! for the game window — the same OS broker, but aimed at the game.
//!
//! HDR: an output in PQ/BT.2020 mode does not hand out SDR code values, so
//! the sampler says `look_hdr` and exits rather than learn from the wrong
//! space.

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use relay_display::learn::{with_input_idle, Analyser, Frame, Order};
use windows::core::Interface;
use windows::Win32::Graphics::Dxgi::IDXGIOutput6;
use windows::Win32::Graphics::Gdi::HMONITOR;

use crate::d3d;
use crate::preview::Preview;
use crate::source::dxgi::DxgiCapture;
use crate::source::FrameSource;

/// Highest sampling rate accepted. More would not learn faster and would
/// start to cost something.
pub const MAX_FPS: u32 = 2;

/// Is this monitor's output in HDR (PQ) mode?
pub fn output_is_hdr(hmonitor: HMONITOR) -> Result<bool> {
    let output = d3d::output_for_monitor(hmonitor)?;
    let Ok(o6) = output.cast::<IDXGIOutput6>() else { return Ok(false) };
    // SAFETY: plain descriptor query on a live output.
    let desc = unsafe { o6.GetDesc1() }.context("GetDesc1")?;
    Ok(relay_display::learn::is_hdr_color_space(desc.ColorSpace.0))
}

/// Milliseconds since the last keyboard/mouse input anywhere on the system.
/// One system-wide timestamp: no hook, no key data.
fn input_idle_ms() -> Option<u64> {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: a sized out-struct of our own.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return None;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount() };
    Some(now.wrapping_sub(info.dwTime) as u64)
}

fn lower_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    };
    // SAFETY: our own pseudo-handle.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
}

/// Run until stdin closes (the core went away or stopped us).
pub fn run(hmonitor: HMONITOR, fps: u32) -> Result<()> {
    let fps = fps.clamp(1, MAX_FPS);
    lower_priority();
    // An output whose colour space cannot be read is skipped like HDR:
    // learning from the wrong space is worse than not learning.
    if output_is_hdr(hmonitor).unwrap_or(true) {
        println!("{}", serde_json::json!({ "event": "look_hdr" }));
        return Ok(());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let paused = paused.clone();
        std::thread::Builder::new().name("look-stdin".into()).spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) if l.trim() == "stop" => break,
                    // Alt-Tab grace: the game is out of focus; sample nothing
                    // until it comes back (the core resumes or stops us).
                    Ok(l) if l.trim() == "pause" => paused.store(true, Ordering::SeqCst),
                    Ok(l) if l.trim() == "resume" => paused.store(false, Ordering::SeqCst),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            stop.store(true, Ordering::SeqCst);
        })?;
    }

    let interval = Duration::from_millis(1000 / fps as u64);
    let mut analyser = Analyser::new();
    while !stop.load(Ordering::SeqCst) {
        // (Re)build on start and after a mode change or exclusive fullscreen
        // took the output away.
        let gpu = d3d::device_for_monitor(hmonitor)?;
        let mut src = match DxgiCapture::monitor(&gpu, hmonitor) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "duplication unavailable; retrying");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let mut preview = Preview::new(&gpu, src.size())?;
        let (w, h) = preview.size();
        let mut next = Instant::now();
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if paused.load(Ordering::SeqCst) {
                std::thread::sleep(interval);
                next = Instant::now();
                // Whatever is on screen now is not the game: no motion
                // reference or content region carries across the pause.
                analyser = Analyser::new();
                continue;
            }
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
            }
            next += interval;
            let frame = match src.next(Duration::from_millis(50)) {
                Ok(Some(f)) => f,
                Ok(None) => continue, // nothing new on screen: no sample
                Err(e) => {
                    tracing::info!(error = %e, "duplication lost; rebuilding");
                    break;
                }
            };
            let bgr = preview.bgr(&gpu, &frame.texture)?;
            drop(frame);
            let report = analyser.analyse(&Frame {
                width: w as usize,
                height: h as usize,
                data: &bgr,
                order: Order::Bgr,
            });
            drop(bgr);
            let report = with_input_idle(report, input_idle_ms());
            println!("{}", serde_json::json!({ "event": "look", "report": report }));
        }
        analyser = Analyser::new();
    }
    Ok(())
}
