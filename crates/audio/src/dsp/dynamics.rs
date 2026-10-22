//! Dynamic EQ — the "tame the bangs, lift the steps" stage (S49).
//!
//! Each band is a bell whose gain follows the band's own level:
//!
//! - **Form.** `y = x + (g − 1)·bp(x)`, where `bp` is the RBJ constant-0-dB
//!   peak band-pass at the band's centre and Q. At the centre the response is
//!   exactly `g`; it is ≥ 1 everywhere for a boost and between `g` and 1 for
//!   a cut, so it never rings past what was asked. Changing the gain changes
//!   one scalar — the filter state never sees it, so a moving gain cannot
//!   zipper or blow up the recursion — and the band-pass output is also the
//!   side-chain: detection costs nothing extra.
//! - **Detection** is stereo-linked: the level of `(bpL + bpR) / 2` (the
//!   band-pass of the mono downmix, which is what the learner measured), so
//!   both channels always get the same gain and the image never shifts. A
//!   one-pole power envelope with the band's attack and release.
//! - **Levels are relative** to the program loudness, tracked the way the
//!   learner tracks it (10 ms frames of the mono downmix, dB-domain average
//!   with a ~30 s time constant, re-based after a sustained volume change,
//!   silence ignored). A band's level is `env_dB − loudness + DYN_REF_DB`,
//!   the scale of the learned histograms, so a learned threshold applies
//!   unchanged at any game or Windows volume. Until the first non-silent
//!   frame nothing acts.
//! - **Gain computer**, every [`SUB_BLOCK`] frames, ramped linearly over the
//!   next sub-block: Tame cuts `(L − T)(1 − 1/ratio)` above the threshold, at
//!   most `range`; Lift boosts `(T − L)(1 − 1/ratio)` below it, at most
//!   `range` and at most `L − floor` (zero at and below the floor). With
//!   `duck_lift`, Lift bands are held back while any Tame band cuts and
//!   through a [`DUCK_RELEASE_MS`] release, so gun tails are not lifted.
//!
//! Zero added latency (no look-ahead: the limiter after this stage owns the
//! peaks), zero allocations in `process`, denormals flushed per block.

use super::biquad::{Biquad, Coeffs, FilterKind};
use super::PrepareError;
pub use crate::params::{DynBandParams, DynMode, DynamicsParams, DYN_REF_DB, MAX_DYN_BANDS};

/// Frames between gain-computer updates.
pub const SUB_BLOCK: usize = 32;
/// Program-loudness frame, ms (the learner's frame).
pub const FRAME_MS: f64 = 10.0;
/// Frames quieter than this (dBFS, mono downmix) do not move the loudness.
pub const SILENCE_DBFS: f64 = -70.0;
/// Loudness time constant, frames (~30 s).
const LT_FRAMES: f64 = 3000.0;
/// Short-term level smoothing per frame (~1 s).
const ST_ALPHA: f64 = 0.01;
/// A short-term level this far from the loudness, held for
/// [`REBASE_FRAMES`], is a volume change: the loudness jumps to it.
const JUMP_DB: f64 = 10.0;
const REBASE_FRAMES: u32 = 400;
/// Tame activity at which Lift is fully held back, dB of cut.
pub const DUCK_FULL_DB: f64 = 3.0;
/// How long Lift stays held back after the Tame bands let go.
pub const DUCK_RELEASE_MS: f64 = 300.0;
const DENORMAL_FLOOR: f64 = 1e-25;

struct Band {
    mode: DynMode,
    threshold: f64,
    floor: f64,
    /// `1 − 1/ratio`.
    slope: f64,
    range: f64,
    bp: [Biquad; 2],
    /// Power envelope of the linked band-pass.
    env: f64,
    att: f64,
    rel: f64,
    /// Linear gain applied to the band-pass branch's sample now, its target
    /// at the end of the current sub-block, and the per-frame step.
    g: f64,
    g_target: f64,
    g_step: f64,
    /// The target in dB (reporting and ducking).
    gain_db: f64,
}

/// The stereo dynamic stage.
pub struct Dynamics {
    bands: Vec<Band>,
    duck_lift: bool,
    duck_db: f64,
    duck_decay: f64,
    sub_pos: usize,
    // Program loudness.
    frame_len: usize,
    frame_pos: usize,
    frame_acc: f64,
    primed: bool,
    lt_db: f64,
    st_db: f64,
    lt_n: u32,
    jump_frames: u32,
}

impl Dynamics {
    /// Design the stage. `Ok(None)` when it would do nothing (no enabled band
    /// with a range and a ratio): the chain then skips it entirely, which is
    /// bit-exact pass-through.
    pub fn prepare(p: &DynamicsParams, sample_rate: u32) -> Result<Option<Self>, PrepareError> {
        if p.bands.len() > MAX_DYN_BANDS {
            return Err(PrepareError::InvalidParam("more than four dynamic bands"));
        }
        p.check().map_err(PrepareError::InvalidParam)?;
        if p.is_inert() {
            return Ok(None);
        }
        let fs = sample_rate as f64;
        let coef = |ms: f32| 1.0 - (-1.0 / (ms as f64 * fs / 1000.0).max(1.0)).exp();
        let mut bands = Vec::with_capacity(MAX_DYN_BANDS);
        for b in p.bands.iter().filter(|b| b.enabled && b.range_db > 0.0 && b.ratio > 1.0) {
            if b.freq_hz as f64 >= fs / 2.0 {
                return Err(PrepareError::InvalidParam("dynamic band above Nyquist"));
            }
            let c = Coeffs::design(FilterKind::Peaking, fs, b.freq_hz as f64, 0.0, b.q as f64)?;
            // RBJ band-pass, constant 0 dB peak gain: b = [α, 0, −α] / a0.
            let w0 = 2.0 * std::f64::consts::PI * b.freq_hz as f64 / fs;
            let alpha = w0.sin() / (2.0 * b.q as f64);
            let a0 = 1.0 + alpha;
            let bp = Coeffs { b0: alpha / a0, b1: 0.0, b2: -alpha / a0, a1: c.a1, a2: c.a2 };
            bands.push(Band {
                mode: b.mode,
                threshold: b.threshold_db as f64,
                floor: b.floor_db as f64,
                slope: 1.0 - 1.0 / b.ratio as f64,
                range: b.range_db as f64,
                bp: [Biquad::new(bp), Biquad::new(bp)],
                env: 0.0,
                att: coef(b.attack_ms),
                rel: coef(b.release_ms),
                g: 1.0,
                g_target: 1.0,
                g_step: 0.0,
                gain_db: 0.0,
            });
        }
        let frame_len = ((FRAME_MS * fs / 1000.0).round() as usize).max(1);
        Ok(Some(Self {
            bands,
            duck_lift: p.duck_lift,
            duck_db: 0.0,
            duck_decay: (-(SUB_BLOCK as f64) / (DUCK_RELEASE_MS * fs / 1000.0)).exp(),
            sub_pos: 0,
            frame_len,
            frame_pos: 0,
            frame_acc: 0.0,
            primed: false,
            lt_db: 0.0,
            st_db: 0.0,
            lt_n: 0,
            jump_frames: 0,
        }))
    }

    /// The gain each band is heading for, dB (cut negative, lift positive).
    pub fn band_gains_db(&self) -> impl Iterator<Item = f64> + '_ {
        self.bands.iter().map(|b| b.gain_db)
    }

    /// The tracked program loudness, dBFS (`None` before the first
    /// non-silent frame).
    pub fn loudness_db(&self) -> Option<f64> {
        self.primed.then_some(self.lt_db)
    }

    fn end_frame(&mut self) {
        let db = 10.0 * (self.frame_acc / self.frame_len as f64).max(1e-30).log10();
        self.frame_acc = 0.0;
        self.frame_pos = 0;
        if db < SILENCE_DBFS {
            return;
        }
        if !self.primed {
            self.primed = true;
            self.lt_db = db;
            self.st_db = db;
            self.lt_n = 1;
            return;
        }
        self.st_db += ST_ALPHA * (db - self.st_db);
        if (self.st_db - self.lt_db).abs() > JUMP_DB {
            self.jump_frames += 1;
        } else {
            self.jump_frames = self.jump_frames.saturating_sub(1);
        }
        if self.jump_frames >= REBASE_FRAMES {
            self.lt_db = self.st_db;
            self.lt_n = 1;
            self.jump_frames = 0;
            return;
        }
        self.lt_n = self.lt_n.saturating_add(1);
        let alpha = (1.0 / LT_FRAMES).max(1.0 / self.lt_n as f64);
        self.lt_db += alpha * (db - self.lt_db);
    }

    /// New targets for the next sub-block.
    fn compute_gains(&mut self) {
        let offset = DYN_REF_DB as f64 - self.lt_db;
        let mut tame_cut: f64 = 0.0;
        for b in self.bands.iter_mut().filter(|b| b.mode == DynMode::Tame) {
            let cut = if self.primed && b.env > 0.0 {
                let level = 10.0 * b.env.log10() + offset;
                ((level - b.threshold) * b.slope).clamp(0.0, b.range)
            } else {
                0.0
            };
            b.gain_db = -cut;
            tame_cut = tame_cut.max(cut);
        }
        self.duck_db = tame_cut.max(self.duck_db * self.duck_decay);
        let hold =
            if self.duck_lift { (1.0 - self.duck_db / DUCK_FULL_DB).clamp(0.0, 1.0) } else { 1.0 };
        for b in self.bands.iter_mut().filter(|b| b.mode == DynMode::Lift) {
            let boost = if self.primed && b.env > 0.0 {
                let level = 10.0 * b.env.log10() + offset;
                if level <= b.floor {
                    0.0
                } else {
                    ((b.threshold - level) * b.slope).min(level - b.floor).clamp(0.0, b.range)
                }
            } else {
                0.0
            };
            b.gain_db = boost * hold;
        }
        let n = SUB_BLOCK as f64;
        for b in self.bands.iter_mut() {
            b.g = b.g_target;
            b.g_target = 10f64.powf(b.gain_db / 20.0);
            b.g_step = (b.g_target - b.g) / n;
        }
    }

    /// Process one block in place (de-interleaved, equal lengths).
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len());
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.sub_pos == 0 {
                self.compute_gains();
                self.sub_pos = SUB_BLOCK;
            }
            self.sub_pos -= 1;
            let (mut x, mut y) = (*l as f64, *r as f64);
            let m = 0.5 * (x + y);
            self.frame_acc += m * m;
            self.frame_pos += 1;
            for b in self.bands.iter_mut() {
                let bl = b.bp[0].process_sample(x);
                let br = b.bp[1].process_sample(y);
                let pm = 0.5 * (bl + br);
                let p = pm * pm;
                b.env += if p > b.env { b.att } else { b.rel } * (p - b.env);
                b.g += b.g_step;
                let k = b.g - 1.0;
                x += k * bl;
                y += k * br;
            }
            *l = x as f32;
            *r = y as f32;
            if self.frame_pos == self.frame_len {
                self.end_frame();
            }
        }
    }

    pub fn flush_denormals(&mut self) {
        for b in &mut self.bands {
            b.bp[0].flush_denormals();
            b.bp[1].flush_denormals();
            if b.env < DENORMAL_FLOOR {
                b.env = 0.0;
            }
        }
    }

    pub fn state_is_denormal_free(&self) -> bool {
        self.bands
            .iter()
            .all(|b| b.bp.iter().all(|q| q.state_is_denormal_free()) && !b.env.is_subnormal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    fn tame(hz: f32, th: f32) -> DynBandParams {
        DynBandParams {
            mode: DynMode::Tame,
            freq_hz: hz,
            q: 0.8,
            threshold_db: th,
            ratio: 4.0,
            range_db: 10.0,
            floor_db: -100.0,
            attack_ms: 2.0,
            release_ms: 100.0,
            enabled: true,
        }
    }

    fn lift(hz: f32, th: f32, floor: f32) -> DynBandParams {
        DynBandParams {
            mode: DynMode::Lift,
            freq_hz: hz,
            q: 1.0,
            threshold_db: th,
            ratio: 3.0,
            range_db: 6.0,
            floor_db: floor,
            attack_ms: 3.0,
            release_ms: 150.0,
            enabled: true,
        }
    }

    fn sine(hz: f64, amp: f64, n: usize, phase0: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (amp * (2.0 * std::f64::consts::PI * hz * (i + phase0) as f64 / FS as f64).sin())
                    as f32
            })
            .collect()
    }

    fn rms_db(x: &[f32]) -> f64 {
        let p: f64 = x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len() as f64;
        10.0 * p.max(1e-30).log10()
    }

    /// Run a mono signal on both channels in 480-frame blocks.
    fn run(d: &mut Dynamics, x: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(x.len());
        for chunk in x.chunks(480) {
            let mut l = chunk.to_vec();
            let mut r = chunk.to_vec();
            d.process(&mut l, &mut r);
            assert_eq!(l, r, "linked: identical channels stay identical");
            out.extend_from_slice(&l);
        }
        out
    }

    #[test]
    fn inert_or_empty_is_skipped() {
        assert!(Dynamics::prepare(&DynamicsParams::default(), FS).unwrap().is_none());
        let mut b = tame(200.0, -40.0);
        b.enabled = false;
        let p = DynamicsParams { bands: vec![b], duck_lift: false };
        assert!(Dynamics::prepare(&p, FS).unwrap().is_none());
        let mut b = tame(200.0, -40.0);
        b.range_db = 0.0;
        let p = DynamicsParams { bands: vec![b], duck_lift: false };
        assert!(Dynamics::prepare(&p, FS).unwrap().is_none());
    }

    #[test]
    fn in_the_chain_inert_dynamics_are_bit_exact_and_live_ones_add_no_latency() {
        use crate::dsp::{BandParams, Chain, ChainParams};
        let x: Vec<f32> = (0..9600).map(|i| ((i * 7919) % 2001) as f32 / 2000.0 - 0.5).collect();
        let base = ChainParams {
            bands: vec![BandParams::peaking(1000.0, 3.0, 1.0)],
            ..ChainParams::default()
        };
        let render = |p: ChainParams| {
            let mut c = Chain::new(p);
            c.prepare(FS, 480).unwrap();
            let mut out = vec![0.0; x.len()];
            for (i, o) in x.chunks(960).zip(out.chunks_mut(960)) {
                c.process(i, o);
            }
            (out, c.latency_frames())
        };
        let (plain, lat) = render(base.clone());
        let mut disabled = tame(200.0, -40.0);
        disabled.enabled = false;
        let inert = ChainParams {
            dynamics: Some(DynamicsParams { bands: vec![disabled], duck_lift: true }),
            ..base.clone()
        };
        let (same, lat2) = render(inert);
        assert_eq!(plain, same, "inert dynamics are bit-exact");
        assert_eq!(lat, lat2);
        let live = ChainParams {
            dynamics: Some(DynamicsParams { bands: vec![tame(200.0, -40.0)], duck_lift: false }),
            ..base
        };
        assert_eq!(render(live).1, lat, "no added latency");
    }

    #[test]
    fn bad_params_are_refused() {
        let bad = |f: &dyn Fn(&mut DynBandParams)| {
            let mut b = tame(200.0, -40.0);
            f(&mut b);
            Dynamics::prepare(&DynamicsParams { bands: vec![b], duck_lift: false }, FS).is_err()
        };
        assert!(bad(&|b| b.freq_hz = f32::NAN));
        assert!(bad(&|b| b.freq_hz = 5.0));
        assert!(bad(&|b| b.freq_hz = 30_000.0));
        assert!(bad(&|b| b.q = 0.0));
        assert!(bad(&|b| b.ratio = 0.5));
        assert!(bad(&|b| b.range_db = 40.0));
        assert!(bad(&|b| b.threshold_db = f32::INFINITY));
        assert!(bad(&|b| b.attack_ms = 0.0));
        assert!(bad(&|b| {
            b.mode = DynMode::Lift;
            b.floor_db = b.threshold_db + 1.0;
        }));
        let five = DynamicsParams { bands: vec![tame(200.0, -40.0); 5], duck_lift: false };
        assert!(Dynamics::prepare(&five, FS).is_err());
        // Above Nyquist at a low rate.
        let p = DynamicsParams { bands: vec![tame(15_000.0, -40.0)], duck_lift: false };
        assert!(Dynamics::prepare(&p, 22_050).is_err());
    }

    #[test]
    fn a_loud_burst_is_tamed_and_a_quiet_bed_is_not() {
        // Bed: 200 Hz at -30 dBFS; burst: +20 dB for 200 ms; then bed again.
        let p = DynamicsParams { bands: vec![tame(200.0, -45.0)], duck_lift: false };
        let mut d = Dynamics::prepare(&p, FS).unwrap().unwrap();
        let bed_amp = 10f64.powf(-30.0 / 20.0) * 2f64.sqrt();
        let n_bed = FS as usize * 4;
        let bed = sine(200.0, bed_amp, n_bed, 0);
        let out_bed = run(&mut d, &bed);
        // Level relative: bed ≈ loudness → band level ≈ -50 (+ the band-pass
        // passing the whole sine): under the threshold, untouched.
        let tail = &out_bed[n_bed - 4800..];
        assert!((rms_db(tail) - rms_db(&bed[n_bed - 4800..])).abs() < 0.1);
        let burst = sine(200.0, bed_amp * 10.0, FS as usize / 5, n_bed);
        let out_burst = run(&mut d, &burst);
        let settled = &out_burst[2400..];
        let cut = rms_db(&burst[2400..]) - rms_db(settled);
        // 20 dB over the bed, threshold 5 dB over it: (15) × (1 − 1/4) ≈ 11
        // dB wanted, range 10.
        assert!(cut > 8.0 && cut <= 10.2, "cut {cut}");
        assert!(d.band_gains_db().next().unwrap() < -8.0);
    }

    #[test]
    fn levels_are_relative_to_the_program_loudness() {
        // The same material at two volumes 20 dB apart gets the same cut.
        let mut cuts = Vec::new();
        for vol_db in [-20.0f64, 0.0] {
            let p = DynamicsParams { bands: vec![tame(200.0, -45.0)], duck_lift: false };
            let mut d = Dynamics::prepare(&p, FS).unwrap().unwrap();
            let a = 10f64.powf((vol_db - 30.0) / 20.0) * 2f64.sqrt();
            run(&mut d, &sine(200.0, a, FS as usize * 4, 0));
            let burst = sine(200.0, a * 10.0, FS as usize / 5, FS as usize * 4);
            let out = run(&mut d, &burst);
            cuts.push(rms_db(&burst[2400..]) - rms_db(&out[2400..]));
        }
        assert!((cuts[0] - cuts[1]).abs() < 0.3, "{cuts:?}");
    }

    #[test]
    fn lift_raises_quiet_cues_but_never_the_floor_or_past_the_threshold() {
        // Loudness reference: a 50 Hz bed at -20 dBFS, far off the 3 kHz
        // band (it leaks in ~36 dB down: under the floor).
        let p = DynamicsParams { bands: vec![lift(3000.0, -50.0, -80.0)], duck_lift: false };
        let measure = |cue_db: f64| -> f64 {
            let mut d = Dynamics::prepare(&p, FS).unwrap().unwrap();
            let n = FS as usize * 3;
            let bed = sine(50.0, 10f64.powf(-20.0 / 20.0) * 2f64.sqrt(), n, 0);
            run(&mut d, &bed);
            // Cue at 3 kHz on top of the same bed.
            let len = FS as usize / 2;
            let bed2 = sine(50.0, 10f64.powf(-20.0 / 20.0) * 2f64.sqrt(), len, n);
            let cue = sine(3000.0, 10f64.powf(cue_db / 20.0) * 2f64.sqrt(), len, 0);
            let mix: Vec<f32> = bed2.iter().zip(&cue).map(|(a, b)| a + b).collect();
            let out = run(&mut d, &mix);
            // Remove the bed (untouched at 300 Hz to within a hair) to look at
            // the cue alone.
            let only: Vec<f32> = out.iter().zip(&bed2).map(|(o, b)| o - b).collect();
            rms_db(&only[4800..]) - rms_db(&cue[4800..])
        };
        // Loudness ≈ -20 dBFS, so relative = absolute + 20 − 50 = abs − 30.
        // A cue at -40 dBFS (relative -70): between floor -80 and threshold
        // -50: lifted by min((−50 − −70)·⅔, −70 − −80, 6) = 6 dB.
        let mid = measure(-40.0);
        assert!(mid > 4.5 && mid < 6.5, "mid {mid}");
        // Below the floor (relative -92): untouched.
        let quiet = measure(-62.0);
        assert!(quiet.abs() < 0.5, "quiet {quiet}");
        // Above the threshold (relative -45): untouched, never louder.
        let loud = measure(-15.0);
        assert!(loud.abs() < 0.5, "loud {loud}");
    }

    #[test]
    fn duck_holds_lift_back_while_taming() {
        let mk = |duck: bool| DynamicsParams {
            bands: vec![tame(200.0, -45.0), lift(3000.0, -50.0, -75.0)],
            duck_lift: duck,
        };
        let mut gains = Vec::new();
        for duck in [false, true] {
            let mut d = Dynamics::prepare(&mk(duck), FS).unwrap().unwrap();
            let bed_amp = 10f64.powf(-30.0 / 20.0) * 2f64.sqrt();
            run(&mut d, &sine(200.0, bed_amp, FS as usize * 3, 0));
            // Loud 200 Hz burst with a quiet 3 kHz component (a gun's tail).
            let n = FS as usize / 5;
            let a = sine(200.0, bed_amp * 10.0, n, 0);
            let b = sine(3000.0, 10f64.powf(-52.0 / 20.0) * 2f64.sqrt(), n, 0);
            let mix: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
            run(&mut d, &mix);
            gains.push(d.band_gains_db().collect::<Vec<_>>());
        }
        assert!(gains[0][1] > 1.0, "without ducking the tail is lifted: {gains:?}");
        assert!(gains[1][1].abs() < 1e-9, "ducked: {gains:?}");
        assert!(gains[1][0] < -5.0);
    }

    #[test]
    fn silence_stays_silent_and_state_flushes() {
        let p = DynamicsParams {
            bands: vec![tame(200.0, -45.0), lift(3000.0, -50.0, -70.0)],
            duck_lift: true,
        };
        let mut d = Dynamics::prepare(&p, FS).unwrap().unwrap();
        let out = run(&mut d, &vec![0.0; FS as usize]);
        assert!(out.iter().all(|&s| s == 0.0));
        assert!(d.loudness_db().is_none());
        // A decaying impulse then silence: no subnormals left behind.
        let mut x = vec![0.0f32; FS as usize];
        x[0] = 1.0;
        let mut l = x.clone();
        let mut r = x;
        d.process(&mut l, &mut r);
        d.flush_denormals();
        assert!(d.state_is_denormal_free());
    }

    #[test]
    fn stereo_image_is_preserved() {
        // Hard-left burst: the right channel's (silent) content stays silent
        // and the gain is shared.
        let p = DynamicsParams { bands: vec![tame(200.0, -45.0)], duck_lift: false };
        let mut d = Dynamics::prepare(&p, FS).unwrap().unwrap();
        let n = FS as usize * 2;
        let mut l = sine(200.0, 0.5, n, 0);
        let mut r = sine(200.0, 0.25, n, 0);
        let (l0, r0) = (l.clone(), r.clone());
        d.process(&mut l, &mut r);
        // Ratio L/R unchanged sample by sample (same gain both sides).
        for i in (n / 2..n).step_by(97) {
            if r0[i].abs() > 1e-3 {
                let a = l[i] as f64 / r[i] as f64;
                let b = l0[i] as f64 / r0[i] as f64;
                assert!((a - b).abs() < 1e-3, "{a} vs {b}");
            }
        }
    }
}
