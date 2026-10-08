//! NDI® output (S51): publish what this engine shows (the receiver) or shares
//! (the sender) as an NDI source, so OBS (with its NDI plugin), vMix,
//! Resolume, NDI Studio Monitor and the rest can take it with no camera or
//! window capture. NDI® is a registered trademark of Vizrt NDI AB;
//! https://ndi.video/.
//!
//! Shape, mirroring `vcam_sink`:
//! - `ffi`     hand-written C declarations and the runtime loader. The
//!   runtime is the one the user installed (`docs/dev/ndi-licensing.md`);
//!   it is loaded only when NDI output is turned on, never at start.
//! - `tee`     the never-blocking hand-off from the render / playback /
//!   capture thread to the NDI workers. Off costs one atomic load.
//! - `convert` NV12 packing, planar audio, frame rate, timecodes.
//! - `video`   (Windows) the double-buffered GPU staging read-back.
//!
//! [`NdiOutput`] owns the switch. Turning it on loads the runtime, creates
//! one NDI sender and starts two workers (video and audio, because the SDK
//! allows both from different threads and a 4K compress must not hold up
//! audio); turning it off drops the producers, which ends the workers, which
//! drops the sender. A missing runtime is a reported state, not an error
//! that touches the share.

pub mod convert;
pub mod ffi;
pub mod tee;
#[cfg(windows)]
pub mod video;

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use tracing::{info, warn};

use convert::AudioClock;
use ffi::LoadError;
use tee::{Tap, TeeStats};

/// Frames that may wait for the video worker. Two: NDI compresses as it
/// sends, and a frame older than that is better dropped than shown late.
const VIDEO_QUEUE: usize = 2;
/// 10 ms buffers that may wait for the audio worker: 80 ms.
const AUDIO_QUEUE: usize = 8;

/// One packed NV12 picture for NDI.
#[derive(Debug, Default)]
pub struct VideoBuf {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub rate: (i32, i32),
    pub timecode: i64,
}

/// One planar float buffer for NDI.
#[derive(Debug, Default)]
pub struct AudioBuf {
    pub planar: Vec<f32>,
    pub frames: usize,
    pub channels: usize,
    pub rate: u32,
    pub timecode: i64,
}

/// One NDI sender. The real one wraps the runtime; tests use a mock.
pub trait Sender: Send + Sync {
    fn video(&self, f: &VideoBuf);
    fn audio(&self, a: &AudioBuf);
    /// NDI receivers connected right now (`NDIlib_send_get_no_connections`).
    fn connections(&self) -> i32;
}

/// Makes senders. The real one loads the runtime on first use.
pub trait Backend: Send + Sync {
    fn create(&self, name: &str) -> Result<Arc<dyn Sender>, LoadError>;
}

struct Live {
    sender: Arc<dyn Sender>,
    video_stats: Arc<TeeStats>,
    audio_stats: Arc<TeeStats>,
    workers: Vec<JoinHandle<()>>,
}

/// The NDI switch for one engine.
pub struct NdiOutput {
    name: String,
    backend: Box<dyn Backend>,
    pub video: Arc<Tap<VideoBuf>>,
    pub audio: Arc<Tap<AudioBuf>>,
    epoch: Instant,
    live: Mutex<Option<Live>>,
    /// The last failure to turn on: (message, runtime missing).
    error: Mutex<Option<(String, bool)>>,
}

impl NdiOutput {
    pub fn new(name: impl Into<String>, backend: Box<dyn Backend>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            backend,
            video: Tap::shared(),
            audio: Tap::shared(),
            epoch: Instant::now(),
            live: Mutex::new(None),
            error: Mutex::new(None),
        })
    }

    /// With the user-installed runtime.
    pub fn with_runtime(name: impl Into<String>) -> Arc<Self> {
        Self::new(name, Box::new(RuntimeBackend))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_on(&self) -> bool {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Ticks (100 ns) since this output was made: the one clock video and
    /// audio timecodes share.
    pub fn now_ticks(&self) -> i64 {
        (self.epoch.elapsed().as_nanos() / 100) as i64
    }

    /// Turn NDI output on or off. Blocking (the first on loads the runtime),
    /// so it belongs on a control thread, never a real-time one. Ok(true)
    /// when the state changed.
    pub fn set(&self, on: bool) -> Result<bool, LoadError> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if on == live.is_some() {
            return Ok(false);
        }
        if !on {
            self.video.clear();
            self.audio.clear();
            if let Some(l) = live.take() {
                for w in l.workers {
                    let _ = w.join();
                }
                // The workers held the last other references; this drops the
                // NDI sender, which leaves the network.
                drop(l.sender);
            }
            info!(name = %self.name, "NDI output off");
            return Ok(true);
        }
        let sender = match self.backend.create(&self.name) {
            Ok(s) => s,
            Err(e) => {
                *self.error.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((e.to_string(), e.runtime_missing()));
                return Err(e);
            }
        };
        let (vtx, vrx) = tee::channel::<VideoBuf>(VIDEO_QUEUE);
        let (atx, arx) = tee::channel::<AudioBuf>(AUDIO_QUEUE);
        let (video_stats, audio_stats) = (vtx.stats.clone(), atx.stats.clone());
        let vs = sender.clone();
        let as_ = sender.clone();
        let mut workers = Vec::new();
        match tee::spawn_worker("relay-ndi-video", vrx, move |f: &VideoBuf| vs.video(f)) {
            Ok(w) => workers.push(w),
            Err(e) => warn!(error = %e, "NDI video worker did not start"),
        }
        match tee::spawn_worker("relay-ndi-audio", arx, move |a: &AudioBuf| as_.audio(a)) {
            Ok(w) => workers.push(w),
            Err(e) => warn!(error = %e, "NDI audio worker did not start"),
        }
        self.video.install(vtx);
        self.audio.install(atx);
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *live = Some(Live { sender, video_stats, audio_stats, workers });
        info!(name = %self.name, "NDI output on");
        Ok(true)
    }

    /// [`NdiOutput::set`], reported on stdout as the engine's NDJSON events
    /// (`ndi_up`, `ndi_down`, `ndi_error`) and never an error: NDI output
    /// failing is never a reason to lose the share.
    pub fn apply(&self, on: bool) {
        match self.set(on) {
            Ok(true) if on => {
                println!("{}", serde_json::json!({ "event": "ndi_up", "name": self.name }))
            }
            Ok(true) => println!("{}", serde_json::json!({ "event": "ndi_down" })),
            Ok(false) => {}
            Err(e) => {
                warn!(error = %e, "NDI output unavailable");
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "ndi_error",
                        "message": e.to_string(),
                        "runtime_missing": e.runtime_missing(),
                    })
                );
            }
        }
    }

    /// A producer hit a failure it cannot recover from (a D3D error): say so
    /// in the stats line and turn output off, off the calling thread. The
    /// share itself is untouched; the user can turn NDI output on again.
    pub fn fail(self: &Arc<Self>, message: String) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some((message, false));
        let me = self.clone();
        let _ = std::thread::Builder::new().name("relay-ndi-off".into()).spawn(move || {
            if let Ok(true) = me.set(false) {
                println!("{}", serde_json::json!({ "event": "ndi_down" }));
            }
        });
    }

    /// The `ndi` object in the engine's stats line.
    pub fn status_json(&self) -> serde_json::Value {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let error = self.error.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match live.as_ref() {
            Some(l) => serde_json::json!({
                "on": true,
                "name": self.name,
                "connections": l.sender.connections().max(0),
                "video": l.video_stats.json(),
                "audio": l.audio_stats.json(),
            }),
            None => serde_json::json!({
                "on": false,
                "name": self.name,
                "error": error.as_ref().map(|e| e.0.clone()),
                "runtime_missing": error.as_ref().is_some_and(|e| e.1),
            }),
        }
    }
}

impl Drop for NdiOutput {
    fn drop(&mut self) {
        let _ = self.set(false);
    }
}

/// The audio side's producer state, owned by the playback (or capture)
/// thread: the timecode clock and the scratch it de-interleaves through.
pub struct AudioProducer {
    out: Arc<NdiOutput>,
    clock: AudioClock,
    rate: u32,
}

impl AudioProducer {
    pub fn new(out: Arc<NdiOutput>, rate: u32) -> Self {
        Self { out, clock: AudioClock::new(rate), rate }
    }

    /// Offer one interleaved buffer that `queued` frames of endpoint buffer
    /// will play ahead of. Never blocks; a no-op while NDI output is off.
    #[inline]
    pub fn push(&mut self, interleaved: &[f32], channels: usize, queued: u32) {
        if !self.out.audio.is_active() || channels == 0 || interleaved.is_empty() {
            return;
        }
        let now = self.out.now_ticks();
        let frames = interleaved.len() / channels;
        let timecode = self.clock.stamp(now, queued, frames);
        let rate = self.rate;
        self.out.audio.try_with(|tx| {
            let Some(mut b) = tx.buffer() else { return };
            b.frames = convert::deinterleave(interleaved, channels, &mut b.planar);
            b.channels = channels;
            b.rate = rate;
            b.timecode = timecode;
            tx.send(b);
        });
    }
}

/// The real backend: the user-installed NDI runtime.
pub struct RuntimeBackend;

#[cfg(windows)]
mod real {
    #![allow(unsafe_code)] // NDI C calls on a live instance; SAFETY notes inline

    use super::*;
    use crate::ndi::ffi::{self, Runtime, SendInstance};
    use std::ffi::CString;

    struct NdiSend {
        rt: &'static Runtime,
        inst: SendInstance,
        _name: CString,
    }

    // SAFETY: the SDK documents a send instance as safe to use from several
    // threads (video and audio from separate threads is the documented way).
    unsafe impl Send for NdiSend {}
    unsafe impl Sync for NdiSend {}

    impl Drop for NdiSend {
        fn drop(&mut self) {
            // SAFETY: created by send_create, destroyed exactly once, after
            // every worker using it has been joined.
            unsafe { (self.rt.api.send_destroy)(self.inst) }
        }
    }

    impl Sender for NdiSend {
        fn video(&self, f: &VideoBuf) {
            let frame = ffi::VideoFrameV2 {
                xres: f.width as i32,
                yres: f.height as i32,
                four_cc: ffi::FOURCC_NV12,
                frame_rate_N: f.rate.0,
                frame_rate_D: f.rate.1,
                picture_aspect_ratio: 0.0, // square pixels: the SDK uses xres/yres
                frame_format_type: ffi::FRAME_FORMAT_PROGRESSIVE,
                timecode: f.timecode,
                p_data: f.data.as_ptr(),
                line_stride_in_bytes: f.stride as i32,
                p_metadata: std::ptr::null(),
                timestamp: 0,
            };
            // SAFETY: a live instance; the synchronous send copies (or
            // compresses) the frame before returning, so `f` may be reused.
            unsafe { (self.rt.api.send_video_v2)(self.inst, &frame) }
        }

        fn audio(&self, a: &AudioBuf) {
            let frame = ffi::AudioFrameV2 {
                sample_rate: a.rate as i32,
                no_channels: a.channels as i32,
                no_samples: a.frames as i32,
                timecode: a.timecode,
                p_data: a.planar.as_ptr(),
                channel_stride_in_bytes: (a.frames * 4) as i32,
                p_metadata: std::ptr::null(),
                timestamp: 0,
            };
            // SAFETY: as above; audio sends are synchronous copies.
            unsafe { (self.rt.api.send_audio_v2)(self.inst, &frame) }
        }

        fn connections(&self) -> i32 {
            // SAFETY: a live instance; timeout 0 = do not wait.
            unsafe { (self.rt.api.send_get_no_connections)(self.inst, 0) }
        }
    }

    impl Backend for RuntimeBackend {
        fn create(&self, name: &str) -> Result<Arc<dyn Sender>, LoadError> {
            let rt = Runtime::get()?;
            let cname = CString::new(name.replace('\0', "")).map_err(|e| {
                LoadError::LoadFailed { path: rt.path.clone(), reason: e.to_string() }
            })?;
            let create = ffi::SendCreate {
                p_ndi_name: cname.as_ptr(),
                p_groups: std::ptr::null(),
                clock_video: false,
                clock_audio: false,
            };
            // SAFETY: a valid settings struct whose strings outlive the call.
            let inst = unsafe { (rt.api.send_create)(&create) };
            if inst.is_null() {
                return Err(LoadError::LoadFailed {
                    path: rt.path.clone(),
                    reason: "NDIlib_send_create returned no sender".into(),
                });
            }
            Ok(Arc::new(NdiSend { rt, inst, _name: cname }))
        }
    }
}

#[cfg(not(windows))]
impl Backend for RuntimeBackend {
    fn create(&self, _name: &str) -> Result<Arc<dyn Sender>, LoadError> {
        Err(LoadError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    /// (frames, channels, timecode, planar samples) of the last audio buffer.
    type LastAudio = Option<(usize, usize, i64, Vec<f32>)>;

    #[derive(Default)]
    struct Mock {
        video: AtomicU64,
        audio: AtomicU64,
        last_audio: Mutex<LastAudio>,
        slow: Option<Duration>,
    }

    impl Sender for Mock {
        fn video(&self, _f: &VideoBuf) {
            if let Some(d) = self.slow {
                std::thread::sleep(d);
            }
            self.video.fetch_add(1, Ordering::Relaxed);
        }
        fn audio(&self, a: &AudioBuf) {
            self.audio.fetch_add(1, Ordering::Relaxed);
            *self.last_audio.lock().unwrap() =
                Some((a.frames, a.channels, a.timecode, a.planar.clone()));
        }
        fn connections(&self) -> i32 {
            2
        }
    }

    struct MockBackend {
        sender: Arc<Mock>,
        creates: Arc<AtomicUsize>,
    }

    impl Backend for MockBackend {
        fn create(&self, _name: &str) -> Result<Arc<dyn Sender>, LoadError> {
            self.creates.fetch_add(1, Ordering::Relaxed);
            Ok(self.sender.clone())
        }
    }

    struct Missing;
    impl Backend for Missing {
        fn create(&self, _name: &str) -> Result<Arc<dyn Sender>, LoadError> {
            Err(LoadError::RuntimeMissing { searched: r"C:\nowhere".into() })
        }
    }

    fn mock(slow: Option<Duration>) -> (Arc<NdiOutput>, Arc<Mock>, Arc<AtomicUsize>) {
        let sender = Arc::new(Mock { slow, ..Mock::default() });
        let creates = Arc::new(AtomicUsize::new(0));
        let out = NdiOutput::new(
            "Relay (from PC1)",
            Box::new(MockBackend { sender: sender.clone(), creates: creates.clone() }),
        );
        (out, sender, creates)
    }

    #[test]
    fn off_by_default_and_costs_nothing() {
        let (out, sender, creates) = mock(None);
        assert!(!out.is_on());
        assert_eq!(creates.load(Ordering::Relaxed), 0, "nothing loaded until asked");
        let mut p = AudioProducer::new(out.clone(), 48_000);
        p.push(&[0.5; 960], 2, 0);
        assert_eq!(sender.audio.load(Ordering::Relaxed), 0);
        let s = out.status_json();
        assert_eq!(s["on"], false);
        assert_eq!(s["name"], "Relay (from PC1)");
    }

    #[test]
    fn on_off_on() {
        let (out, _sender, creates) = mock(None);
        assert_eq!(out.set(true), Ok(true));
        assert_eq!(out.set(true), Ok(false), "a repeat is a no-op");
        assert!(out.video.is_active() && out.audio.is_active());
        assert_eq!(out.status_json()["connections"], 2);
        assert_eq!(out.set(false), Ok(true));
        assert!(!out.video.is_active() && !out.audio.is_active());
        assert_eq!(out.set(true), Ok(true));
        assert_eq!(creates.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn audio_reaches_the_sender_planar_with_a_timecode() {
        let (out, sender, _) = mock(None);
        out.set(true).unwrap();
        let mut p = AudioProducer::new(out.clone(), 48_000);
        let interleaved: Vec<f32> =
            (0..960).map(|i| if i % 2 == 0 { 0.25 } else { -0.25 }).collect();
        p.push(&interleaved, 2, 480);
        out.set(false).unwrap(); // joins the workers: everything sent is in
        let (frames, ch, tc, planar) = sender.last_audio.lock().unwrap().clone().unwrap();
        assert_eq!((frames, ch), (480, 2));
        assert!(tc >= 100_000, "stamped for when it is heard: {tc}");
        assert!(planar[..480].iter().all(|s| *s == 0.25));
        assert!(planar[480..].iter().all(|s| *s == -0.25));
    }

    #[test]
    fn a_missing_runtime_is_reported_not_raised() {
        let out = NdiOutput::new("Relay", Box::new(Missing));
        let e = out.set(true).unwrap_err();
        assert!(e.runtime_missing());
        assert!(!out.is_on());
        assert!(!out.video.is_active());
        let s = out.status_json();
        assert_eq!(s["on"], false);
        assert_eq!(s["runtime_missing"], true);
        assert!(s["error"].as_str().unwrap().contains("needs the NDI runtime"));
        // `apply` swallows it (the share carries on).
        out.apply(true);
    }

    #[test]
    fn a_slow_ndi_sender_never_holds_up_the_producer() {
        let (out, sender, _) = mock(Some(Duration::from_millis(40)));
        out.set(true).unwrap();
        let start = Instant::now();
        for i in 0..120 {
            out.video.try_with(|tx| {
                if let Some(mut b) = tx.buffer() {
                    b.timecode = i;
                    tx.send(b);
                }
            });
        }
        assert!(start.elapsed() < Duration::from_millis(40), "took {:?}", start.elapsed());
        let s = out.status_json();
        assert!(s["video"]["dropped"].as_u64().unwrap() >= 110);
        out.set(false).unwrap();
        assert!(sender.video.load(Ordering::Relaxed) <= 4);
    }
}
