//! `relay-share learn-file` — S48: learn a game's sound and look from a
//! local gameplay video, faster than real time.
//!
//! Spawned by the core on the user's request ("Learn from a video file…"),
//! below-normal priority, and gone when done. It:
//! - opens the file **read-only** through Media Foundation's source reader
//!   (mp4 / mkv / mov / webm; whatever decoders Windows has). Nothing is
//!   copied, written next to it, or sent anywhere;
//! - decodes the audio as fast as the CPU allows at the file's own rate
//!   (never resampled) into the same S46 [`Analyzer`], folding one second
//!   of content at a time into the game's record and taking checkpoints on
//!   content time, exactly as the live learner does on play time;
//! - decodes the video (DXVA when the GPU can, else the CPU) and samples a
//!   frame every half second of content ([`SAMPLE_FPS`]) into the same S47
//!   [`Analyser`] and a fresh [`Learner`] that it prints once at the end for
//!   the core to merge. Frames between samples are released undecoded to
//!   memory; a sampled one is read back once, shrunk to 480×270 and dropped;
//! - has no input-idle signal (a file has no player); the content
//!   classifiers — letterbox, loading, static menus, music, voice-over and
//!   player chat — still apply.
//!
//! `stop` on stdin (or stdin closing) cancels: the helper leaves without
//! saving anything from the file.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use relay_audio::learn::{Analyzer, Goal, LearnRecord, Limits, Thresholds};
use relay_core::game_eq::{load_record_at, lock_record, save_record_at, LOCK_TIMEOUT};
use relay_display::learn::{Analyser, Frame, Learner, Order, SAMPLE_FPS};
use windows::core::{Interface, HSTRING};
use windows::Win32::Media::MediaFoundation::*;

/// How often progress is printed.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(500);
/// Content time between sampled frames, 100 ns units (1 / [`SAMPLE_FPS`]).
pub const SAMPLE_INTERVAL_100NS: i64 = 10_000_000 / SAMPLE_FPS as i64;

#[derive(Debug, Clone, PartialEq)]
pub struct LearnFileArgs {
    pub file: PathBuf,
    pub exe: String,
    pub record: PathBuf,
    pub goal: Goal,
}

impl LearnFileArgs {
    pub fn parse(args: &[String]) -> Result<Self> {
        let (mut file, mut exe, mut record, mut goal) = (None, None, None, Goal::default());
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().with_context(|| format!("{a} needs a value"));
            match a.as_str() {
                "--file" => file = Some(PathBuf::from(val()?)),
                "--exe" => exe = Some(val()?.clone()),
                "--record" => record = Some(PathBuf::from(val()?)),
                "--goal" => goal = serde_json::from_str(&format!("\"{}\"", val()?))?,
                other => bail!("unknown learn-file option {other}"),
            }
        }
        Ok(Self {
            file: file.context("--file is required")?,
            exe: exe.context("--exe is required")?,
            record: record.context("--record is required")?,
            goal,
        })
    }
}

/// Where each decoder has got to, in content time (100 ns).
#[derive(Debug, Default)]
pub struct Progress {
    pub audio_100ns: AtomicI64,
    pub video_100ns: AtomicI64,
    pub audio_done: AtomicBool,
    pub video_done: AtomicBool,
}

impl Progress {
    /// The slower of the two streams still running, seconds.
    pub fn position_secs(&self) -> f64 {
        let a = (!self.audio_done.load(Ordering::Relaxed))
            .then(|| self.audio_100ns.load(Ordering::Relaxed));
        let v = (!self.video_done.load(Ordering::Relaxed))
            .then(|| self.video_100ns.load(Ordering::Relaxed));
        let pos = match (a, v) {
            (Some(a), Some(v)) => a.min(v),
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => self
                .audio_100ns
                .load(Ordering::Relaxed)
                .max(self.video_100ns.load(Ordering::Relaxed)),
        };
        pos.max(0) as f64 / 1e7
    }
}

/// What one file taught.
#[derive(Debug)]
pub struct FileOutcome {
    /// The game's record with the file's audio folded in.
    pub rec: LearnRecord,
    pub audio_secs: f64,
    pub look: Learner,
    pub look_frames: u64,
    pub duration_secs: Option<f64>,
    pub notes: Vec<String>,
}

/// The run was cancelled.
#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

struct Com;
impl Com {
    fn init() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        // SAFETY: per-thread COM init, balanced in Drop.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        Com
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: balances `init` on this thread.
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

fn stream(c: MF_SOURCE_READER_CONSTANTS) -> u32 {
    c.0 as u32
}

/// A source reader on `path` (read-only). With `manager`, video decodes on
/// the GPU (DXVA) when the decoder can.
fn open_reader(path: &Path, manager: Option<&IMFDXGIDeviceManager>) -> Result<IMFSourceReader> {
    // SAFETY: attribute store + reader creation on live COM objects.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 4)?;
        let attrs = attrs.context("MFCreateAttributes")?;
        if let Some(m) = manager {
            attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, m)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        }
        let url = HSTRING::from(path.as_os_str());
        MFCreateSourceReaderFromURL(&url, &attrs)
            .with_context(|| "Windows could not open this video file".to_string())
    }
}

/// The file's duration, when the container says.
fn duration_secs(reader: &IMFSourceReader) -> Option<f64> {
    // SAFETY: a VT_UI8 PROPVARIANT read by hand (windows 0.62 has no
    // conversion); PROPVARIANT's Drop clears it.
    unsafe {
        let pv = reader
            .GetPresentationAttribute(stream(MF_SOURCE_READER_MEDIASOURCE), &MF_PD_DURATION)
            .ok()?;
        let inner = &pv.Anonymous.Anonymous;
        if inner.vt != windows::Win32::System::Variant::VT_UI8 {
            return None;
        }
        let d = inner.Anonymous.uhVal as f64 / 1e7;
        (d > 0.0).then_some(d)
    }
}

/// The source says it can seek (MKV files without a cue index cannot).
fn can_seek(reader: &IMFSourceReader) -> bool {
    // SAFETY: a VT_UI4 PROPVARIANT read by hand, cleared by its Drop.
    unsafe {
        let Ok(pv) = reader.GetPresentationAttribute(
            stream(MF_SOURCE_READER_MEDIASOURCE),
            &MF_SOURCE_READER_MEDIASOURCE_CHARACTERISTICS,
        ) else {
            return false;
        };
        let inner = &pv.Anonymous.Anonymous;
        inner.vt == windows::Win32::System::Variant::VT_UI4
            && inner.Anonymous.ulVal & MFMEDIASOURCE_CAN_SEEK.0 as u32 != 0
    }
}

/// Process CPU time so far, seconds (user + kernel, all threads).
pub fn cpu_secs() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: our own pseudo-handle and out-structs.
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.is_err() {
        return 0.0;
    }
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
    t(k) + t(u)
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// Decode every audio sample into the S46 analyser; one second of content
/// at a time goes into `rec`, with a checkpoint considered after each, as
/// the live learner does once a second of play. Returns the seconds heard,
/// or `None` when the file has no audio this PC can decode.
fn audio_pass(
    path: &Path,
    rec: &mut LearnRecord,
    cancel: &AtomicBool,
    progress: &Progress,
) -> Result<Option<f64>> {
    let _com = Com::init();
    let reader = open_reader(path, None)?;
    let s = stream(MF_SOURCE_READER_FIRST_AUDIO_STREAM);
    // SAFETY: plain source-reader calls on a live reader; buffers are locked
    // and unlocked in pairs and only read while locked.
    unsafe {
        reader.SetStreamSelection(stream(MF_SOURCE_READER_ALL_STREAMS), false)?;
        if reader.SetStreamSelection(s, true).is_err() {
            return Ok(None);
        }
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        if reader.SetCurrentMediaType(s, None, &t).is_err() {
            return Ok(None);
        }
        let cur = reader.GetCurrentMediaType(s)?;
        let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
        let channels = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?.max(1) as usize;
        anyhow::ensure!((8_000..=384_000).contains(&rate), "unsupported audio rate {rate}");
        let th = Thresholds::default();
        let limits = Limits::default();
        let mut an = Analyzer::new(rate);
        let mut since: u64 = 0;
        let mut total: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            let (mut flags, mut ts) = (0u32, 0i64);
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(s, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                bail!("the audio stream could not be read");
            }
            if let Some(sample) = sample {
                let buf = sample.ConvertToContiguousBuffer()?;
                let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                buf.Lock(&mut p, None, Some(&mut len))?;
                let n = len as usize / 4;
                let pcm = std::slice::from_raw_parts(p as *const f32, n);
                an.push_interleaved(pcm, channels);
                buf.Unlock()?;
                let frames = (n / channels) as u64;
                since += frames;
                total += frames;
                while since >= rate as u64 {
                    since -= rate as u64;
                    rec.absorb(&an.take_stats(), &th);
                    rec.checkpoint(&th, &limits);
                }
                progress.audio_100ns.store(ts, Ordering::Relaxed);
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        rec.absorb(&an.take_stats(), &th);
        rec.checkpoint(&th, &limits);
        Ok(Some(total as f64 / rate as f64))
    }
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

/// A D3D11 device manager on the default adapter, for DXVA decode. `None`
/// when the GPU path is not available (the CPU decoders then run).
fn dxva_manager() -> Option<IMFDXGIDeviceManager> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Multithread, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    };
    if std::env::var_os("RELAY_LEARN_FILE_CPU").is_some() {
        return None;
    }
    // SAFETY: standard device + manager creation; out params filled on success.
    unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .ok()?;
        let (device, context) = (device?, context?);
        if let Ok(mt) = context.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let mut token = 0u32;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager).ok()?;
        let manager = manager?;
        manager.ResetDevice(&device, token).ok()?;
        Some(manager)
    }
}

/// NV12 → packed BGR at `out` size, BT.709 limited range (what decoders
/// emit). Each output pixel averages up to 4×4 luma taps of the source area
/// it covers, so fine shadow texture is not invented by point sampling;
/// chroma is taken at the area's centre.
#[allow(clippy::too_many_arguments)]
pub fn nv12_to_bgr_scaled(
    y_plane: &[u8],
    uv_plane: &[u8],
    pitch: usize,
    w: usize,
    h: usize,
    ow: usize,
    oh: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; ow * oh * 3];
    if w == 0 || h == 0 || ow == 0 || oh == 0 {
        return out;
    }
    let taps = |a: usize, b: usize| -> [usize; 4] {
        let span = (b - a).max(1);
        [a, a + span / 4, a + span / 2, a + (3 * span) / 4]
    };
    for oy in 0..oh {
        let (y0, y1) = (oy * h / oh, ((oy + 1) * h / oh).max(oy * h / oh + 1).min(h));
        let ys = taps(y0, y1);
        for ox in 0..ow {
            let (x0, x1) = (ox * w / ow, ((ox + 1) * w / ow).max(ox * w / ow + 1).min(w));
            let xs = taps(x0, x1);
            let mut sum = 0u32;
            for &yy in &ys {
                for &xx in &xs {
                    sum +=
                        *y_plane.get(yy.min(h - 1) * pitch + xx.min(w - 1)).unwrap_or(&16) as u32;
                }
            }
            let y = sum as f32 / 16.0;
            let (cx, cy) = (((x0 + x1) / 2).min(w - 1) & !1, ((y0 + y1) / 2).min(h - 1) / 2);
            let u = *uv_plane.get(cy * pitch + cx).unwrap_or(&128) as f32 - 128.0;
            let v = *uv_plane.get(cy * pitch + cx + 1).unwrap_or(&128) as f32 - 128.0;
            let yy = (y - 16.0) * 1.164_383;
            let r = yy + 1.792_741 * v;
            let g = yy - 0.213_249 * u - 0.532_909 * v;
            let b = yy + 2.112_402 * u;
            let px = (oy * ow + ox) * 3;
            out[px] = b.clamp(0.0, 255.0) as u8;
            out[px + 1] = g.clamp(0.0, 255.0) as u8;
            out[px + 2] = r.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Is a frame at `ts` due for sampling? Advances `next`. The first frame is
/// always due; a gap (a seek, a dropped stretch) restarts the schedule.
pub fn sample_due(next: &mut Option<i64>, ts: i64) -> bool {
    match *next {
        None => {
            *next = Some(ts + SAMPLE_INTERVAL_100NS);
            true
        }
        Some(n) if ts >= n => {
            *next = Some(if ts - n >= SAMPLE_INTERVAL_100NS {
                ts + SAMPLE_INTERVAL_100NS
            } else {
                n + SAMPLE_INTERVAL_100NS
            });
            true
        }
        Some(_) => false,
    }
}

/// Read one decoded NV12 sample back and shrink it to the analysis size.
///
/// # Safety
/// `sample` must be a live NV12 sample of `size` (surface) / `shown`
/// (display area) from the reader.
unsafe fn sample_bgr(
    sample: &IMFSample,
    size: (u32, u32),
    shown: (u32, u32),
    out: (u32, u32),
) -> Result<Vec<u8>> {
    let (w, h) = (shown.0 as usize, shown.1 as usize);
    let alloc_h = size.1 as usize;
    unsafe {
        let buf = sample.GetBufferByIndex(0)?;
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let (mut p, mut pitch) = (std::ptr::null_mut(), 0i32);
            b2.Lock2D(&mut p, &mut pitch)?;
            let pitch = pitch.unsigned_abs() as usize;
            let y = std::slice::from_raw_parts(p as *const u8, pitch * alloc_h);
            let uv = std::slice::from_raw_parts(
                p.add(pitch * alloc_h) as *const u8,
                pitch * alloc_h.div_ceil(2),
            );
            let bgr = nv12_to_bgr_scaled(y, uv, pitch, w, h, out.0 as usize, out.1 as usize);
            b2.Unlock2D()?;
            return Ok(bgr);
        }
        let buf = sample.ConvertToContiguousBuffer()?;
        let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut p, None, Some(&mut len))?;
        let pitch = size.0 as usize;
        let need = pitch * alloc_h * 3 / 2;
        let res = if (len as usize) < need {
            Err(anyhow::anyhow!("short video buffer"))
        } else {
            let y = std::slice::from_raw_parts(p as *const u8, pitch * alloc_h);
            let uv = std::slice::from_raw_parts(
                p.add(pitch * alloc_h) as *const u8,
                pitch * alloc_h / 2,
            );
            Ok(nv12_to_bgr_scaled(y, uv, pitch, w, h, out.0 as usize, out.1 as usize))
        };
        buf.Unlock()?;
        res
    }
}

/// What the video pass found.
struct VideoResult {
    look: Learner,
    frames: u64,
    gpu: bool,
}

/// The video is decoded in up to this many stretches at once, each with its
/// own reader. One GPU decode session is bound by its per-frame round trip,
/// not by the decoder (S48 measured ~400 fps at 1080p on one session), so
/// several sessions side by side are what gets a long file past 10x.
pub const VIDEO_SEGMENTS: usize = 4;
/// A stretch is at least this long: each starts at the keyframe before its
/// start, so a GOP's worth is decoded twice, and on short files one reader
/// is quicker than four.
pub const MIN_SEGMENT_SECS: f64 = 30.0;

/// How many stretches a file of `duration` is split into.
pub fn segments_for(duration: Option<f64>) -> usize {
    match duration {
        Some(d) if d.is_finite() && d > 0.0 => {
            ((d / MIN_SEGMENT_SECS).floor() as usize).clamp(1, VIDEO_SEGMENTS)
        }
        _ => 1,
    }
}

/// Seek a reader to `t` (100 ns). It lands on the keyframe before.
///
/// # Safety
/// `reader` must be live.
unsafe fn seek(reader: &IMFSourceReader, t: i64) -> Result<()> {
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    let mut pv = PROPVARIANT::default();
    // SAFETY: a VT_I8 PROPVARIANT built by hand; nothing to free.
    unsafe {
        let inner = &mut pv.Anonymous.Anonymous;
        inner.vt = windows::Win32::System::Variant::VT_I8;
        inner.Anonymous.hVal = t;
        reader.SetCurrentPosition(&windows::core::GUID::zeroed(), &pv)?;
    }
    Ok(())
}

/// Frame reports with their content times, and whether the GPU decoded.
type SegmentReports = (Vec<(i64, relay_display::learn::FrameReport)>, bool);

/// One stretch `[start, end)` of the video: decode, sample on the
/// half-second grid from `start`, analyse. Returns the frame reports with
/// their times. `Ok(None)` when this PC cannot decode the picture.
fn video_segment(
    path: &Path,
    start: i64,
    end: i64,
    cancel: &AtomicBool,
    progress: &Progress,
) -> Result<Option<SegmentReports>> {
    let _com = Com::init();
    let manager = dxva_manager();
    let reader = match open_reader(path, manager.as_ref()) {
        Ok(r) => r,
        Err(_) if manager.is_some() => open_reader(path, None)?,
        Err(e) => return Err(e),
    };
    let s = stream(MF_SOURCE_READER_FIRST_VIDEO_STREAM);
    // SAFETY: plain source-reader calls on a live reader.
    unsafe {
        reader.SetStreamSelection(stream(MF_SOURCE_READER_ALL_STREAMS), false)?;
        if reader.SetStreamSelection(s, true).is_err() {
            return Ok(None);
        }
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        if reader.SetCurrentMediaType(s, None, &t).is_err() {
            return Ok(None);
        }
        if start > 0 {
            seek(&reader, start)?;
        }
        let geometry = || -> Result<((u32, u32), (u32, u32))> {
            let cur = reader.GetCurrentMediaType(s)?;
            let fs = cur.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let size = ((fs >> 32) as u32, fs as u32);
            let mut area = MFVideoArea::default();
            let bytes = std::slice::from_raw_parts_mut(
                &mut area as *mut MFVideoArea as *mut u8,
                std::mem::size_of::<MFVideoArea>(),
            );
            let shown = if cur.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None).is_ok()
                && area.Area.cx > 0
                && area.Area.cy > 0
            {
                ((area.Area.cx as u32).min(size.0), (area.Area.cy as u32).min(size.1))
            } else {
                size
            };
            Ok((size, shown))
        };
        let (mut size, mut shown) = geometry()?;
        let mut thumb = crate::preview::thumb_size(shown);
        let mut analyser = Analyser::new();
        let mut reports = Vec::new();
        // The grid starts at this stretch's start, so stretches tile.
        let mut next = Some(start);
        let mut done_to = start;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            let (mut flags, mut ts) = (0u32, 0i64);
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(s, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                bail!("the video stream could not be read");
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                (size, shown) = geometry()?;
                thumb = crate::preview::thumb_size(shown);
                analyser = Analyser::new();
            }
            if let Some(sample) = sample {
                if ts >= end {
                    break;
                }
                // Before the start: the frames from the keyframe up to it.
                if ts >= start && sample_due(&mut next, ts) {
                    let bgr = sample_bgr(&sample, size, shown, thumb)?;
                    drop(sample);
                    let report = analyser.analyse(&Frame {
                        width: thumb.0 as usize,
                        height: thumb.1 as usize,
                        data: &bgr,
                        order: Order::Bgr,
                    });
                    // No player behind a file: no input-idle signal.
                    reports.push((ts, report));
                }
                if ts > done_to {
                    progress.video_100ns.fetch_add(ts - done_to, Ordering::Relaxed);
                    done_to = ts;
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        Ok(Some((reports, manager.is_some())))
    }
}

/// Decode the video and analyse a frame every half second of content, in
/// [`segments_for`] stretches at once; the reports then go to one learner in
/// time order, as if read straight through. `Ok(None)` when the file has no
/// video this PC can decode.
fn video_pass(
    path: &Path,
    duration: Option<f64>,
    seekable: bool,
    cancel: &AtomicBool,
    progress: &Progress,
) -> Result<Option<VideoResult>> {
    let n = if seekable { segments_for(duration) } else { 1 };
    match video_pass_n(path, duration, n, cancel, progress) {
        // A source that claimed it could seek but refused: straight through.
        Err(e) if n > 1 && !e.is::<Cancelled>() => {
            tracing::info!(error = %e, "seeking failed; reading the video straight through");
            progress.video_100ns.store(0, Ordering::Relaxed);
            video_pass_n(path, duration, 1, cancel, progress)
        }
        r => r,
    }
}

fn video_pass_n(
    path: &Path,
    duration: Option<f64>,
    n: usize,
    cancel: &AtomicBool,
    progress: &Progress,
) -> Result<Option<VideoResult>> {
    let total = duration.map(|d| (d * 1e7) as i64).unwrap_or(i64::MAX);
    let bounds: Vec<(i64, i64)> = (0..n)
        .map(|k| {
            let a = if k == 0 { 0 } else { total / n as i64 * k as i64 };
            let b = if k + 1 == n { i64::MAX } else { total / n as i64 * (k as i64 + 1) };
            (a, b)
        })
        .collect();
    let parts = std::thread::scope(|sc| {
        let handles: Vec<_> = bounds
            .iter()
            .map(|&(a, b)| sc.spawn(move || video_segment(path, a, b, cancel, progress)))
            .collect();
        handles.into_iter().map(|h| h.join()).collect::<Vec<_>>()
    });
    let mut reports = Vec::new();
    let mut gpu = true;
    for p in parts {
        match p.map_err(|_| anyhow::anyhow!("the video decoder crashed"))?? {
            Some((r, g)) => {
                reports.extend(r);
                gpu &= g;
            }
            None => return Ok(None),
        }
    }
    reports.sort_by_key(|(ts, _)| *ts);
    let mut look = Learner::new("");
    for (_, r) in &reports {
        look.observe(r);
    }
    Ok(Some(VideoResult { look, frames: reports.len() as u64, gpu }))
}

/// Learn from `path` into `rec` (the audio) and a fresh look learner. Both
/// passes run at once, each as fast as its decoder goes. `Err(Cancelled)`
/// when `cancel` was set: the caller keeps nothing.
pub fn learn(
    path: &Path,
    mut rec: LearnRecord,
    cancel: &AtomicBool,
    progress: &Progress,
) -> Result<FileOutcome> {
    let (duration, seekable) = {
        let _com = Com::init();
        let r = open_reader(path, None)?;
        (duration_secs(&r), can_seek(&r))
    };
    let (audio, video) = std::thread::scope(|sc| {
        let a = sc.spawn(|| {
            let r = audio_pass(path, &mut rec, cancel, progress);
            progress.audio_done.store(true, Ordering::Relaxed);
            r
        });
        let v = sc.spawn(|| {
            let r = video_pass(path, duration, seekable, cancel, progress);
            progress.video_done.store(true, Ordering::Relaxed);
            r
        });
        (a.join(), v.join())
    });
    let audio = audio.map_err(|_| anyhow::anyhow!("the audio decoder crashed"))?;
    let video = video.map_err(|_| anyhow::anyhow!("the video decoder crashed"))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    let mut notes = Vec::new();
    let audio_secs = match audio {
        Ok(Some(s)) => s,
        Ok(None) => {
            notes.push(
                "This video has no sound this PC can decode; only its look was learned.".into(),
            );
            0.0
        }
        Err(e) => {
            if e.is::<Cancelled>() {
                return Err(e);
            }
            notes.push(format!("The sound could not be read ({e:#}); only the look was learned."));
            0.0
        }
    };
    let (look, look_frames) = match video {
        Ok(Some(v)) => {
            if !v.gpu {
                notes.push("The video was decoded on the CPU.".into());
            }
            (v.look, v.frames)
        }
        Ok(None) => {
            notes.push(
                "This PC cannot decode this video's picture (an HEVC file may need the HEVC Video \
                 Extensions); only its sound was learned."
                    .into(),
            );
            (Learner::new(""), 0)
        }
        Err(e) => {
            if e.is::<Cancelled>() {
                return Err(e);
            }
            notes.push(format!(
                "The picture could not be read ({e:#}); only the sound was learned."
            ));
            (Learner::new(""), 0)
        }
    };
    if audio_secs == 0.0 && look_frames == 0 {
        bail!("nothing in this file could be decoded on this PC");
    }
    Ok(FileOutcome { rec, audio_secs, look, look_frames, duration_secs: duration, notes })
}

fn emit(v: serde_json::Value) {
    println!("{v}");
}

/// The helper's main: lock and load the game's record, learn, and save only
/// when the run finished.
pub fn run(args: LearnFileArgs) -> Result<()> {
    let file = relay_core::learn_file::validate_video_path(&args.file.display().to_string())?;
    lower_priority();
    let _mf = crate::probe::MediaFoundation::start()?;
    let started = Instant::now();
    let cpu0 = cpu_secs();
    // Held for the whole run: a live learner for this game waits, and so do
    // the core's Reset / Relearn / goal change.
    let _lock = lock_record(&args.record, LOCK_TIMEOUT)?;
    let mut rec = load_record_at(&args.record, &args.exe)
        .unwrap_or_else(|| LearnRecord::new(&args.exe, None));
    rec.set_goal(args.goal, &Limits::default());
    // A file has no exe version: never a relearn, never a version adopted.
    rec.begin_session(None);

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        std::thread::Builder::new().name("learn-file-stdin".into()).spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line.as_deref().map(str::trim) {
                    Ok("stop") | Ok("cancel") | Err(_) => break,
                    Ok(_) => continue,
                }
            }
            cancel.store(true, Ordering::Relaxed);
        })?;
    }
    let progress = Arc::new(Progress::default());
    let duration = {
        let _com = Com::init();
        duration_secs(&open_reader(&file, None)?)
    };
    let done = Arc::new(AtomicBool::new(false));
    let ticker = {
        let (progress, done) = (progress.clone(), done.clone());
        std::thread::Builder::new().name("learn-file-progress".into()).spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(PROGRESS_EVERY);
                let pos = progress.position_secs();
                let el = started.elapsed().as_secs_f64().max(1e-3);
                emit(serde_json::json!({
                    "event": "progress",
                    "pos_secs": pos,
                    "duration_secs": duration,
                    "speed": pos / el,
                }));
            }
        })?
    };
    let result = learn(&file, rec, &cancel, &progress);
    done.store(true, Ordering::Relaxed);
    let _ = ticker.join();
    let out = match result {
        Ok(o) => o,
        Err(e) if e.is::<Cancelled>() => {
            emit(serde_json::json!({ "event": "cancelled" }));
            return Ok(());
        }
        Err(e) => {
            emit(serde_json::json!({ "event": "error", "message": format!("{e:#}") }));
            return Ok(());
        }
    };
    if out.audio_secs > 0.0 {
        save_record_at(&args.record, &out.rec)?;
    }
    for n in &out.notes {
        emit(serde_json::json!({ "event": "note", "message": n }));
    }
    emit(serde_json::json!({ "event": "look_result", "learner": out.look }));
    let el = started.elapsed().as_secs_f64().max(1e-3);
    let content =
        out.duration_secs.unwrap_or(out.audio_secs.max(out.look_frames as f64 / SAMPLE_FPS as f64));
    emit(serde_json::json!({
        "event": "done",
        "audio_secs": out.audio_secs,
        "look_frames": out.look_frames,
        "speed": content / el,
        "cpu_secs": cpu_secs() - cpu0,
        "audio_progress": out.rec.progress(&Thresholds::default()),
    }));
    Ok(())
}

fn lower_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    };
    // SAFETY: our own pseudo-handle.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
}

#[cfg(test)]
pub(crate) mod testclip;

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core::share::RecordingContainer;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_parse_and_are_required() {
        let a = LearnFileArgs::parse(&s(&[
            "--file",
            "C:\\v\\a.mp4",
            "--exe",
            "game.exe",
            "--record",
            "r.json",
            "--goal",
            "immersion",
        ]))
        .unwrap();
        assert_eq!(a.goal, Goal::Immersion);
        assert_eq!(a.file, PathBuf::from("C:\\v\\a.mp4"));
        assert!(LearnFileArgs::parse(&s(&["--exe", "g.exe", "--record", "r"])).is_err());
        assert!(LearnFileArgs::parse(&s(&[
            "--file", "a.mp4", "--exe", "g", "--record", "r", "--url", "x"
        ]))
        .is_err());
    }

    #[test]
    fn long_files_are_decoded_in_parallel_stretches() {
        assert_eq!((VIDEO_SEGMENTS, MIN_SEGMENT_SECS), (4, 30.0));
        assert_eq!(segments_for(None), 1);
        assert_eq!(segments_for(Some(12.0)), 1);
        assert_eq!(segments_for(Some(65.0)), 2);
        assert_eq!(segments_for(Some(3600.0)), VIDEO_SEGMENTS);
        assert_eq!(segments_for(Some(f64::NAN)), 1);
    }

    #[test]
    fn frames_are_sampled_every_half_second_of_content() {
        assert_eq!(SAMPLE_INTERVAL_100NS, 5_000_000);
        let mut next = None;
        // 60 fps for 10 s: 20 samples.
        let n = (0..600).filter(|i| sample_due(&mut next, i * 10_000_000 / 60)).count();
        assert_eq!(n, 20);
        // A gap restarts the schedule instead of sampling a burst.
        let mut next = None;
        assert!(sample_due(&mut next, 0));
        assert!(sample_due(&mut next, 100_000_000));
        assert!(!sample_due(&mut next, 100_000_001));
        assert!(sample_due(&mut next, 105_000_000));
    }

    #[test]
    fn shrinking_nv12_keeps_levels_and_aspect() {
        let (w, h) = (64usize, 36usize);
        let pitch = 80;
        let mut y = vec![0u8; pitch * h];
        let uv = vec![128u8; pitch * h / 2];
        for r in 0..h {
            for c in 0..w {
                y[r * pitch + c] = if c < w / 2 { 16 } else { 235 };
            }
        }
        let out = nv12_to_bgr_scaled(&y, &uv, pitch, w, h, 32, 18);
        assert_eq!(out.len(), 32 * 18 * 3);
        assert!(out[..3].iter().all(|&v| v <= 1), "black stays black: {:?}", &out[..3]);
        let right = (18 * 32 - 1) * 3;
        assert!(out[right..right + 3].iter().all(|&v| v >= 254), "white stays white");
        assert!(nv12_to_bgr_scaled(&[], &[], 0, 0, 0, 4, 4).iter().all(|&v| v == 0));
    }

    fn learn_clip(container: RecordingContainer, secs: u32) -> (FileOutcome, f64, f64) {
        let dir = std::env::temp_dir().join(format!("relay-s48-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ext = if container == RecordingContainer::Mkv { "mkv" } else { "mp4" };
        let path = dir.join(format!("clip-{secs}.{ext}"));
        testclip::write_clip(&path, container, secs, 640, 360, 30).expect("write the clip");
        let before = std::fs::read(&path).unwrap();
        let _mf = crate::probe::MediaFoundation::start().unwrap();
        let t = Instant::now();
        let cancel = AtomicBool::new(false);
        let out = learn(&path, LearnRecord::new("game.exe", None), &cancel, &Progress::default())
            .expect("learn from the clip");
        let el = t.elapsed().as_secs_f64();
        assert_eq!(std::fs::read(&path).unwrap(), before, "the file is never changed");
        std::fs::remove_file(&path).ok();
        (out, el, secs as f64)
    }

    /// A generated clip (H.264 + Opus, through Relay's own muxers) decodes:
    /// its audio fills the record and its frames feed the look learner,
    /// faster than real time.
    #[test]
    fn a_generated_mp4_clip_is_learned_faster_than_real_time() {
        let (out, el, secs) = learn_clip(RecordingContainer::Mp4, 12);
        eprintln!(
            "S48 learn-file mp4: {secs} s clip in {el:.2} s ({:.1}x), notes {:?}",
            secs / el,
            out.notes
        );
        assert!((out.audio_secs - secs).abs() < 1.0, "{} s of audio", out.audio_secs);
        assert!(out.rec.total_active_frames > 0, "the audio reached the analyser");
        assert!((20..=26).contains(&out.look_frames), "{} frames", out.look_frames);
        assert!(out.look.agg.frames + out.look.excluded.total() == out.look_frames);
        assert!(secs / el > 2.0, "{:.1}x", secs / el);
    }

    #[test]
    fn a_generated_mkv_clip_is_learned() {
        let (out, el, secs) = learn_clip(RecordingContainer::Mkv, 8);
        eprintln!(
            "S48 learn-file mkv: {secs} s clip in {el:.2} s ({:.1}x), notes {:?}",
            secs / el,
            out.notes
        );
        assert!((out.audio_secs - secs).abs() < 1.0, "{} s of audio", out.audio_secs);
        assert!(out.look_frames >= 14, "{}", out.look_frames);
    }

    /// Long enough to be split into stretches: every half second is still
    /// sampled once, in both containers.
    #[test]
    fn a_split_file_is_sampled_whole_in_both_containers() {
        for container in [RecordingContainer::Mp4, RecordingContainer::Mkv] {
            let dir = std::env::temp_dir().join(format!("relay-s48l-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let ext = if container == RecordingContainer::Mkv { "mkv" } else { "mp4" };
            let path = dir.join(format!("long.{ext}"));
            testclip::write_clip(&path, container, 70, 320, 180, 30).unwrap();
            let _mf = crate::probe::MediaFoundation::start().unwrap();
            let out = learn(
                &path,
                LearnRecord::new("game.exe", None),
                &AtomicBool::new(false),
                &Progress::default(),
            )
            .unwrap();
            assert_eq!(segments_for(out.duration_secs), 2, "{:?}", out.duration_secs);
            assert!(
                (138..=142).contains(&out.look_frames),
                "{container:?}: {} frames, {:?}",
                out.look_frames,
                out.notes
            );
            assert!(out.notes.iter().all(|n| !n.contains("could not")), "{:?}", out.notes);
            std::fs::remove_file(&path).ok();
        }
    }

    /// Writes a clip for a by-hand run of `relay-share learn-file`
    /// (`RELAY_S48_CLIP=<path.mp4|.mkv>`, ignored by default).
    #[test]
    #[ignore]
    fn write_clip_for_a_manual_run() {
        let Some(p) = std::env::var_os("RELAY_S48_CLIP") else { return };
        let p = PathBuf::from(p);
        let c = if p.extension().is_some_and(|e| e == "mkv") {
            RecordingContainer::Mkv
        } else {
            RecordingContainer::Mp4
        };
        testclip::write_clip(&p, c, 90, 1280, 720, 60).unwrap();
    }

    #[test]
    fn cancel_stops_at_once_and_keeps_nothing() {
        let dir = std::env::temp_dir().join(format!("relay-s48c-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cancel.mp4");
        testclip::write_clip(&path, RecordingContainer::Mp4, 4, 320, 180, 30).unwrap();
        let _mf = crate::probe::MediaFoundation::start().unwrap();
        let cancel = AtomicBool::new(true);
        let r = learn(&path, LearnRecord::new("game.exe", None), &cancel, &Progress::default());
        assert!(r.unwrap_err().is::<Cancelled>());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_that_is_not_a_video_fails_with_a_sentence() {
        let dir = std::env::temp_dir().join(format!("relay-s48e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("junk.mp4");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
        let _mf = crate::probe::MediaFoundation::start().unwrap();
        let r = learn(
            &path,
            LearnRecord::new("game.exe", None),
            &AtomicBool::new(false),
            &Progress::default(),
        );
        assert!(r.is_err());
        std::fs::remove_file(&path).ok();
    }

    /// The speed measurement for the doc: a 1080p60 clip, 60 s. Ignored by
    /// default (it encodes a minute of video first).
    /// `cargo test --release -p relay-capture learn_file::tests::speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn speed_1080p60() {
        for container in [RecordingContainer::Mp4, RecordingContainer::Mkv] {
            let dir = std::env::temp_dir().join(format!("relay-s48s-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!(
                "speed.{}",
                if container == RecordingContainer::Mkv { "mkv" } else { "mp4" }
            ));
            testclip::write_clip(&path, container, 120, 1920, 1080, 60).unwrap();
            let _mf = crate::probe::MediaFoundation::start().unwrap();
            for cpu_only in [false, true] {
                if cpu_only {
                    std::env::set_var("RELAY_LEARN_FILE_CPU", "1");
                } else {
                    std::env::remove_var("RELAY_LEARN_FILE_CPU");
                }
                let (t, c) = (Instant::now(), cpu_secs());
                let out = learn(
                    &path,
                    LearnRecord::new("game.exe", None),
                    &AtomicBool::new(false),
                    &Progress::default(),
                )
                .unwrap();
                let (el, cpu) = (t.elapsed().as_secs_f64(), cpu_secs() - c);
                eprintln!(
                    "S48 speed {container:?} 1080p60 120 s ({}): {el:.2} s = {:.1}x real time, CPU {cpu:.1} s ({:.0} % of one core avg), {} frames sampled, notes {:?}",
                    if cpu_only { "CPU decode" } else { "GPU decode" },
                    120.0 / el,
                    100.0 * cpu / el,
                    out.look_frames, out.notes
                );
            }
            std::env::remove_var("RELAY_LEARN_FILE_CPU");
            std::fs::remove_file(&path).ok();
        }
    }
}
