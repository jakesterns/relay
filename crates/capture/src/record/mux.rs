//! Fragmented-MP4 muxer for the recorded share bitstream: HEVC (`hvc1` +
//! `hvcC`) or H.264 (`avc1` + `avcC`), and Opus (`dOps`). Pure and byte-deterministic — the same inputs
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
use crate::codec::VideoCodec;

/// 100 ns units per second — the video track timescale, chosen so encoder
/// PTS values (QPC 100 ns) map exactly with no rounding.
pub const VIDEO_TIMESCALE: u32 = 10_000_000;

#[derive(Debug, Clone)]
pub struct MuxConfig {
    /// The share's negotiated video codec. The constructors default to HEVC;
    /// see [`MuxConfig::with_codec`].
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    /// One entry per audio track, in track order; empty = video-only file.
    /// Track 0 is the program mix, track 1 (when present) the microphone.
    pub audio: Vec<AudioConfig>,
    /// Target fragment duration in 100 ns units (default 1 s).
    pub fragment_100ns: i64,
}

#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u8,
    /// Opus pre-skip in 48 kHz samples (0 is fine for a live stream).
    pub pre_skip: u16,
    /// `hdlr` track name, so an editor can tell the two apart.
    pub name: &'static str,
}

impl MuxConfig {
    pub fn video_only(width: u32, height: u32) -> Self {
        Self {
            codec: VideoCodec::Hevc,
            width,
            height,
            audio: Vec::new(),
            fragment_100ns: 10_000_000,
        }
    }

    pub fn with_codec(mut self, codec: VideoCodec) -> Self {
        self.codec = codec;
        self
    }

    /// Video plus the program-mix Opus track.
    pub fn with_opus(width: u32, height: u32) -> Self {
        Self {
            codec: VideoCodec::Hevc,
            width,
            height,
            audio: vec![opus_track(PROGRAM_TRACK_NAME)],
            fragment_100ns: 10_000_000,
        }
    }

    /// Video plus the program mix *and* a second Opus track for the
    /// microphone. Both are 48 kHz stereo; see
    /// `docs/dev/dual-audio-decision.md` for why they stay unmixed.
    pub fn with_opus_and_mic(width: u32, height: u32) -> Self {
        Self {
            codec: VideoCodec::Hevc,
            width,
            height,
            audio: vec![opus_track(PROGRAM_TRACK_NAME), opus_track(MIC_TRACK_NAME)],
            fragment_100ns: 10_000_000,
        }
    }
}

/// `hdlr` names for the two audio tracks, so players and editors label them.
pub const PROGRAM_TRACK_NAME: &str = "Relay Audio";
pub const MIC_TRACK_NAME: &str = "Relay Microphone";

fn opus_track(name: &'static str) -> AudioConfig {
    AudioConfig { sample_rate: 48_000, channels: 2, pre_skip: 0, name }
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
    /// Pending samples per audio track, parallel to `cfg.audio`.
    audio: Vec<Vec<PendingSample>>,
    /// Video AUs dropped while waiting for the first keyframe.
    pub dropped_awaiting_key: u64,
    bytes_written: u64,
    last_video_dur: i64,
    /// Random-access points per track — index 0 is video, then one entry per
    /// audio track — accumulated as fragments are written and emitted as
    /// `mfra` by `finalize`. Without this index the Windows video-editing API
    /// (`Windows.Media.Editing.MediaClip`) refuses the file outright — see
    /// `docs/dev/container-compat.md`.
    sync: Vec<Vec<SyncPoint>>,
}

impl<W: Write> Mp4Muxer<W> {
    pub fn new(w: W, cfg: MuxConfig) -> Self {
        let audio_len = cfg.audio.len();
        let audio = cfg.audio.iter().map(|_| Vec::new()).collect();
        Self {
            w,
            cfg,
            state: MuxState::AwaitingKeyframe,
            t0: 0,
            seq: 0,
            video: Vec::new(),
            audio,
            dropped_awaiting_key: 0,
            bytes_written: 0,
            last_video_dur: 166_667, // 60 fps until the stream says otherwise
            sync: (0..=audio_len).map(|_| Vec::new()).collect(),
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
                let init = init_segment(&self.cfg, &entry);
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
            data: annexb::to_mp4_sample_for(self.cfg.codec, annexb_au),
            pts,
            dur: None,
            key: keyframe,
        });
        Ok(())
    }

    /// Feed one Opus packet to audio track `track` (0 = program mix, 1 = mic).
    /// Dropped (not an error) before the first video keyframe so the file
    /// always starts on a decodable picture, and dropped for a track this
    /// file does not carry — a mic packet arriving at a program-only muxer is
    /// a configuration mismatch, not a reason to fail a recording.
    pub fn push_audio(
        &mut self,
        track: usize,
        packet: &[u8],
        pts_100ns: i64,
        dur_100ns: i64,
    ) -> Result<()> {
        match self.state {
            MuxState::Finalized => bail!("muxer already finalized"),
            MuxState::AwaitingKeyframe => return Ok(()),
            MuxState::Streaming => {}
        }
        let Some(pending) = self.audio.get_mut(track) else {
            return Ok(());
        };
        pending.push(PendingSample {
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
            let mfra = mfra(&self.cfg, &self.sync);
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
        let audio: Vec<Vec<PendingSample>> = self
            .audio
            .iter_mut()
            .map(|pending| {
                let split =
                    pending.iter().position(|a| a.pts >= until_pts).unwrap_or(pending.len());
                pending.drain(..split).collect()
            })
            .collect();
        let video = std::mem::take(&mut self.video);

        // Which audio tracks have samples this fragment, in track order. Only
        // those get a `traf`, so a `traf` number is a *position* in this moof,
        // not a track id — with two audio tracks the mic can be traf 2 or 3
        // depending on whether the program mix had anything to say.
        let written: Vec<usize> =
            (0..self.cfg.audio.len()).filter(|&i| !audio[i].is_empty()).collect();

        // Index this fragment's random-access points before writing it, while
        // `bytes_written` still points at the `moof` about to go out.
        let moof_offset = self.bytes_written;
        for (i, s) in video.iter().enumerate() {
            if s.key {
                self.sync[0].push(SyncPoint {
                    time: to_timescale(s.pts, VIDEO_TIMESCALE),
                    moof_offset,
                    traf: 1,
                    sample: i as u32 + 1,
                });
            }
        }
        // Every Opus packet is a random-access point; one entry per fragment
        // is enough to seek by and keeps the index small.
        for (pos, &i) in written.iter().enumerate() {
            self.sync[i + 1].push(SyncPoint {
                time: to_timescale(audio[i][0].pts, self.cfg.audio[i].sample_rate),
                moof_offset,
                traf: pos as u32 + 2, // video is always traf 1
                sample: 1,
            });
        }

        self.seq += 1;
        let frag = fragment(&self.cfg, self.seq, &video, &audio, &written);
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
fn init_segment(cfg: &MuxConfig, entry: &VideoEntry) -> Vec<u8> {
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
        b.u32(cfg.audio.len() as u32 + 2); // next_track_ID
        b.close(mvhd);

        video_trak(&mut b, cfg, entry);
        for (i, a) in cfg.audio.iter().enumerate() {
            audio_trak(&mut b, a, audio_track_id(i));
        }

        let mvex = b.open(b"mvex");
        for track_id in 1..=(1 + cfg.audio.len() as u32) {
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

fn video_trak(b: &mut Boxes, cfg: &MuxConfig, entry: &VideoEntry) {
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
                    visual_sample_entry(b, cfg, entry);
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

/// How a video track describes its codec, derived once from the first
/// keyframe. MP4 wraps `config` in `config_box` inside a `sample_entry` box;
/// Matroska carries the identical bytes as `CodecPrivate` under
/// `matroska_codec_id`.
pub(crate) struct VideoEntry {
    pub sample_entry: [u8; 4],
    pub config_box: [u8; 4],
    pub config: Vec<u8>,
    pub matroska_codec_id: &'static str,
}

pub(crate) fn video_entry(codec: VideoCodec, keyframe_au: &[u8]) -> Result<VideoEntry> {
    match codec {
        VideoCodec::Hevc => {
            let ps = annexb::extract_param_sets(keyframe_au)
                .context("keyframe carries no VPS/SPS/PPS")?;
            let sps = annexb::parse_sps_summary(&ps.sps).context("unparseable SPS")?;
            Ok(VideoEntry {
                sample_entry: *b"hvc1",
                config_box: *b"hvcC",
                config: hvcc_payload(&ps, &sps),
                matroska_codec_id: "V_MPEGH/ISO/HEVC",
            })
        }
        VideoCodec::H264 => {
            let ps = annexb::extract_param_sets_for(codec, keyframe_au)
                .context("keyframe carries no SPS/PPS")?;
            let sps = annexb::parse_avc_sps_summary(&ps.sps).context("unparseable SPS")?;
            Ok(VideoEntry {
                sample_entry: *b"avc1",
                config_box: *b"avcC",
                config: avcc_payload(&ps, &sps),
                matroska_codec_id: "V_MPEG4/ISO/AVC",
            })
        }
    }
}

fn visual_sample_entry(b: &mut Boxes, cfg: &MuxConfig, video: &VideoEntry) {
    let entry = b.open(&video.sample_entry);
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

    let config = b.open(&video.config_box);
    b.bytes(&video.config);
    b.close(config);
    b.close(entry);
}

/// The `avcC` decoder-configuration payload (ISO/IEC 14496-15 §5.3.3.1).
pub(crate) fn avcc_payload(ps: &annexb::ParamSets, sps: &annexb::AvcSummary) -> Vec<u8> {
    let mut b = Boxes::new();
    b.u8(1); // configurationVersion
    b.u8(sps.profile_idc);
    b.u8(sps.profile_compatibility);
    b.u8(sps.level_idc);
    b.u8(0xFC | 3); // lengthSizeMinusOne = 3: 4-byte lengths
    b.u8(0xE0 | 1); // one SPS
    b.u16(ps.sps.len() as u16);
    b.bytes(&ps.sps);
    b.u8(1); // one PPS
    b.u16(ps.pps.len() as u16);
    b.bytes(&ps.pps);
    // High and above carry the chroma/bit-depth trailer. NV12 in, so 4:2:0
    // 8-bit is fixed by the encoder setup, exactly as `hvcc_payload` assumes.
    if matches!(sps.profile_idc, 100 | 110 | 122 | 144) {
        b.u8(0xFC | 1); // chroma_format = 1
        b.u8(0xF8); // bit_depth_luma_minus8 = 0
        b.u8(0xF8); // bit_depth_chroma_minus8 = 0
        b.u8(0); // numOfSequenceParameterSetExt
    }
    b.buf
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

/// Track ids: video is 1, audio tracks follow in order.
fn audio_track_id(index: usize) -> u32 {
    index as u32 + 2
}

fn audio_trak(b: &mut Boxes, a: &AudioConfig, track_id: u32) {
    let trak = b.open(b"trak");
    {
        let tkhd = b.full(b"tkhd", 0, 3);
        b.u32(0);
        b.u32(0);
        b.u32(track_id);
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

            let mut name = a.name.as_bytes().to_vec();
            name.push(0);
            hdlr(b, b"soun", &name);

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
fn mfra(cfg: &MuxConfig, sync: &[Vec<SyncPoint>]) -> Vec<u8> {
    let mut b = Boxes::new();
    let mfra = b.open(b"mfra");
    // `sync[0]` is video (track 1); the rest follow `cfg.audio` in order.
    let ids = std::iter::once(1u32).chain((0..cfg.audio.len()).map(audio_track_id));
    for (track_id, points) in ids.zip(sync) {
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
    audio: &[Vec<PendingSample>],
    // Audio track indices with samples this fragment, in track order — the
    // caller already worked this out to build the `mfra` index, and both must
    // agree on which tracks got a `traf`.
    written: &[usize],
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
    for &i in written {
        traf(
            &mut b,
            audio_track_id(i),
            cfg.audio[i].sample_rate,
            &audio[i],
            false,
            &mut offset_patches,
        );
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
    for &i in written {
        if let Some(at) = patches.next() {
            patch(&mut b, at, &audio[i]);
        }
    }

    let mdat = b.open(b"mdat");
    for s in video.iter().chain(written.iter().flat_map(|&i| audio[i].iter())) {
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

    /// A synthetic H.264 keyframe AU: AUD, SPS (High, level 5.2), PPS, IDR.
    pub(crate) fn h264_key_au() -> Vec<u8> {
        let mut au = Vec::new();
        for n in [
            &[0x09u8, 0xF0][..],
            &[0x67, 100, 0, 52, 0xAC, 0x2B],
            &[0x68, 0xEE, 0x3C, 0x80],
            &[0x65, 0x88, 0x84, 0x21],
        ] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(n);
        }
        au
    }

    pub(crate) fn h264_p_au(n: u8) -> Vec<u8> {
        vec![0, 0, 0, 1, 0x41, 0x9A, n, n]
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
        m.push_audio(0, b"opus", 0, 100_000).unwrap();
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
            m.push_audio(0, &[0xA0 + i as u8], (i - 1) * 100_000, 100_000).unwrap();
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
        assert!(m2.push_audio(0, b"x", 0, 1).is_err());
    }

    #[test]
    fn video_only_config_ignores_audio() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::video_only(640, 480));
        m.push_video(&key_au(), 0, true).unwrap();
        m.push_audio(0, b"opus", 0, 100_000).unwrap();
        let out = m.finalize().unwrap();
        // One traf only: no audio track anywhere in the moof.
        let pos = out.windows(4).filter(|w| w == b"traf").count();
        assert_eq!(pos, 1);
    }

    #[test]
    fn mic_track_is_a_second_audio_track_in_the_same_file() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::with_opus_and_mic(1920, 1080));
        m.push_video(&key_au(), 0, true).unwrap();
        for i in 0..4i64 {
            m.push_audio(0, &[0xB0, i as u8], i * 100_000, 100_000).unwrap();
            m.push_audio(1, &[0xC0, i as u8], i * 100_000, 100_000).unwrap();
        }
        let out = m.finalize().unwrap();
        assert_eq!(
            out.windows(4).filter(|w| w == b"traf").count(),
            3,
            "video + program + mic each get a traf"
        );
        assert_eq!(out.windows(4).filter(|w| w == b"trak").count(), 3);
        let count = |needle: &[u8]| out.windows(needle.len()).filter(|w| *w == needle).count();
        assert_eq!(count(PROGRAM_TRACK_NAME.as_bytes()), 1, "program track is named");
        assert_eq!(count(MIC_TRACK_NAME.as_bytes()), 1, "mic track is named");
        for i in 0..4u8 {
            assert!(count(&[0xB0, i]) > 0, "program packet {i} in the file");
            assert!(count(&[0xC0, i]) > 0, "mic packet {i} in the file");
        }
    }

    /// Opening a two-track file but never feeding the mic (the user muted it,
    /// or the capture endpoint vanished) must still produce a valid file: the
    /// empty track gets a `trak` but no `traf`, and offsets stay in step.
    #[test]
    fn a_silent_mic_track_does_not_desync_the_fragment() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::with_opus_and_mic(1920, 1080));
        m.push_video(&key_au(), 0, true).unwrap();
        m.push_audio(0, b"program", 0, 100_000).unwrap();
        m.push_video(&p_au(1), 166_667, false).unwrap();
        let out = m.finalize().unwrap();
        assert_eq!(out.windows(4).filter(|w| w == b"traf").count(), 2);
        assert_eq!(out.windows(4).filter(|w| w == b"trak").count(), 3);
        boxes_at(&out); // sizes all consistent
        assert_mdat_offsets(&out);
    }

    /// The mic feeding but the program silent is the mirror case, and the one
    /// that would break a naive offset patcher: the *second* audio track's
    /// data offset must point past the video, not past a program run that is
    /// not there.
    #[test]
    fn a_silent_program_track_keeps_the_mic_offset_right() {
        let mut m = Mp4Muxer::new(Vec::new(), MuxConfig::with_opus_and_mic(1920, 1080));
        m.push_video(&key_au(), 0, true).unwrap();
        m.push_audio(1, b"mic-only-packet", 0, 100_000).unwrap();
        m.push_video(&p_au(1), 166_667, false).unwrap();
        let out = m.finalize().unwrap();
        assert_eq!(out.windows(4).filter(|w| w == b"traf").count(), 2);
        assert_mdat_offsets(&out);
    }

    /// Walk each `moof`'s `trun` data offsets and check they tile the `mdat`
    /// payload exactly: sum of sample sizes per track, laid end to end from
    /// the first offset, must land on the end of the mdat.
    fn assert_mdat_offsets(data: &[u8]) {
        let mut i = 0usize;
        while i + 8 <= data.len() {
            let size = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
            if &data[i + 4..i + 8] == b"moof" {
                let moof = &data[i..i + size];
                let mdat_size =
                    u32::from_be_bytes(data[i + size..i + size + 4].try_into().unwrap()) as usize;
                let mut runs: Vec<(usize, usize)> = Vec::new(); // (offset, total bytes)
                let mut j = 0usize;
                while j + 8 <= moof.len() {
                    if &moof[j + 4..j + 8] == b"trun" {
                        let count =
                            u32::from_be_bytes(moof[j + 12..j + 16].try_into().unwrap()) as usize;
                        let offset =
                            u32::from_be_bytes(moof[j + 16..j + 20].try_into().unwrap()) as usize;
                        let bytes: usize = (0..count)
                            .map(|k| {
                                let at = j + 20 + k * 12 + 4;
                                u32::from_be_bytes(moof[at..at + 4].try_into().unwrap()) as usize
                            })
                            .sum();
                        runs.push((offset, bytes));
                    }
                    j += 1;
                }
                runs.sort_unstable();
                let mut cursor = size + 8; // first payload byte, from moof start
                for (offset, bytes) in &runs {
                    assert_eq!(*offset, cursor, "run starts where the previous one ended");
                    cursor += bytes;
                }
                assert_eq!(cursor, size + mdat_size, "runs tile the mdat exactly");
                i += size + mdat_size;
                continue;
            }
            i += size;
        }
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
                m.push_audio(0, &[0xB0, i as u8], base + (i - 1) * 100_000, 100_000).unwrap();
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
            m.push_audio(0, &[0xA0 + i as u8], (i - 1) * 100_000, 100_000).unwrap();
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

    #[test]
    fn h264_stream_writes_avc1_with_a_complete_avcc() {
        let cfg = MuxConfig::with_opus(1920, 1080).with_codec(VideoCodec::H264);
        let mut m = Mp4Muxer::new(Vec::new(), cfg);
        m.push_video(&h264_p_au(9), 0, false).unwrap(); // before the keyframe: dropped
        m.push_video(&h264_key_au(), 1_000, true).unwrap();
        for i in 1..5 {
            m.push_video(&h264_p_au(i as u8), 1_000 + i * 166_666, false).unwrap();
        }
        let out = m.finalize().unwrap();
        let find = |needle: &[u8]| out.windows(needle.len()).position(|w| w == needle);
        assert!(find(b"avc1").is_some(), "avc1 sample entry");
        assert!(find(b"hvc1").is_none() && find(b"hvcC").is_none(), "no HEVC boxes");
        let at = find(b"avcC").expect("avcC box");
        let body = &out[at + 4..];
        assert_eq!(&body[..4], &[1, 100, 0, 52], "version, profile, compat, level");
        assert_eq!(body[4], 0xFF, "4-byte NAL lengths");
        assert_eq!(body[5], 0xE1, "one SPS");
        assert_eq!(&body[6..8], &6u16.to_be_bytes());
        assert_eq!(&body[8..14], &[0x67, 100, 0, 52, 0xAC, 0x2B]);
        assert_eq!(body[14], 1, "one PPS");
        assert_eq!(&body[17..21], &[0x68, 0xEE, 0x3C, 0x80]);
        assert_eq!(&body[21..25], &[0xFD, 0xF8, 0xF8, 0], "High-profile trailer");
        // The box length covers exactly that payload.
        let size = u32::from_be_bytes(out[at - 4..at].try_into().unwrap()) as usize;
        assert_eq!(size, 8 + 25);
        // The keyframe sample carries the IDR (length-prefixed), never the SPS.
        assert!(find(&[0, 0, 0, 4, 0x65, 0x88, 0x84, 0x21]).is_some());
        assert!(find(&[0, 0, 0, 6, 0x67]).is_none());
        assert_mdat_offsets(&out);
    }

    #[test]
    fn h264_keyframe_without_pps_is_an_error() {
        let mut m =
            Mp4Muxer::new(Vec::new(), MuxConfig::video_only(640, 480).with_codec(VideoCodec::H264));
        let au = vec![0, 0, 0, 1, 0x67, 100, 0, 52, 0, 0, 0, 1, 0x65, 0x88];
        let err = m.push_video(&au, 0, true).unwrap_err().to_string();
        assert!(err.contains("SPS/PPS"), "{err}");
        // And an HEVC keyframe fed to an H.264 file is refused, not mislabelled.
        let mut m =
            Mp4Muxer::new(Vec::new(), MuxConfig::video_only(640, 480).with_codec(VideoCodec::H264));
        assert!(m.push_video(&key_au(), 0, true).is_err());
    }
}
