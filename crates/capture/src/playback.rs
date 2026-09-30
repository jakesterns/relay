//! Receiver audio playback: Opus decode → WASAPI shared-mode render on the
//! default endpoint, or on an explicitly named endpoint (the interim
//! virtual-mic route feeds a VB-Cable / VoiceMeeter input this way).
//! Shared mode only; nothing about any endpoint is changed.
//!
//! The sender may ship two audio tracks — the program mix and its
//! microphone. They arrive here separately and are summed one op before the
//! render buffer, which is the last moment they could be: a plain call hears
//! one stream, and the routing work in S19 has somewhere to cut in. See
//! `docs/dev/dual-audio-decision.md`.
//!
//! Latency (B16). Everything between a packet arriving and its samples
//! reaching the endpoint is counted in [`PlaybackStats`]. The delay is held
//! by two small, bounded stores: a per-track [`JitterQueue`] (decoded PCM,
//! primed to [`PRIME_FRAMES`]) and the WASAPI buffer, which is never filled
//! past [`MAX_PADDING_MS`]. Before S33 the render buffer was one second long
//! and topped up to the brim with silence on the first wake, so every sample
//! queued behind a full second: that *was* the ~1 s. `RELAY_AUDIO_LEGACY=1`
//! brings that behaviour back so the two can be measured by the same binary.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};

use crate::devices::{self, DeviceSlot, Flow, Opened};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Opus decodes to this, and the render stream is opened at it.
const RATE: u32 = 48_000;
const FRAMES_PER_MS: usize = (RATE / 1000) as usize;

/// A track starts (and restarts after running dry) once this much is decoded.
/// Four 10 ms packets: half goes straight into the render buffer, and what
/// stays rides out a packet arriving a period late.
pub const PRIME_FRAMES: usize = 40 * FRAMES_PER_MS;
/// The render buffer is never topped up beyond this. Two engine periods.
pub const MAX_PADDING_MS: u32 = 20;
/// Slew band for the queue's one-second low-water mark: above it a frame is
/// skipped per block, below it one is repeated, so two sound cards a few
/// dozen ppm apart never run the queue dry or let it grow.
const SLEW_HIGH_FRAMES: usize = 35 * FRAMES_PER_MS;
const SLEW_LOW_FRAMES: usize = 10 * FRAMES_PER_MS;
/// One frame in this many is skipped or repeated while slewing: 0.2 %, which
/// outruns any real clock mismatch and is far below audible pitch change.
const SLEW_BLOCK: usize = 480;
/// A low-water mark this deep is not drift, it is a burst after a stall;
/// drop back to the prime depth at once rather than slew for a minute.
const HARD_HIGH_FRAMES: usize = 150 * FRAMES_PER_MS;
const WINDOW_FRAMES: usize = RATE as usize;

/// Where received audio is waiting, for the stats line. All lock-free; the
/// render thread writes, the stats ticker reads.
#[derive(Default)]
pub struct PlaybackStats {
    /// Frames sitting in the WASAPI render buffer (at 48 kHz).
    pub render_padding_frames: AtomicU32,
    /// The size WASAPI actually gave us.
    pub render_buffer_frames: AtomicU32,
    /// `IAudioClient::GetStreamLatency`, µs: the engine + endpoint's own share.
    pub stream_latency_us: AtomicU32,
    /// Decoded program-track frames not yet handed to WASAPI.
    pub queue_frames: AtomicU32,
    /// Packets received but not yet decoded (channel depth), program track.
    pub channel_packets: AtomicU32,
    /// Times the program track ran dry after it had started.
    pub underruns: AtomicU64,
    /// Frames skipped (+) or repeated, to follow the sender's sample clock.
    pub slew_skipped: AtomicU64,
    pub slew_repeated: AtomicU64,
    /// Frames thrown away by the hard high-water drop.
    pub dropped_frames: AtomicU64,
    /// Peak of the last rendered buffer after the faders and the sum, ×1e3.
    /// What reached the endpoint, not what arrived.
    pub peak_milli: AtomicU32,
}

impl PlaybackStats {
    /// Arrival-to-endpoint delay of the program track right now, in ms.
    pub fn buffered_ms(&self) -> f64 {
        let frames = self.render_padding_frames.load(Ordering::Relaxed)
            + self.queue_frames.load(Ordering::Relaxed);
        frames as f64 / FRAMES_PER_MS as f64
            + self.channel_packets.load(Ordering::Relaxed) as f64 * 10.0
            + self.stream_latency_us.load(Ordering::Relaxed) as f64 / 1e3
    }

    pub fn json(&self) -> serde_json::Value {
        let ms = |a: &AtomicU32| a.load(Ordering::Relaxed) as f64 / FRAMES_PER_MS as f64;
        serde_json::json!({
            "buffered_ms": (self.buffered_ms() * 10.0).round() / 10.0,
            "render_ms": ms(&self.render_padding_frames),
            "queue_ms": ms(&self.queue_frames),
            "channel_packets": self.channel_packets.load(Ordering::Relaxed),
            "engine_ms": self.stream_latency_us.load(Ordering::Relaxed) as f64 / 1e3,
            "render_buffer_ms": ms(&self.render_buffer_frames),
            "underruns": self.underruns.load(Ordering::Relaxed),
            "slew_skipped": self.slew_skipped.load(Ordering::Relaxed),
            "slew_repeated": self.slew_repeated.load(Ordering::Relaxed),
            "dropped_frames": self.dropped_frames.load(Ordering::Relaxed),
            "peak": self.peak_milli.load(Ordering::Relaxed) as f64 / 1e3,
        })
    }
}

/// One incoming Opus track for the render loop: its packet channel and the
/// fader it reads. The first stream is the one the depth stats describe.
pub type Stream = (tokio::sync::mpsc::Receiver<Vec<u8>>, crate::mixer::Track);

/// Decode `streams` and render their fader-weighted sum on the endpoint
/// `device` names (the default render endpoint when it is empty) until
/// `stop`. The receiver runs it with the sender's tracks; since S19 the
/// sender runs it too, with the one track coming back from the call. Since
/// S40 the output reopens in place when `device` changes, or when it follows
/// the default and the OS default moves.
pub fn run(
    streams: Vec<Stream>,
    stop: Receiver<()>,
    device: Arc<DeviceSlot>,
    stats: Arc<PlaybackStats>,
    faders: Arc<crate::mixer::Faders>,
) -> Result<()> {
    anyhow::ensure!(!streams.is_empty(), "playback needs at least one stream");
    // SAFETY: COM for this thread; WASAPI render loop; balanced on return.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
        let result = render_loop(streams, stop, device, stats, faders);
        CoUninitialize();
        result
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Slew {
    #[default]
    None,
    Skip,
    Repeat,
}

/// Decoded stereo PCM waiting for the render buffer: a small jitter buffer
/// that primes before it plays, re-primes after running dry, and follows the
/// sender's sample clock by skipping or repeating one frame per block.
#[derive(Default)]
pub struct JitterQueue {
    /// Interleaved L/R.
    q: VecDeque<f32>,
    primed: bool,
    /// Legacy mode: no priming, no slewing — play whatever is there.
    passthrough: bool,
    /// Lowest depth seen in the current window, and frames popped in it.
    low_water: usize,
    window_popped: usize,
    slew: Slew,
    since_slew: usize,
    last: (f32, f32),
    pub underruns: u64,
    pub skipped: u64,
    pub repeated: u64,
    pub dropped: u64,
}

impl JitterQueue {
    pub fn passthrough() -> Self {
        Self { passthrough: true, ..Self::default() }
    }

    pub fn frames(&self) -> usize {
        self.q.len() / 2
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        self.q.extend(interleaved);
    }

    /// Next stereo pair, or silence when this track has nothing to give. A
    /// track that has not started yet, or has fallen behind, contributes
    /// silence rather than stalling the others.
    pub fn pop_pair(&mut self) -> (f32, f32) {
        if self.passthrough {
            return self.take().unwrap_or((0.0, 0.0));
        }
        if !self.primed {
            if self.frames() < PRIME_FRAMES {
                return (0.0, 0.0);
            }
            self.primed = true;
            self.low_water = self.frames();
            self.window_popped = 0;
            self.slew = Slew::None;
        }
        self.low_water = self.low_water.min(self.frames());
        self.window_popped += 1;
        if self.window_popped >= WINDOW_FRAMES {
            self.end_window();
        }

        self.since_slew += 1;
        if self.slew != Slew::None && self.since_slew >= SLEW_BLOCK {
            self.since_slew = 0;
            match self.slew {
                Slew::Skip if self.frames() > 1 => {
                    self.take();
                    self.skipped += 1;
                }
                Slew::Repeat => {
                    self.repeated += 1;
                    return self.last;
                }
                _ => {}
            }
        }
        match self.take() {
            Some(pair) => {
                self.last = pair;
                pair
            }
            None => {
                self.primed = false;
                self.underruns += 1;
                (0.0, 0.0)
            }
        }
    }

    fn take(&mut self) -> Option<(f32, f32)> {
        let l = self.q.pop_front()?;
        Some((l, self.q.pop_front().unwrap_or(l)))
    }

    /// Decide the next second's correction from this second's low-water mark.
    fn end_window(&mut self) {
        if self.low_water > HARD_HIGH_FRAMES {
            let excess = self.frames().saturating_sub(PRIME_FRAMES);
            self.q.drain(..excess * 2);
            self.dropped += excess as u64;
            self.slew = Slew::None;
        } else if self.low_water > SLEW_HIGH_FRAMES {
            self.slew = Slew::Skip;
        } else if self.low_water < SLEW_LOW_FRAMES {
            self.slew = Slew::Repeat;
        } else {
            self.slew = Slew::None;
        }
        self.low_water = self.frames();
        self.window_popped = 0;
    }
}

/// One incoming Opus track: its packet channel, decoder and jitter queue.
struct DecodedStream {
    rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    decoder: opus::Decoder,
    queue: JitterQueue,
}

impl DecodedStream {
    fn new(rx: tokio::sync::mpsc::Receiver<Vec<u8>>, legacy: bool) -> Result<Self> {
        Ok(Self {
            rx,
            decoder: opus::Decoder::new(RATE, opus::Channels::Stereo)?,
            queue: if legacy { JitterQueue::passthrough() } else { JitterQueue::default() },
        })
    }

    /// Decode everything waiting without blocking.
    fn drain(&mut self, pcm: &mut [f32]) {
        // `try_recv` ends the drain on empty or closed alike: either way
        // there is nothing more to decode this pass.
        while let Ok(pkt) = self.rx.try_recv() {
            if let Ok(n) = self.decoder.decode_float(&pkt, pcm, false) {
                self.queue.push(&pcm[..n * 2]);
            }
        }
    }
}

/// Sum the sources and keep the result in range. Two independently normalised
/// sources can exceed full scale together; hard-clipping is the honest cheap
/// answer here — the sources are not ours to compress, and a limiter on the
/// receiver would be DSP on the share path.
pub fn mix_sum(samples: &[f32]) -> f32 {
    samples.iter().sum::<f32>().clamp(-1.0, 1.0)
}

/// 48 kHz stereo float. Opened with AUTOCONVERTPCM so an endpoint whose mix
/// format is anything else is converted by the audio engine, not played at
/// the wrong pitch; the endpoint's own format is never touched.
fn render_format() -> WAVEFORMATEXTENSIBLE {
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    const IEEE_FLOAT: windows::core::GUID =
        windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xFFFE, // WAVE_FORMAT_EXTENSIBLE
            nChannels: 2,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
        dwChannelMask: 0x3, // front left | front right
        SubFormat: IEEE_FLOAT,
    }
}

/// One opened render stream. Dropping it stops the client.
struct Output {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: HANDLE,
    buffer_frames: u32,
    max_padding: u32,
}

impl Drop for Output {
    fn drop(&mut self) {
        // SAFETY: created by `open_output`, released exactly once here.
        unsafe {
            self.client.Stop().ok();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Open and start a 48 kHz stereo render stream on `device_id`, or on the
/// default render endpoint when `None`.
unsafe fn open_output(
    enumerator: &IMMDeviceEnumerator,
    device_id: Option<&str>,
    legacy: bool,
    stats: &PlaybackStats,
) -> Result<Output> {
    let device = match device_id {
        Some(id) => {
            let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
            enumerator
                .GetDevice(PCWSTR(wide.as_ptr()))
                .with_context(|| format!("open audio endpoint {id}"))?
        }
        None => enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?,
    };
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

    // The requested size is only a ceiling on what we may queue; the loop
    // below decides how much actually sits there.
    let format = render_format();
    let buffer_hns = if legacy { 10_000_000 } else { 1_000_000 };
    client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            buffer_hns,
            0,
            &format.Format,
            None,
        )
        .context("IAudioClient::Initialize (48 kHz stereo float)")?;

    let event = CreateEventW(None, false, false, windows::core::PCWSTR::null())?;
    if let Err(e) = client.SetEventHandle(event) {
        let _ = CloseHandle(event);
        return Err(e.into());
    }
    let render: IAudioRenderClient = match client.GetService() {
        Ok(r) => r,
        Err(e) => {
            let _ = CloseHandle(event);
            return Err(e.into());
        }
    };
    let buffer_frames = client.GetBufferSize().unwrap_or(0);
    let engine_us = (client.GetStreamLatency().unwrap_or(0) / 10) as u32;
    let max_padding = if legacy { buffer_frames } else { MAX_PADDING_MS * FRAMES_PER_MS as u32 }
        .min(buffer_frames);
    stats.render_buffer_frames.store(buffer_frames, Ordering::Relaxed);
    stats.stream_latency_us.store(engine_us, Ordering::Relaxed);
    tracing::info!(
        device = device_id.unwrap_or("default"),
        buffer_ms = buffer_frames as f64 / FRAMES_PER_MS as f64,
        max_padding_ms = max_padding as f64 / FRAMES_PER_MS as f64,
        engine_ms = engine_us as f64 / 1e3,
        legacy,
        "audio playback up"
    );
    let out = Output { client, render, event, buffer_frames, max_padding };
    out.client.Start()?;
    Ok(out)
}

/// Why [`play`] returned without an error.
enum PlayEnd {
    Stopped,
    Reopen,
}

/// Mixer state that outlives any one render stream, so a reopen onto a new
/// endpoint picks up exactly where the last one left off.
struct Mix {
    streams: Vec<DecodedStream>,
    tracks: Vec<crate::mixer::Track>,
    gains: Vec<f32>,
    steps: Vec<f32>,
    targets: Vec<f32>,
    pcm: Vec<f32>,
}

unsafe fn play(
    out: &Output,
    mix: &mut Mix,
    stop: &Receiver<()>,
    device: &DeviceSlot,
    opened: Opened,
    stats: &PlaybackStats,
    faders: &crate::mixer::Faders,
) -> Result<PlayEnd> {
    let n = mix.streams.len();
    loop {
        match stop.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => return Ok(PlayEnd::Stopped),
            Err(TryRecvError::Empty) => {}
        }
        if devices::should_reopen(
            &opened,
            device.generation(),
            devices::default_generation(Flow::Render),
        ) {
            return Ok(PlayEnd::Reopen);
        }
        let woke = WaitForSingleObject(out.event, 20) == WAIT_OBJECT_0;
        stats.channel_packets.store(mix.streams[0].rx.len() as u32, Ordering::Relaxed);
        for s in &mut mix.streams {
            s.drain(&mut mix.pcm);
        }
        if !woke {
            continue;
        }
        let padding = out.client.GetCurrentPadding()?;
        let room = out.max_padding.saturating_sub(padding).min(out.buffer_frames - padding);
        if room > 0 {
            let ptr = out.render.GetBuffer(room)?;
            let buf = std::slice::from_raw_parts_mut(ptr as *mut f32, room as usize * 2);
            for i in 0..n {
                mix.targets[i] = faders.get(mix.tracks[i]).target();
                mix.steps[i] = (mix.targets[i] - mix.gains[i]) / room as f32;
            }
            let mut peak = 0f32;
            for frame in buf.chunks_exact_mut(2) {
                let (mut l, mut r) = (0f32, 0f32);
                for i in 0..n {
                    mix.gains[i] += mix.steps[i];
                    let (pl, pr) = mix.streams[i].queue.pop_pair();
                    l += pl * mix.gains[i];
                    r += pr * mix.gains[i];
                }
                frame[0] = l.clamp(-1.0, 1.0);
                frame[1] = r.clamp(-1.0, 1.0);
                peak = peak.max(frame[0].abs()).max(frame[1].abs());
            }
            // Land exactly, so float drift over many buffers cannot creep.
            mix.gains.copy_from_slice(&mix.targets);
            stats.peak_milli.store((peak * 1e3) as u32, Ordering::Relaxed);
            out.render.ReleaseBuffer(room, 0)?;
        }

        let q = &mix.streams[0].queue;
        stats.render_padding_frames.store(padding + room, Ordering::Relaxed);
        stats.queue_frames.store(q.frames() as u32, Ordering::Relaxed);
        stats.underruns.store(q.underruns, Ordering::Relaxed);
        stats.slew_skipped.store(q.skipped, Ordering::Relaxed);
        stats.slew_repeated.store(q.repeated, Ordering::Relaxed);
        stats.dropped_frames.store(q.dropped, Ordering::Relaxed);
    }
}

/// Wait up to `ms` for a stop; true when stopped.
fn stopped_within(stop: &Receiver<()>, ms: u64) -> bool {
    !matches!(
        stop.recv_timeout(std::time::Duration::from_millis(ms)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
}

unsafe fn render_loop(
    inputs: Vec<Stream>,
    stop: Receiver<()>,
    device: Arc<DeviceSlot>,
    stats: Arc<PlaybackStats>,
    faders: Arc<crate::mixer::Faders>,
) -> Result<()> {
    let legacy = std::env::var_os("RELAY_AUDIO_LEGACY").is_some();
    devices::watch_defaults();
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

    // One per incoming track. A sender with no mic or no rest track simply
    // never feeds that stream, which then contributes silence and costs one
    // add. Order matches `gains` and `tracks`.
    let mut streams = Vec::with_capacity(inputs.len());
    let mut tracks = Vec::with_capacity(inputs.len());
    for (rx, track) in inputs {
        streams.push(DecodedStream::new(rx, legacy)?);
        tracks.push(track);
    }
    let n = streams.len();
    // The gain each stream ended the last buffer on; a fader change ramps
    // from here across the next buffer (S37), so a mute never clicks.
    let mut mix = Mix {
        streams,
        tracks,
        gains: vec![1.0f32; n],
        steps: vec![0f32; n],
        targets: vec![1.0f32; n],
        // Room for the longest Opus frame (120 ms), whatever the sender chose.
        pcm: vec![0f32; 5760 * 2],
    };

    // The first open fails the call, as it always has; after that a lost or
    // changed endpoint is reopened and the share carries on (S40).
    let mut first = true;
    loop {
        let (choice, slot_gen) = device.get();
        let mut opened = Opened {
            slot_gen,
            follows_default: choice.is_none(),
            default_gen: devices::default_generation(Flow::Render),
        };
        let out = match open_output(&enumerator, choice.as_deref(), legacy, &stats) {
            Ok(o) => Ok(o),
            Err(e) if choice.is_some() => {
                tracing::warn!(
                    error = %e, device = ?choice,
                    "the chosen output would not open; playing on the System default"
                );
                opened.follows_default = true;
                open_output(&enumerator, None, legacy, &stats)
            }
            Err(e) => Err(e),
        };
        let out = match out {
            Ok(o) => o,
            Err(e) if first => return Err(e),
            Err(e) => {
                tracing::warn!(error = %e, "audio output would not reopen; retrying");
                if stopped_within(&stop, 500) {
                    break;
                }
                continue;
            }
        };
        if !first {
            tracing::info!(device = ?choice, "audio playback reopened");
        }
        first = false;
        match play(&out, &mut mix, &stop, &device, opened, &stats, &faders) {
            Ok(PlayEnd::Stopped) => break,
            Ok(PlayEnd::Reopen) => {
                tracing::info!("audio output: device choice or OS default changed; reopening")
            }
            Err(e) => {
                tracing::warn!(error = %e, "audio output lost; reopening");
                drop(out);
                if stopped_within(&stop, 500) {
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_sums_and_clamps() {
        assert_eq!(mix_sum(&[0.0, 0.0]), 0.0);
        assert_eq!(mix_sum(&[0.25, 0.5]), 0.75);
        assert_eq!(mix_sum(&[-0.25, 0.25]), 0.0);
        // Two loud sources together must not wrap or exceed full scale.
        assert_eq!(mix_sum(&[0.9, 0.8]), 1.0);
        assert_eq!(mix_sum(&[-0.9, -0.8]), -1.0);
    }

    #[test]
    fn one_track_alone_is_unchanged_by_the_mixer() {
        for v in [-1.0f32, -0.3, 0.0, 0.3, 1.0] {
            assert_eq!(mix_sum(&[v, 0.0]), v, "a silent second track must not colour the first");
        }
    }

    fn packet(value: f32) -> Vec<f32> {
        vec![value; 480 * 2]
    }

    #[test]
    fn a_track_is_silent_until_primed_then_plays_in_order() {
        let mut q = JitterQueue::default();
        q.push(&packet(0.1));
        q.push(&packet(0.2));
        q.push(&packet(0.3));
        assert_eq!(q.pop_pair(), (0.0, 0.0), "30 ms is short of the prime depth");
        assert_eq!(q.frames(), 1440, "nothing is consumed while priming");
        q.push(&packet(0.4));
        assert_eq!(q.pop_pair(), (0.1, 0.1));
        assert_eq!(q.underruns, 0);
    }

    #[test]
    fn running_dry_counts_once_and_primes_again() {
        let mut q = JitterQueue::default();
        for _ in 0..4 {
            q.push(&packet(0.5));
        }
        for _ in 0..1920 {
            assert_eq!(q.pop_pair(), (0.5, 0.5));
        }
        assert_eq!(q.pop_pair(), (0.0, 0.0));
        assert_eq!(q.pop_pair(), (0.0, 0.0));
        assert_eq!(q.underruns, 1, "one dry spell is one underrun, however long");
        q.push(&packet(0.5));
        assert_eq!(q.pop_pair(), (0.0, 0.0), "must prime again, not dribble");
    }

    /// Feed 10 ms packets at `feed_ppm` off nominal against a reader taking
    /// 1 ms at a time for `secs`; returns the queue and its depth extremes.
    fn simulate(feed_ppm: f64, secs: usize) -> (JitterQueue, usize, usize) {
        let mut q = JitterQueue::default();
        let interval_us = 10_000.0 / (1.0 + feed_ppm / 1e6);
        let mut next_feed_us = 0.0f64;
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        for ms in 0..secs * 1000 {
            while next_feed_us <= (ms * 1000) as f64 {
                q.push(&packet(0.25));
                next_feed_us += interval_us;
            }
            for _ in 0..FRAMES_PER_MS {
                q.pop_pair();
            }
            if ms > 3000 {
                lo = lo.min(q.frames());
                hi = hi.max(q.frames());
            }
        }
        (q, lo, hi)
    }

    #[test]
    fn a_fast_sender_is_followed_by_skipping_not_by_growing() {
        // 200 ppm for an hour is 720 ms of surplus if nothing follows it.
        let (q, _, hi) = simulate(200.0, 3600);
        assert_eq!(q.underruns, 0);
        assert_eq!(q.dropped, 0, "slewing should keep up without the hard drop");
        assert!(q.skipped > 0);
        assert!(hi <= 60 * FRAMES_PER_MS, "depth peaked at {} ms", hi / FRAMES_PER_MS);
    }

    #[test]
    fn a_slow_sender_is_followed_by_repeating_not_by_running_dry() {
        let (q, _, _) = simulate(-200.0, 3600);
        assert_eq!(q.underruns, 0, "an hour at -200 ppm must not click");
        assert!(q.repeated > 0);
    }

    #[test]
    fn matched_clocks_are_left_alone() {
        let (q, lo, hi) = simulate(0.0, 600);
        assert_eq!((q.underruns, q.skipped, q.repeated, q.dropped), (0, 0, 0, 0));
        assert!(lo >= SLEW_LOW_FRAMES && hi <= PRIME_FRAMES + 480, "{lo}..{hi}");
    }

    #[test]
    fn a_burst_after_a_stall_is_dropped_at_once() {
        let mut q = JitterQueue::default();
        for _ in 0..50 {
            q.push(&packet(0.1)); // half a second lands together
        }
        for _ in 0..100 {
            q.push(&packet(0.1));
            for _ in 0..480 {
                q.pop_pair();
            }
        }
        assert!(q.dropped > 0);
        assert!(q.frames() <= PRIME_FRAMES + 480, "still {} ms deep", q.frames() / 48);
    }

    #[test]
    fn legacy_passthrough_plays_without_priming() {
        let mut q = JitterQueue::passthrough();
        q.push(&[0.7, 0.7]);
        assert_eq!(q.pop_pair(), (0.7, 0.7));
        assert_eq!(q.pop_pair(), (0.0, 0.0));
        assert_eq!(q.underruns, 0);
    }
}
