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

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

pub fn run(
    opus_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    mic_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    stop: Receiver<()>,
    device_id: Option<String>,
) -> Result<()> {
    // SAFETY: COM for this thread; WASAPI render loop; balanced on return.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
        let result = render_loop(opus_rx, mic_rx, stop, device_id);
        CoUninitialize();
        result
    }
}

/// One decoded Opus stream waiting to be rendered. Each incoming track gets
/// one; the render loop pops a frame from every stream and sums.
struct DecodedStream {
    rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    decoder: opus::Decoder,
    queue: VecDeque<f32>,
}

impl DecodedStream {
    fn new(rx: tokio::sync::mpsc::Receiver<Vec<u8>>) -> Result<Self> {
        Ok(Self {
            rx,
            decoder: opus::Decoder::new(48_000, opus::Channels::Stereo)?,
            queue: VecDeque::new(),
        })
    }

    /// Decode everything waiting without blocking.
    fn drain(&mut self, pcm: &mut [f32]) {
        // `try_recv` ends the drain on empty or closed alike: either way
        // there is nothing more to decode this pass.
        while let Ok(pkt) = self.rx.try_recv() {
            if let Ok(n) = self.decoder.decode_float(&pkt, pcm, false) {
                self.queue.extend(&pcm[..n * 2]);
            }
        }
    }

    /// Next stereo pair, or silence when this stream has nothing to give.
    /// A stream that has not started yet, or has fallen behind, contributes
    /// silence rather than stalling the others.
    fn pop_pair(&mut self) -> (f32, f32) {
        match self.queue.pop_front() {
            Some(l) => (l, self.queue.pop_front().unwrap_or(l)),
            None => (0.0, 0.0),
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

unsafe fn render_loop(
    opus_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    mic_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    stop: Receiver<()>,
    device_id: Option<String>,
) -> Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = match &device_id {
        Some(id) => {
            let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
            enumerator
                .GetDevice(PCWSTR(wide.as_ptr()))
                .with_context(|| format!("open audio endpoint {id}"))?
        }
        None => enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?,
    };
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let mix = client.GetMixFormat()?;
    let rate = (*mix).nSamplesPerSec;
    let channels = (*mix).nChannels;

    // Render as float; the mix format is already float on modern Windows.
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        10_000_000,
        0,
        mix,
        None,
    )?;
    windows::Win32::System::Com::CoTaskMemFree(Some(mix as *const _ as *const _));

    let event = CreateEventW(None, false, false, windows::core::PCWSTR::null())?;
    client.SetEventHandle(event)?;
    let render: IAudioRenderClient = client.GetService()?;
    let buffer_frames = client.GetBufferSize()?;

    // One per incoming track. A sender with no mic simply never feeds the
    // second stream, which then contributes silence and costs one add.
    let mut streams = [DecodedStream::new(opus_rx)?, DecodedStream::new(mic_rx)?];
    let mut pcm = vec![0f32; 480 * 2];

    client.Start()?;
    loop {
        if stop.try_recv().is_ok() {
            break;
        }
        for s in &mut streams {
            s.drain(&mut pcm);
        }

        if WaitForSingleObject(event, 20) != WAIT_OBJECT_0 {
            continue;
        }
        let padding = client.GetCurrentPadding()?;
        let free = buffer_frames - padding;
        if free == 0 {
            continue;
        }
        let ptr = render.GetBuffer(free)?;
        let out =
            std::slice::from_raw_parts_mut(ptr as *mut f32, free as usize * channels as usize);
        for frame in out.chunks_mut(channels as usize) {
            let pairs = streams.each_mut().map(|s| s.pop_pair());
            let l = mix_sum(&pairs.map(|p| p.0));
            let r = mix_sum(&pairs.map(|p| p.1));
            // Opus is stereo; map to the device channel count (dup or take 2).
            for (i, ch) in frame.iter_mut().enumerate() {
                *ch = if i == 0 {
                    l
                } else if i == 1 {
                    r
                } else {
                    0.0
                };
            }
        }
        render.ReleaseBuffer(free, 0)?;
        let _ = rate;
    }
    client.Stop().ok();
    let _ = CloseHandle(event);
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
}
