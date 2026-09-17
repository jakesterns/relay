//! Recording alongside the live share. The pipelines tee their already-
//! encoded output into a bounded channel with `try_send` — the recorder can
//! drop (counted) but never block the live path. One writer thread owns the
//! ring buffer, the muxer, file rolling and the disk budget.
//!
//! Submodules are pure and unit-tested: `annexb` (bitstream forms), `mux`
//! (fragmented MP4, golden fixtures), `mkv` (Matroska, golden fixtures),
//! `ring` (replay accounting), `budget` (prune/stop planning).
//!
//! The container is a per-preset choice and never changes what is recorded:
//! both muxers take the same teed access units. fMP4 is the default; MKV is
//! there because a recording cut off by a crash still imports into an editor,
//! where a fragmented MP4 does not (`docs/dev/container-compat.md`).

pub mod annexb;
pub mod budget;
pub mod mkv;
pub mod mux;
pub mod ring;

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{info, warn};

use budget::DiskBudget;
use mkv::MkvMuxer;
use mux::{Mp4Muxer, MuxConfig};
use relay_core::share::RecordingContainer;
use ring::{ItemKind, ReplayRing, RingItem};

/// The open output file, in whichever container the preset asked for. Both
/// arms take the identical teed bitstream; only the framing differs.
enum AnyMuxer {
    Mp4(Mp4Muxer<BufWriter<File>>),
    Mkv(MkvMuxer<BufWriter<File>>),
}

impl AnyMuxer {
    fn new(container: RecordingContainer, w: BufWriter<File>, cfg: MuxConfig) -> Self {
        match container {
            RecordingContainer::Mp4 => Self::Mp4(Mp4Muxer::new(w, cfg)),
            RecordingContainer::Mkv => Self::Mkv(MkvMuxer::new(w, cfg)),
        }
    }

    fn push_video(&mut self, au: &[u8], pts_100ns: i64, keyframe: bool) -> Result<()> {
        match self {
            Self::Mp4(m) => m.push_video(au, pts_100ns, keyframe),
            Self::Mkv(m) => m.push_video(au, pts_100ns, keyframe),
        }
    }

    fn push_audio(
        &mut self,
        track: usize,
        packet: &[u8],
        pts_100ns: i64,
        dur_100ns: i64,
    ) -> Result<()> {
        match self {
            Self::Mp4(m) => m.push_audio(track, packet, pts_100ns, dur_100ns),
            Self::Mkv(m) => m.push_audio(track, packet, pts_100ns, dur_100ns),
        }
    }

    fn bytes_written(&self) -> u64 {
        match self {
            Self::Mp4(m) => m.bytes_written(),
            Self::Mkv(m) => m.bytes_written(),
        }
    }

    fn finalize(self) -> Result<()> {
        match self {
            Self::Mp4(m) => m.finalize().map(|_| ()),
            Self::Mkv(m) => m.finalize().map(|_| ()),
        }
    }
}

/// Everything the writer thread receives.
pub enum RecordMsg {
    Video { data: Vec<u8>, pts_100ns: i64, keyframe: bool },
    Audio { track: usize, data: Vec<u8>, pts_100ns: i64, dur_100ns: i64 },
    SetRecording(bool),
    SaveReplay,
}

/// Live counters for the instrument strip, sampled by the sender's stats tick.
#[derive(Default)]
pub struct RecordStats {
    pub recording: AtomicBool,
    pub bytes_written: AtomicU64,
    /// Tee messages dropped because the writer was behind (disk stall).
    pub dropped: AtomicU64,
    /// Replay ring fill, 0..=1000.
    pub ring_fill_milli: AtomicU32,
    pub replays_saved: AtomicU64,
    /// Last replay save duration in ms.
    pub replay_save_ms: AtomicU64,
    /// Recording refused/stopped to protect the disk budget.
    pub stopped_for_disk: AtomicBool,
}

#[derive(Debug, Clone)]
pub struct RecordConfig {
    /// The share's negotiated video codec; the file describes it faithfully.
    pub codec: crate::codec::VideoCodec,
    /// Recording folder, e.g. `%USERPROFILE%\Videos\Relay`.
    pub dir: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Mux the program-mix Opus track too.
    pub audio: bool,
    /// Mux a second Opus track for the microphone. Ignored without `audio`:
    /// a file whose only audio is the mic still puts it on track 0, so a
    /// player finds it where it looks.
    pub mic: bool,
    /// Replay window; 0 disables the ring.
    pub replay_secs: u32,
    /// RAM cap for the ring (bitrate-aware, chosen by the caller).
    pub ring_max_bytes: usize,
    pub budget: DiskBudget,
    /// Continuous-recording file roll interval (default hourly).
    pub roll_secs: u64,
    /// Start with continuous recording on.
    pub record_on_start: bool,
    /// Container to mux into.
    pub container: RecordingContainer,
}

/// Handle owned by the sender; all pushes are non-blocking.
pub struct Recorder {
    tx: SyncSender<RecordMsg>,
    stats: Arc<RecordStats>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Recorder {
    pub fn start(cfg: RecordConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.dir)
            .with_context(|| format!("creating recording folder {}", cfg.dir.display()))?;
        // ~4 s of 4K60 video messages; the writer only stalls on disk I/O.
        let (tx, rx) = sync_channel::<RecordMsg>(256);
        let stats = Arc::new(RecordStats::default());
        let stats2 = stats.clone();
        let join = std::thread::Builder::new().name("relay-record".into()).spawn(move || {
            if let Err(e) = writer_thread(cfg, rx, stats2) {
                warn!(error = %e, "recorder stopped");
            }
        })?;
        Ok(Self { tx, stats, join: Some(join) })
    }

    pub fn stats(&self) -> Arc<RecordStats> {
        self.stats.clone()
    }

    /// Tee one encoded access unit (pre-SEI Annex B). Never blocks.
    pub fn push_video(&self, data: &[u8], pts_100ns: i64, keyframe: bool) {
        self.try_send(RecordMsg::Video { data: data.to_vec(), pts_100ns, keyframe });
    }

    /// Tee one Opus packet onto audio track `track` (0 = program mix,
    /// 1 = microphone). Never blocks.
    pub fn push_audio(&self, track: usize, data: &[u8], pts_100ns: i64, dur_100ns: i64) {
        self.try_send(RecordMsg::Audio { track, data: data.to_vec(), pts_100ns, dur_100ns });
    }

    fn try_send(&self, msg: RecordMsg) {
        match self.tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Toggle continuous recording (control path — may briefly block).
    pub fn set_recording(&self, on: bool) {
        let _ = self.tx.send(RecordMsg::SetRecording(on));
    }

    /// Save the replay ring to disk (control path).
    pub fn save_replay(&self) {
        let _ = self.tx.send(RecordMsg::SaveReplay);
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Closing the channel is the shutdown signal; the thread finalizes
        // the open file before exiting.
        drop(std::mem::replace(&mut self.tx, sync_channel(1).0));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct ActiveFile {
    muxer: AnyMuxer,
    path: PathBuf,
    opened: Instant,
    /// Bytes written at the last budget check.
    checked_at_bytes: u64,
}

fn writer_thread(
    cfg: RecordConfig,
    rx: Receiver<RecordMsg>,
    stats: Arc<RecordStats>,
) -> Result<()> {
    let mut ring =
        (cfg.replay_secs > 0).then(|| ReplayRing::new(cfg.replay_secs, cfg.ring_max_bytes.max(1)));
    let mut active: Option<ActiveFile> = None;
    let mut want_recording = cfg.record_on_start;

    while let Ok(msg) = rx.recv() {
        match msg {
            RecordMsg::SetRecording(on) => {
                want_recording = on;
                if !on {
                    close(&mut active, &stats);
                    stats.stopped_for_disk.store(false, Ordering::Relaxed);
                }
                // Turning on waits for the next keyframe (see Video below) so
                // the file starts decodable; the sender forces an IDR.
            }
            RecordMsg::SaveReplay => {
                if let Some(r) = &ring {
                    let t0 = Instant::now();
                    match save_replay(&cfg, r) {
                        Ok(path) => {
                            let ms = t0.elapsed().as_millis() as u64;
                            stats.replays_saved.fetch_add(1, Ordering::Relaxed);
                            stats.replay_save_ms.store(ms, Ordering::Relaxed);
                            info!(path = %path.display(), ms, "replay saved");
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "replay_saved",
                                    "path": path.display().to_string(),
                                    "ms": ms,
                                })
                            );
                        }
                        Err(e) => warn!(error = %e, "replay save failed"),
                    }
                }
            }
            RecordMsg::Video { data, pts_100ns, keyframe } => {
                // Roll / open only at a keyframe so every file starts clean.
                if keyframe {
                    let roll = active
                        .as_ref()
                        .map(|a| a.opened.elapsed().as_secs() >= cfg.roll_secs)
                        .unwrap_or(false);
                    if roll {
                        close(&mut active, &stats);
                    }
                    if want_recording && active.is_none() {
                        match open_recording(&cfg, &stats) {
                            Ok(f) => active = Some(f),
                            Err(e) => {
                                warn!(error = %e, "cannot start recording");
                                want_recording = false;
                            }
                        }
                    }
                }
                if let Some(a) = &mut active {
                    a.muxer.push_video(&data, pts_100ns, keyframe)?;
                    stats.bytes_written.store(a.muxer.bytes_written(), Ordering::Relaxed);
                    // Re-check the budget every ~256 MB.
                    if a.muxer.bytes_written() - a.checked_at_bytes > 256 * 1024 * 1024 {
                        a.checked_at_bytes = a.muxer.bytes_written();
                        let open_path = a.path.clone();
                        if !enforce_budget(&cfg, Some(&open_path)) {
                            warn!("disk budget floor reached; stopping recording");
                            stats.stopped_for_disk.store(true, Ordering::Relaxed);
                            close(&mut active, &stats);
                            want_recording = false;
                        }
                    }
                }
                if let Some(r) = &mut ring {
                    r.push(RingItem { kind: ItemKind::Video { keyframe }, pts_100ns, data });
                    stats.ring_fill_milli.store((r.fill() * 1000.0) as u32, Ordering::Relaxed);
                }
            }
            RecordMsg::Audio { track, data, pts_100ns, dur_100ns } => {
                if let Some(a) = &mut active {
                    a.muxer.push_audio(track, &data, pts_100ns, dur_100ns)?;
                }
                if let Some(r) = &mut ring {
                    r.push(RingItem {
                        kind: ItemKind::Audio { track, dur_100ns },
                        pts_100ns,
                        data,
                    });
                }
            }
        }
    }
    close(&mut active, &stats);
    Ok(())
}

fn close(active: &mut Option<ActiveFile>, stats: &RecordStats) {
    if let Some(a) = active.take() {
        match a.muxer.finalize() {
            Ok(_) => info!(path = %a.path.display(), "recording closed"),
            Err(e) => warn!(error = %e, "finalizing recording failed"),
        }
    }
    stats.recording.store(false, Ordering::Relaxed);
}

fn open_recording(cfg: &RecordConfig, stats: &RecordStats) -> Result<ActiveFile> {
    if !enforce_budget(cfg, None) {
        stats.stopped_for_disk.store(true, Ordering::Relaxed);
        anyhow::bail!("free space below the floor even after pruning");
    }
    let path = unique_path(&cfg.dir, &file_name("Relay", local_time_parts(), cfg.container));
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let muxer =
        AnyMuxer::new(cfg.container, BufWriter::with_capacity(1 << 20, file), mux_config(cfg));
    stats.recording.store(true, Ordering::Relaxed);
    stats.stopped_for_disk.store(false, Ordering::Relaxed);
    info!(path = %path.display(), "recording started");
    println!(
        "{}",
        serde_json::json!({ "event": "recording", "on": true, "path": path.display().to_string() })
    );
    Ok(ActiveFile { muxer, path, opened: Instant::now(), checked_at_bytes: 0 })
}

fn mux_config(cfg: &RecordConfig) -> MuxConfig {
    match (cfg.audio, cfg.mic) {
        (true, true) => MuxConfig::with_opus_and_mic(cfg.width, cfg.height),
        (true, false) => MuxConfig::with_opus(cfg.width, cfg.height),
        (false, _) => MuxConfig::video_only(cfg.width, cfg.height),
    }
    .with_codec(cfg.codec)
}

fn save_replay(cfg: &RecordConfig, ring: &ReplayRing) -> Result<PathBuf> {
    anyhow::ensure!(!ring.is_empty(), "replay buffer is empty");
    anyhow::ensure!(enforce_budget(cfg, None), "free space below the floor");
    let path = unique_path(&cfg.dir, &file_name("Relay Replay", local_time_parts(), cfg.container));
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut muxer =
        AnyMuxer::new(cfg.container, BufWriter::with_capacity(1 << 20, file), mux_config(cfg));
    for item in ring.save_slice(cfg.replay_secs) {
        match item.kind {
            ItemKind::Video { keyframe } => {
                muxer.push_video(&item.data, item.pts_100ns, keyframe)?
            }
            ItemKind::Audio { track, dur_100ns } => {
                muxer.push_audio(track, &item.data, item.pts_100ns, dur_100ns)?
            }
        }
    }
    muxer.finalize()?;
    Ok(path)
}

/// Apply the disk budget: prune finished recordings, report whether recording
/// may continue. `exclude` is the file currently being written.
fn enforce_budget(cfg: &RecordConfig, exclude: Option<&Path>) -> bool {
    let files = list_recordings(&cfg.dir, exclude);
    let free = free_space(&cfg.dir).unwrap_or(u64::MAX);
    let plan = budget::plan(&cfg.budget, &files, free);
    for path in &plan.delete {
        match std::fs::remove_file(path) {
            Ok(()) => info!(path = %path.display(), "pruned old recording (disk budget)"),
            Err(e) => warn!(path = %path.display(), error = %e, "pruning failed"),
        }
    }
    !plan.stop
}

fn list_recordings(dir: &Path, exclude: Option<&Path>) -> Vec<budget::RecordingFile> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let path = e.path();
            // Both containers count against the disk budget, whatever the
            // preset in force today wrote: a folder can hold a mix of them.
            if !matches!(path.extension().and_then(|x| x.to_str()), Some("mp4") | Some("mkv")) {
                return None;
            }
            if exclude.is_some_and(|x| x == path) {
                return None;
            }
            let meta = e.metadata().ok()?;
            let modified_secs =
                meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
            Some(budget::RecordingFile { path, bytes: meta.len(), modified_secs })
        })
        .collect()
}

/// `Relay 2026-09-10 21-15-03.mp4` from (y, mo, d, h, mi, s).
pub fn file_name(
    prefix: &str,
    t: (u16, u8, u8, u8, u8, u8),
    container: RecordingContainer,
) -> String {
    format!(
        "{prefix} {:04}-{:02}-{:02} {:02}-{:02}-{:02}.{}",
        t.0,
        t.1,
        t.2,
        t.3,
        t.4,
        t.5,
        container.extension()
    )
}

/// Avoid clobbering when two saves land in the same second.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    for i in 2..100 {
        let (stem, ext) = name.rsplit_once('.').unwrap_or((name, "mp4"));
        let alt = dir.join(format!("{stem} ({i}).{ext}"));
        if !alt.exists() {
            return alt;
        }
    }
    candidate
}

#[cfg(windows)]
fn local_time_parts() -> (u16, u8, u8, u8, u8, u8) {
    // SAFETY: plain struct out-parameter.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (t.wYear, t.wMonth as u8, t.wDay as u8, t.wHour as u8, t.wMinute as u8, t.wSecond as u8)
}

#[cfg(not(windows))]
fn local_time_parts() -> (u16, u8, u8, u8, u8, u8) {
    (1970, 1, 1, 0, 0, 0)
}

#[cfg(windows)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    // SAFETY: NUL-terminated path, out-parameter for the caller-available bytes.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(not(windows))]
fn free_space(_dir: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_sortable_and_explorer_safe() {
        let n = file_name("Relay", (2026, 9, 10, 21, 5, 3), RecordingContainer::Mp4);
        assert_eq!(n, "Relay 2026-09-10 21-05-03.mp4");
        let later = file_name("Relay", (2026, 9, 10, 21, 5, 4), RecordingContainer::Mp4);
        assert!(later > n, "lexicographic order follows time");
        assert!(!n.contains(':'), "no characters Windows rejects");
    }

    #[test]
    fn unique_path_suffixes_on_collision() {
        let dir = std::env::temp_dir().join(format!("relay-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = "Relay 2026-01-01 00-00-00.mp4";
        assert_eq!(unique_path(&dir, name), dir.join(name));
        std::fs::write(dir.join(name), b"x").unwrap();
        assert_eq!(unique_path(&dir, name), dir.join("Relay 2026-01-01 00-00-00 (2).mp4"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// End-to-end through the writer thread with a real temp dir: record two
    /// GOPs, save a replay, verify both files exist and are non-trivial.
    /// Run for each container: rolling recording and replay save must both
    /// work whichever one the preset selected.
    fn records_and_saves_replay_in(container: RecordingContainer) {
        let dir = std::env::temp_dir().join(format!(
            "relay-rec-{}-{}",
            std::process::id(),
            container.extension()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = RecordConfig {
            codec: crate::codec::VideoCodec::Hevc,
            dir: dir.clone(),
            width: 640,
            height: 480,
            audio: true,
            mic: true,
            replay_secs: 60,
            ring_max_bytes: 10 << 20,
            budget: DiskBudget { cap_bytes: 0, free_floor_bytes: 0 },
            roll_secs: 3600,
            record_on_start: true,
            container,
        };
        let rec = Recorder::start(cfg).unwrap();
        let f = 166_667i64;
        for i in 0..120i64 {
            rec.push_video(&mux::tests::key_or_p(i % 60 == 0, i as u8), i * f, i % 60 == 0);
            if i % 2 == 0 {
                rec.push_audio(0, &[0xAA, i as u8], i * f, 100_000);
                rec.push_audio(1, &[0xBB, i as u8], i * f, 100_000);
            }
        }
        rec.save_replay();
        drop(rec); // joins the thread, finalizing the file

        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        let names: Vec<String> =
            entries.iter().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(entries.len(), 2, "one recording + one replay: {names:?}");
        assert!(names.iter().any(|n| n.starts_with("Relay Replay ")), "{names:?}");
        let ext = format!(".{}", container.extension());
        assert!(names.iter().all(|n| n.ends_with(&ext)), "{names:?} should all end {ext}");
        for e in &entries {
            let data = std::fs::read(e.path()).unwrap();
            assert!(data.len() > 500, "{:?} only {} bytes", e.file_name(), data.len());
            match container {
                RecordingContainer::Mp4 => assert_eq!(&data[4..8], b"ftyp"),
                // EBML magic: every Matroska file starts with it.
                RecordingContainer::Mkv => assert_eq!(&data[..4], &[0x1A, 0x45, 0xDF, 0xA3]),
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writer_thread_records_and_saves_replay_mp4() {
        records_and_saves_replay_in(RecordingContainer::Mp4);
    }

    #[test]
    fn writer_thread_records_and_saves_replay_mkv() {
        records_and_saves_replay_in(RecordingContainer::Mkv);
    }
}
