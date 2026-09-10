//! Receiver audio playback: Opus decode → WASAPI shared-mode render on the
//! default endpoint. Shared mode only; nothing about the endpoint is changed.

use std::sync::mpsc::Receiver;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

pub fn run(opus_rx: tokio::sync::mpsc::Receiver<Vec<u8>>, stop: Receiver<()>) -> Result<()> {
    // SAFETY: COM for this thread; WASAPI render loop; balanced on return.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("CoInitializeEx")?;
        let result = render_loop(opus_rx, stop);
        CoUninitialize();
        result
    }
}

unsafe fn render_loop(
    mut opus_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    stop: Receiver<()>,
) -> Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
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

    let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo)?;
    let mut pcm = vec![0f32; 480 * 2];
    let mut queue: std::collections::VecDeque<f32> = std::collections::VecDeque::new();

    client.Start()?;
    loop {
        if stop.try_recv().is_ok() {
            break;
        }
        // Drain any decoded audio waiting in the channel.
        while let Ok(pkt) = opus_rx.try_recv() {
            if let Ok(n) = decoder.decode_float(&pkt, &mut pcm, false) {
                for s in &pcm[..n * 2] {
                    queue.push_back(*s);
                }
            }
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
            // Opus is stereo; map to the device's channel count (dup or take 2).
            let l = queue.pop_front().unwrap_or(0.0);
            let r = queue.pop_front().unwrap_or(l);
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
