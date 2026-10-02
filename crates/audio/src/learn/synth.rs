//! Synthetic "game audio" for the learning tests. Deterministic (seeded
//! xorshift), streamed in blocks so a 20-minute run never sits in memory.
//! Every segment except `Silence` and `Clipped` sits on the same pink-noise
//! ambience bed.
//!
//! - **Gameplay**: footsteps every `step_ms` and an explosion every `boom_ms`.
//! - **GameplayQuiet**: the same, 20 dB down (someone turned the game down).
//! - **Ambience**: the bed alone.
//! - **Footsteps**: a steady walk, one 60 ms 2–5 kHz burst every 500 ms.
//! - **Reload**: 8 ms clicks at irregular gaps (0.4–2.2 s, often in pairs).
//! - **Foliage**: 300–500 ms soft-attack rustles of 1.5–9 kHz noise at random.
//! - **Gunfire**: a broadband crack (1 ms attack) plus a low thump, ~1 per s.
//! - **Explosions**: a loud < 120 Hz burst every 4 s.
//! - **Vehicle**: an engine — harmonics of a fundamental that glides
//!   45 → 90 → 45 Hz over 6 s, steady level.
//! - **Music**: a held harmonic melody (one note per second).
//! - **Speech**: game voice-over — a voiced source at `voice_hz` (gliding
//!   ±15 %), full band (fundamental through 8 kHz, plus breath noise),
//!   chopped into 180–320 ms syllables (≈ 4 Hz).
//! - **PlayerChat**: the same talker through a voice codec: band-limited to
//!   300 Hz – 3.4 kHz, with a hiss floor, and level jumping between phrases.
//! - **MixedScene**: footsteps over a vehicle, a music bed and a talker.
//! - **Clipped**: hard-clipped noise. **Silence**.

use crate::coeffs::Coeffs;
use crate::params::FilterKind;

/// The rate the long tests run at: the top analysis band (10 kHz) still fits
/// and it is half the work of 48 kHz.
pub const FS: u32 = 24_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Gameplay,
    GameplayQuiet,
    Ambience,
    Footsteps,
    Reload,
    Foliage,
    Gunfire,
    /// Automatic fire: a crack + thump every 120 ms with a 300 ms tail
    /// riding under the next shots, over the ambience bed.
    GunBurst,
    Explosions,
    Vehicle,
    Music,
    Speech,
    PlayerChat,
    MixedScene,
    Clipped,
    #[allow(dead_code)]
    Silence,
}

struct Rng(u64);
impl Rng {
    fn white(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
    fn unit(&mut self) -> f32 {
        0.5 * (self.white() + 1.0)
    }
}

/// Paul Kellet's pink filter.
struct Pink([f32; 7]);
impl Pink {
    fn next(&mut self, w: f32) -> f32 {
        let b = &mut self.0;
        b[0] = 0.99886 * b[0] + w * 0.0555179;
        b[1] = 0.99332 * b[1] + w * 0.0750759;
        b[2] = 0.96900 * b[2] + w * 0.153_852;
        b[3] = 0.86650 * b[3] + w * 0.3104856;
        b[4] = 0.55000 * b[4] + w * 0.5329522;
        b[5] = -0.7616 * b[5] - w * 0.0168980;
        let out = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
        b[6] = w * 0.115926;
        out * 0.11
    }
}

struct Biquad {
    c: Coeffs,
    s: [f64; 2],
}
impl Biquad {
    fn new(kind: FilterKind, fs: u32, hz: f64) -> Self {
        Self { c: Coeffs::design(kind, fs as f64, hz, 0.0, 0.707).unwrap(), s: [0.0; 2] }
    }
    fn run(&mut self, x: f64) -> f64 {
        let c = &self.c;
        let y = c.b0 * x + self.s[0];
        self.s[0] = c.b1 * x - c.a1 * y + self.s[1];
        self.s[1] = c.b2 * x - c.a2 * y;
        y
    }
}

/// Syllable clock for the talkers: 180–320 ms voiced, 0–120 ms pause.
#[derive(Default)]
struct Syllables {
    left: u32,
    len: u32,
    pause: u32,
    /// Per-phrase gain (chat jumps level between phrases).
    gain: f32,
    count: u32,
}

/// A one-shot envelope scheduler: next start sample and current position.
#[derive(Default)]
struct Shot {
    next: u64,
    pos: u64,
    len: u64,
    on: bool,
}

pub struct Synth {
    fs: u32,
    rng: Rng,
    pink: Pink,
    step_hp: Biquad,
    step_lp: Biquad,
    boom_lp: Biquad,
    leaf_hp: Biquad,
    leaf_lp: Biquad,
    gun_lp: Biquad,
    chat_hp: [Biquad; 3],
    chat_lp: [Biquad; 4],
    /// Samples rendered so far (event clocks run on it).
    t: u64,
    phase: f64,
    engine_phase: f64,
    voice: Syllables,
    click: Shot,
    leaf: Shot,
    gun: Shot,
    pub bed: f32,
    pub step: f32,
    pub step_ms: u32,
    pub boom: f32,
    pub boom_ms: u32,
    pub voice_hz: f64,
}

impl Synth {
    pub fn new(fs: u32, seed: u64) -> Self {
        let bq = |k, hz| Biquad::new(k, fs, hz);
        Self {
            fs,
            rng: Rng(seed.max(1)),
            pink: Pink([0.0; 7]),
            step_hp: bq(FilterKind::HighPass, 2000.0),
            step_lp: bq(FilterKind::LowPass, 5000.0),
            boom_lp: bq(FilterKind::LowPass, 120.0),
            leaf_hp: bq(FilterKind::HighPass, 1500.0),
            leaf_lp: bq(FilterKind::LowPass, 9000.0_f64.min(fs as f64 * 0.45)),
            gun_lp: bq(FilterKind::LowPass, 150.0),
            chat_hp: [
                bq(FilterKind::HighPass, 300.0),
                bq(FilterKind::HighPass, 300.0),
                bq(FilterKind::HighPass, 300.0),
            ],
            chat_lp: [
                bq(FilterKind::LowPass, 3400.0),
                bq(FilterKind::LowPass, 3400.0),
                bq(FilterKind::LowPass, 3400.0),
                bq(FilterKind::LowPass, 3400.0),
            ],
            t: 0,
            phase: 0.0,
            engine_phase: 0.0,
            voice: Syllables { gain: 1.0, ..Default::default() },
            click: Shot::default(),
            leaf: Shot::default(),
            gun: Shot::default(),
            bed: 0.05,
            step: 0.25,
            step_ms: 500,
            boom: 3.0,
            boom_ms: 4000,
            voice_hz: 130.0,
        }
    }

    /// Render `secs` of `seg`, handing out blocks of up to 100 ms.
    pub fn render(&mut self, seg: Segment, secs: f32, mut out: impl FnMut(&[f32])) {
        let total = (self.fs as f32 * secs) as usize;
        let mut buf = vec![0f32; self.fs as usize / 10];
        let mut done = 0;
        while done < total {
            let n = buf.len().min(total - done);
            for x in buf[..n].iter_mut() {
                *x = self.next(seg);
            }
            out(&buf[..n]);
            done += n;
        }
    }

    fn ms(&self, ms: f32) -> u64 {
        (self.fs as f32 * ms / 1000.0) as u64
    }

    fn bed(&mut self) -> f32 {
        self.bed * self.pink.next(self.rng.white())
    }

    fn footstep(&mut self, every_ms: u32) -> f32 {
        let fs = self.fs as u64;
        let w = self.rng.white() as f64;
        let f = self.step_lp.run(self.step_hp.run(w));
        let every = (fs * every_ms as u64 / 1000).max(1);
        let ps = self.t % every;
        if ps < self.ms(60.0) {
            // 5 ms attack, exponential decay.
            let t = ps as f32 / fs as f32;
            let env = (t / 0.005).min(1.0) * (-(t / 0.02)).exp();
            self.step * env * f as f32
        } else {
            0.0
        }
    }

    fn explosion(&mut self) -> f32 {
        let fs = self.fs as u64;
        let every = (fs * self.boom_ms as u64 / 1000).max(1);
        let pb = (self.t + every / 2) % every;
        let b = self.boom_lp.run(self.rng.white() as f64);
        if pb < self.ms(700.0) {
            let t = pb as f32 / fs as f32;
            let env = (t / 0.01).min(1.0) * (-(t / 0.3)).exp();
            self.boom * env * b as f32
        } else {
            0.0
        }
    }

    fn engine(&mut self) -> f32 {
        let fs = self.fs as f64;
        // Triangle glide 45 -> 90 -> 45 Hz over 6 s.
        let p = (self.t as f64 / fs / 6.0).fract();
        let tri = if p < 0.5 { 2.0 * p } else { 2.0 - 2.0 * p };
        let f0 = 45.0 * 2f64.powf(tri);
        self.engine_phase = (self.engine_phase + f0 / fs).fract();
        let mut y = 0.0;
        for h in 1..=10 {
            y += (0.1 / h as f64)
                * (2.0 * std::f64::consts::PI * self.engine_phase * h as f64).sin();
        }
        y as f32
    }

    fn melody(&mut self) -> f32 {
        const NOTES: [f64; 6] = [262.0, 330.0, 392.0, 523.0, 440.0, 349.0];
        let fs = self.fs as f64;
        let note_len = self.fs as u64;
        let k = (self.t / note_len) as usize % NOTES.len();
        let pos = (self.t % note_len) as f64 / fs;
        let env = (pos / 0.02).min(1.0) * ((1.0 - pos) / 0.02).min(1.0);
        self.phase = (self.phase + NOTES[k] / fs).fract();
        let mut y = 0.0;
        for h in 1..=6 {
            y += (0.12 / h as f64) * (2.0 * std::f64::consts::PI * self.phase * h as f64).sin();
        }
        (env * y) as f32
    }

    /// One talker sample (before any codec). `chat` jumps the level per phrase.
    fn talker(&mut self, chat: bool) -> f32 {
        let fs = self.fs as f64;
        let v = &mut self.voice;
        if v.left == 0 && v.pause == 0 {
            v.len = ((0.18 + 0.14 * self.rng.unit()) * fs as f32) as u32;
            v.left = v.len;
            v.pause = ((0.12 * self.rng.unit()) * fs as f32) as u32;
            v.count += 1;
            if chat && v.count % 6 == 0 {
                // A new phrase (or another player) at a different level.
                v.gain = 0.4 + 0.8 * self.rng.unit();
            }
        }
        let v = &mut self.voice;
        let env = if v.left > 0 {
            let p = 1.0 - v.left as f64 / v.len as f64;
            v.left -= 1;
            (std::f64::consts::PI * p).sin().powi(2)
        } else {
            v.pause -= 1;
            0.0
        };
        let gain = v.gain as f64;
        let f0 = self.voice_hz
            * (1.0 + 0.15 * (2.0 * std::f64::consts::PI * 0.7 * self.t as f64 / fs).sin());
        self.phase = (self.phase + f0 / fs).fract();
        let mut y = 0.0;
        let mut h = 1;
        while f0 * h as f64 <= 8000.0_f64.min(fs * 0.45) {
            let hz = f0 * h as f64;
            // Flat to 1 kHz, -9 dB/octave above: a talker's spectrum.
            let a = if hz <= 1000.0 { 0.08 } else { 0.08 * (1000.0 / hz).powf(1.5) };
            y += a * (2.0 * std::f64::consts::PI * self.phase * h as f64).sin();
            h += 1;
        }
        // Breath / fricative noise riding on the syllable.
        let breath = 0.01 * self.rng.white() as f64;
        (gain * env * (y + breath)) as f32
    }

    /// Fire a one-shot when due; returns (position, length) while it plays.
    fn shot(s: &mut Shot, t: u64, rng: &mut Rng, len: u64, gap: (u64, u64)) -> Option<(u64, u64)> {
        if !s.on && t >= s.next {
            s.on = true;
            s.pos = 0;
            s.len = len;
        }
        if s.on {
            let p = s.pos;
            s.pos += 1;
            if s.pos >= s.len {
                s.on = false;
                s.next = t + gap.0 + ((gap.1 - gap.0) as f32 * rng.unit()) as u64;
            }
            Some((p, len))
        } else {
            None
        }
    }

    fn next(&mut self, seg: Segment) -> f32 {
        let fs = self.fs as f32;
        let x = match seg {
            Segment::Silence => 0.0,
            Segment::Clipped => (3.0 * self.rng.white()).clamp(-1.0, 1.0),
            Segment::Ambience => self.bed(),
            Segment::Gameplay => {
                let s = self.footstep(self.step_ms);
                self.bed() + s + self.explosion()
            }
            Segment::GameplayQuiet => {
                let s = self.footstep(self.step_ms);
                0.1 * (self.bed() + s + self.explosion())
            }
            Segment::Footsteps => self.bed() + self.footstep(500),
            Segment::Explosions => self.bed() + self.explosion(),
            Segment::Reload => {
                let w = self.rng.white();
                let (len, gap) = (self.ms(8.0), (self.ms(150.0), self.ms(2200.0)));
                let hp = self.step_hp.run(w as f64) as f32;
                let y = match Self::shot(&mut self.click, self.t, &mut self.rng, len, gap) {
                    Some((p, _)) => 0.4 * (-(p as f32 / fs) / 0.002).exp() * hp,
                    None => 0.0,
                };
                self.bed() + y
            }
            Segment::Foliage => {
                let w = self.rng.white() as f64;
                let n = self.leaf_lp.run(self.leaf_hp.run(w)) as f32;
                let len = self.ms(300.0 + 200.0 * 0.5);
                let gap = (self.ms(500.0), self.ms(1500.0));
                let y = match Self::shot(&mut self.leaf, self.t, &mut self.rng, len, gap) {
                    Some((p, l)) => {
                        // 80 ms attack, hold, 120 ms release: a rustle.
                        let t = p as f32 / fs;
                        let r = (l - p) as f32 / fs;
                        0.15 * (t / 0.08).min(1.0) * (r / 0.12).min(1.0) * n
                    }
                    None => 0.0,
                };
                self.bed() + y
            }
            Segment::Gunfire => {
                let w = self.rng.white();
                let thump = self.gun_lp.run(self.rng.white() as f64) as f32;
                let (len, gap) = (self.ms(250.0), self.ms(700.0));
                let y = match Self::shot(&mut self.gun, self.t, &mut self.rng, len, (gap, gap + 1))
                {
                    Some((p, _)) => {
                        let t = p as f32 / fs;
                        let env = (t / 0.001).min(1.0) * (-(t / 0.04)).exp();
                        env * (0.6 * w + 2.0 * thump)
                    }
                    None => 0.0,
                };
                self.bed() + y
            }
            Segment::GunBurst => {
                let w = self.rng.white();
                let thump = self.gun_lp.run(self.rng.white() as f64) as f32;
                let every = self.ms(120.0);
                let p = self.t % every;
                let t = p as f32 / fs;
                // Each shot: 1 ms attack, a 30 ms crack, a 120 ms decaying tail.
                let crack = (t / 0.001).min(1.0) * (-(t / 0.03)).exp();
                let tail = (-(t / 0.12)).exp();
                self.bed() + crack * 0.6 * w + 2.0 * tail * thump
            }
            Segment::Vehicle => self.bed() + self.engine(),
            Segment::Music => 0.3 * self.bed() + self.melody(),
            Segment::Speech => 0.3 * self.bed() + self.talker(false),
            Segment::PlayerChat => {
                let hiss = 0.006 * self.rng.white();
                let mut c = (self.talker(true) + hiss) as f64;
                for f in self.chat_hp.iter_mut().chain(self.chat_lp.iter_mut()) {
                    c = f.run(c);
                }
                0.3 * self.bed() + c as f32
            }
            Segment::MixedScene => {
                let s = 2.0 * self.footstep(500);
                0.4 * self.engine()
                    + 0.3 * self.melody()
                    + 0.3 * self.talker(false)
                    + self.bed()
                    + s
            }
        };
        self.t += 1;
        x
    }
}
