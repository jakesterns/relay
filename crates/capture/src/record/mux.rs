//! Fragmented-MP4 muxer for the recorded share bitstream: HEVC (`hvc1` +
//! `hvcC`) and Opus (`dOps`). Pure and byte-deterministic — the same inputs
//! always produce the same file, which is what the golden-fixture tests lock.
//!
//! State machine: `AwaitingKeyframe` (drop non-key video, drop audio) →
//! `Streaming` after the first keyframe (init segment written, timeline
//! rebased to that frame) → `Finalized`. While streaming, samples buffer into
//! the current fragment; a fragment closes when the *next* video sample would
//! make it longer than the target duration (so every sample's duration is
//! known from its successor — no guessing). Every closed fragment is a valid
//! `moof`+`mdat` pair, so a crash loses at most the open fragment.

use std::io::Write;

use anyhow::{bail, Context, Result};

use super::annexb;

/// 100 ns units per second — the video track timescale, chosen so encoder
/// PTS values (QPC 100 ns) map exactly with no rounding.
pub const VIDEO_TIMESCALE: u32 = 10_000_000;

#[derive(Debug, Clone)]
pub struct MuxConfig {
    pub width: u32,
    pub height: u32,
    /// `None` = video-only file.
    pub audio: Option<AudioConfig>,
    /// Target fragment duration in 100 ns units (default 1 s).
    pub fragment_100ns: i64,
}

#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u8,
    /// Opus pre-skip in 48 kHz samples (0 is fine for a live stream).
    pub pre_skip: u16,
}

impl MuxConfig {
    pub fn video_only(width: u32, height: u32) -> Self {
        Self { width, height, audio: None, fragment_100ns: 10_000_000 }
    }

    pub fn with_opus(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            audio: Some(AudioConfig { sample_rate: 48_000, channels: 2, pre_skip: 0 }),
            fragment_100ns: 10_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxState {
    AwaitingKeyframe,
    Streaming,
    Finalized,
}

struct PendingSample {
    data: Vec<u8>,
    /// Rebased PTS in 100 ns.
    pts: i64,
    /// Known duration (audio); video durations come from the successor.
    dur: Option<i64>,
    key: bool,
}

/// One random-access point, for the `tfra` index written at finalize.
struct SyncPoint {
    /// Decode time in the *track's* timescale.
    time: u64,
    /// Byte offset of the enclosing `moof` from the start of the file.
    moof_offset: u64,
    /// 1-based index of the `traf` within that `moof`.
    traf: u32,
    /// 1-based index of the sample within the `trun`.
    sample: u32,
}

pub struct Mp4Muxer<W: Write> {
    w: W,
    cfg: MuxConfig,
    state: MuxState,
    t0: i64,
    seq: u32,
    video: Vec<PendingSample>,
    audio: Vec<PendingSample>,
    /// Video AUs dropped while waiting for the first keyframe.
    pub dropped_awaiting_key: u64,
    bytes_written: u64,
    last_video_dur: i64,
    /// Random-access points per track, accumulated as fragments are written
    /// and emitted as `mfra` by `finalize`. Without this index the Windows
    /// video-editing API (`Windows.Media.Editing.MediaClip`) refuses the file
    /// outright — see `docs/dev/container-compat.md`.
    sync_video: Vec<SyncPoint>,
    sync_audio: Vec<SyncPoint>,
}

impl<W: Write> Mp4Muxer<W> {
    pub fn new(w: W, cfg: MuxConfig) -> Self {
        Self {
            w,
            cfg,
            state: MuxState::AwaitingKeyframe,
            t0: 0,
            seq: 0,
            video: Vec::new(),
            audio: Vec::new(),
            dropped_awaiting_key: 0,
            bytes_written: 0,
            last_video_dur: 166_667, // 60 fps until the stream says otherwise
            sync_video: Vec::new(),
            sync_audio: Vec::new(),
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
                let ps = annexb::extract_param_sets(annexb_au)
                    .context("keyframe carries no VPS/SPS/PPS")?;
                let sps = annexb::parse_sps_summary(&ps.sps).context("unparseable SPS")?;
                let init = init_segment(&self.cfg, &ps, &sps);
                self.w.write_all(&init)?;
                self.bytes_written += init.len() as u64;
                self.t0 = pts_100ns;
                self.state = MuxState::Streaming;
            }
            MuxState::Streaming => {}
        }
        let pts = pts_100ns - self.t0;
        // The newcomer fixes the previous sample's duration; if the fragment
        // is now over target, close it before this sample starts the next.
        if let Some(prev) = self.video.last_mut() {
            prev.dur = Some((pts - prev.pts).max(1));
        }
        if let (Some(first), Some(_)) = (self.video.first(), self.video.last()) {
            if pts - first.pts >= self.cfg.fragment_100ns {
                self.flush_fragment(pts)?;
            }
        }
        self.video.push(PendingSample {
            data: annexb::to_mp4_sample(annexb_au),
            pts,
            dur: None,
            key: keyframe,
        });
        Ok(())
    }

    /// Feed one Opus packet. Dropped (not an error) before the first video
    /// keyframe so the file always starts on a decodable picture.
    pub fn push_audio(&mut self, packet: &[u8], pts_100ns: i64, dur_100ns: i64) -> Result<()> {
        match self.state {
            MuxState::Finalized => bail!("muxer already finalized"),
            MuxState::AwaitingKeyframe => return Ok(()),
            MuxState::Streaming => {}
        }
        if self.cfg.audio.is_none() {
            return Ok(());
        }
        self.audio.push(PendingSample {
            data: packet.to_vec(),
            pts: (pts_100ns - self.t0).max(0),
            dur: Some(dur_100ns),
            key: true,
        });
        Ok(())
    }

    /// Close the file: give the last video sample its running duration and
    /// flush the open fragment.
    pub fn finalize(mut self) -> Result<W> {
        if self.state == MuxState::Streaming {
            if let Some(last) = self.video.last_mut() {
                if last.dur.is_none() {
                    last.dur = Some(self.last_video_dur);
                }
            }
            // Flush everything pending: audio cut-off = end of time.
            self.flush_fragment(i64::MAX)?;
            // The random-access index goes last, so a crash before this point
            // simply leaves an unindexed — but still playable — file.
            let mfra = mfra(&self.sync_video, &self.sync_audio);
            self.w.write_all(&mfra)?;
            self.bytes_written += mfra.len() as u64;
            self.w.flush()?;
        }
        self.state = MuxState::Finalized;
        Ok(self.w)
    }

    /// Write the pending samples as one `moof`+`mdat`. Audio up to
    /// `until_pts` goes with them; later packets wait for the next fragment.
    fn flush_fragment(&mut self, until_pts: i64) -> Result<()> {
        if self.video.is_empty() {
            return Ok(());
        }
        if let Some(d) = self.video.last().and_then(|s| s.dur) {
            self.last_video_dur = d;
        }
        let split = self.audio.iter().position(|a| a.pts >= until_pts).unwrap_or(self.audio.len());
        let audio: Vec<PendingSample> = self.audio.drain(..split).collect();
        let video = std::mem::take(&mut self.video);

        // Index this fragment's random-access points before writing it, while
        // `bytes_written` still points at the `moof` about to go out.
        let moof_offset = self.bytes_written;
        for (i, s) in video.iter().enumerate() {
            if s.key {
                self.sync_video.push(SyncPoint {
                    time: to_timescale(s.pts, VIDEO_TIMESCALE),
                    moof_offset,
                    traf: 1,
                    sample: i as u32 + 1,
                });
            }
        }
        // Every Opus packet is a random-access point; one entry per fragment
        // is enough to seek by and keeps the index small. The audio `traf` is
        // always written second, after the video one.
        if let (Some(a), Some(first)) = (&self.cfg.audio, audio.first()) {
            self.sync_audio.push(SyncPoint {
                time: to_timescale(first.pts, a.sample_rate),
                moof_offset,
                traf: 2,
                sample: 1,
            });
        }

        self.seq += 1;
        let frag = fragment(&self.cfg, self.seq, &video, &audio);
        self.w.write_all(&frag)?;
        self.bytes_written += frag.len() as u64;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Box building
// ---------------------------------------------------------------------------

/// Growing buffer with nested ISO-BMFF boxes; `open` returns a token that
/// `close` uses to patch the 32-bit size.
struct Boxes {
    buf: Vec<u8>,
}

impl Boxes {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn open(&mut self, fourcc: &[u8; 4]) -> usize {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0; 4]);
        self.buf.extend_from_slice(fourcc);
        at
    }

    fn full(&mut self, fourcc: &[u8; 4], version: u8, flags: u32) -> usize {
        let at = self.open(fourcc);
        self.buf.push(version);
        self.buf.extend_from_slice(&flags.to_be_bytes()[1..]);
        at
    }

    fn close(&mut self, at: usize) {
        let size = (self.buf.len() - at) as u32;
        self.buf[at..at + 4].copy_from_slice(&size.to_be_bytes());
    }

    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }
}

const MATRIX_IDENTITY: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// `ftyp` + `moov` for this stream configuration.
fn init_segment(cfg: &MuxConfig, ps: &annexb::ParamSets, sps: &annexb::SpsSummary) -> Vec<u8> {
    let mut b = Boxes::new();

    let ftyp = b.open(b"ftyp");
    b.bytes(b"isom");
    b.u32(512);
    b.bytes(b"isomiso5iso6mp41");
    b.close(ftyp);

    let moov = b.open(b"moov");
    {
        let mvhd = b.full(b"mvhd", 0, 0);
        b.u32(0); // creation (deterministic: epoch)
        b.u32(0); // modification
        b.u32(1000); // timescale
        b.u32(0); // duration unknown (fragmented)
        b.u32(0x0001_0000); // rate 1.0
        b.u16(0x0100); // volume 1.0
        b.zeros(10);
        for m in MATRIX_IDENTITY {
            b.u32(m);
        }
        b.zeros(24); // pre_defined
        b.u32(u32::from(cfg.audio.is_some()) + 2); // next_track_ID
        b.close(mvhd);

        video_trak(&mut b, cfg, ps, sps);
        if let Some(a) = &cfg.audio {
            audio_trak(&mut b, a);
        }

        let mvex = b.open(b"mvex");
        for track_id in 1..=(1 + u32::from(cfg.audio.is_some())) {
            let trex = b.full(b"trex", 0, 0);
            b.u32(track_id);
            b.u32(1); // default_sample_description_index
            b.u32(0);
            b.u32(0);
            b.u32(0);
            b.close(trex);
        }
        b.close(mvex);
    }
    b.close(moov);
    b.buf
}

fn video_trak(b: &mut Boxes, cfg: &MuxConfig, ps: &annexb::ParamSets, sps: &annexb::SpsSummary) {
    let trak = b.open(b"trak");
    {
        let tkhd = b.full(b"tkhd", 0, 3); // enabled + in movie
        b.u32(0);
        b.u32(0);
        b.u32(1); // track id
        b.u32(0); // reserved
        b.u32(0); // duration
        b.zeros(8);
        b.u16(0); // layer
        b.u16(0); // alternate group
        b.u16(0); // volume (video)
        b.u16(0);
        for m in MATRIX_IDENTITY {
            b.u32(m);
        }
        b.u32(cfg.width << 16);
        b.u32(cfg.height << 16);
        b.close(tkhd);

        let mdia = b.open(b"mdia");
        {
            let mdhd = b.full(b"mdhd", 0, 0);
            b.u32(0);
            b.u32(0);
            b.u32(VIDEO_TIMESCALE);
            b.u32(0);
            b.u16(0x55C4); // 'und'
            b.u16(0);
            b.close(mdhd);

            hdlr(b, b"vide", b"Relay Video\0");

            let minf = b.open(b"minf");
            {
                let vmhd = b.full(b"vmhd", 0, 1);
                b.zeros(8); // graphicsmode + opcolor
                b.close(vmhd);
                dinf(b);
                let stbl = b.open(b"stbl");
                {
                    let stsd = b.full(b"stsd", 0, 0);
                    b.u32(1);
                    hvc1(b, cfg, ps, sps);
                    b.close(stsd);
                    empty_stbl_tail(b);
                }
                b.close(stbl);
            }
            b.close(minf);
        }
        b.close(mdia);
    }
    b.close(trak);
}

fn hvc1(b: &mut Boxes, cfg: &MuxConfig, ps: &annexb::ParamSets, sps: &annexb::SpsSummary) {
    let entry = b.open(b"hvc1");
    b.zeros(6); // reserved
    b.u16(1); // data_reference_index
    b.zeros(16); // pre_defined + reserved
    b.u16(cfg.width as u16);
    b.u16(cfg.height as u16);
    b.u32(0x0048_0000); // 72 dpi
    b.u32(0x0048_0000);
    b.u32(0);
    b.u16(1); // frame count
    b.zeros(32); // compressor name
    b.u16(0x0018); // depth
    b.i16(-1); // pre_defined

    let hvcc = b.open(b"hvcC");
    b.bytes(&hvcc_payload(ps, sps));
    b.close(hvcc);
    b.close(entry);
}

/// The `hvcC` decoder-configuration payload (everything after the box header).
/// Matroska carries the identical bytes as the video track's `CodecPrivate`,
/// so both containers describe the stream the same way.
pub(crate) fn hvcc_payload(ps: &annexb::ParamSets, sps: &annexb::SpsSummary) -> Vec<u8> {
    let mut b = Boxes::new();
    b.u8(1); // configurationVersion
    b.u8((sps.general_profile_space << 6) | (sps.general_tier_flag << 5) | sps.general_profile_idc);
    b.u32(sps.general_profile_compatibility_flags);
    b.bytes(&sps.general_constraint_indicator_flags.to_be_bytes()[2..8]);
    b.u8(sps.general_level_idc);
    b.u16(0xF000); // min_spatial_segmentation_idc = 0
    b.u8(0xFC); // parallelismType = 0
                // NV12 in, Main profile out: 4:2:0, 8-bit is fixed by the encoder setup.
    b.u8(0xFC | 1); // chroma_format_idc = 1
    b.u8(0xF8); // bit_depth_luma_minus8 = 0
    b.u8(0xF8); // bit_depth_chroma_minus8 = 0
    b.u16(0); // avgFrameRate unknown
    b.u8((1 << 3) | (1 << 2) | 3); // 1 temporal layer, nested, 4-byte lengths
    b.u8(3); // numOfArrays
    for (ty, nal) in
        [(annexb::NAL_VPS, &ps.vps), (annexb::NAL_SPS, &ps.sps), (annexb::NAL_PPS, &ps.pps)]
    {
        b.u8(0x80 | ty); // array_completeness = 1
        b.u16(1);
        b.u16(nal.len() as u16);
        b.bytes(nal);
    }
    b.buf
}

fn audio_trak(b: &mut Boxes, a: &AudioConfig) {
    let trak = b.open(b"trak");
    {
        let tkhd = b.full(b"tkhd", 0, 3);
        b.u32(0);
        b.u32(0);
        b.u32(2); // track id
        b.u32(0);
        b.u32(0);
        b.zeros(8);
        b.u16(0);
        b.u16(0);
        b.u16(0x0100); // volume 1.0
        b.u16(0);
        for m in MATRIX_IDENTITY {
            b.u32(m);
        }
        b.u32(0);
        b.u32(0);
        b.close(tkhd);

        let mdia = b.open(b"mdia");
        {
            let mdhd = b.full(b"mdhd", 0, 0);
            b.u32(0);
            b.u32(0);
            b.u32(a.sample_rate);
            b.u32(0);
            b.u16(0x55C4);
            b.u16(0);
            b.close(mdhd);

            hdlr(b, b"soun", b"Relay Audio\0");

            let minf = b.open(b"minf");
            {
                let smhd = b.full(b"smhd", 0, 0);
                b.u32(0); // balance + reserved
                b.close(smhd);
                dinf(b);
                let stbl = b.open(b"stbl");
                {
                    let stsd = b.full(b"stsd", 0, 0);
                    b.u32(1);
                    let entry = b.open(b"Opus");
                    b.zeros(6);
                    b.u16(1); // data_reference_index
                    b.zeros(8);
                    b.u16(a.channels as u16);
                    b.u16(16); // samplesize
                    b.u32(0);
                    b.u32(a.sample_rate << 16);
                    let dops = b.open(b"dOps");
                    b.u8(0); // Version
                    b.u8(a.channels);
                    b.u16(a.pre_skip);
                    b.u32(a.sample_rate);
                    b.i16(0); // OutputGain
                    b.u8(0); // ChannelMappingFamily
                    b.close(dops);
                    b.close(entry);
                    b.close(stsd);
                    empty_stbl_tail(b);
                }
                b.close(stbl);
            }
            b.close(minf);
        }
        b.close(mdia);
    }
    b.close(trak);
}

fn hdlr(b: &mut Boxes, handler: &[u8; 4], name: &[u8]) {
    let h = b.full(b"hdlr", 0, 0);
    b.u32(0);
    b.bytes(handler);
    b.zeros(12);
    b.bytes(name);
    b.close(h);
}

fn dinf(b: &mut Boxes) {
    let dinf = b.open(b"dinf");
    let dref = b.full(b"dref", 0, 0);
    b.u32(1);
    let url = b.full(b"url ", 0, 1); // self-contained
    b.close(url);
    b.close(dref);
    b.close(dinf);
}

/// Empty stts/stsc/stsz/stco — samples live in fragments.
fn empty_stbl_tail(b: &mut Boxes) {
    for fourcc in [b"stts", b"stsc"] {
        let x = b.full(fourcc, 0, 0);
        b.u32(0);
        b.close(x);
    }
    let stsz = b.full(b"stsz", 0, 0);
    b.u32(0);
    b.u32(0);
    b.close(stsz);
    let stco = b.full(b"stco", 0, 0);
    b.u32(0);
    b.close(stco);
}

const TRUN_FLAGS: u32 = 0x000701; // data-offset + duration + size + flags per sample
/// The `mfra` movie-fragment random-access box: one `tfra` per track that has
/// any random-access points, then `mfro` with the box's own total size so a
/// reader can find it by seeking to the end of the file.
fn mfra(video: &[SyncPoint], audio: &[SyncPoint]) -> Vec<u8> {
    let mut b = Boxes::new();
    let mfra = b.open(b"mfra");
    for (track_id, points) in [(1u32, video), (2u32, audio)] {
        if points.is_empty() {
            continue;
        }
        let tfra = b.full(b"tfra", 1, 0);
        b.u32(track_id);
        // 26 reserved bits, then 2 bits each of length-1 for traf / trun /
        // sample number: 0 = 1 byte, 3 = 4 bytes. Sample numbers can exceed
        // 255 in a long fragment, so give that field 4 bytes.
        b.u32(0b11);
        b.u32(points.len() as u32);
        for p in points {
            b.u64(p.time);
            b.u64(p.moof_offset);
            b.u8(p.traf as u8);
            b.u8(1); // trun number: one trun per traf
            b.u32(p.sample);
        }
        b.close(tfra);
    }
    let mfro = b.full(b"mfro", 0, 0);
    b.u32(0); // patched below: size of the whole mfra
    b.close(mfro);
    b.close(mfra);
    let total = b.buf.len() as u32;
    let at = b.buf.len() - 4;
    b.buf[at..].copy_from_slice(&total.to_be_bytes());
    b.buf
}

/// Rebased 100 ns PTS → a track timescale, saturating at zero.
fn to_timescale(pts_100ns: i64, timescale: u32) -> u64 {
    (pts_100ns.max(0) as u128 * timescale as u128 / 10_000_000u128) as u64
}

const SAMPLE_FLAG_SYNC: u32 = 0x0200_0000; // depends_on = no
const SAMPLE_FLAG_NON_SYNC: u32 = 0x0101_0000; // depends_on = yes, non-sync

/// One `moof` + `mdat` pair for the pending samples.
fn fragment(
    cfg: &MuxConfig,
    seq: u32,
    video: &[PendingSample],
    audio: &[PendingSample],
) -> Vec<u8> {
    let mut b = Boxes::new();
    let moof = b.open(b"moof");
    {
        let mfhd = b.full(b"mfhd", 0, 0);
        b.u32(seq);
        b.close(mfhd);
    }
    let mut offset_patches: Vec<usize> = Vec::new();
    traf(&mut b, 1, VIDEO_TIMESCALE, video, true, &mut offset_patches);
    if let Some(a) = &cfg.audio {
        if !audio.is_empty() {
            traf(&mut b, 2, a.sample_rate, audio, false, &mut offset_patches);
        }
    }
    b.close(moof);

    // Data offsets are from the start of the moof; mdat payload begins after
    // the moof plus the 8-byte mdat header.
    let mut data_offset = (b.buf.len() + 8) as u32;
    let mut patch = |b: &mut Boxes, at: usize, samples: &[PendingSample]| {
        b.buf[at..at + 4].copy_from_slice(&data_offset.to_be_bytes());
        data_offset += samples.iter().map(|s| s.data.len() as u32).sum::<u32>();
    };
    let mut patches = offset_patches.into_iter();
    if let Some(at) = patches.next() {
        patch(&mut b, at, video);
    }
    if let Some(at) = patches.next() {
        patch(&mut b, at, audio);
    }

    let mdat = b.open(b"mdat");
    for s in video.iter().chain(audio.iter()) {
        b.bytes(&s.data);
    }
    b.close(mdat);
    b.buf
}

fn traf(
    b: &mut Boxes,
    track_id: u32,
    timescale: u32,
    samples: &[PendingSample],
    video: bool,
    offset_patches: &mut Vec<usize>,
) {
    let to_ts = |pts_100ns: i64| -> u64 { to_timescale(pts_100ns, timescale) };
    let traf = b.open(b"traf");
    {
        let tfhd = b.full(b"tfhd", 0, 0x020000); // default-base-is-moof
        b.u32(track_id);
        b.close(tfhd);

        let tfdt = b.full(b"tfdt", 1, 0);
        b.u64(to_ts(samples[0].pts));
        b.close(tfdt);

        let trun = b.full(b"trun", 0, TRUN_FLAGS);
        b.u32(samples.len() as u32);
        offset_patches.push(b.buf.len());
        b.u32(0); // data_offset, patched later
        for (i, s) in samples.iter().enumerate() {
            let dur = if video {
                // Duration in track timescale from the pts ladder so rounding
                // never accumulates: next_ts - this_ts.
                let this_ts = to_ts(s.pts);
                let next_ts = match samples.get(i + 1) {
                    Some(n) => to_ts(n.pts),
                    None => this_ts + to_ts(s.dur.unwrap_or(0)).max(1),
                };
                (next_ts - this_ts) as u32
            } else {
                to_ts(s.dur.unwrap_or(0)) as u32
            };
            b.u32(dur.max(1));
            b.u32(s.data.len() as u32);
            b.u32(if s.key { SAMPLE_FLAG_SYNC } else { SAMPLE_FLAG_NON_SYNC });
        }
        b.close(trun);
    }
    b.close(traf);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Convenience for other record tests: a key AU or a P AU.
    pub(crate) fn key_or_p(key: bool, n: u8) -> Vec<u8> {
        if key {
            key_au()
        } else {
            p_au(n)
        }
    }

    pub(crate) fn nal(ty: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![ty << 1, 0x01];
        v.extend_from_slice(body);
        v
    }

    /// A synthetic keyframe AU with plausible parameter sets.
    pub(crate) fn key_au() -> Vec<u8> {
        let mut sps = vec![0x01]; // vps_id/sub_layers/nesting byte
        sps.push(0x01); // Main profile, space 0, tier 0
        sps.extend_from_slice(&0x6000_0000u32.to_be_bytes());
        sps.extend_from_slice(&[0x90, 0, 0, 0, 0, 0]);
        sps.push(0x5D); // level 3.1
        let mut au = Vec::new();
        for (ty, body) in [
            (annexb::NAL_VPS, b"vps-body".as_slice()),
            (annexb::NAL_SPS, &sps),
            (annexb::NAL_PPS, b"pps-body"),
            (19u8, b"idr-slice-data"),
        ] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(&nal(ty, body));
        }
        au
    }

    pub(crate) fn p_au(n: u8) -> Vec<u8> {
        let mut au = vec![0, 0, 0, 1];
        au.extend_from_slice(&nal(1, &[b'p', n, n, n]));
        au
    }

    fn boxes_at(data: &[u8]) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 8 <= data.len() {
            let size = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
            let name = String::from_utf8_lossy(&data[i + 4..i + 8]).into_owned();
            assert!(size >= 8 && i + size <= data.len(), "box {name} size {size} at {i}");
            out.push((name, size));
            i += size;
        }
        assert_eq!(i, data.len(), "trailing bytes after the last box");
        out
    }

    #[test]
    fn state_machine_waits_for_a_keyframe() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::with_opus(1920, 1080));
        assert_eq!(m.state(), MuxState::AwaitingKeyframe);
        m.push_video(&p_au(1), 0, false).unwrap();
        m.push_audio(b"opus", 0, 100_000).unwrap();
        assert_eq!(m.dropped_awaiting_key, 1);
        assert_eq!(m.bytes_written(), 0, "nothing written before the first keyframe");

        m.push_video(&key_au(), 1_000_000, true).unwrap();
        assert_eq!(m.state(), MuxState::Streaming);
        assert!(m.bytes_written() > 0, "init segment written on first keyframe");

        let out = m.finalize().unwrap();
        let names: Vec<_> = boxes_at(&out).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["ftyp", "moov", "moof", "mdat", "mfra"]);
    }

    #[test]
    fn keyframe_without_param_sets_is_an_error() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::video_only(640, 480));
        let mut au = vec![0, 0, 0, 1];
        au.extend_from_slice(&nal(19, b"idr-but-bare"));
        assert!(m.push_video(&au, 0, true).is_err());
    }

    #[test]
    fn fragments_roll_on_target_duration_and_audio_follows_video() {
        let mut cfg = MuxConfig::with_opus(1920, 1080);
        cfg.fragment_100ns = 500_000; // 50 ms fragments
        let mut m = Mp4Muxer::new(Vec::new(), cfg);
        let f = 166_667i64; // 60 fps
        m.push_video(&key_au(), 0, true).unwrap();
        for i in 1..=6 {
            m.push_audio(&[0xA0 + i as u8], (i - 1) * 100_000, 100_000).unwrap();
            m.push_video(&p_au(i as u8), i * f, false).unwrap();
        }
        let out = m.finalize().unwrap();
        let names: Vec<_> = boxes_at(&out).into_iter().map(|(n, _)| n).collect();
        // 7 frames at 60 fps with 50 ms fragments → fragment closes when a
        // newcomer crosses the boundary: 2 mid-stream rolls + final flush.
        assert_eq!(
            names,
            ["ftyp", "moov", "moof", "mdat", "moof", "mdat", "moof", "mdat", "mfra"],
            "every closed fragment is a self-contained moof+mdat"
        );
    }

    #[test]
    fn finalized_muxer_rejects_input() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::video_only(640, 480));
        m.push_video(&key_au(), 0, true).unwrap();
        let mut m2 = Mp4Muxer::new(m.finalize().unwrap(), MuxConfig::video_only(640, 480));
        m2.state = MuxState::Finalized;
        assert!(m2.push_video(&key_au(), 0, true).is_err());
        assert!(m2.push_audio(b"x", 0, 1).is_err());
    }

    #[test]
    fn video_only_config_ignores_audio() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::video_only(640, 480));
        m.push_video(&key_au(), 0, true).unwrap();
        m.push_audio(b"opus", 0, 100_000).unwrap();
        let out = m.finalize().unwrap();
        // One traf only: no audio track anywhere in the moof.
        let pos = out.windows(4).filter(|w| w == b"traf").count();
        assert_eq!(pos, 1);
    }

    #[test]
    fn deterministic_and_matches_golden_fixture() {
        let build = || {
            let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::with_opus(2560, 1440));
            let f = 166_667i64;
            let base = 123_456_789i64; // non-zero start proves rebasing
            m.push_video(&p_au(9), base - f, false).unwrap(); // dropped
            m.push_video(&key_au(), base, true).unwrap();
            for i in 1..=4 {
                m.push_audio(&[0xB0, i as u8], base + (i - 1) * 100_000, 100_000).unwrap();
                m.push_video(&p_au(i as u8), base + i * f, false).unwrap();
            }
            m.finalize().unwrap()
        };
        let a = build();
        assert_eq!(a, build(), "same input, same bytes");

        let golden_path =
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden-recording.mp4");
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

    /// The `mfra` index is what makes the file importable into the Windows
    /// video-editing API; a wrong offset in it is worse than none at all, so
    /// check every entry actually lands on a `moof`.
    #[test]
    fn mfra_indexes_every_keyframe_at_a_real_moof_offset() {
        let mut cfg = MuxConfig::with_opus(1920, 1080);
        cfg.fragment_100ns = 500_000; // 50 ms fragments
        let mut m = Mp4Muxer::new(Vec::new(), cfg);
        let f = 166_667i64;
        m.push_video(&key_au(), 0, true).unwrap();
        for i in 1..=6i64 {
            m.push_audio(&[0xA0 + i as u8], (i - 1) * 100_000, 100_000).unwrap();
            // Every third frame is a keyframe, so the index has to carry more
            // than just the first one.
            m.push_video(&key_or_p(i % 3 == 0, i as u8), i * f, i % 3 == 0).unwrap();
        }
        let out = m.finalize().unwrap();

        let boxes = boxes_at(&out);
        let mut offsets = Vec::new();
        let mut at = 0usize;
        for (name, size) in &boxes {
            if name == "moof" {
                offsets.push(at as u64);
            }
            at += size;
        }
        let mfra_at = at - boxes.last().unwrap().1;
        let mfra = &out[mfra_at..];

        // mfro's trailing u32 must restate the whole mfra size.
        let stated = u32::from_be_bytes(out[out.len() - 4..].try_into().unwrap()) as usize;
        assert_eq!(stated, mfra.len(), "mfro size must match the mfra box");

        // Walk the first tfra (track 1, video) and check its entries.
        let tfra_at = mfra.windows(4).position(|w| w == b"tfra").unwrap() - 4;
        let track = u32::from_be_bytes(mfra[tfra_at + 12..tfra_at + 16].try_into().unwrap());
        assert_eq!(track, 1);
        let lengths = u32::from_be_bytes(mfra[tfra_at + 16..tfra_at + 20].try_into().unwrap());
        assert_eq!(lengths, 0b11, "1-byte traf/trun numbers, 4-byte sample number");
        let count =
            u32::from_be_bytes(mfra[tfra_at + 20..tfra_at + 24].try_into().unwrap()) as usize;
        assert_eq!(count, 3, "one entry per keyframe: the IDR plus frames 3 and 6");

        let mut e = tfra_at + 24;
        let mut times = Vec::new();
        for _ in 0..count {
            let time = u64::from_be_bytes(mfra[e..e + 8].try_into().unwrap());
            let moof = u64::from_be_bytes(mfra[e + 8..e + 16].try_into().unwrap());
            let traf = mfra[e + 16];
            let trun = mfra[e + 17];
            let sample = u32::from_be_bytes(mfra[e + 18..e + 22].try_into().unwrap());
            assert!(offsets.contains(&moof), "entry points at {moof}, not a moof: {offsets:?}");
            assert_eq!(traf, 1);
            assert_eq!(trun, 1);
            assert!(sample >= 1, "sample numbers are 1-based");
            times.push(time);
            e += 22;
        }
        assert_eq!(times[0], 0, "first keyframe is the rebased origin");
        assert!(times.windows(2).all(|w| w[0] < w[1]), "times ascend: {times:?}");
    }

    #[test]
    fn timestamps_rebase_to_first_keyframe() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::video_only(640, 480));
        m.push_video(&key_au(), 5_000_000, true).unwrap();
        m.push_video(&p_au(1), 5_166_667, false).unwrap();
        let out = m.finalize().unwrap();
        // tfdt (version 1) holds the 64-bit base decode time; rebased → 0.
        let at = out.windows(4).position(|w| w == b"tfdt").unwrap();
        let base = u64::from_be_bytes(out[at + 8..at + 16].try_into().unwrap());
        assert_eq!(base, 0);
    }
}
