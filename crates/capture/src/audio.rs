//! Audio capture for the share: WASAPI loopback of the default render
//! endpoint (everything you hear), process loopback (one game only), or the
//! default microphone. All paths deliver f32 interleaved stereo at the
//! endpoint rate; `OpusStream` packs 10 ms frames and encodes Opus.
//!
//! Shared-mode WASAPI only — nothing here touches the endpoint's
//! configuration, volume, or default-device selection.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};
use windows::core::{implement, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, ActivateAudioInterfaceAsync,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::devices::{self, DeviceSlot, Flow, Opened};
use crate::time::qpc_now_100ns;

/// What to capture.
#[derive(Debug, Clone)]
pub enum AudioSource {
    /// Default render endpoint loopback: everything the user hears.
    Desktop,
    /// One process tree only (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`).
    Process { pid: u32 },
    /// Everything on the endpoint *except* one process tree (S37): the same
    /// activation with the exclude flag, so "the rest of the PC" is one
    /// capture the audio engine has already mixed, not a session walk of ours.
    Rest { pid: u32 },
    /// Default capture endpoint (microphone).
    Microphone,
}

/// A block of f32 interleaved samples with its QPC arrival time.
pub struct AudioBlock {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
    pub qpc_100ns: i64,
}

pub struct AudioCapture {
    rx: Receiver<AudioBlock>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioCapture {
    pub fn start(source: AudioSource) -> Result<Self> {
        Self::start_on(source, None)
    }

    /// Start on the endpoint `slot` names (S40): the System default when the
    /// slot is empty or absent. Endpoint sources (desktop loopback, the mic)
    /// reopen in place when the slot changes, or when they follow the default
    /// and the OS default moves; the consumer sees the blocks carry on, with
    /// the new endpoint's rate and channel count on each block.
    pub fn start_on(source: AudioSource, slot: Option<Arc<DeviceSlot>>) -> Result<Self> {
        let (tx, rx) = sync_channel::<AudioBlock>(32);
        let stop = Arc::new(AtomicBool::new(false));
        let (fmt_tx, fmt_rx) = std::sync::mpsc::channel::<Result<(u32, u16)>>();
        let stop2 = stop.clone();
        let join =
            std::thread::Builder::new().name("relay-audio-capture".into()).spawn(move || {
                if let Err(e) = capture_thread(source, slot, tx, stop2, fmt_tx.clone()) {
                    let _ = fmt_tx.send(Err(e));
                }
            })?;
        let (sample_rate, channels) =
            fmt_rx.recv().context("audio capture thread died before reporting a format")??;
        Ok(Self { rx, stop, join: Some(join), sample_rate, channels })
    }

    pub fn next(&self, timeout: Duration) -> Option<AudioBlock> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Completion handler for `ActivateAudioInterfaceAsync`: just signals an event.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivateHandler {
    done: HANDLE,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateHandler_Impl {
    fn ActivateCompleted(
        &self,
        _op: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // SAFETY: the event handle outlives the activation call.
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(self.done);
        }
        Ok(())
    }
}

fn activate_client(source: &AudioSource, device_id: Option<&str>) -> Result<(IAudioClient, u32)> {
    // SAFETY: COM calls on this thread (already CoInitialize'd by the caller).
    unsafe {
        match source {
            AudioSource::Desktop | AudioSource::Microphone => {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let flow = if matches!(source, AudioSource::Desktop) { eRender } else { eCapture };
                let device = match device_id {
                    Some(id) => {
                        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                        enumerator
                            .GetDevice(PCWSTR(wide.as_ptr()))
                            .with_context(|| format!("open audio endpoint {id}"))?
                    }
                    None => enumerator.GetDefaultAudioEndpoint(flow, eConsole)?,
                };
                let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                Ok((client, 0))
            }
            AudioSource::Process { pid } | AudioSource::Rest { pid } => {
                let mode = if matches!(source, AudioSource::Rest { .. }) {
                    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                } else {
                    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                };
                let params = AUDIOCLIENT_ACTIVATION_PARAMS {
                    ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                    Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                        ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                            TargetProcessId: *pid,
                            ProcessLoopbackMode: mode,
                        },
                    },
                };
                // VT_BLOB pointing at stack memory: PROPVARIANT's Drop would
                // CoTaskMemFree the pointer, so keep it in ManuallyDrop.
                let mut blob = std::mem::ManuallyDrop::new(
                    windows::Win32::System::Com::StructuredStorage::PROPVARIANT::default(),
                );
                let b = &mut blob.Anonymous.Anonymous;
                b.vt = windows::Win32::System::Variant::VT_BLOB;
                b.Anonymous.blob.cbSize =
                    std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
                b.Anonymous.blob.pBlobData = &params as *const _ as *mut u8;

                let done = CreateEventW(None, false, false, PCWSTR::null())?;
                let handler: IActivateAudioInterfaceCompletionHandler =
                    ActivateHandler { done }.into();
                let op = ActivateAudioInterfaceAsync(
                    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                    &IAudioClient::IID,
                    Some(&*blob),
                    &handler,
                )?;
                if WaitForSingleObject(done, 5000) != WAIT_OBJECT_0 {
                    let _ = CloseHandle(done);
                    bail!("process-loopback activation timed out");
                }
                let _ = CloseHandle(done);
                let mut hr = windows::core::HRESULT(0);
                let mut unk: Option<windows::core::IUnknown> = None;
                op.GetActivateResult(&mut hr, &mut unk)?;
                hr.ok().context("process-loopback activation failed")?;
                let client: IAudioClient = unk.context("no interface")?.cast()?;
                Ok((client, 1))
            }
        }
    }
}

/// One opened WASAPI capture stream. Dropping it stops the client.
struct OpenStream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    rate: u32,
    channels: u16,
}

impl Drop for OpenStream {
    fn drop(&mut self) {
        // SAFETY: the client and event were created by `open_stream` and are
        // released exactly once here.
        unsafe {
            self.client.Stop().ok();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Activate, initialise and start a capture stream on `device_id` (the
/// default endpoint for the source's direction when `None`).
///
/// # Safety
/// COM must be initialised on the calling thread.
unsafe fn open_stream(source: &AudioSource, device_id: Option<&str>) -> Result<OpenStream> {
    let (client, is_process) = activate_client(source, device_id)?;

    // Process loopback has no mix format; the endpoint paths use theirs.
    let (format_ptr, rate, channels): (*mut WAVEFORMATEX, u32, u16) = if is_process == 1 {
        (std::ptr::null_mut(), 48_000, 2)
    } else {
        let p = client.GetMixFormat()?;
        ((p), (*p).nSamplesPerSec, (*p).nChannels)
    };
    if !format_ptr.is_null() {
        if let Err(e) = check_float32(format_ptr) {
            CoTaskMemFree(Some(format_ptr as *const _));
            return Err(e);
        }
    }
    let own_format = WAVEFORMATEX {
        wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * channels as u32 * 4,
        nBlockAlign: channels * 4,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    let fmt: *const WAVEFORMATEX =
        if format_ptr.is_null() { &own_format } else { format_ptr as *const _ };

    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
    if matches!(source, AudioSource::Desktop) || is_process == 1 {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    let init = client
        .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 200_000, 0, fmt, None)
        .context("IAudioClient::Initialize");
    if !format_ptr.is_null() {
        CoTaskMemFree(Some(format_ptr as *const _));
    }
    init?;
    let event = CreateEventW(None, false, false, PCWSTR::null())?;
    if let Err(e) = client.SetEventHandle(event) {
        let _ = CloseHandle(event);
        return Err(e.into());
    }
    let capture: IAudioCaptureClient = match client.GetService() {
        Ok(c) => c,
        Err(e) => {
            let _ = CloseHandle(event);
            return Err(e.into());
        }
    };
    let s = OpenStream { client, capture, event, rate, channels };
    s.client.Start()?;
    Ok(s)
}

/// Why [`pump`] returned without an error.
enum PumpEnd {
    Stopped,
    Reopen,
}

/// Move packets from `s` to `tx` until stopped, until the device choice (or
/// the default it follows) changes, or until WASAPI fails.
///
/// # Safety
/// COM must be initialised on the calling thread.
unsafe fn pump(
    s: &OpenStream,
    tx: &SyncSender<AudioBlock>,
    stop: &AtomicBool,
    watch: Option<(&DeviceSlot, Opened, Flow)>,
) -> Result<PumpEnd> {
    while !stop.load(Ordering::Relaxed) {
        if let Some((slot, opened, flow)) = watch {
            if devices::should_reopen(&opened, slot.generation(), devices::default_generation(flow))
            {
                return Ok(PumpEnd::Reopen);
            }
        }
        if WaitForSingleObject(s.event, 100) != WAIT_OBJECT_0 {
            continue;
        }
        loop {
            let next = s.capture.GetNextPacketSize()?;
            if next == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut dflags = 0u32;
            s.capture.GetBuffer(&mut data, &mut frames, &mut dflags, None, None)?;
            let n = frames as usize * s.channels as usize;
            let samples = if dflags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                vec![0.0f32; n]
            } else {
                std::slice::from_raw_parts(data as *const f32, n).to_vec()
            };
            s.capture.ReleaseBuffer(frames)?;
            let block = AudioBlock {
                samples,
                channels: s.channels,
                sample_rate: s.rate,
                qpc_100ns: qpc_now_100ns(),
            };
            if tx.try_send(block).is_err() {
                debug!("audio consumer busy; block dropped");
            }
        }
    }
    Ok(PumpEnd::Stopped)
}

/// Sleep up to `ms`, waking early on stop.
fn nap(stop: &AtomicBool, ms: u64) {
    let mut left = ms;
    while left > 0 && !stop.load(Ordering::Relaxed) {
        let step = left.min(50);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
}

fn capture_thread(
    source: AudioSource,
    slot: Option<Arc<DeviceSlot>>,
    tx: SyncSender<AudioBlock>,
    stop: Arc<AtomicBool>,
    fmt_tx: std::sync::mpsc::Sender<Result<(u32, u16)>>,
) -> Result<()> {
    // SAFETY: standard WASAPI capture loop; COM is initialised for this thread
    // and every stream is dropped before it is uninitialised.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
        let result = (|| -> Result<()> {
            // Endpoint sources follow a device choice and the OS default;
            // process loopback is not tied to an endpoint and does neither.
            let endpoint = matches!(source, AudioSource::Desktop | AudioSource::Microphone);
            let flow =
                if matches!(source, AudioSource::Desktop) { Flow::Render } else { Flow::Capture };
            let slot = slot.unwrap_or_else(|| DeviceSlot::shared(None));
            if endpoint {
                devices::watch_defaults();
            }
            // Present until the first open: a failure then is the caller's
            // error, as it always was. After that, the share carries on and
            // the track retries.
            let mut fmt_tx = Some(fmt_tx);
            while !stop.load(Ordering::Relaxed) {
                let (choice, slot_gen) = slot.get();
                let choice = if endpoint { choice } else { None };
                let mut opened = Opened {
                    slot_gen,
                    follows_default: choice.is_none(),
                    default_gen: devices::default_generation(flow),
                };
                let opened_stream = match open_stream(&source, choice.as_deref()) {
                    Ok(s) => Ok(s),
                    Err(e) if choice.is_some() => {
                        warn!(
                            error = %e, device = ?choice,
                            "the chosen audio endpoint would not open; using the System default"
                        );
                        opened.follows_default = true;
                        open_stream(&source, None)
                    }
                    Err(e) => Err(e),
                };
                let s = match opened_stream {
                    Ok(s) => s,
                    Err(e) if fmt_tx.is_some() => return Err(e),
                    Err(e) => {
                        warn!(error = %e, ?source, "audio endpoint would not reopen; retrying");
                        nap(&stop, 500);
                        continue;
                    }
                };
                match fmt_tx.take() {
                    Some(t) => {
                        t.send(Ok((s.rate, s.channels))).ok();
                    }
                    None => info!(
                        ?source, device = ?choice, rate = s.rate, channels = s.channels,
                        "audio capture reopened"
                    ),
                }
                let watch = endpoint.then_some((&*slot, opened, flow));
                match pump(&s, &tx, &stop, watch) {
                    Ok(PumpEnd::Stopped) => break,
                    Ok(PumpEnd::Reopen) => info!(
                        ?source,
                        "audio capture: device choice or OS default changed; reopening"
                    ),
                    Err(e) if endpoint => {
                        warn!(error = %e, ?source, "audio endpoint lost; reopening");
                        drop(s);
                        nap(&stop, 500);
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })();
        CoUninitialize();
        result
    }
}

/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
const SUBTYPE_IEEE_FLOAT: windows::core::GUID =
    windows::core::GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// The capture loop reads the shared buffer as `f32`, so a mix format that is
/// not 32-bit float would be reinterpreted rather than converted. Shared-mode
/// WASAPI mixes in 32-bit float on every supported Windows version, so this is
/// a guard against an exotic endpoint rather than an expected path — but it
/// has to say what it found, not just fail.
///
/// # Safety
/// `fmt` must point at a valid `WAVEFORMATEX` (with `cbSize` bytes of extra
/// data following it when it is a `WAVEFORMATEXTENSIBLE`).
unsafe fn check_float32(fmt: *const WAVEFORMATEX) -> Result<()> {
    // WAVEFORMATEX is `#[repr(packed)]`, so every field is read out by value.
    let f = std::ptr::read_unaligned(fmt);
    let is_float = match f.wFormatTag {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_EXTENSIBLE if f.cbSize >= 22 => {
            let ext = fmt as *const WAVEFORMATEXTENSIBLE;
            std::ptr::addr_of!((*ext).SubFormat).read_unaligned() == SUBTYPE_IEEE_FLOAT
        }
        _ => false,
    };
    if is_float && f.wBitsPerSample == 32 {
        return Ok(());
    }
    let (bits, tag) = (f.wBitsPerSample, f.wFormatTag);
    bail!(
        "this audio endpoint mixes in {}-bit format 0x{:04X}, not the 32-bit float \
         (0x0003) that shared-mode WASAPI is expected to use, so Relay cannot read its \
         buffer. Open Sound settings > the device > Properties > Advanced, pick a standard \
         format such as \"2 channel, 24 bit, 48000 Hz\", and start the share again — or \
         share without audio",
        bits,
        tag
    )
}

/// 10 ms Opus frames from an [`AudioCapture`]. Opus on the share path is fixed
/// at 48 kHz stereo; whatever the endpoint runs at is folded and converted by
/// [`crate::resample`] on the way in, so a 44.1 kHz interface or a mono headset
/// microphone shares like anything else.
pub struct OpusStream {
    capture: AudioCapture,
    encoder: opus::Encoder,
    convert: crate::resample::ToOpus48,
    src_channels: u16,
    /// The endpoint's rate right now; changes when a reopen lands elsewhere.
    src_rate: u32,
    pending: Vec<f32>,
    /// Arrival stamps for the samples in `pending`: `(qpc, samples left from
    /// that block)`, oldest first. Lets a packet report when its first sample
    /// reached us, which is the packetization latency the bench reports.
    stamps: VecDeque<(i64, usize)>,
    frame_samples: usize,
    /// Set when the endpoint is not already 48 kHz stereo; surfaced once in the
    /// log so an audio-quality question has the conversion visible.
    pub conversion: Option<String>,
    /// Peak of the last encoded frame, 0..=1 (for the instrument strip).
    /// Measured *after* the fader, so a muted track meters as silence —
    /// what is sent, not what was captured.
    pub peak: f32,
    /// The fader this track reads, if the share has a mixer (S37).
    faders: Option<(Arc<crate::mixer::Faders>, crate::mixer::Track)>,
    /// Gain the previous frame ended on, so a change ramps from there.
    gain: f32,
    /// NDI output's audio tee on the sender (S51): each 10 ms frame as it
    /// is encoded, after the fader. Off costs one atomic load.
    ndi: Option<crate::ndi::AudioProducer>,
}

pub struct OpusPacket {
    pub data: Vec<u8>,
    /// QPC when the packet finished encoding.
    pub qpc_100ns: i64,
    /// QPC when WASAPI delivered the block this packet starts in.
    pub captured_qpc_100ns: i64,
    pub duration: Duration,
}

/// Encoder tuning for one track.
///
/// The program mix is music-grade and keeps exactly the settings it has always
/// had. A microphone is speech: it does not need 160 kb/s stereo at libopus's
/// default complexity, and since the second encoder's CPU is the whole cost of
/// the mic track on the share hot path, it gets a cheaper one.
#[derive(Debug, Clone, Copy)]
pub struct OpusProfile {
    pub bitrate_bps: i32,
    pub application: opus::Application,
    /// `None` leaves libopus's default, which is what the program mix has
    /// always encoded at — this keeps the single-track path unchanged.
    pub complexity: Option<i32>,
}

impl OpusProfile {
    /// The desktop mix or a game: music-grade, unchanged since M4.
    pub fn program() -> Self {
        Self { bitrate_bps: 160_000, application: opus::Application::Audio, complexity: None }
    }

    /// A microphone: speech at a sane rate and a cheaper search.
    pub fn voice() -> Self {
        Self { bitrate_bps: 64_000, application: opus::Application::Voip, complexity: Some(5) }
    }
}

/// Spend `frame_samples` worth of capture stamps and return the capture time
/// of the oldest sample in that frame — what the receiver rebases A/V sync on.
///
/// Extracted so the arithmetic is testable without WASAPI, because the way it
/// breaks is silent: the counts must be in the same units as the buffer they
/// describe (post-conversion interleaved samples). Count source samples
/// instead and the deque drains at the wrong rate, so the reported capture
/// time slides further behind real time the longer the share runs — and only
/// on endpoints that are not already 48 kHz stereo.
fn take_capture_stamp(stamps: &mut VecDeque<(i64, usize)>, frame_samples: usize) -> i64 {
    let captured = stamps.front().map(|(q, _)| *q).unwrap_or(0);
    let mut left = frame_samples;
    while left > 0 {
        let Some((_, n)) = stamps.front_mut() else { break };
        let take = left.min(*n);
        *n -= take;
        left -= take;
        if *n == 0 {
            stamps.pop_front();
        }
    }
    captured
}

impl OpusStream {
    pub fn new(source: AudioSource, profile: OpusProfile) -> Result<Self> {
        Self::new_on(source, profile, None)
    }

    /// As [`OpusStream::new`], on the endpoint `slot` names (S40).
    pub fn new_on(
        source: AudioSource,
        profile: OpusProfile,
        slot: Option<Arc<DeviceSlot>>,
    ) -> Result<Self> {
        let capture = AudioCapture::start_on(source, slot)?;
        let (rate, channels) = (capture.sample_rate, capture.channels);
        crate::resample::check_format(rate, channels)?;
        let conversion = crate::resample::conversion_note(rate, channels);
        if let Some(note) = conversion.as_deref() {
            tracing::info!("{note}");
        }
        let mut encoder = opus::Encoder::new(48_000, opus::Channels::Stereo, profile.application)?;
        encoder.set_bitrate(opus::Bitrate::Bits(profile.bitrate_bps))?;
        if let Some(c) = profile.complexity {
            encoder.set_complexity(c)?;
        }
        let frame_samples = 480 * 2; // 10 ms stereo
        Ok(Self {
            capture,
            encoder,
            convert: crate::resample::ToOpus48::new(rate),
            src_channels: channels,
            src_rate: rate,
            pending: Vec::with_capacity(frame_samples * 4),
            stamps: VecDeque::new(),
            frame_samples,
            conversion,
            peak: 0.0,
            faders: None,
            gain: 1.0,
            ndi: None,
        })
    }

    /// Read gain and mute for `track` from `faders` before every frame (S37).
    /// Without this the stream encodes at unity, as it always has.
    pub fn set_faders(&mut self, faders: Arc<crate::mixer::Faders>, track: crate::mixer::Track) {
        self.faders = Some((faders, track));
    }

    /// Also hand each frame to NDI output (S51), as the share hears it.
    pub fn set_ndi(&mut self, out: Arc<crate::ndi::NdiOutput>) {
        self.ndi = Some(crate::ndi::AudioProducer::new(out, 48_000));
    }

    /// The endpoint's own sample rate, before conversion.
    pub fn endpoint_rate(&self) -> u32 {
        self.src_rate
    }

    /// The endpoint's own channel count, before folding.
    pub fn endpoint_channels(&self) -> u16 {
        self.src_channels
    }

    /// Block up to `timeout` for the next encoded 10 ms packet.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<OpusPacket>> {
        let deadline = std::time::Instant::now() + timeout;
        while self.pending.len() < self.frame_samples {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            match self.capture.next(deadline - now) {
                Some(block) => {
                    // A reopen onto another endpoint (S40) can change the
                    // rate or channel count mid-stream: follow it, or drop
                    // the block if the new format is one we cannot convert.
                    if block.sample_rate != self.src_rate || block.channels != self.src_channels {
                        if let Err(e) =
                            crate::resample::check_format(block.sample_rate, block.channels)
                        {
                            debug!(error = %e, "block from the reopened endpoint dropped");
                            continue;
                        }
                        self.src_rate = block.sample_rate;
                        self.src_channels = block.channels;
                        self.convert = crate::resample::ToOpus48::new(block.sample_rate);
                        self.conversion =
                            crate::resample::conversion_note(block.sample_rate, block.channels);
                        info!(
                            rate = block.sample_rate,
                            channels = block.channels,
                            "audio track now on a different endpoint format"
                        );
                    }
                    // The stamp must count what lands in `pending`, not what
                    // WASAPI handed us: the resampler changes the sample count,
                    // and the drain below spends `pending` units. Counting
                    // source samples here would walk A/V sync off by the rate
                    // ratio — silently, and only on the endpoints that are not
                    // already 48 kHz stereo, which is exactly where nobody
                    // would be looking. The sub-millisecond skew from the
                    // converter's own history is far inside one 10 ms packet,
                    // so the block's capture time still describes its samples.
                    let stereo = crate::resample::fold_to_stereo(&block.samples, self.src_channels);
                    let converted = self.convert.push(&stereo);
                    if !converted.is_empty() {
                        self.stamps.push_back((block.qpc_100ns, converted.len()));
                        self.pending.extend_from_slice(&converted);
                    }
                }
                None => return Ok(None),
            }
        }
        let captured_qpc_100ns = take_capture_stamp(&mut self.stamps, self.frame_samples);
        let mut frame: Vec<f32> = self.pending.drain(..self.frame_samples).collect();
        // The fader, ramped across this frame (S37). A muted track keeps
        // sending: silence is cheaper to reason about on both ends than a
        // track that comes and goes.
        if let Some((faders, track)) = self.faders.as_ref() {
            self.gain =
                crate::mixer::apply_gain(&mut frame, self.gain, faders.get(*track).target());
        }
        self.peak = frame.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        if let Some(ndi) = self.ndi.as_mut() {
            ndi.push(&frame, 2, 0);
        }
        let data = self.encoder.encode_vec_float(&frame, 1500)?;
        Ok(Some(OpusPacket {
            data,
            qpc_100ns: qpc_now_100ns(),
            captured_qpc_100ns,
            duration: Duration::from_millis(10),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resample::{fold_to_stereo, ToOpus48};

    const FRAME_SAMPLES: usize = 480 * 2; // 10 ms stereo

    #[test]
    fn a_stamp_is_spent_in_step_with_the_buffer_it_describes() {
        let mut stamps: VecDeque<(i64, usize)> = VecDeque::new();
        stamps.push_back((100, 600));
        stamps.push_back((200, 600));
        stamps.push_back((300, 600));

        // 960 spends all of block 100 and 360 of block 200.
        assert_eq!(take_capture_stamp(&mut stamps, FRAME_SAMPLES), 100);
        // The oldest sample left is now in block 200, which has 240 to give;
        // the rest comes from block 300.
        assert_eq!(take_capture_stamp(&mut stamps, FRAME_SAMPLES), 200);
        // Everything is spent: 1800 stamped samples covered only one and
        // seven-eighths frames, so the deque is empty rather than negative.
        assert!(stamps.is_empty());
        // Running dry reports 0 rather than panicking or reusing a stale time.
        assert_eq!(take_capture_stamp(&mut stamps, FRAME_SAMPLES), 0);
    }

    /// The merge trap, made loud. S2 stamps each WASAPI block with the number
    /// of samples it contributed; S7 put a resampler in front of that buffer.
    /// If the stamp keeps counting *source* samples it describes a different
    /// quantity than the buffer it is spent against, and the receiver's A/V
    /// sync loses its reference — silently, and only on endpoints that are not
    /// already 48 kHz stereo, which is exactly where nobody is looking.
    ///
    /// This drives the real converter through ten seconds of a 44.1 kHz mono
    /// microphone and asserts both halves: counting converted samples gives
    /// every packet a real capture time, and counting source samples does not.
    #[test]
    fn stamps_counted_in_source_samples_lose_the_capture_time() {
        let (rate, channels) = (44_100u32, 1u16);
        let mut convert = ToOpus48::new(rate);
        let mut pending = 0usize;
        let mut correct: VecDeque<(i64, usize)> = VecDeque::new();
        let mut wrong: VecDeque<(i64, usize)> = VecDeque::new();
        let (mut source_total, mut converted_total) = (0usize, 0usize);
        let (mut packets, mut disagreed, mut skew_total) = (0usize, 0usize, 0i64);

        for block in 1..=1000i64 {
            let samples = vec![0.0f32; rate as usize / 100]; // 10 ms mono
            let stereo = fold_to_stereo(&samples, channels);
            let out = convert.push(&stereo);
            source_total += samples.len();
            converted_total += out.len();
            if !out.is_empty() {
                correct.push_back((block, out.len()));
                wrong.push_back((block, samples.len()));
                pending += out.len();
            }
            while pending >= FRAME_SAMPLES {
                pending -= FRAME_SAMPLES;
                packets += 1;
                let good = take_capture_stamp(&mut correct, FRAME_SAMPLES);
                let bad = take_capture_stamp(&mut wrong, FRAME_SAMPLES);
                if good != bad {
                    disagreed += 1;
                }
                // Block numbers stand in for capture time: one per 10 ms.
                skew_total += bad - good;
            }
        }

        // The units really do differ, or this test proves nothing: one mono
        // sample becomes two stereo samples, then the rate ratio scales it
        // again — about 2.18x.
        let ratio = converted_total as f64 / source_total as f64;
        assert!((2.1..2.3).contains(&ratio), "conversion ratio {ratio}");
        assert!(packets > 900, "only {packets} packets from ten seconds");

        // Counted in converted samples the stamps stay in step with the
        // buffer: what is left in the deque is exactly the residue still
        // sitting in `pending`, after ten seconds.
        let left: usize = correct.iter().map(|(_, n)| *n).sum();
        assert_eq!(left, pending, "stamps drifted out of step with the buffer");

        // Counted in source samples the deque is fed 441 per block but spent
        // 960 per packet, so it is permanently starved: instead of naming the
        // block the oldest queued sample came from, it names whichever block
        // was pushed most recently. The reported capture time is biased new,
        // which shows up downstream as under-reported latency and audio that
        // will not line up with video.
        assert!(
            disagreed > packets * 9 / 10,
            "source-counted stamps should disagree on nearly every packet; {disagreed} of {packets}"
        );
        let mean_skew = skew_total as f64 / packets as f64;
        assert!(mean_skew > 0.5, "source-counted stamps should read newer; mean skew {mean_skew}");
    }
}
