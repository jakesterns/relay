//! Per-frame analysis of a game's audio: band energies, onsets, pitch,
//! events and sound classes.
//!
//! Input is f32 at the capture rate (interleaved, any channel count; it is
//! downmixed here). The capture is process loopback of the game's own PID,
//! so Discord and every other app are never in it; only chat the *game*
//! plays (in-game / proximity voice) can be. Every 10 ms frame:
//!
//! 1. A 24-band 1/3-octave filterbank (2nd-order band-pass per band) gives
//!    each band's level in dBFS, plus the frame's broadband short-term level.
//!    A small autocorrelation pitch tracker on a ~4 kHz decimated copy gives
//!    the frame's pitch and how periodic it is.
//! 2. **Gate (context).** The frame only counts if none of these hold:
//!    - silence (below [`SILENCE_DBFS`]);
//!    - clipping ([`CLIP_MIN_SAMPLES`] samples at or above [`CLIP_LEVEL`]);
//!    - a level jump: the short-term level has sat more than
//!      [`LEVEL_JUMP_DB`] away from the session loudness (net
//!      [`LEVEL_JUMP_REJECT_FRAMES`]) — someone moved a volume slider. At
//!      [`LEVEL_JUMP_REBASE_FRAMES`] the session loudness re-bases to it;
//!    - **nobody is playing**: no keyboard / mouse / pad input anywhere on
//!      the PC for more than [`INPUT_IDLE_MS`]. That is what separates a
//!      cutscene, a menu left open or AFK (voice and music with no input)
//!      from play (callouts and NPC voices while the player is moving). The
//!      helper reads it from `GetLastInputInfo` — one system-wide timestamp,
//!      no hooks, no keys — and passes it in with
//!      [`Analyzer::set_input_idle_ms`]; the analyzer never asks the OS;
//!    - **player chat**: voice whose added energy is codec band-limited —
//!      both the region below 180 Hz and the region above 4.5 kHz sit more
//!      than [`CHAT_EDGE_DB`] under the 315 Hz – 3.15 kHz speech band. Other
//!      players are not the game's mix and never shape its EQ.
//!
//!    An event in progress when the gate closes is thrown away.
//! 3. **Stationary label** for the frame, from look-back windows:
//!    - **voice** (game voice-over, NPCs): the 250 Hz – 2 kHz envelope is
//!      modulated at syllable rate (3–7 Hz carries [`SPEECH_MOD_FRACTION`] of
//!      its variance, depth [`SPEECH_MIN_DEPTH_DB`]) and that band dominates
//!      the highs by [`SPEECH_DOMINANCE_DB`]; full-band, unlike chat;
//!    - **vehicle**: a continuous periodic tone ([`VEHICLE_MIN_VOICED`] of the
//!      last second voiced, pitch [`VEHICLE_MIN_HZ`]–[`VEHICLE_MAX_HZ`]) whose
//!      pitch *glides* (range at least [`VEHICLE_MIN_GLIDE`], no jump over
//!      [`VEHICLE_MAX_STEP`]) — an engine tracking speed;
//!    - **music**: tonal (band levels steady frame to frame,
//!      [`TONAL_MIN_FRACTION`]);
//!    - otherwise **ambience**.
//! 4. A slow per-band *floor* tracks the steady background. An *onset* is a
//!    frame where at least two bands jump above both the previous frame and
//!    the floor; it opens an *event*, which closes when every band is back
//!    near the floor (or after 3 s). At close the event is classified from
//!    its *excess* spectrum (the energy it added over the floor), its
//!    duration, onset sharpness and spread, and rhythm:
//!    - **gunshot**: very sharp ([`GUN_MIN_ONSET_DB`]) *broadband* onset
//!      (low and high bands rise together), loud, with a tail;
//!    - **explosion / crash**: low-weighted and loud or long;
//!    - **mechanical** (reloads, clicks): very short ([`MECH_MAX_MS`]), sharp,
//!      high-weighted;
//!    - **foliage / cloth**: soft onset ([`FOLIAGE_MAX_ONSET_DB`]),
//!      noise-like (energy spread over [`FOLIAGE_MIN_SPREAD`] high bands),
//!      up to [`FOLIAGE_MAX_MS`];
//!    - **footsteps**: short, sharp, high-weighted; *rhythmic* when the last
//!      two inter-onset intervals agree within [`RHYTHM_TOLERANCE`] and sit in
//!      [`RHYTHM_MIN_MS`]–[`RHYTHM_MAX_MS`];
//!    - a long event under a stationary label takes that label (a vehicle
//!      pulling up, music starting).
//!
//! Every level that reaches a histogram is **relative to the session
//! loudness** (plus [`REL_REF_DB`]), so turning the game's volume up or down
//! does not move the statistics.
//!
//! Only aggregates are kept ([`Stats`]): per class and band, a level
//! histogram; per class, an event count and a frame count; overlap counts;
//! voice pitch clusters (to count distinct voices); and how many frames each
//! gate rule rejected. No audio is stored anywhere. Overlapping sounds are
//! not separated: the per-band histograms of what each class sounds like,
//! and how much of the time each class is on, are what the masking matrix
//! in [`super::derive`] is built from.
//!
//! Real-time rule: [`Analyzer::push`] never allocates. All buffers are sized
//! in [`Analyzer::new`].

use serde::{Deserialize, Serialize};

use super::{bin_of, BANDS_HZ, HIST_BINS, NBANDS};

/// Analysis frame length.
pub const FRAME_MS: u32 = 10;
/// Frames whose broadband level is below this are silence and ignored.
pub const SILENCE_DBFS: f32 = -70.0;
/// A sample at or above this magnitude is clipped...
pub const CLIP_LEVEL: f32 = 0.999;
/// ...and a frame with this many clipped samples is rejected.
pub const CLIP_MIN_SAMPLES: u32 = 3;
/// Short-term level this far from the session loudness is a level jump.
pub const LEVEL_JUMP_DB: f32 = 10.0;
/// Net frames (off-level frames minus on-level ones) before frames are
/// rejected: 2 s. An explosion's tail does not last this long.
pub const LEVEL_JUMP_REJECT_FRAMES: u32 = 200;
/// Net frames after which the session loudness re-bases to the new level.
pub const LEVEL_JUMP_REBASE_FRAMES: u32 = 400;
/// Histogram levels are `level - session loudness + REL_REF_DB`.
pub const REL_REF_DB: f32 = -50.0;
/// No user input for longer than this (20 s) means nobody is playing.
pub const INPUT_IDLE_MS: u32 = 20_000;

/// Speech detector window (2 s of 10 ms envelope samples).
pub const SPEECH_WIN_FRAMES: usize = 200;
/// Syllable-rate modulation band, Hz.
pub const SPEECH_MOD_LO_HZ: f32 = 3.0;
pub const SPEECH_MOD_HI_HZ: f32 = 7.0;
/// Share of the speech-band envelope's variance that must sit at syllable rate.
pub const SPEECH_MOD_FRACTION: f32 = 0.4;
/// Envelope standard deviation needed (dB): speech is strongly modulated.
pub const SPEECH_MIN_DEPTH_DB: f32 = 3.0;
/// The 250 Hz – 2 kHz band must exceed the 2.5 kHz+ band by this much.
pub const SPEECH_DOMINANCE_DB: f32 = 6.0;
/// Re-evaluate the speech detector every this many frames.
const SPEECH_EVAL_EVERY: u32 = 10;
/// Player chat: below 180 Hz and above 4.5 kHz both at least this far under
/// the speech band (in added energy) — a voice codec's band limit.
pub const CHAT_EDGE_DB: f32 = -20.0;
/// Smoothing of the chat measure over voiced frames (~0.5 s).
const CHAT_ALPHA: f32 = 0.05;
/// Voice pitch range, Hz.
pub const VOICE_MIN_HZ: f32 = 70.0;
pub const VOICE_MAX_HZ: f32 = 400.0;
/// A frame is voiced when its normalised autocorrelation peak reaches this.
pub const VOICED_MIN: f32 = 0.5;
/// Voiced frames per pitch measurement for voice clustering (0.5 s).
pub const VOICE_CHUNK_FRAMES: u32 = 50;
/// Chunks within this many semitones belong to the same voice.
pub const VOICE_CLUSTER_SEMITONES: f32 = 5.0;
/// Most distinct voices tracked.
pub const MAX_VOICES: usize = 8;

/// Vehicle: look-back window (1 s).
pub const VEHICLE_WIN_FRAMES: usize = 100;
/// Share of the window that must be periodic in the engine range.
pub const VEHICLE_MIN_VOICED: f32 = 0.9;
/// Engine fundamental range, Hz.
pub const VEHICLE_MIN_HZ: f32 = 25.0;
pub const VEHICLE_MAX_HZ: f32 = 250.0;
/// The pitch must move at least this much over the window (natural log, ≈ 0.9
/// semitone): an engine tracks speed; a held note does not.
pub const VEHICLE_MIN_GLIDE: f32 = 0.05;
/// ...and never jump by more than this between frames (a new note does).
pub const VEHICLE_MAX_STEP: f32 = 0.03;

/// A band is "steady" (tonal) when it moved less than this since the last frame.
pub const TONAL_STEADY_DB: f32 = 0.5;
/// Smoothed share of steady bands that marks music.
pub const TONAL_MIN_FRACTION: f32 = 0.6;

/// A band "rises" when it is this far above the previous frame and the floor.
pub const RISE_HF_DB: f32 = 5.0;
/// Below 1 kHz the bands are narrow and noisy at 10 ms; demand a bigger jump.
pub const RISE_LF_DB: f32 = 12.0;
pub const ONSET_MIN_BANDS: usize = 2;
/// An event is over when no band is more than this above the floor (the
/// narrow, noisy bands below 1 kHz need a wider margin)...
const EVENT_OFF_DB: f32 = 4.0;
const EVENT_OFF_LF_DB: f32 = 10.0;
/// ...for this many frames in a row.
const EVENT_OFF_FRAMES: u32 = 2;
/// Longest event; past this the floor resumes.
pub const EVENT_MAX_FRAMES: usize = 300;
/// Footsteps are shorter than this.
pub const CUE_MAX_MS: u32 = 300;
/// A footstep's onset is at least this sharp.
pub const CUE_MIN_ONSET_DB: f32 = 7.0;
/// Mechanical clicks (reloads) last at most this long...
pub const MECH_MAX_MS: u32 = 40;
/// ...with at least this sharp an onset.
pub const MECH_MIN_ONSET_DB: f32 = 12.0;
/// Foliage / cloth rustle: a soft onset...
pub const FOLIAGE_MAX_ONSET_DB: f32 = 15.0;
/// ...lasting at most this long...
pub const FOLIAGE_MAX_MS: u32 = 800;
/// ...with its added energy spread over at least this many ≥ 1 kHz bands
/// within 6 dB of the strongest (noise-like, not a tone).
pub const FOLIAGE_MIN_SPREAD: usize = 5;
/// Footstep rhythm: inter-onset intervals in this range...
pub const RHYTHM_MIN_MS: u32 = 250;
pub const RHYTHM_MAX_MS: u32 = 1200;
/// ...agreeing within this fraction.
pub const RHYTHM_TOLERANCE: f32 = 0.25;
/// A cue's added energy peaks at or above this band: footsteps, foliage and
/// clicks are bright; a syllable's energy peaks lower, around its formants.
pub const CUE_MIN_PEAK_HZ: f32 = 1600.0;
/// Frames of recent history whose per-band minimum is "what was there just
/// before" a cue that lands inside another sound.
const RECENT_FRAMES: usize = 8;
/// A gunshot's crack: its added energy above 1 kHz is at least this share
/// of the energy it added below 250 Hz (an explosion's is far less).
pub const GUN_MIN_HIGH_RATIO: f32 = 0.25;
/// Gunshot: onset at least this sharp...
pub const GUN_MIN_ONSET_DB: f32 = 15.0;
/// ...rising that sharply in at least this many bands below 250 Hz and
/// above 1 kHz (a crack and a thump together: broadband)...
pub const GUN_MIN_LOW_BANDS: usize = 2;
pub const GUN_MIN_HIGH_BANDS: usize = 4;
/// ...this far above the session loudness...
pub const GUN_LOUD_DB: f32 = 15.0;
/// ...with a tail at least this long.
pub const GUN_MIN_TAIL_MS: u32 = 50;
/// Events this far above the running loudness are loud.
pub const LOUD_ABOVE_DB: f32 = 30.0;
/// Spectral weighting margin (power ratio, 3 dB).
pub const WEIGHT_RATIO: f32 = 2.0;
/// A long event takes its frames' stationary label when it lasts this long.
pub const ADOPT_LABEL_MS: u32 = 500;
/// Frames of a stationary class that count as one of its "events" (0.5 s),
/// so voice, vehicles and music count evidence like discrete sounds do.
pub const STATIONARY_EVENT_FRAMES: u64 = 50;

/// Floor smoothing per frame, on power.
const FLOOR_DOWN: f32 = 0.1;
const FLOOR_UP: f32 = 0.05;
/// Session loudness, ~30 s time constant (dB domain).
const LT_ALPHA: f32 = 1.0 / 3000.0;
/// Short-term level, ~1 s time constant (dB domain, like the session
/// loudness, so sparse loud impacts do not read as a volume change).
const ST_ALPHA: f32 = 0.01;
/// Tonality smoothing, ~1 s.
const TONAL_ALPHA: f32 = 0.01;
/// Pitch tracker: decimated rate target and window.
const PITCH_FS: u32 = 4000;
const PITCH_WIN: usize = 256;
const TINY: f32 = 1e-12;

/// Sound classes. The first three are the "cues" a player listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundClass {
    Footsteps,
    Foliage,
    Mechanical,
    Voice,
    Gunshot,
    Explosion,
    Vehicle,
    Music,
    Ambience,
}

/// Number of classes.
pub const NCLASSES: usize = 9;

impl SoundClass {
    pub const ALL: [SoundClass; NCLASSES] = [
        SoundClass::Footsteps,
        SoundClass::Foliage,
        SoundClass::Mechanical,
        SoundClass::Voice,
        SoundClass::Gunshot,
        SoundClass::Explosion,
        SoundClass::Vehicle,
        SoundClass::Music,
        SoundClass::Ambience,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// Discrete events (with a class histogram per event) rather than
    /// stationary sources (a histogram per frame).
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            SoundClass::Footsteps
                | SoundClass::Foliage
                | SoundClass::Mechanical
                | SoundClass::Gunshot
                | SoundClass::Explosion
        )
    }

    pub fn is_cue(self) -> bool {
        matches!(self, SoundClass::Footsteps | SoundClass::Foliage | SoundClass::Mechanical)
    }
}

/// Why a frame did not count as active gameplay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Silence,
    Clipped,
    LevelJump,
    InputIdle,
    PlayerChat,
}

/// One distinct voice: its pitch and how many half-second chunks matched it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VoiceCluster {
    pub hz: f32,
    pub chunks: u32,
}

/// Long-term aggregates. This, and only this, is what persists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// Frames that counted as active gameplay (10 ms each).
    pub active_frames: u64,
    /// Per class: discrete events (transients) or 0.5 s chunks (stationary).
    pub events: Vec<u64>,
    /// Per class: frames attributed to it.
    pub class_frames: Vec<u64>,
    /// `NCLASSES × NBANDS × HIST_BINS`: per class and band, a level histogram
    /// (cues: each event's own added level; other transients: each event's
    /// level; stationary classes: frame levels).
    #[serde(with = "sparse")]
    pub class_hist: Vec<u32>,
    /// `NBANDS × HIST_BINS`: every active frame's level — the mix overall.
    /// Cue frames contribute the floor they sat on.
    #[serde(with = "sparse")]
    pub frame_hist: Vec<u32>,
    /// Per class: cue-like onsets heard *inside* one of its events.
    pub overlaps: Vec<u64>,
    /// Footsteps that were part of a rhythmic train.
    pub rhythmic_steps: u64,
    /// Distinct voices by pitch (game voice only; chat is not kept).
    pub voices: Vec<VoiceCluster>,
    /// Frames rejected by each gate rule.
    pub silent_frames: u64,
    pub clipped_frames: u64,
    pub level_jump_frames: u64,
    pub input_idle_frames: u64,
    pub chat_frames: u64,
    /// Volume changes the session loudness re-based over.
    pub level_jumps: u64,
    /// Events cut short by the gate and thrown away.
    pub discarded_events: u64,
    /// Events that fitted no class.
    pub unclassified_events: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            active_frames: 0,
            events: vec![0; NCLASSES],
            class_frames: vec![0; NCLASSES],
            class_hist: vec![0; NCLASSES * NBANDS * HIST_BINS],
            frame_hist: vec![0; NBANDS * HIST_BINS],
            overlaps: vec![0; NCLASSES],
            rhythmic_steps: 0,
            voices: Vec::new(),
            silent_frames: 0,
            clipped_frames: 0,
            level_jump_frames: 0,
            input_idle_frames: 0,
            chat_frames: 0,
            level_jumps: 0,
            discarded_events: 0,
            unclassified_events: 0,
        }
    }
}

impl Stats {
    /// True when every vector has its expected shape (a loaded file can lie).
    pub fn well_formed(&self) -> bool {
        self.events.len() == NCLASSES
            && self.class_frames.len() == NCLASSES
            && self.overlaps.len() == NCLASSES
            && self.class_hist.len() == NCLASSES * NBANDS * HIST_BINS
            && self.frame_hist.len() == NBANDS * HIST_BINS
            && self.voices.len() <= MAX_VOICES
            && self.voices.iter().all(|v| v.hz.is_finite() && v.hz > 0.0)
    }

    /// Seconds of active gameplay analysed.
    pub fn active_secs(&self) -> u64 {
        self.active_frames * FRAME_MS as u64 / 1000
    }

    pub fn count(&self, c: SoundClass) -> u64 {
        self.events[c.index()]
    }

    /// Footsteps + foliage + mechanical.
    pub fn cue_events(&self) -> u64 {
        SoundClass::ALL.iter().filter(|c| c.is_cue()).map(|&c| self.count(c)).sum()
    }

    /// Distinct voices heard (clusters with at least two chunks).
    pub fn distinct_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.chunks >= 2).count()
    }

    /// The histogram block for one class (`NBANDS × HIST_BINS`).
    pub fn hist(&self, c: SoundClass) -> &[u32] {
        let n = NBANDS * HIST_BINS;
        &self.class_hist[c.index() * n..(c.index() + 1) * n]
    }

    fn hist_mut(&mut self, c: SoundClass) -> &mut [u32] {
        let n = NBANDS * HIST_BINS;
        &mut self.class_hist[c.index() * n..(c.index() + 1) * n]
    }

    /// Frames the gate rejected, all reasons.
    pub fn rejected_frames(&self) -> u64 {
        self.silent_frames
            + self.clipped_frames
            + self.level_jump_frames
            + self.input_idle_frames
            + self.chat_frames
    }

    /// Add `other` into `self`. A malformed `other` is ignored.
    pub fn merge(&mut self, other: &Stats) {
        if !other.well_formed() {
            return;
        }
        if !self.well_formed() {
            *self = Stats::default();
        }
        self.active_frames += other.active_frames;
        for (a, b) in [
            (&mut self.events, &other.events),
            (&mut self.class_frames, &other.class_frames),
            (&mut self.overlaps, &other.overlaps),
        ] {
            for (x, y) in a.iter_mut().zip(b.iter()) {
                *x += *y;
            }
        }
        for (a, b) in
            [(&mut self.class_hist, &other.class_hist), (&mut self.frame_hist, &other.frame_hist)]
        {
            for (x, y) in a.iter_mut().zip(b.iter()) {
                *x = x.saturating_add(*y);
            }
        }
        self.rhythmic_steps += other.rhythmic_steps;
        for v in &other.voices {
            add_voice(&mut self.voices, v.hz, v.chunks);
        }
        self.silent_frames += other.silent_frames;
        self.clipped_frames += other.clipped_frames;
        self.level_jump_frames += other.level_jump_frames;
        self.input_idle_frames += other.input_idle_frames;
        self.chat_frames += other.chat_frames;
        self.level_jumps += other.level_jumps;
        self.discarded_events += other.discarded_events;
        self.unclassified_events += other.unclassified_events;
    }

    /// Median level (dB) of band `b` in `hist`, or `None` when it is empty.
    pub fn median(hist: &[u32], b: usize) -> Option<f32> {
        Self::percentile(hist, b, 0.5)
    }

    /// The `p` quantile of band `b` in `hist`.
    pub fn percentile(hist: &[u32], b: usize, p: f32) -> Option<f32> {
        let row = &hist[b * HIST_BINS..(b + 1) * HIST_BINS];
        let total: u64 = row.iter().map(|&c| c as u64).sum();
        if total == 0 {
            return None;
        }
        let want = (total as f64 * p as f64).max(1.0);
        let mut acc = 0u64;
        for (i, &c) in row.iter().enumerate() {
            acc += c as u64;
            if acc as f64 >= want {
                return Some(super::bin_db(i));
            }
        }
        Some(super::bin_db(HIST_BINS - 1))
    }

    /// Fraction of entries in band `b` of `hist` at or above `db`.
    pub fn fraction_at_or_above(hist: &[u32], b: usize, db: f32) -> f32 {
        let row = &hist[b * HIST_BINS..(b + 1) * HIST_BINS];
        let total: u64 = row.iter().map(|&c| c as u64).sum();
        if total == 0 {
            return 0.0;
        }
        let from = bin_of(db);
        let above: u64 = row[from..].iter().map(|&c| c as u64).sum();
        above as f32 / total as f32
    }

    fn add(hist: &mut [u32], levels: &[f32; NBANDS], offset: f32) {
        for (b, &db) in levels.iter().enumerate() {
            let slot = &mut hist[b * HIST_BINS + bin_of(db + offset)];
            *slot = slot.saturating_add(1);
        }
    }
}

/// File `chunks` voice chunks at `hz` into the clusters.
fn add_voice(voices: &mut Vec<VoiceCluster>, hz: f32, chunks: u32) {
    if !(hz.is_finite() && hz > 0.0) || chunks == 0 {
        return;
    }
    let semis = |a: f32, b: f32| 12.0 * (a / b).log2().abs();
    let near = voices
        .iter()
        .enumerate()
        .filter(|(_, v)| semis(v.hz, hz) <= VOICE_CLUSTER_SEMITONES)
        .min_by(|a, b| semis(a.1.hz, hz).total_cmp(&semis(b.1.hz, hz)))
        .map(|(i, _)| i);
    match near {
        Some(i) => {
            let v = &mut voices[i];
            // Running geometric mean, weighted by chunks.
            let total = (v.chunks + chunks) as f32;
            v.hz = ((v.hz.ln() * v.chunks as f32 + hz.ln() * chunks as f32) / total).exp();
            v.chunks += chunks;
            // A centre that drifted into a neighbour's range joins it.
            let c = voices[i];
            if let Some(j) = (0..voices.len())
                .find(|&j| j != i && semis(voices[j].hz, c.hz) <= VOICE_CLUSTER_SEMITONES)
            {
                let o = voices[j];
                let total = (o.chunks + c.chunks) as f32;
                voices[j].hz =
                    ((o.hz.ln() * o.chunks as f32 + c.hz.ln() * c.chunks as f32) / total).exp();
                voices[j].chunks += c.chunks;
                voices.remove(i);
            }
        }
        None if voices.len() < MAX_VOICES => voices.push(VoiceCluster { hz, chunks }),
        None => {}
    }
}

/// Histograms are mostly zeros: store them as `{len, nz: [[index, count]...]}`.
mod sparse {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Sparse {
        len: usize,
        nz: Vec<(u32, u32)>,
    }

    pub fn serialize<S: Serializer>(v: &[u32], s: S) -> Result<S::Ok, S::Error> {
        let nz =
            v.iter().enumerate().filter(|(_, &c)| c != 0).map(|(i, &c)| (i as u32, c)).collect();
        Sparse { len: v.len(), nz }.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u32>, D::Error> {
        let sp = Sparse::deserialize(d)?;
        // A forged length could ask for gigabytes; nothing real exceeds this.
        if sp.len > 1 << 16 {
            return Err(serde::de::Error::custom("histogram too large"));
        }
        let mut v = vec![0u32; sp.len];
        for (i, c) in sp.nz {
            *v.get_mut(i as usize)
                .ok_or_else(|| serde::de::Error::custom("index out of range"))? = c;
        }
        Ok(v)
    }
}

/// One 2nd-order band-pass (RBJ, constant 0 dB peak), transposed direct form II.
#[derive(Debug, Clone, Copy, Default)]
struct BandPass {
    b0: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
    enabled: bool,
}

impl BandPass {
    fn new(fs: f32, f0: f32, q: f32) -> Self {
        if f0 >= fs * 0.45 {
            return Self::default();
        }
        let w0 = 2.0 * std::f32::consts::PI * f0 / fs;
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            a1: -2.0 * w0.cos() / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
            enabled: true,
        }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        // b1 = 0, b2 = -b0.
        let y = self.b0 * x + self.z1;
        self.z1 = -self.a1 * y + self.z2;
        self.z2 = -self.b0 * x - self.a2 * y;
        y
    }
}

/// The frame's stationary label (also used for frames inside events).
fn label_class(voice: bool, vehicle: bool, music: bool) -> SoundClass {
    if voice {
        SoundClass::Voice
    } else if vehicle {
        SoundClass::Vehicle
    } else if music {
        SoundClass::Music
    } else {
        SoundClass::Ambience
    }
}

/// The open event, if any. Fixed-size: nothing here allocates.
struct Event {
    open: bool,
    /// Frames that were above the off threshold (the event's body).
    loud_frames: u32,
    /// Consecutive quiet frames at the tail.
    quiet: u32,
    onset_db: f32,
    rising_low: usize,
    rising_high: usize,
    /// The frame before the onset, so a sharp attack that straddles two
    /// frames is still measured as sharp.
    pre: [f32; NBANDS],
    start_frame: u64,
    peak_db: f32,
    sum_lin: [f32; NBANDS],
    floor_db: [f32; NBANDS],
    floor_lin: [f32; NBANDS],
    /// Band levels and stationary label of each frame, so they can be filed
    /// once the event's class is known.
    frames: Box<[[f32; NBANDS]]>,
    labels: Box<[u8]>,
    nframes: usize,
    overlaps: u32,
}

/// Look-back state for the labels.
struct Labels {
    /// Speech-band envelope ring (dB).
    env: Box<[f32]>,
    env_pos: usize,
    env_filled: usize,
    dominance_db: f32,
    cos: Box<[f32]>,
    sin: Box<[f32]>,
    nbins: usize,
    speech: bool,
    eval_count: u32,
    /// Smoothed share of steady (tonal) bands.
    tonal: f32,
    /// Engine-range log pitch per frame (NaN when unvoiced) over the last second.
    vpitch: Box<[f32]>,
    vpos: usize,
    /// Smoothed chat measures: low and high regions against the speech band (dB).
    chat_low_db: f32,
    chat_high_db: f32,
    /// Voice clustering: current chunk.
    chunk_sum: f32,
    chunk_n: u32,
    /// Stationary frame counters per class.
    stationary_frames: [u64; NCLASSES],
}

impl Labels {
    fn new() -> Self {
        let secs = SPEECH_WIN_FRAMES as f32 * FRAME_MS as f32 / 1000.0;
        let lo = (SPEECH_MOD_LO_HZ * secs).ceil() as usize;
        let hi = (SPEECH_MOD_HI_HZ * secs).floor() as usize;
        let nbins = hi + 1 - lo;
        let mut cos = vec![0f32; nbins * SPEECH_WIN_FRAMES];
        let mut sin = vec![0f32; nbins * SPEECH_WIN_FRAMES];
        for (j, k) in (lo..=hi).enumerate() {
            for n in 0..SPEECH_WIN_FRAMES {
                let ph = 2.0 * std::f32::consts::PI * (k * n) as f32 / SPEECH_WIN_FRAMES as f32;
                cos[j * SPEECH_WIN_FRAMES + n] = ph.cos();
                sin[j * SPEECH_WIN_FRAMES + n] = ph.sin();
            }
        }
        Self {
            env: vec![0f32; SPEECH_WIN_FRAMES].into_boxed_slice(),
            env_pos: 0,
            env_filled: 0,
            dominance_db: 0.0,
            cos: cos.into_boxed_slice(),
            sin: sin.into_boxed_slice(),
            nbins,
            speech: false,
            eval_count: 0,
            tonal: 0.0,
            vpitch: vec![f32::NAN; VEHICLE_WIN_FRAMES].into_boxed_slice(),
            vpos: 0,
            chat_low_db: 0.0,
            chat_high_db: 0.0,
            chunk_sum: 0.0,
            chunk_n: 0,
            stationary_frames: [0; NCLASSES],
        }
    }

    /// Syllable-rate share of the envelope's variance and its depth (dB std).
    fn modulation(&self) -> (f32, f32) {
        let n = SPEECH_WIN_FRAMES;
        let mean = self.env.iter().sum::<f32>() / n as f32;
        let var: f32 = self.env.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>();
        if var <= TINY {
            return (0.0, 0.0);
        }
        let mut band = 0f32;
        for j in 0..self.nbins {
            let (mut re, mut im) = (0f32, 0f32);
            for i in 0..n {
                let x = self.env[(self.env_pos + i) % n] - mean;
                re += x * self.cos[j * n + i];
                im -= x * self.sin[j * n + i];
            }
            band += re * re + im * im;
        }
        // Parseval: a real signal's positive-frequency bins carry half of n·Σx².
        let frac = (2.0 * band / (n as f32 * var)).min(1.0);
        (frac, (var / n as f32).sqrt())
    }

    /// The engine-glide rule over the last second.
    fn vehicle(&self) -> bool {
        let voiced = self.vpitch.iter().filter(|p| p.is_finite()).count();
        if (voiced as f32) < VEHICLE_MIN_VOICED * VEHICLE_WIN_FRAMES as f32 {
            return false;
        }
        let (mut lo, mut hi, mut max_step) = (f32::MAX, f32::MIN, 0f32);
        let mut prev = f32::NAN;
        for i in 0..VEHICLE_WIN_FRAMES {
            let p = self.vpitch[(self.vpos + i) % VEHICLE_WIN_FRAMES];
            if p.is_finite() {
                lo = lo.min(p);
                hi = hi.max(p);
                if prev.is_finite() {
                    max_step = max_step.max((p - prev).abs());
                }
                prev = p;
            }
        }
        hi - lo >= VEHICLE_MIN_GLIDE && max_step <= VEHICLE_MAX_STEP
    }
}

/// Autocorrelation pitch tracker on a boxcar-decimated copy (~4 kHz).
struct Pitch {
    decim: usize,
    fs: f32,
    acc: f32,
    n: usize,
    ring: Box<[f32]>,
    pos: usize,
    lin: Box<[f32]>,
    r: Box<[f32]>,
    min_lag: usize,
    max_lag: usize,
}

impl Pitch {
    fn new(sample_rate: f32) -> Self {
        let decim = ((sample_rate / PITCH_FS as f32).round() as usize).max(1);
        let fs = sample_rate / decim as f32;
        Self {
            decim,
            fs,
            acc: 0.0,
            n: 0,
            ring: vec![0f32; PITCH_WIN].into_boxed_slice(),
            pos: 0,
            lin: vec![0f32; PITCH_WIN].into_boxed_slice(),
            r: vec![0f32; PITCH_WIN].into_boxed_slice(),
            min_lag: ((fs / VOICE_MAX_HZ).floor() as usize).max(2),
            max_lag: ((fs / VEHICLE_MIN_HZ).ceil() as usize).min(PITCH_WIN * 3 / 4),
        }
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.acc += x;
        self.n += 1;
        if self.n >= self.decim {
            self.ring[self.pos] = self.acc / self.n as f32;
            self.pos = (self.pos + 1) % PITCH_WIN;
            self.acc = 0.0;
            self.n = 0;
        }
    }

    /// `(pitch_hz, periodicity)`; periodicity is the normalised
    /// autocorrelation peak (0 when silent).
    fn estimate(&mut self) -> (f32, f32) {
        for i in 0..PITCH_WIN {
            self.lin[i] = self.ring[(self.pos + i) % PITCH_WIN];
        }
        let x = &self.lin;
        let mut best = (0usize, 0f32);
        for lag in self.min_lag..=self.max_lag {
            let m = PITCH_WIN - lag;
            let (mut xy, mut xx, mut yy) = (0f32, 0f32, 0f32);
            for i in 0..m {
                let (a, b) = (x[i], x[i + lag]);
                xy += a * b;
                xx += a * a;
                yy += b * b;
            }
            let r = if xx > TINY && yy > TINY { xy / (xx * yy).sqrt() } else { 0.0 };
            self.r[lag] = r;
            if r > best.1 {
                best = (lag, r);
            }
        }
        if best.0 == 0 {
            return (0.0, 0.0);
        }
        // The shortest lag that is a local peak nearly as good as the best:
        // avoids reporting an octave (or more) below the true pitch.
        let mut lag = best.0;
        for l in self.min_lag..=self.max_lag {
            let left = if l > self.min_lag { self.r[l - 1] } else { f32::MIN };
            let right = if l < self.max_lag { self.r[l + 1] } else { f32::MIN };
            if self.r[l] >= 0.9 * best.1 && self.r[l] >= left && self.r[l] >= right {
                lag = l;
                break;
            }
        }
        (self.fs / lag as f32, self.r[lag].max(0.0))
    }
}

/// Streaming analyzer. Feed it audio with [`push`](Self::push) or
/// [`push_interleaved`](Self::push_interleaved).
pub struct Analyzer {
    frame_len: usize,
    filters: [BandPass; NBANDS],
    acc: [f32; NBANDS],
    acc_total: f32,
    clipped: u32,
    n: usize,
    floor: [f32; NBANDS],
    prev: [f32; NBANDS],
    primed: bool,
    lt_db: f32,
    /// Active frames since priming: the session loudness is a plain running
    /// mean until it has 30 s behind it, so a loud first frame does not
    /// skew it for minutes.
    lt_n: u32,
    st_db: f32,
    jump_frames: u32,
    /// The last few frames' band levels (a ring).
    recent: [[f32; NBANDS]; RECENT_FRAMES],
    recent_pos: usize,
    /// Frames since start (for rhythm).
    now: u64,
    /// Onset frames of the last two footstep-like events.
    last_steps: [u64; 2],
    event: Event,
    labels: Labels,
    pitch: Pitch,
    input_idle_ms: u32,
    stats: Stats,
}

impl Analyzer {
    /// A fresh analyzer at `sample_rate`. Allocates; call off the audio path.
    pub fn new(sample_rate: u32) -> Self {
        let fs = sample_rate.max(8000) as f32;
        // 1/3-octave bandwidth: Q = sqrt(2^(1/3)) / (2^(1/3) - 1).
        let q = 2f32.powf(1.0 / 6.0) / (2f32.powf(1.0 / 3.0) - 1.0);
        let mut filters = [BandPass::default(); NBANDS];
        for (f, &hz) in filters.iter_mut().zip(BANDS_HZ.iter()) {
            *f = BandPass::new(fs, hz, q);
        }
        Self {
            frame_len: (fs as usize * FRAME_MS as usize / 1000).max(1),
            filters,
            acc: [0.0; NBANDS],
            acc_total: 0.0,
            clipped: 0,
            n: 0,
            floor: [-120.0; NBANDS],
            prev: [-120.0; NBANDS],
            primed: false,
            lt_db: -40.0,
            lt_n: 0,
            st_db: -40.0,
            jump_frames: 0,
            recent: [[-120.0; NBANDS]; RECENT_FRAMES],
            recent_pos: 0,
            now: 0,
            last_steps: [0; 2],
            event: Event {
                open: false,
                loud_frames: 0,
                quiet: 0,
                onset_db: 0.0,
                rising_low: 0,
                rising_high: 0,
                pre: [-120.0; NBANDS],
                start_frame: 0,
                peak_db: -120.0,
                sum_lin: [0.0; NBANDS],
                floor_db: [-120.0; NBANDS],
                floor_lin: [0.0; NBANDS],
                frames: vec![[0.0; NBANDS]; EVENT_MAX_FRAMES].into_boxed_slice(),
                labels: vec![0u8; EVENT_MAX_FRAMES].into_boxed_slice(),
                nframes: 0,
                overlaps: 0,
            },
            labels: Labels::new(),
            pitch: Pitch::new(fs),
            input_idle_ms: 0,
            stats: Stats::default(),
        }
    }

    /// How long since the user last touched keyboard, mouse or pad (from
    /// `GetLastInputInfo`, read by the caller about once a second).
    pub fn set_input_idle_ms(&mut self, ms: u32) {
        self.input_idle_ms = ms;
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Hand over what has been gathered since the last call and start a
    /// fresh aggregate. The analysis state (floor, loudness, labels) carries on.
    pub fn take_stats(&mut self) -> Stats {
        std::mem::take(&mut self.stats)
    }

    pub fn into_stats(self) -> Stats {
        self.stats
    }

    /// Feed interleaved samples with `channels` channels; downmixed to mono.
    /// A clipped sample on any channel counts as clipped. Never allocates.
    pub fn push_interleaved(&mut self, samples: &[f32], channels: usize) {
        let ch = channels.max(1);
        let inv = 1.0 / ch as f32;
        for frame in samples.chunks_exact(ch) {
            let mut sum = 0f32;
            let mut clip = false;
            for &x in frame {
                let x = if x.is_finite() { x } else { 0.0 };
                clip |= x.abs() >= CLIP_LEVEL;
                sum += x;
            }
            self.sample(sum * inv, clip);
        }
    }

    /// Feed mono samples. Never allocates.
    pub fn push(&mut self, samples: &[f32]) {
        for &x in samples {
            let x = if x.is_finite() { x } else { 0.0 };
            self.sample(x, x.abs() >= CLIP_LEVEL);
        }
    }

    #[inline]
    fn sample(&mut self, x: f32, clipped: bool) {
        for (f, a) in self.filters.iter_mut().zip(self.acc.iter_mut()) {
            if f.enabled {
                let y = f.run(x);
                *a += y * y;
            }
        }
        self.pitch.push(x);
        self.acc_total += x * x;
        self.clipped += clipped as u32;
        self.n += 1;
        if self.n >= self.frame_len {
            self.frame();
        }
    }

    fn frame(&mut self) {
        self.now += 1;
        let n = self.n as f32;
        let mut level = [0f32; NBANDS];
        for (l, a) in level.iter_mut().zip(self.acc.iter_mut()) {
            *l = db(*a / n);
            *a = 0.0;
        }
        let total_pow = self.acc_total / n;
        let total = db(total_pow);
        let clipped = self.clipped;
        self.acc_total = 0.0;
        self.clipped = 0;
        self.n = 0;

        if total < SILENCE_DBFS {
            self.reject(Reject::Silence);
            self.prev = level;
            return;
        }
        if !self.primed {
            self.floor = level;
            self.prev = level;
            self.lt_db = total;
            self.st_db = total;
            self.primed = true;
        }

        // Onset: count bands that jumped above both the last frame and the floor.
        let (mut rising, mut rising_hf, mut rising_lf) = (0usize, 0usize, 0usize);
        let (mut sharp_lf, mut sharp_hf) = (0usize, 0usize);
        let mut onset = 0f32;
        let mut onset_band = 0usize;
        for b in 0..NBANDS {
            let rise = level[b] - self.prev[b].max(self.floor[b]);
            let hf = BANDS_HZ[b] >= 1000.0;
            if (hf && rise >= RISE_HF_DB) || rise >= RISE_LF_DB {
                rising += 1;
                if hf {
                    rising_hf += 1;
                }
                if BANDS_HZ[b] < 250.0 {
                    rising_lf += 1;
                }
                if rise >= GUN_MIN_ONSET_DB {
                    if hf {
                        sharp_hf += 1;
                    } else if BANDS_HZ[b] < 250.0 {
                        sharp_lf += 1;
                    }
                }
                if rise > onset {
                    onset = rise;
                    onset_band = b;
                }
            }
        }
        let is_onset = rising >= ONSET_MIN_BANDS;

        let (label, chat) = self.update_labels(&level);

        let verdict = self.gate_frame(total, clipped).or(chat.then_some(Reject::PlayerChat));
        if let Some(why) = verdict {
            self.reject(why);
            if !is_onset {
                self.follow_floor(&level);
            }
            self.prev = level;
            return;
        }

        self.stats.active_frames += 1;
        self.lt_n = self.lt_n.saturating_add(1);
        let alpha = LT_ALPHA.max(1.0 / self.lt_n as f32);
        self.lt_db += alpha * (total - self.lt_db);
        self.count_stationary(label);

        if self.event.open {
            // The background can always be found to be lower, even mid-event.
            for b in 0..NBANDS {
                if level[b] < self.floor[b] {
                    let (l, f) = (lin(level[b]), lin(self.floor[b]));
                    self.floor[b] = db(f + FLOOR_DOWN * (l - f));
                }
            }
            if is_onset && rising_hf >= ONSET_MIN_BANDS && self.event.loud_frames > 2 {
                // A cue-like onset inside a running event: time overlap.
                self.event.overlaps = self.event.overlaps.saturating_add(1);
                // Bright, sharp, and nothing below 1 kHz rising with it (a
                // syllable lifts its harmonics; a footstep does not).
                if BANDS_HZ[onset_band] >= CUE_MIN_PEAK_HZ
                    && onset >= CUE_MIN_ONSET_DB
                    && rising == rising_hf
                    && (label != SoundClass::Voice || self.rhythmic(self.now))
                {
                    // A bright transient over something else (a step during
                    // an explosion, a reload over music): file it as a cue now.
                    self.overlapped_cue(&level);
                }
            }
            self.extend_event(&level, total, label);
        } else if is_onset {
            let _ = rising_lf;
            self.open_event(onset, sharp_lf, sharp_hf);
            self.extend_event(&level, total, label);
        } else {
            // Background frame: the floor follows it; it is the mix and it
            // belongs to its stationary label.
            self.follow_floor(&level);
            let rel = self.rel();
            Stats::add(&mut self.stats.frame_hist, &level, rel);
            Stats::add(self.stats.hist_mut(label), &level, rel);
            self.stats.class_frames[label.index()] += 1;
        }
        self.recent[self.recent_pos] = level;
        self.recent_pos = (self.recent_pos + 1) % RECENT_FRAMES;
        self.prev = level;
    }

    /// A bright onset inside another event: footsteps when it keeps the
    /// walking rhythm, otherwise a mechanical click. Its level is what it
    /// added over the quietest of the last few frames.
    fn overlapped_cue(&mut self, level: &[f32; NBANDS]) {
        let mut own = [0f32; NBANDS];
        for b in 0..NBANDS {
            // Below 1 kHz the rise belongs to whatever it landed in.
            own[b] = if BANDS_HZ[b] < 1000.0 {
                -120.0
            } else {
                let before = self.recent.iter().map(|r| r[b]).fold(f32::MAX, f32::min);
                db((lin(level[b]) - lin(before)).max(TINY))
            };
        }
        let rhythmic = self.rhythmic(self.now);
        let c = if rhythmic { SoundClass::Footsteps } else { SoundClass::Mechanical };
        if rhythmic {
            self.stats.rhythmic_steps += 1;
        }
        self.last_steps = [self.last_steps[1], self.now];
        let rel = self.rel();
        self.stats.events[c.index()] += 1;
        self.stats.class_frames[c.index()] += 1;
        Stats::add(self.stats.hist_mut(c), &own, rel);
    }

    /// Update the look-back detectors; this frame's stationary label and
    /// whether it is player chat.
    fn update_labels(&mut self, level: &[f32; NBANDS]) -> (SoundClass, bool) {
        let (pitch_hz, periodicity) = self.pitch.estimate();
        let floor = &self.floor;
        let l = &mut self.labels;

        // Tonality: share of the energetic 200 Hz – 1.25 kHz bands that held still.
        let (mut steady, mut lit) = (0u32, 0u32);
        let top = (6..=14).map(|b| level[b]).fold(f32::MIN, f32::max);
        for b in 6..=14 {
            if level[b] >= top - 20.0 {
                lit += 1;
                if (level[b] - self.prev[b]).abs() < TONAL_STEADY_DB {
                    steady += 1;
                }
            }
        }
        let t = if lit > 0 { steady as f32 / lit as f32 } else { 0.0 };
        l.tonal += TONAL_ALPHA * (t - l.tonal);

        // Speech envelope: 250 Hz – 2 kHz against 2.5 kHz and up.
        let (mut mid, mut high) = (0f32, 0f32);
        // Chat: added energy (over the floor) below 250 Hz, in the speech
        // band, and above 4.5 kHz.
        let (mut ex_low, mut ex_speech, mut ex_high) = (0f32, 0f32, 0f32);
        for (b, &hz) in BANDS_HZ.iter().enumerate() {
            let p = lin(level[b]);
            if (250.0..=2000.0).contains(&hz) {
                mid += p;
            } else if hz >= 2500.0 {
                high += p;
            }
            let ex = (p - lin(floor[b])).max(0.0);
            if hz < 180.0 {
                ex_low += ex;
            } else if (315.0..=3150.0).contains(&hz) {
                ex_speech += ex;
            } else if hz >= 5000.0 {
                ex_high += ex;
            }
        }
        l.env[l.env_pos] = db(mid);
        l.env_pos = (l.env_pos + 1) % SPEECH_WIN_FRAMES;
        l.env_filled = (l.env_filled + 1).min(SPEECH_WIN_FRAMES);
        l.dominance_db += 0.02 * ((db(mid) - db(high)) - l.dominance_db);
        l.eval_count += 1;
        if l.eval_count >= SPEECH_EVAL_EVERY {
            l.eval_count = 0;
            l.speech = if l.env_filled >= SPEECH_WIN_FRAMES {
                let (frac, depth) = l.modulation();
                frac >= SPEECH_MOD_FRACTION
                    && depth >= SPEECH_MIN_DEPTH_DB
                    && l.dominance_db >= SPEECH_DOMINANCE_DB
            } else {
                false
            };
        }

        // Engine pitch track.
        let engine =
            periodicity >= VOICED_MIN && (VEHICLE_MIN_HZ..=VEHICLE_MAX_HZ).contains(&pitch_hz);
        l.vpitch[l.vpos] = if engine { pitch_hz.ln() } else { f32::NAN };
        l.vpos = (l.vpos + 1) % VEHICLE_WIN_FRAMES;

        let voiced = periodicity >= VOICED_MIN && (VOICE_MIN_HZ..=VOICE_MAX_HZ).contains(&pitch_hz);
        let voice = l.speech;
        if voice && voiced && ex_speech > TINY {
            let s = db(ex_speech);
            l.chat_low_db += CHAT_ALPHA * ((db(ex_low) - s).max(-60.0) - l.chat_low_db);
            l.chat_high_db += CHAT_ALPHA * ((db(ex_high) - s).max(-60.0) - l.chat_high_db);
        }
        let chat = voice && l.chat_low_db < CHAT_EDGE_DB && l.chat_high_db < CHAT_EDGE_DB;
        let vehicle = !voice && l.vehicle();
        let music = !voice && !vehicle && l.tonal >= TONAL_MIN_FRACTION;

        // Voice clustering, half a second of voiced game voice at a time.
        if voice && voiced && !chat {
            l.chunk_sum += pitch_hz.ln();
            l.chunk_n += 1;
        }
        (label_class(voice, vehicle, music), chat)
    }

    /// Count stationary frames into "events" and close voice chunks.
    fn count_stationary(&mut self, label: SoundClass) {
        let l = &mut self.labels;
        if !label.is_transient() {
            let k = label.index();
            l.stationary_frames[k] += 1;
            if l.stationary_frames[k] % STATIONARY_EVENT_FRAMES == 0 {
                self.stats.events[k] += 1;
            }
        }
        if l.chunk_n >= VOICE_CHUNK_FRAMES {
            let hz = (l.chunk_sum / l.chunk_n as f32).exp();
            add_voice(&mut self.stats.voices, hz, 1);
            l.chunk_sum = 0.0;
            l.chunk_n = 0;
        }
    }

    /// Offset that makes a level relative to the session loudness.
    fn rel(&self) -> f32 {
        REL_REF_DB - self.lt_db
    }

    fn follow_floor(&mut self, level: &[f32; NBANDS]) {
        for b in 0..NBANDS {
            let k = if level[b] < self.floor[b] { FLOOR_DOWN } else { FLOOR_UP };
            let (l, f) = (lin(level[b]), lin(self.floor[b]));
            self.floor[b] = db(f + k * (l - f));
        }
    }

    /// Count a rejected frame and drop any event in progress.
    fn reject(&mut self, why: Reject) {
        let s = &mut self.stats;
        match why {
            Reject::Silence => s.silent_frames += 1,
            Reject::Clipped => s.clipped_frames += 1,
            Reject::LevelJump => s.level_jump_frames += 1,
            Reject::InputIdle => s.input_idle_frames += 1,
            Reject::PlayerChat => s.chat_frames += 1,
        }
        // A voice chunk straddling a cutscene or chat is not game voice.
        if why != Reject::Silence {
            self.labels.chunk_sum = 0.0;
            self.labels.chunk_n = 0;
        }
        if self.event.open {
            if why == Reject::Silence {
                // A sound that decays into silence is complete: classify it.
                self.close_event();
            } else {
                self.event.open = false;
                self.event.nframes = 0;
                self.stats.discarded_events += 1;
            }
        }
    }

    /// The context gate: `None` when this frame counts.
    fn gate_frame(&mut self, total: f32, clipped: u32) -> Option<Reject> {
        // Level jump: short-term level off the session loudness, for long.
        // Up while off it, down while on it: an explosion pokes out for a
        // moment, a volume change stays out.
        self.st_db += ST_ALPHA * (total - self.st_db);
        let st_db = self.st_db;
        if (st_db - self.lt_db).abs() > LEVEL_JUMP_DB {
            self.jump_frames += 1;
        } else {
            self.jump_frames = self.jump_frames.saturating_sub(1);
        }

        if clipped >= CLIP_MIN_SAMPLES {
            return Some(Reject::Clipped);
        }
        if self.jump_frames >= LEVEL_JUMP_REBASE_FRAMES {
            // A new volume: re-base the session loudness and the floor to it.
            let delta = st_db - self.lt_db;
            self.lt_db = st_db;
            self.lt_n = 0;
            for f in self.floor.iter_mut() {
                *f += delta;
            }
            self.jump_frames = 0;
            self.stats.level_jumps += 1;
            return Some(Reject::LevelJump);
        }
        if self.jump_frames >= LEVEL_JUMP_REJECT_FRAMES {
            return Some(Reject::LevelJump);
        }
        if self.input_idle_ms > INPUT_IDLE_MS {
            return Some(Reject::InputIdle);
        }
        None
    }

    fn open_event(&mut self, onset: f32, rising_low: usize, rising_high: usize) {
        let e = &mut self.event;
        e.open = true;
        e.loud_frames = 0;
        e.quiet = 0;
        e.onset_db = onset;
        e.rising_low = rising_low;
        e.rising_high = rising_high;
        e.pre = self.prev;
        e.start_frame = self.now;
        e.peak_db = -120.0;
        e.sum_lin = [0.0; NBANDS];
        e.floor_db = self.floor;
        for (l, &d) in e.floor_lin.iter_mut().zip(self.floor.iter()) {
            *l = lin(d);
        }
        e.nframes = 0;
        e.overlaps = 0;
    }

    fn extend_event(&mut self, level: &[f32; NBANDS], total: f32, label: SoundClass) {
        if self.event.nframes == 1 {
            // Second frame: re-measure the attack against the frame before
            // the onset, keeping whichever reading is sharper.
            let e = &mut self.event;
            let (mut on, mut sl, mut sh) = (0f32, 0usize, 0usize);
            for b in 0..NBANDS {
                let rise = level[b] - e.pre[b].max(e.floor_db[b]);
                on = on.max(rise);
                if rise >= GUN_MIN_ONSET_DB {
                    if BANDS_HZ[b] >= 1000.0 {
                        sh += 1;
                    } else if BANDS_HZ[b] < 250.0 {
                        sl += 1;
                    }
                }
            }
            e.onset_db = e.onset_db.max(on);
            e.rising_low = e.rising_low.max(sl);
            e.rising_high = e.rising_high.max(sh);
        }
        let mut still_on = false;
        for b in 0..NBANDS {
            if self.filters[b].enabled {
                let margin = if BANDS_HZ[b] >= 1000.0 { EVENT_OFF_DB } else { EVENT_OFF_LF_DB };
                still_on |= level[b] - self.event.floor_db[b] >= margin;
            }
        }
        let e = &mut self.event;
        if e.nframes < EVENT_MAX_FRAMES {
            e.frames[e.nframes] = *level;
            e.labels[e.nframes] = label.index() as u8;
            e.nframes += 1;
        }
        if !still_on {
            e.quiet += 1;
            if e.quiet >= EVENT_OFF_FRAMES {
                self.close_event();
            }
            return;
        }
        e.quiet = 0;
        e.loud_frames += 1;
        e.peak_db = e.peak_db.max(total);
        for (s, &l) in e.sum_lin.iter_mut().zip(level.iter()) {
            *s += lin(l);
        }
        if e.nframes >= EVENT_MAX_FRAMES {
            self.close_event();
            // A 3 s event is steady material: the floor catches up to the
            // quietest of the last few frames (not this one, which may be a cue).
            for b in 0..NBANDS {
                self.floor[b] = self.recent.iter().map(|r| r[b]).fold(level[b], f32::min);
            }
        }
    }

    fn close_event(&mut self) {
        let class = self.classify();
        let rel = self.rel();
        if class.is_some_and(SoundClass::is_cue) {
            // Every short bright sound feeds the rhythm detector, so a walk
            // is recognised even when its first steps were heard as rustle.
            if class == Some(SoundClass::Footsteps) && self.rhythmic(self.event.start_frame) {
                self.stats.rhythmic_steps += 1;
            }
            self.last_steps = [self.last_steps[1], self.event.start_frame];
        }
        let e = &mut self.event;
        e.open = false;
        let s = &mut self.stats;
        let n = NBANDS * HIST_BINS;
        match class {
            Some(c) if c.is_transient() => {
                s.events[c.index()] += 1;
                s.overlaps[c.index()] += e.overlaps as u64;
                let loud = e.loud_frames.max(1) as f32;
                let mut lvl = [0f32; NBANDS];
                for b in 0..NBANDS {
                    let mean = e.sum_lin[b] / loud;
                    // Cues: the energy they added (what must be heard).
                    // Maskers: their whole level (what covers).
                    lvl[b] =
                        if c.is_cue() { db((mean - e.floor_lin[b]).max(TINY)) } else { db(mean) };
                }
                Stats::add(&mut s.class_hist[c.index() * n..(c.index() + 1) * n], &lvl, rel);
                s.class_frames[c.index()] += e.nframes as u64;
                for f in e.frames[..e.nframes].iter() {
                    // The mix overall: a cue's frames count at the floor it
                    // sat on, a masker's at its own level.
                    let bg = if c.is_cue() { &e.floor_db } else { f };
                    Stats::add(&mut s.frame_hist, bg, rel);
                }
            }
            Some(c) => {
                // A long event under a stationary label: its frames are that class.
                s.overlaps[c.index()] += e.overlaps as u64;
                for f in e.frames[..e.nframes].iter() {
                    Stats::add(&mut s.frame_hist, f, rel);
                    Stats::add(&mut s.class_hist[c.index() * n..(c.index() + 1) * n], f, rel);
                }
                s.class_frames[c.index()] += e.nframes as u64;
            }
            None => {
                // No class: each frame belongs to its stationary label.
                if e.loud_frames > 0 {
                    s.unclassified_events += 1;
                }
                for (f, &lab) in e.frames[..e.nframes].iter().zip(e.labels.iter()) {
                    let k = lab as usize;
                    Stats::add(&mut s.frame_hist, f, rel);
                    Stats::add(&mut s.class_hist[k * n..(k + 1) * n], f, rel);
                    s.class_frames[k] += 1;
                }
            }
        }
        e.nframes = 0;
    }

    /// The last two inter-onset intervals agree and sit in the walking range.
    fn rhythmic(&self, onset: u64) -> bool {
        let [a, b] = self.last_steps;
        if a == 0 || b == 0 || onset <= b || b <= a {
            return false;
        }
        let i1 = ((onset - b) * FRAME_MS as u64) as f32;
        let i2 = ((b - a) * FRAME_MS as u64) as f32;
        let range = RHYTHM_MIN_MS as f32..=RHYTHM_MAX_MS as f32;
        range.contains(&i1) && range.contains(&i2) && (i1 - i2).abs() <= RHYTHM_TOLERANCE * i2
    }

    fn classify(&self) -> Option<SoundClass> {
        let e = &self.event;
        if e.loud_frames == 0 {
            return None;
        }
        let n = e.loud_frames as f32;
        let (mut low, mut high) = (0f32, 0f32);
        let mut hf_added = [0f32; NBANDS];
        for b in 0..NBANDS {
            let added = (e.sum_lin[b] / n - e.floor_lin[b]).max(0.0);
            if BANDS_HZ[b] < 250.0 {
                low += added;
            } else if BANDS_HZ[b] >= 1000.0 {
                high += added;
                hf_added[b] = added;
            }
        }
        let hf_max = hf_added.iter().cloned().fold(0f32, f32::max);
        // Where the added energy peaks, over the whole spectrum.
        let mut peak_b = 0;
        for b in 0..NBANDS {
            let a = (e.sum_lin[b] / n - e.floor_lin[b]).max(0.0);
            if a > (e.sum_lin[peak_b] / n - e.floor_lin[peak_b]).max(0.0) {
                peak_b = b;
            }
        }
        let bright = BANDS_HZ[peak_b] >= CUE_MIN_PEAK_HZ;
        let spread = hf_added.iter().filter(|&&a| hf_max > 0.0 && a >= hf_max * 0.25).count();
        let dur_ms = e.loud_frames * FRAME_MS;
        let above = e.peak_db - self.lt_db;
        let loud = above >= LOUD_ABOVE_DB;
        let hf = high > low * WEIGHT_RATIO;
        let lf = low > high * WEIGHT_RATIO;

        // A long event that is really a sustained source.
        if dur_ms >= ADOPT_LABEL_MS {
            let mut votes = [0u32; NCLASSES];
            for &l in &e.labels[..e.nframes] {
                votes[l as usize] += 1;
            }
            for c in [SoundClass::Voice, SoundClass::Vehicle, SoundClass::Music] {
                if votes[c.index()] * 2 > e.nframes as u32 {
                    return Some(c);
                }
            }
        }
        // Under a talker, sharp onsets are syllables and consonants.
        let voiced = e.labels[..e.nframes]
            .iter()
            .filter(|&&l| l as usize == SoundClass::Voice.index())
            .count();
        let under_voice = voiced * 2 > e.nframes;
        if !under_voice
            && e.onset_db >= GUN_MIN_ONSET_DB
            && e.rising_low >= GUN_MIN_LOW_BANDS
            && e.rising_high >= GUN_MIN_HIGH_BANDS
            && above >= GUN_LOUD_DB
            && dur_ms >= GUN_MIN_TAIL_MS
            && high >= low * GUN_MIN_HIGH_RATIO
        {
            return Some(SoundClass::Gunshot);
        }
        if lf && (loud || dur_ms >= CUE_MAX_MS) {
            return Some(SoundClass::Explosion);
        }
        if hf && bright && !loud {
            let rhythmic = self.rhythmic(e.start_frame);
            if rhythmic && dur_ms < CUE_MAX_MS && e.onset_db >= CUE_MIN_ONSET_DB {
                return Some(SoundClass::Footsteps);
            }
            // Under a talker, a lone bright tick is a consonant, not a cue.
            if under_voice {
                return None;
            }
            if dur_ms <= MECH_MAX_MS && e.onset_db >= MECH_MIN_ONSET_DB {
                return Some(SoundClass::Mechanical);
            }
            if e.onset_db < FOLIAGE_MAX_ONSET_DB
                && dur_ms <= FOLIAGE_MAX_MS
                && spread >= FOLIAGE_MIN_SPREAD
            {
                return Some(SoundClass::Foliage);
            }
            if dur_ms < CUE_MAX_MS && e.onset_db >= CUE_MIN_ONSET_DB {
                // A single step, or the first of a train.
                return Some(SoundClass::Footsteps);
            }
        }
        if loud && dur_ms >= 100 {
            // Crashes and bangs that are not bass-heavy.
            return Some(SoundClass::Explosion);
        }
        None
    }
}

#[inline]
fn db(power: f32) -> f32 {
    10.0 * (power.max(TINY)).log10()
}

#[inline]
fn lin(db: f32) -> f32 {
    10f32.powf(db / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::synth::{Segment, Synth, FS};

    fn run(segments: &[(Segment, f32)]) -> Stats {
        let mut a = Analyzer::new(FS);
        let mut s = Synth::new(FS, 0x5eed);
        for &(seg, secs) in segments {
            s.render(seg, secs, |block| a.push(block));
        }
        a.into_stats()
    }

    fn share(s: &Stats, c: SoundClass) -> f32 {
        s.class_frames[c.index()] as f32 / s.active_frames.max(1) as f32
    }

    #[test]
    fn named_thresholds_have_their_documented_values() {
        assert_eq!(FRAME_MS, 10);
        assert_eq!(SILENCE_DBFS, -70.0);
        assert_eq!(INPUT_IDLE_MS, 20_000);
        assert_eq!(CUE_MAX_MS, 300);
        assert_eq!(LEVEL_JUMP_DB, 10.0);
        assert_eq!(CHAT_EDGE_DB, -20.0);
        assert!(LEVEL_JUMP_REJECT_FRAMES < LEVEL_JUMP_REBASE_FRAMES);
        assert_eq!(SPEECH_WIN_FRAMES as u32 * FRAME_MS, 2000);
        assert!(SPEECH_MOD_LO_HZ < 4.0 && SPEECH_MOD_HI_HZ > 4.0, "4 Hz syllable rate inside");
        assert!(RISE_LF_DB > RISE_HF_DB);
        assert!(MECH_MAX_MS < CUE_MAX_MS && CUE_MAX_MS < FOLIAGE_MAX_MS);
        assert!(RHYTHM_MIN_MS < RHYTHM_MAX_MS && RHYTHM_TOLERANCE < 0.5);
        assert!(VEHICLE_MAX_STEP < VEHICLE_MIN_GLIDE);
        assert!(VOICE_MIN_HZ < VOICE_MAX_HZ && VEHICLE_MIN_HZ < VEHICLE_MAX_HZ);
        assert_eq!(SoundClass::ALL.len(), NCLASSES);
        for (i, c) in SoundClass::ALL.iter().enumerate() {
            assert_eq!(c.index(), i);
        }
    }

    #[test]
    fn silence_learns_nothing() {
        let mut a = Analyzer::new(48_000);
        a.push(&vec![0.0; 48_000]);
        assert_eq!(a.stats().active_frames, 0);
        assert_eq!(a.stats().silent_frames, 100);
        assert!(a.stats().events.iter().all(|&e| e == 0));
    }

    #[test]
    fn ambience_is_stationary_background_with_no_transients() {
        let s = run(&[(Segment::Ambience, 30.0)]);
        assert_eq!(s.cue_events(), 0, "{:?}", s.events);
        assert!(
            s.count(SoundClass::Explosion) + s.count(SoundClass::Gunshot) < 5,
            "{:?}",
            s.events
        );
        assert!(share(&s, SoundClass::Ambience) > 0.9, "{:?}", s.class_frames);
    }

    #[test]
    fn rhythmic_footsteps_are_footsteps() {
        let s = run(&[(Segment::Footsteps, 30.0)]);
        let steps = s.count(SoundClass::Footsteps);
        assert!(steps >= 50, "{:?}", s.events);
        assert!(s.rhythmic_steps >= steps / 2, "{} of {steps}", s.rhythmic_steps);
        let others = s.count(SoundClass::Mechanical) + s.count(SoundClass::Foliage);
        assert!(steps > 3 * others, "{:?}", s.events);
    }

    #[test]
    fn irregular_clicks_are_mechanical() {
        let s = run(&[(Segment::Reload, 30.0)]);
        let m = s.count(SoundClass::Mechanical);
        assert!(m >= 20, "{:?}", s.events);
        assert!(m > 2 * s.count(SoundClass::Footsteps), "{:?}", s.events);
    }

    #[test]
    fn soft_rustles_are_foliage() {
        let s = run(&[(Segment::Foliage, 30.0)]);
        let f = s.count(SoundClass::Foliage);
        assert!(f >= 15, "{:?}", s.events);
        let others = s.count(SoundClass::Footsteps) + s.count(SoundClass::Mechanical);
        assert!(f > 2 * others, "{:?}", s.events);
    }

    #[test]
    fn sharp_broadband_bursts_are_gunshots() {
        let s = run(&[(Segment::Gunfire, 30.0)]);
        let g = s.count(SoundClass::Gunshot);
        assert!(g >= 15, "{:?}", s.events);
        assert!(g > 2 * s.count(SoundClass::Explosion), "{:?}", s.events);
    }

    #[test]
    fn low_loud_bursts_are_explosions() {
        let s = run(&[(Segment::Explosions, 40.0)]);
        let x = s.count(SoundClass::Explosion);
        assert!((8..=12).contains(&x), "{:?}", s.events);
        assert_eq!(s.count(SoundClass::Gunshot), 0, "{:?}", s.events);
    }

    #[test]
    fn a_gliding_engine_is_a_vehicle() {
        let s = run(&[(Segment::Vehicle, 30.0)]);
        assert!(share(&s, SoundClass::Vehicle) > 0.6, "{:?}", s.class_frames);
        assert!(s.count(SoundClass::Vehicle) >= 30, "{:?}", s.events);
        assert!(share(&s, SoundClass::Voice) < 0.1, "{:?}", s.class_frames);
    }

    #[test]
    fn a_held_melody_is_music_not_a_vehicle() {
        let s = run(&[(Segment::Music, 30.0)]);
        assert!(share(&s, SoundClass::Music) > 0.6, "{:?}", s.class_frames);
        assert!(share(&s, SoundClass::Vehicle) < 0.1, "{:?}", s.class_frames);
    }

    #[test]
    fn speech_is_voice_and_two_talkers_are_two_voices() {
        let mut syn = Synth::new(FS, 0x5eed);
        syn.voice_hz = 110.0;
        let mut a = Analyzer::new(FS);
        syn.render(Segment::Speech, 20.0, |b| a.push(b));
        syn.voice_hz = 220.0;
        syn.render(Segment::Speech, 20.0, |b| a.push(b));
        let s = a.into_stats();
        assert!(share(&s, SoundClass::Voice) > 0.6, "{:?}", s.class_frames);
        assert_eq!(s.distinct_voices(), 2, "{:?}", s.voices);
        let mut hz: Vec<f32> = s.voices.iter().filter(|v| v.chunks >= 2).map(|v| v.hz).collect();
        hz.sort_by(f32::total_cmp);
        assert!((hz[0] / 110.0 - 1.0).abs() < 0.2 && (hz[1] / 220.0 - 1.0).abs() < 0.2, "{hz:?}");
    }

    #[test]
    fn band_limited_noisy_chat_is_excluded_full_band_voice_over_is_kept() {
        let vo = run(&[(Segment::Speech, 20.0)]);
        let chat = run(&[(Segment::PlayerChat, 20.0)]);
        // Voice-over counts as game voice...
        assert!(share(&vo, SoundClass::Voice) > 0.6, "{:?}", vo.class_frames);
        assert!(vo.chat_frames < 200, "VO mistaken for chat: {}", vo.chat_frames);
        // ...chat does not reach the statistics at all.
        assert!(
            chat.chat_frames >= 1200,
            "chat frames {} {:?}",
            chat.chat_frames,
            chat.class_frames
        );
        assert!(chat.class_frames[SoundClass::Voice.index()] < 400, "{:?}", chat.class_frames);
        // At most the half second before the chat measure settles.
        assert!(chat.voices.iter().map(|v| v.chunks).sum::<u32>() <= 2, "{:?}", chat.voices);
    }

    #[test]
    fn voice_and_music_with_no_input_are_a_cutscene() {
        // Context: voice counts while the player is giving input (callouts,
        // NPCs) and is excluded, with music, after 20 s without any.
        let mut syn = Synth::new(FS, 3);
        let mut a = Analyzer::new(FS);
        a.set_input_idle_ms(1_000);
        syn.render(Segment::Speech, 10.0, |b| a.push(b));
        let playing = a.stats().clone();
        assert!(
            playing.class_frames[SoundClass::Voice.index()] > 300,
            "{:?}",
            playing.class_frames
        );
        a.set_input_idle_ms(INPUT_IDLE_MS + 1);
        syn.render(Segment::Speech, 10.0, |b| a.push(b));
        syn.render(Segment::Music, 10.0, |b| a.push(b));
        let s = a.stats();
        assert!(s.input_idle_frames + s.level_jump_frames >= 1990, "{s:?}");
        assert_eq!(s.active_frames, playing.active_frames);
    }

    #[test]
    fn no_input_for_twenty_seconds_does_not_count() {
        let mut a = Analyzer::new(FS);
        let mut s = Synth::new(FS, 7);
        a.set_input_idle_ms(500);
        s.render(Segment::Gameplay, 10.0, |b| a.push(b));
        let playing = a.stats().clone();
        assert!(playing.active_frames >= 900 && playing.input_idle_frames == 0);
        // Exactly at the limit still counts; past it, nothing does.
        a.set_input_idle_ms(INPUT_IDLE_MS);
        s.render(Segment::Gameplay, 1.0, |b| a.push(b));
        assert_eq!(a.stats().input_idle_frames, 0);
        a.set_input_idle_ms(INPUT_IDLE_MS + 1);
        s.render(Segment::Gameplay, 10.0, |b| a.push(b));
        assert_eq!(a.stats().input_idle_frames, 1000);
    }

    #[test]
    fn footsteps_and_explosions_in_a_mix_are_separated() {
        let s = run(&[(Segment::Gameplay, 60.0)]);
        assert!(s.cue_events() >= 80, "{:?}", s.events);
        assert!((12..=20).contains(&s.count(SoundClass::Explosion)), "{:?}", s.events);
        let b3k = BANDS_HZ.iter().position(|&f| f == 3150.0).unwrap();
        let b63 = BANDS_HZ.iter().position(|&f| f == 63.0).unwrap();
        let steps = s.hist(SoundClass::Footsteps);
        assert!(Stats::median(steps, b3k).unwrap() > Stats::median(steps, b63).unwrap() + 20.0);
        let booms = s.hist(SoundClass::Explosion);
        assert!(Stats::median(booms, b63).unwrap() > Stats::median(booms, b3k).unwrap() + 10.0);
        // Steps keep coming during explosions: some land inside one.
        assert!(s.overlaps[SoundClass::Explosion.index()] > 0, "{:?}", s.overlaps);
    }

    #[test]
    fn mixed_scene_keeps_every_layer() {
        // Footsteps over a vehicle, a music bed and a talker, all at once.
        let s = run(&[(Segment::MixedScene, 60.0)]);
        // The steps still come through as steps, not rustle or clicks...
        assert!(s.count(SoundClass::Footsteps) >= 40, "{:?}", s.events);
        assert!(s.rhythmic_steps >= 20, "{}", s.rhythmic_steps);
        // ...and the sustained layers are not mistaken for impacts. (Overlaps
        // are not separated: with four layers at once a frame carries one
        // stationary label, and the masking matrix works from the mix.)
        assert!(
            s.count(SoundClass::Gunshot) + s.count(SoundClass::Explosion) < 5,
            "{:?}",
            s.events
        );
    }

    #[test]
    fn clipping_is_rejected() {
        let s = run(&[(Segment::Clipped, 5.0)]);
        assert!(s.clipped_frames >= 450, "{}", s.clipped_frames);
        assert_eq!(s.cue_events(), 0);
    }

    #[test]
    fn a_volume_change_is_normalised_away() {
        let loud = run(&[(Segment::Gameplay, 60.0)]);
        let both = run(&[(Segment::Gameplay, 60.0), (Segment::GameplayQuiet, 60.0)]);
        assert!(both.level_jumps >= 1, "the re-base happened");
        let b3k = BANDS_HZ.iter().position(|&f| f == 3150.0).unwrap();
        let a = Stats::median(loud.hist(SoundClass::Footsteps), b3k).unwrap();
        let b = Stats::median(both.hist(SoundClass::Footsteps), b3k).unwrap();
        assert!((a - b).abs() <= 4.0, "cue level moved {a} -> {b}");
    }

    #[test]
    fn push_does_not_allocate_per_frame() {
        let mut a = Analyzer::new(44_100);
        let cap = a.event.frames.len();
        let env = a.labels.env.len();
        let mut s = Synth::new(44_100, 1);
        s.render(Segment::Gameplay, 10.0, |b| a.push(b));
        assert_eq!(a.event.frames.len(), cap);
        assert_eq!(a.labels.env.len(), env);
    }

    #[test]
    fn interleaved_stereo_downmixes_and_sees_one_channel_clipping() {
        let mut a = Analyzer::new(48_000);
        let mut st = Vec::new();
        for i in 0..48_000 {
            st.push(if i % 100 < 5 { 1.0 } else { 0.1 });
            st.push(0.0);
        }
        a.push_interleaved(&st, 2);
        assert!(a.stats().clipped_frames >= 90, "{}", a.stats().clipped_frames);
    }

    /// Budget: under 1 % of one core while learning. 48 kHz stereo; prints
    /// the measured share. Asserted only in an optimised build.
    #[test]
    fn cpu_cost_is_under_one_percent_of_a_core() {
        let mut s = Synth::new(48_000, 5);
        let mut mono = Vec::new();
        s.render(Segment::MixedScene, 20.0, |b| mono.extend_from_slice(b));
        let stereo: Vec<f32> = mono.iter().flat_map(|&x| [x, x]).collect();
        let mut a = Analyzer::new(48_000);
        let t = std::time::Instant::now();
        for block in stereo.chunks(960) {
            a.push_interleaved(block, 2);
        }
        let pct = t.elapsed().as_secs_f64() / 20.0 * 100.0;
        eprintln!("S46 analyzer: {pct:.3} % of one core (48 kHz stereo)");
        if !cfg!(debug_assertions) {
            assert!(pct < 1.0, "{pct} %");
        }
    }

    #[test]
    fn merge_adds_and_survives_json() {
        let a = run(&[(Segment::Gameplay, 10.0), (Segment::Speech, 5.0)]);
        let mut m = a.clone();
        m.merge(&a);
        assert_eq!(m.cue_events(), 2 * a.cue_events());
        let sum = |h: &[u32]| h.iter().map(|&x| x as u64).sum::<u64>();
        assert_eq!(sum(&m.frame_hist), 2 * sum(&a.frame_hist));
        let mut bad = Stats::default();
        bad.class_hist.truncate(3);
        m.merge(&bad);
        assert_eq!(m.cue_events(), 2 * a.cue_events(), "a malformed aggregate is ignored");
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.len() < 40_000, "sparse histograms keep it small: {}", json.len());
        let back: Stats = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        // A forged histogram length is refused, not allocated.
        let forged = json.replacen("\"len\":10800", "\"len\":4000000000", 1);
        assert!(serde_json::from_str::<Stats>(&forged).is_err());
    }
}
