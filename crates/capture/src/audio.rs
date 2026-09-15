//! Audio capture for the share: WASAPI loopback of the default render
//! endpoint (everything you hear), process loopback (one game only), or the
//! default microphone. All paths deliver f32 interleaved stereo at the
//! endpoint rate; `OpusStream` packs 10 ms frames and encodes Opus.
//!
//! Shared-mode WASAPI only — nothing here touches the endpoint's
//! configuration, volume, or default-device selection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::debug;
use windows::core::{implement, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, ActivateAudioInterfaceAsync,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::time::qpc_now_100ns;

/// What to capture.
#[derive(Debug, Clone)]
pub enum AudioSource {
    /// Default render endpoint loopback: everything the user hears.
    Desktop,
    /// One process tree only (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`).
    Process { pid: u32 },
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
        let (tx, rx) = sync_channel::<AudioBlock>(32);
        let stop = Arc::new(AtomicBool::new(false));
        let (fmt_tx, fmt_rx) = std::sync::mpsc::channel::<Result<(u32, u16)>>();
        let stop2 = stop.clone();
        let join =
            std::thread::Builder::new().name("relay-audio-capture".into()).spawn(move || {
                if let Err(e) = capture_thread(source, tx, stop2, fmt_tx.clone()) {
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

fn activate_client(source: &AudioSource) -> Result<(IAudioClient, u32)> {
    // SAFETY: COM calls on this thread (already CoInitialize'd by the caller).
    unsafe {
        match source {
            AudioSource::Desktop | AudioSource::Microphone => {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let flow = if matches!(source, AudioSource::Desktop) { eRender } else { eCapture };
                let device = enumerator.GetDefaultAudioEndpoint(flow, eConsole)?;
                let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                Ok((client, 0))
            }
            AudioSource::Process { pid } => {
                let params = AUDIOCLIENT_ACTIVATION_PARAMS {
                    ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                    Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                        ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                            TargetProcessId: *pid,
                            ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
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

fn capture_thread(
    source: AudioSource,
    tx: SyncSender<AudioBlock>,
    stop: Arc<AtomicBool>,
    fmt_tx: std::sync::mpsc::Sender<Result<(u32, u16)>>,
) -> Result<()> {
    // SAFETY: standard WASAPI capture loop; COM is initialised for this thread.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
        let result = (|| -> Result<()> {
            let (client, is_process) = activate_client(&source)?;

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
            client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 200_000, 0, fmt, None)
                .context("IAudioClient::Initialize")?;
            if !format_ptr.is_null() {
                CoTaskMemFree(Some(format_ptr as *const _));
            }
            let event = CreateEventW(None, false, false, PCWSTR::null())?;
            client.SetEventHandle(event)?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            fmt_tx.send(Ok((rate, channels))).ok();

            while !stop.load(Ordering::Relaxed) {
                if WaitForSingleObject(event, 100) != WAIT_OBJECT_0 {
                    continue;
                }
                loop {
                    let next = capture.GetNextPacketSize().unwrap_or(0);
                    if next == 0 {
                        break;
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut dflags = 0u32;
                    capture.GetBuffer(&mut data, &mut frames, &mut dflags, None, None)?;
                    let n = frames as usize * channels as usize;
                    let samples = if dflags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                        vec![0.0f32; n]
                    } else {
                        std::slice::from_raw_parts(data as *const f32, n).to_vec()
                    };
                    capture.ReleaseBuffer(frames)?;
                    let block = AudioBlock {
                        samples,
                        channels,
                        sample_rate: rate,
                        qpc_100ns: qpc_now_100ns(),
                    };
                    if tx.try_send(block).is_err() {
                        debug!("audio consumer busy; block dropped");
                    }
                }
            }
            client.Stop().ok();
            let _ = CloseHandle(event);
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
    pending: Vec<f32>,
    frame_samples: usize,
    /// Set when the endpoint is not already 48 kHz stereo; surfaced once in the
    /// log so an audio-quality question has the conversion visible.
    pub conversion: Option<String>,
    /// Peak of the last encoded frame, 0..=1 (for the instrument strip).
    pub peak: f32,
}

pub struct OpusPacket {
    pub data: Vec<u8>,
    pub qpc_100ns: i64,
    pub duration: Duration,
}

impl OpusStream {
    pub fn new(source: AudioSource, bitrate: i32) -> Result<Self> {
        let capture = AudioCapture::start(source)?;
        let (rate, channels) = (capture.sample_rate, capture.channels);
        crate::resample::check_format(rate, channels)?;
        let conversion = crate::resample::conversion_note(rate, channels);
        if let Some(note) = conversion.as_deref() {
            tracing::info!("{note}");
        }
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)?;
        encoder.set_bitrate(opus::Bitrate::Bits(bitrate))?;
        let frame_samples = 480 * 2; // 10 ms stereo
        Ok(Self {
            capture,
            encoder,
            convert: crate::resample::ToOpus48::new(rate),
            src_channels: channels,
            pending: Vec::with_capacity(frame_samples * 4),
            frame_samples,
            conversion,
            peak: 0.0,
        })
    }

    /// The endpoint's own sample rate, before conversion.
    pub fn endpoint_rate(&self) -> u32 {
        self.capture.sample_rate
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
                    let stereo = crate::resample::fold_to_stereo(&block.samples, self.src_channels);
                    self.pending.extend_from_slice(&self.convert.push(&stereo));
                }
                None => return Ok(None),
            }
        }
        let frame: Vec<f32> = self.pending.drain(..self.frame_samples).collect();
        self.peak = frame.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        let data = self.encoder.encode_vec_float(&frame, 1500)?;
        Ok(Some(OpusPacket {
            data,
            qpc_100ns: qpc_now_100ns(),
            duration: Duration::from_millis(10),
        }))
    }
}
