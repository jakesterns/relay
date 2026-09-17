//! Matroska muxer for the recorded share bitstream: HEVC
//! (`V_MPEGH/ISO/HEVC`) and Opus (`A_OPUS`). The per-preset alternative to the
//! fragmented-MP4 muxer in [`super::mux`], offered for the reason OBS defaults
//! to MKV: a file cut off mid-write is still a usable file.
//!
//! Why it survives a crash where fMP4 does not: the `Segment` is written with
//! *unknown size*, and every `Cluster` is self-delimiting and carries its own
//! absolute timestamp, so a reader simply consumes clusters until the bytes
//! run out. Nothing structural waits until the end. The one field that does
//! get rewritten — `Duration` — is patched in place after *every* cluster
//! rather than at `finalize`, so even a killed process leaves a file whose
//! duration is at most one cluster stale.
//!
//! The fMP4 muxer, by contrast, can only write its `mfra` random-access index
//! once, in `finalize`. A recording that never reaches `finalize` has no
//! index, and without one the Windows video-editing API rejects the file
//! outright — measured both ways in `docs/dev/container-compat.md`.
//!
//! The elementary bitstream is identical in both containers: the same
//! length-prefixed NAL samples (`annexb::to_mp4_sample`) and the same `hvcC`
//! bytes, here as the track's `CodecPrivate`. Recording is a tee of the
//! share's encoder output, so switching container never re-encodes anything.
//!
//! Mirrors `Mp4Muxer`'s state machine exactly: `AwaitingKeyframe` (drop
//! non-key video, drop audio) → `Streaming` after the first keyframe (header +
//! tracks written, timeline rebased to that frame) → `Finalized`. Clusters
//! close on the same target duration as fMP4 fragments and always open on a
//! keyframe where one is available.

use std::io::{Seek, SeekFrom, Write};

use anyhow::{bail, Result};

use super::annexb;
use super::mux::{video_entry, AudioConfig, MuxConfig, MuxState, VideoEntry};

/// Matroska `TimestampScale`, in nanoseconds per tick. 100 000 ns = 0.1 ms:
/// fine enough that 60 fps frame times never collide, and coarse enough that a
/// one-second cluster's 16-bit relative block timestamps (±32767) cannot
/// overflow.
pub const TIMESTAMP_SCALE_NS: u64 = 100_000;

/// 100 ns units per `TimestampScale` tick.
const TICKS_PER_100NS: i64 = (TIMESTAMP_SCALE_NS / 100) as i64;

/// Opus decoder delay is carried explicitly in Matroska; 80 ms of pre-roll is
/// the value the spec recommends and what every other muxer writes.
const OPUS_SEEK_PREROLL_NS: u64 = 80_000_000;

struct PendingSample {
    data: Vec<u8>,
    /// Rebased PTS in 100 ns.
    pts: i64,
    key: bool,
    /// Matroska track number: [`TRACK_VIDEO`], or an audio track from
    /// [`audio_track_number`].
    track: u64,
}

pub struct MkvMuxer<W: Write + Seek> {
    w: W,
    cfg: MuxConfig,
    state: MuxState,
    t0: i64,
    /// Samples buffered for the open cluster, in arrival order.
    pending: Vec<PendingSample>,
    /// Rebased PTS the open cluster starts at.
    cluster_start: Option<i64>,
    /// Video AUs dropped while waiting for the first keyframe.
    pub dropped_awaiting_key: u64,
    bytes_written: u64,
    /// File offset of the `Duration` payload, patched in place as clusters
    /// close. Without a `Duration` the Windows video-editing API refuses the
    /// file exactly as it refuses an unindexed fMP4 — see
    /// `docs/dev/container-compat.md`.
    duration_at: u64,
    /// Highest rebased PTS seen, in 100 ns.
    max_pts: i64,
}

impl<W: Write + Seek> MkvMuxer<W> {
    pub fn new(w: W, cfg: MuxConfig) -> Self {
        Self {
            w,
            cfg,
            state: MuxState::AwaitingKeyframe,
            t0: 0,
            pending: Vec::new(),
            cluster_start: None,
            dropped_awaiting_key: 0,
            bytes_written: 0,
            duration_at: 0,
            max_pts: 0,
        }
    }

    pub fn state(&self) -> MuxState {
        self.state
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Feed one Annex B access unit. The first accepted AU must be a keyframe
    /// carrying VPS/SPS/PPS; earlier non-key AUs are counted and dropped.
    pub fn push_video(&mut self, annexb_au: &[u8], pts_100ns: i64, keyframe: bool) -> Result<()> {
        match self.state {
            MuxState::Finalized => bail!("muxer already finalized"),
            MuxState::AwaitingKeyframe => {
                if !keyframe {
                    self.dropped_awaiting_key += 1;
                    return Ok(());
                }
                let entry = video_entry(self.cfg.codec, annexb_au)?;
                let (head, duration_at) = header(&self.cfg, &entry);
                self.w.write_all(&head)?;
                self.bytes_written += head.len() as u64;
                self.duration_at = duration_at as u64;
                self.t0 = pts_100ns;
                self.state = MuxState::Streaming;
            }
            MuxState::Streaming => {}
        }
        let pts = pts_100ns - self.t0;
        // Close the open cluster when this frame would take it past target.
        // Prefer a keyframe boundary so every cluster opens on a seek point,
        // which is what makes a truncated file recoverable frame-accurately.
        if let Some(start) = self.cluster_start {
            let over = pts - start >= self.cfg.fragment_100ns;
            if over && keyframe {
                self.flush_cluster()?;
            } else if pts - start >= self.cfg.fragment_100ns * 2 {
                // No keyframe arrived in time; cut anyway rather than let the
                // relative block timestamps grow past what i16 can hold.
                self.flush_cluster()?;
            }
        }
        if self.cluster_start.is_none() {
            self.cluster_start = Some(pts);
        }
        self.pending.push(PendingSample {
            data: annexb::to_mp4_sample_for(self.cfg.codec, annexb_au),
            pts,
            key: keyframe,
            track: TRACK_VIDEO,
        });
        Ok(())
    }

    /// Feed one Opus packet to audio track `track` (0 = program mix, 1 = mic).
    /// Dropped (not an error) before the first video keyframe so the file
    /// always starts on a decodable picture, and dropped for a track this file
    /// does not carry — same contract as [`super::mux::Mp4Muxer::push_audio`].
    pub fn push_audio(
        &mut self,
        track: usize,
        packet: &[u8],
        pts_100ns: i64,
        _dur_100ns: i64,
    ) -> Result<()> {
        match self.state {
            MuxState::Finalized => bail!("muxer already finalized"),
            MuxState::AwaitingKeyframe => return Ok(()),
            MuxState::Streaming => {}
        }
        if track >= self.cfg.audio.len() {
            return Ok(());
        }
        let pts = (pts_100ns - self.t0).max(0);
        if self.cluster_start.is_none() {
            self.cluster_start = Some(pts);
        }
        self.pending.push(PendingSample {
            data: packet.to_vec(),
            pts,
            key: true, // every Opus packet is independently decodable
            track: audio_track_number(track),
        });
        Ok(())
    }

    /// Close the file. Only the `Duration` is patched — the `Segment` keeps
    /// its unknown size — so a crash instead of this call costs at most the
    /// open cluster and leaves the duration stale by the same amount.
    pub fn finalize(mut self) -> Result<W> {
        if self.state == MuxState::Streaming {
            self.flush_cluster()?;
            self.w.flush()?;
        }
        self.state = MuxState::Finalized;
        Ok(self.w)
    }

    fn flush_cluster(&mut self) -> Result<()> {
        let Some(start) = self.cluster_start.take() else {
            return Ok(());
        };
        if self.pending.is_empty() {
            return Ok(());
        }
        let samples = std::mem::take(&mut self.pending);
        self.max_pts = samples.iter().map(|s| s.pts).max().unwrap_or(0).max(self.max_pts);
        let c = cluster(start, &samples);
        self.w.write_all(&c)?;
        self.bytes_written += c.len() as u64;
        self.patch_duration()?;
        Ok(())
    }

    /// Rewrite the `Duration` in place after each cluster, so the value on
    /// disk is never more than one cluster behind what the file contains. A
    /// crashed recording therefore still carries an honest duration, which is
    /// the difference between an editor importing it and rejecting it.
    fn patch_duration(&mut self) -> Result<()> {
        let ticks = (self.max_pts / TICKS_PER_100NS) as f64;
        let here = self.w.stream_position()?;
        self.w.seek(SeekFrom::Start(self.duration_at))?;
        self.w.write_all(&ticks.to_be_bytes())?;
        self.w.seek(SeekFrom::Start(here))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// EBML writing
// ---------------------------------------------------------------------------

/// EBML element IDs, written as the literal big-endian bytes the spec gives
/// (the ID's own length marker is part of the value).
mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const EBML_VERSION: u32 = 0x4286;
    pub const EBML_READ_VERSION: u32 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_VERSION: u32 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;

    pub const SEGMENT: u32 = 0x1853_8067;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
    pub const DURATION: u32 = 0x4489;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;

    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const NAME: u32 = 0x536E;
    pub const FLAG_LACING: u32 = 0x9C;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const CODEC_DELAY: u32 = 0x56AA;
    pub const SEEK_PRE_ROLL: u32 = 0x56BB;
    pub const DEFAULT_DURATION: u32 = 0x23E383;

    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;

    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;

    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
}

const TRACK_VIDEO: u64 = 1;

/// Audio track numbers follow the video track, in `cfg.audio` order — so
/// track 2 is the program mix and track 3 the microphone, matching the fMP4
/// muxer's track ids.
fn audio_track_number(index: usize) -> u64 {
    index as u64 + 2
}

/// Growing buffer of EBML elements. `open` writes the ID and reserves a
/// fixed-width 8-byte size so `close` can patch it without moving any bytes —
/// deterministic, and no second pass over the payload.
struct Ebml {
    buf: Vec<u8>,
}

impl Ebml {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn id(&mut self, id: u32) {
        // Element IDs are stored as their significant bytes, big-endian.
        let bytes = id.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap_or(3);
        self.buf.extend_from_slice(&bytes[first..]);
    }

    /// Open an element with a patchable 8-byte size field.
    fn open(&mut self, id_: u32) -> usize {
        self.id(id_);
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0; 8]);
        at
    }

    fn close(&mut self, at: usize) {
        let size = (self.buf.len() - at - 8) as u64;
        // 8-byte VINT: marker bit in the top byte, then a 56-bit length.
        let v = size | (1u64 << 56);
        self.buf[at..at + 8].copy_from_slice(&v.to_be_bytes());
    }

    /// Open an element of *unknown* size — the all-ones VINT. Used for the
    /// `Segment`, which is what lets a reader keep consuming clusters out of a
    /// file whose writer never got to finish.
    fn open_unknown(&mut self, id_: u32) {
        self.id(id_);
        self.buf.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    /// A data size as the shortest legal VINT.
    fn size(&mut self, n: u64) {
        for len in 1..=8u32 {
            let max = (1u64 << (7 * len)) - 1;
            if n < max {
                let v = n | (1u64 << (7 * len));
                self.buf.extend_from_slice(&v.to_be_bytes()[(8 - len as usize)..]);
                return;
            }
        }
        unreachable!("element larger than 2^56 bytes");
    }

    fn elem(&mut self, id_: u32, payload: &[u8]) {
        self.id(id_);
        self.size(payload.len() as u64);
        self.bytes(payload);
    }

    fn uint(&mut self, id_: u32, v: u64) {
        let be = v.to_be_bytes();
        let first = be.iter().position(|b| *b != 0).unwrap_or(7);
        self.elem(id_, &be[first..]);
    }

    fn string(&mut self, id_: u32, v: &str) {
        self.elem(id_, v.as_bytes());
    }

    #[allow(dead_code)]
    fn float64(&mut self, id_: u32, v: f64) {
        self.elem(id_, &v.to_be_bytes());
    }
}

/// EBML header + the open `Segment`, its `Info` and its `Tracks`. Returns the
/// bytes and the offset of the `Duration` payload within them, which is also
/// its offset in the file: the header is the first thing written.
fn header(cfg: &MuxConfig, video: &VideoEntry) -> (Vec<u8>, usize) {
    let mut e = Ebml::new();

    let ebml = e.open(id::EBML);
    e.uint(id::EBML_VERSION, 1);
    e.uint(id::EBML_READ_VERSION, 1);
    e.uint(id::EBML_MAX_ID_LENGTH, 4);
    e.uint(id::EBML_MAX_SIZE_LENGTH, 8);
    e.string(id::DOC_TYPE, "matroska");
    e.uint(id::DOC_TYPE_VERSION, 4);
    e.uint(id::DOC_TYPE_READ_VERSION, 2);
    e.close(ebml);

    // Unknown size: the segment stays open forever, so a truncated file is
    // still a structurally valid one.
    e.open_unknown(id::SEGMENT);

    let info = e.open(id::INFO);
    e.uint(id::TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS);
    // Fixed-width so it can be patched in place as the recording grows.
    e.id(id::DURATION);
    e.size(8);
    let duration_at = e.buf.len();
    e.bytes(&0f64.to_be_bytes());
    e.string(id::MUXING_APP, "Relay");
    e.string(id::WRITING_APP, "Relay");
    e.close(info);

    let tracks = e.open(id::TRACKS);
    {
        let t = e.open(id::TRACK_ENTRY);
        e.uint(id::TRACK_NUMBER, TRACK_VIDEO);
        e.uint(id::TRACK_UID, TRACK_VIDEO);
        e.uint(id::TRACK_TYPE, 1); // video
        e.uint(id::FLAG_LACING, 0);
        e.string(id::CODEC_ID, video.matroska_codec_id);
        e.elem(id::CODEC_PRIVATE, &video.config);
        let v = e.open(id::VIDEO);
        e.uint(id::PIXEL_WIDTH, cfg.width as u64);
        e.uint(id::PIXEL_HEIGHT, cfg.height as u64);
        e.close(v);
        e.close(t);
    }
    for (i, a) in cfg.audio.iter().enumerate() {
        let n = audio_track_number(i);
        let t = e.open(id::TRACK_ENTRY);
        e.uint(id::TRACK_NUMBER, n);
        e.uint(id::TRACK_UID, n);
        e.uint(id::TRACK_TYPE, 2); // audio
        e.uint(id::FLAG_LACING, 0);
        // Matroska has a real track name, so the program mix and the
        // microphone are labelled rather than left as "Track 2"/"Track 3".
        e.string(id::NAME, a.name);
        e.string(id::CODEC_ID, "A_OPUS");
        e.elem(id::CODEC_PRIVATE, &opus_head(a));
        // Matroska carries Opus pre-skip as an explicit delay in nanoseconds.
        e.uint(id::CODEC_DELAY, u64::from(a.pre_skip) * 1_000_000_000 / 48_000);
        e.uint(id::SEEK_PRE_ROLL, OPUS_SEEK_PREROLL_NS);
        e.uint(id::DEFAULT_DURATION, 20_000_000); // 20 ms frames
        let au = e.open(id::AUDIO);
        e.float64(id::SAMPLING_FREQUENCY, a.sample_rate as f64);
        e.uint(id::CHANNELS, u64::from(a.channels));
        e.close(au);
        e.close(t);
    }
    e.close(tracks);

    (e.buf, duration_at)
}

/// The 19-byte `OpusHead` identification header, Matroska's `CodecPrivate` for
/// `A_OPUS`. Same fields as the fMP4 `dOps` box, little-endian per RFC 7845.
fn opus_head(a: &AudioConfig) -> Vec<u8> {
    let mut v = Vec::with_capacity(19);
    v.extend_from_slice(b"OpusHead");
    v.push(1); // version
    v.push(a.channels);
    v.extend_from_slice(&a.pre_skip.to_le_bytes());
    v.extend_from_slice(&a.sample_rate.to_le_bytes());
    v.extend_from_slice(&0i16.to_le_bytes()); // output gain
    v.push(0); // channel mapping family
    v
}

/// One self-delimiting `Cluster`: an absolute timestamp plus a `SimpleBlock`
/// per sample, each timed relative to it.
fn cluster(start_100ns: i64, samples: &[PendingSample]) -> Vec<u8> {
    let mut e = Ebml::new();
    let c = e.open(id::CLUSTER);
    let base = start_100ns / TICKS_PER_100NS;
    e.uint(id::TIMESTAMP, base.max(0) as u64);
    for s in samples {
        let rel = (s.pts / TICKS_PER_100NS) - base;
        let rel = rel.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
        let track = s.track;

        // SimpleBlock payload: track number VINT, i16 relative timestamp,
        // flags, then the frame.
        let mut blk = Vec::with_capacity(s.data.len() + 5);
        blk.push(0x80 | track as u8); // 1-byte VINT, tracks 1 and 2 only
        blk.extend_from_slice(&rel.to_be_bytes());
        blk.push(if s.key { 0x80 } else { 0x00 }); // keyframe flag
        blk.extend_from_slice(&s.data);
        e.elem(id::SIMPLE_BLOCK, &blk);
    }
    e.close(c);
    e.buf
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::super::mux::tests::{key_au, key_or_p, p_au};
    use super::*;

    fn build(cfg: MuxConfig, frames: i64) -> Vec<u8> {
        let mut m = MkvMuxer::new(Cursor::new(Vec::new()), cfg);
        let f = 166_667i64;
        let base = 123_456_789i64; // non-zero start proves rebasing
        m.push_video(&p_au(9), base - f, false).unwrap(); // dropped
        m.push_video(&key_au(), base, true).unwrap();
        for i in 1..=frames {
            m.push_audio(0, &[0xB0, i as u8], base + (i - 1) * 100_000, 100_000).unwrap();
            m.push_video(&key_or_p(i % 3 == 0, i as u8), base + i * f, i % 3 == 0).unwrap();
        }
        m.finalize().unwrap().into_inner()
    }

    /// Walk the top-level elements so the tests can talk about structure
    /// rather than offsets. Returns (name-ish id, size or None for unknown).
    fn top_level(d: &[u8]) -> Vec<(u32, Option<u64>)> {
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + 2 <= d.len() {
            let lead = d[i];
            if lead == 0 {
                break;
            }
            let id_len = 8 - lead.ilog2() as usize;
            let mut id_ = 0u32;
            for k in 0..id_len {
                id_ = (id_ << 8) | u32::from(d[i + k]);
            }
            let j = i + id_len;
            let slead = d[j];
            let sz_len = 8 - slead.ilog2() as usize;
            let mask = if sz_len >= 8 { 0u8 } else { 0xFFu8 >> sz_len };
            let mut sz = u64::from(slead & mask);
            for k in 1..sz_len {
                sz = (sz << 8) | u64::from(d[j + k]);
            }
            let unknown = sz == (1u64 << (7 * sz_len)) - 1;
            let body = j + sz_len;
            out.push((id_, if unknown { None } else { Some(sz) }));
            if id_ == id::SEGMENT {
                // Descend: the segment is open-ended, its children follow.
                i = body;
                continue;
            }
            i = body + sz as usize;
        }
        out
    }

    #[test]
    fn state_machine_waits_for_a_keyframe() {
        let mut m = MkvMuxer::new(Cursor::new(Vec::new()), MuxConfig::with_opus(1920, 1080));
        assert_eq!(m.state(), MuxState::AwaitingKeyframe);
        m.push_video(&p_au(1), 0, false).unwrap();
        m.push_audio(0, b"opus", 0, 100_000).unwrap();
        assert_eq!(m.dropped_awaiting_key, 1);
        assert_eq!(m.bytes_written(), 0, "nothing written before the first keyframe");

        m.push_video(&key_au(), 1_000_000, true).unwrap();
        assert_eq!(m.state(), MuxState::Streaming);
        assert!(m.bytes_written() > 0, "header written on first keyframe");
    }

    /// Both sources in one MKV: two audio TrackEntries, both named, and every
    /// packet landing on its own track number.
    #[test]
    fn mic_track_is_a_second_audio_track_in_the_same_file() {
        let mut m =
            MkvMuxer::new(Cursor::new(Vec::new()), MuxConfig::with_opus_and_mic(1920, 1080));
        m.push_video(&key_au(), 0, true).unwrap();
        for i in 0..4i64 {
            m.push_audio(0, &[0xB0, i as u8], i * 100_000, 100_000).unwrap();
            m.push_audio(1, &[0xC0, i as u8], i * 100_000, 100_000).unwrap();
        }
        let out = m.finalize().unwrap().into_inner();
        let count = |n: &[u8]| out.windows(n.len()).filter(|w| *w == n).count();
        assert_eq!(count(super::super::mux::PROGRAM_TRACK_NAME.as_bytes()), 1);
        assert_eq!(count(super::super::mux::MIC_TRACK_NAME.as_bytes()), 1);
        for i in 0..4u8 {
            assert!(count(&[0xB0, i]) > 0, "program packet {i} in the file");
            assert!(count(&[0xC0, i]) > 0, "mic packet {i} in the file");
        }
        // Three TrackEntry elements: video, program mix, microphone.
        assert_eq!(count(&[0xAE, 0x01]), 3, "one TrackEntry per track");
    }

    /// A packet for a track this file does not carry is dropped, not an error
    /// and not misfiled onto the program track.
    #[test]
    fn audio_for_a_track_the_file_lacks_is_dropped() {
        let mut m = MkvMuxer::new(Cursor::new(Vec::new()), MuxConfig::with_opus(1920, 1080));
        m.push_video(&key_au(), 0, true).unwrap();
        m.push_audio(1, b"mic-packet-nowhere-to-go", 0, 100_000).unwrap();
        let out = m.finalize().unwrap().into_inner();
        let needle = b"mic-packet-nowhere-to-go";
        assert_eq!(out.windows(needle.len()).filter(|w| *w == needle).count(), 0);
    }

    #[test]
    fn keyframe_without_param_sets_is_an_error() {
        let mut m = MkvMuxer::new(Cursor::new(Vec::new()), MuxConfig::video_only(640, 480));
        let mut au = vec![0, 0, 0, 1];
        au.extend_from_slice(&super::super::mux::tests::nal(19, b"idr-but-bare"));
        assert!(m.push_video(&au, 0, true).is_err());
    }

    #[test]
    fn finalized_muxer_rejects_input() {
        let mut m = MkvMuxer::new(Cursor::new(Vec::new()), MuxConfig::video_only(640, 480));
        m.push_video(&key_au(), 0, true).unwrap();
        let mut m2 = MkvMuxer::new(m.finalize().unwrap(), MuxConfig::video_only(640, 480));
        m2.state = MuxState::Finalized;
        assert!(m2.push_video(&key_au(), 0, true).is_err());
        assert!(m2.push_audio(0, b"x", 0, 1).is_err());
    }

    #[test]
    fn video_only_config_ignores_audio() {
        let mut m = MkvMuxer::new(Cursor::new(Vec::new()), MuxConfig::video_only(640, 480));
        m.push_video(&key_au(), 0, true).unwrap();
        m.push_audio(0, b"opus", 0, 100_000).unwrap();
        let out = m.finalize().unwrap().into_inner();
        // Exactly one TrackEntry: no audio track was declared.
        let entries = out.windows(2).filter(|w| w == b"\xAE\x01").count();
        assert_eq!(entries, 1, "video-only files declare one track");
    }

    /// The property the whole container choice rests on: the `Segment` never
    /// gets a size, so a file that stops mid-write is still structurally whole.
    #[test]
    fn segment_size_stays_unknown_even_after_finalize() {
        let out = build(MuxConfig::with_opus(2560, 1440), 4);
        let top = top_level(&out);
        let seg = top.iter().find(|(id_, _)| *id_ == id::SEGMENT).expect("a Segment");
        assert_eq!(seg.1, None, "Segment must keep the unknown-size VINT");
        assert!(
            top.iter().any(|(id_, _)| *id_ == id::CLUSTER),
            "clusters follow the segment header"
        );
    }

    /// Without a `Duration` the Windows video-editing API rejects the file;
    /// with one it imports. It must track the content, not stay at zero.
    #[test]
    fn duration_is_patched_to_match_the_content() {
        let mut cfg = MuxConfig::with_opus(1920, 1080);
        cfg.fragment_100ns = 500_000; // 50 ms clusters, so several get written
        let out = build(cfg, 12);

        let at = out.windows(2).position(|w| w == b"\x44\x89").expect("a Duration element");
        assert_eq!(out[at + 2], 0x88, "fixed 8-byte payload so it can be patched in place");
        let ticks = f64::from_be_bytes(out[at + 3..at + 11].try_into().unwrap());

        // 12 frames at 60 fps ≈ 200 ms; ticks are 0.1 ms.
        let secs = ticks * TIMESTAMP_SCALE_NS as f64 / 1e9;
        assert!(ticks > 0.0, "duration must not stay at the placeholder");
        assert!((secs - 0.2).abs() < 0.02, "duration {secs}s should be about 0.2s");
    }

    #[test]
    fn clusters_open_on_keyframes_and_rebase_to_the_first() {
        let mut cfg = MuxConfig::with_opus(1920, 1080);
        cfg.fragment_100ns = 500_000;
        let out = build(cfg, 12);
        let clusters = top_level(&out).into_iter().filter(|(id_, _)| *id_ == id::CLUSTER).count();
        assert!(clusters >= 2, "12 frames at 50 ms target should span several clusters");

        // The first cluster's Timestamp must be 0: the timeline is rebased to
        // the first keyframe just as the fMP4 muxer rebases its tfdt.
        let c = out.windows(4).position(|w| w == b"\x1F\x43\xB6\x75").unwrap();
        let ts = out[c + 4 + 8..].iter().position(|b| *b == 0xE7).unwrap() + c + 12;
        assert_eq!(out[ts], 0xE7);
        let len = (out[ts + 1] & 0x7F) as usize;
        let v: u64 = out[ts + 2..ts + 2 + len].iter().fold(0, |a, b| (a << 8) | u64::from(*b));
        assert_eq!(v, 0, "first cluster starts at zero");
    }

    #[test]
    fn deterministic_and_matches_golden_fixture() {
        let a = build(MuxConfig::with_opus(2560, 1440), 4);
        assert_eq!(a, build(MuxConfig::with_opus(2560, 1440), 4), "same input, same bytes");

        let golden_path =
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden-recording.mkv");
        if std::env::var("UPDATE_GOLDEN").is_ok() {
            std::fs::write(golden_path, &a).unwrap();
            return;
        }
        let golden = std::fs::read(golden_path).expect(
            "golden fixture missing — regenerate with \
             UPDATE_GOLDEN=1 cargo test -p relay-capture golden",
        );
        assert_eq!(a, golden, "muxer bytes drifted from the golden fixture");
    }

    #[test]
    fn h264_track_is_mpeg4_avc_with_avcc_private_data() {
        use super::super::mux::tests::{h264_key_au, h264_p_au};
        let cfg = MuxConfig::with_opus(1920, 1080).with_codec(crate::codec::VideoCodec::H264);
        let mut m = MkvMuxer::new(Cursor::new(Vec::new()), cfg);
        m.push_video(&h264_key_au(), 0, true).unwrap();
        m.push_video(&h264_p_au(1), 166_666, false).unwrap();
        let out = m.finalize().unwrap().into_inner();
        let find = |needle: &[u8]| out.windows(needle.len()).position(|w| w == needle);
        assert!(find(b"V_MPEG4/ISO/AVC").is_some());
        assert!(find(b"V_MPEGH/ISO/HEVC").is_none());
        // CodecPrivate is the avcC record: version 1, High, level 5.2.
        assert!(find(&[1, 100, 0, 52, 0xFF, 0xE1]).is_some());
        assert!(
            find(&[0, 0, 0, 4, 0x65, 0x88, 0x84, 0x21]).is_some(),
            "IDR block, length-prefixed"
        );
    }
}
