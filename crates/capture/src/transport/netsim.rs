//! A Wi-Fi link model, for proving the share's behaviour on one PC (S49).
//!
//! No test PC has Wi-Fi, and loopback never loses, delays or reorders a
//! packet, so nothing that only goes wrong on Wi-Fi could be exercised. This
//! is the model the receive path is run through instead: pure, seeded, and
//! the same code whether it is driven by a unit test or sits under a real
//! peer connection ([`super::netio`] applies it to arriving RTP and RTCP).
//!
//! What it models, and why each one matters to a share:
//!
//! * **Bursty loss** (Gilbert-Elliott): Wi-Fi does not lose packets
//!   independently. Interference and retries fail in runs, which is what turns
//!   one NACK-able hole into a hole NACK cannot fill.
//! * **Jitter with spikes**: a few milliseconds of per-packet variation, plus
//!   occasional 5-80 ms holds where the radio is busy (another station, a
//!   power-save wake, a rate change). Everything behind a spike waits for it:
//!   802.11 delivers in order per traffic class, so it is a *hold*, not a
//!   reorder.
//! * **Stalls**: 100-300 ms with nothing delivered at all, as during a
//!   background channel scan. The queue then drains at link rate.
//! * **Capacity drops**: a link rate that falls (someone walks between the PC
//!   and the access point, a neighbour starts a download) for seconds at a
//!   time. Packets queue at the bottleneck and the queue's delay grows until
//!   the access point's buffer is full and it tail-drops. This is the one a
//!   delay-based bitrate controller exists for.
//!
//! The order of effects per packet is the order a real link applies them:
//! the bottleneck queue (and its tail drop), then the hold/stall, then
//! per-packet jitter, then radio loss.

use std::time::Duration;

/// Gilbert-Elliott two-state loss: `p_gb` is the chance per packet of
/// moving from good to bad, `p_bg` of moving back; each state loses packets
/// at its own rate. Mean burst length is `1 / p_bg` packets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GilbertElliott {
    pub p_gb: f64,
    pub p_bg: f64,
    pub loss_good: f64,
    pub loss_bad: f64,
}

impl GilbertElliott {
    pub const NONE: Self = Self { p_gb: 0.0, p_bg: 1.0, loss_good: 0.0, loss_bad: 0.0 };

    /// Long-run loss rate: time in the bad state times its loss, plus good.
    pub fn mean_loss(&self) -> f64 {
        let bad =
            if self.p_gb + self.p_bg > 0.0 { self.p_gb / (self.p_gb + self.p_bg) } else { 0.0 };
        bad * self.loss_bad + (1.0 - bad) * self.loss_good
    }
}

/// Per-packet jitter and the short holds ("spikes") Wi-Fi produces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Jitter {
    /// Uniform 0..=`base` extra delay per packet.
    pub base: Duration,
    /// Spikes per second (Poisson).
    pub spikes_per_sec: f64,
    pub spike_min: Duration,
    pub spike_max: Duration,
}

/// Periodic stalls: nothing delivered for `min..=max`, every
/// `every_min..=every_max`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stalls {
    pub every_min: Duration,
    pub every_max: Duration,
    pub min: Duration,
    pub max: Duration,
}

/// The bottleneck: a link rate that drops to `low_bps` for `low_for` every
/// `period`, and an access-point queue that tail-drops past `queue`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capacity {
    pub bps: u64,
    pub low_bps: u64,
    /// When the first drop starts, and how often one recurs.
    pub period: Duration,
    pub low_for: Duration,
    /// Most queueing delay the access point holds before it drops.
    pub queue: Duration,
}

impl Capacity {
    pub fn rate_at(&self, t: Duration) -> u64 {
        if self.period.is_zero() || self.low_for.is_zero() {
            return self.bps;
        }
        let into = Duration::from_nanos((t.as_nanos() % self.period.as_nanos()) as u64);
        // The drop sits at the end of each period, so a share starts on a
        // good link and the first drop comes after the controller settled.
        if into >= self.period.saturating_sub(self.low_for) {
            self.low_bps
        } else {
            self.bps
        }
    }
}

/// One named link model.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub loss: GilbertElliott,
    pub jitter: Jitter,
    pub stalls: Option<Stalls>,
    pub capacity: Option<Capacity>,
    pub seed: u64,
}

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

impl Profile {
    /// A link that does nothing: the regression baseline.
    pub fn wired() -> Self {
        Self {
            name: "wired".into(),
            loss: GilbertElliott::NONE,
            jitter: Jitter {
                base: Duration::ZERO,
                spikes_per_sec: 0.0,
                spike_min: ms(0),
                spike_max: ms(0),
            },
            stalls: None,
            capacity: None,
            seed: 1,
        }
    }

    /// A good 5 GHz link in the same room: little loss, a few ms of jitter,
    /// the odd spike, a short scan stall every half minute, plenty of rate.
    pub fn wifi_good() -> Self {
        Self {
            name: "wifi-good".into(),
            loss: GilbertElliott { p_gb: 0.0005, p_bg: 0.25, loss_good: 0.0002, loss_bad: 0.3 },
            jitter: Jitter {
                base: ms(3),
                spikes_per_sec: 0.5,
                spike_min: ms(5),
                spike_max: ms(25),
            },
            stalls: Some(Stalls {
                every_min: ms(25_000),
                every_max: ms(35_000),
                min: ms(100),
                max: ms(150),
            }),
            capacity: Some(Capacity {
                bps: 300_000_000,
                low_bps: 300_000_000,
                period: Duration::ZERO,
                low_for: Duration::ZERO,
                queue: ms(200),
            }),
            seed: 0x5ee1,
        }
    }

    /// A busy home network: bursty loss around 1 %, 5-80 ms spikes, a scan
    /// stall every 10-20 s, and the rate falling from 120 to 30 Mb/s for 8 s
    /// every 30 s.
    pub fn wifi_busy() -> Self {
        Self {
            name: "wifi-busy".into(),
            loss: GilbertElliott { p_gb: 0.003, p_bg: 0.2, loss_good: 0.001, loss_bad: 0.5 },
            jitter: Jitter {
                base: ms(5),
                spikes_per_sec: 2.0,
                spike_min: ms(5),
                spike_max: ms(80),
            },
            stalls: Some(Stalls {
                every_min: ms(10_000),
                every_max: ms(20_000),
                min: ms(100),
                max: ms(300),
            }),
            capacity: Some(Capacity {
                bps: 120_000_000,
                low_bps: 30_000_000,
                period: ms(30_000),
                low_for: ms(8_000),
                queue: ms(150),
            }),
            seed: 0xb057,
        }
    }

    /// A poor link (2.4 GHz through walls): 40 Mb/s that falls to 12 for
    /// 10 s every 25 s, loss near 3 % in bursts, stalls every 8 s.
    pub fn wifi_bad() -> Self {
        Self {
            name: "wifi-bad".into(),
            loss: GilbertElliott { p_gb: 0.008, p_bg: 0.2, loss_good: 0.003, loss_bad: 0.6 },
            jitter: Jitter {
                base: ms(8),
                spikes_per_sec: 3.0,
                spike_min: ms(10),
                spike_max: ms(80),
            },
            stalls: Some(Stalls {
                every_min: ms(6_000),
                every_max: ms(10_000),
                min: ms(150),
                max: ms(300),
            }),
            capacity: Some(Capacity {
                bps: 40_000_000,
                low_bps: 12_000_000,
                period: ms(25_000),
                low_for: ms(10_000),
                queue: ms(150),
            }),
            seed: 0xbad,
        }
    }

    /// Only a capacity drop, nothing else: isolates the delay controller.
    pub fn capacity_drop() -> Self {
        Self {
            name: "capacity-drop".into(),
            capacity: Some(Capacity {
                bps: 200_000_000,
                low_bps: 20_000_000,
                period: ms(30_000),
                low_for: ms(12_000),
                queue: ms(200),
            }),
            ..Self::wired()
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        match name {
            "wired" => Some(Self::wired()),
            "wifi-good" => Some(Self::wifi_good()),
            "wifi-busy" => Some(Self::wifi_busy()),
            "wifi-bad" => Some(Self::wifi_bad()),
            "capacity-drop" => Some(Self::capacity_drop()),
            _ => None,
        }
    }

    /// `RELAY_TEST_NET`: a profile name, optionally followed by overrides.
    ///
    /// `wifi-busy` · `wifi-bad,seed=7` · `wired,cap=100:15:20:6,queue=150`
    ///
    /// Overrides: `seed=N`; `ge=p_gb:p_bg:loss_good:loss_bad`;
    /// `jitter=base_ms:spikes_per_s:min_ms:max_ms`;
    /// `stall=every_min_ms:every_max_ms:min_ms:max_ms` (`stall=off`);
    /// `cap=mbps:low_mbps:period_s:low_s` (`cap=off`); `queue=ms`.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut parts = spec.split(',').map(str::trim).filter(|p| !p.is_empty());
        let mut p = Self::named(parts.next()?)?;
        for part in parts {
            let (k, v) = part.split_once('=')?;
            let nums = || v.split(':').map(|x| x.parse::<f64>().ok()).collect::<Option<Vec<f64>>>();
            let msf = |x: f64| Duration::from_secs_f64(x / 1e3);
            match k {
                "seed" => p.seed = v.parse().ok()?,
                "ge" => {
                    let n = nums()?;
                    let [a, b, c, d] = n[..] else { return None };
                    p.loss = GilbertElliott { p_gb: a, p_bg: b, loss_good: c, loss_bad: d };
                }
                "jitter" => {
                    let n = nums()?;
                    let [a, b, c, d] = n[..] else { return None };
                    p.jitter = Jitter {
                        base: msf(a),
                        spikes_per_sec: b,
                        spike_min: msf(c),
                        spike_max: msf(d),
                    };
                }
                "stall" if v == "off" => p.stalls = None,
                "stall" => {
                    let n = nums()?;
                    let [a, b, c, d] = n[..] else { return None };
                    p.stalls = Some(Stalls {
                        every_min: msf(a),
                        every_max: msf(b),
                        min: msf(c),
                        max: msf(d),
                    });
                }
                "cap" if v == "off" => p.capacity = None,
                "cap" => {
                    let n = nums()?;
                    let [a, b, c, d] = n[..] else { return None };
                    let queue = p.capacity.map_or(ms(150), |c| c.queue);
                    p.capacity = Some(Capacity {
                        bps: (a * 1e6) as u64,
                        low_bps: (b * 1e6) as u64,
                        period: Duration::from_secs_f64(c),
                        low_for: Duration::from_secs_f64(d),
                        queue,
                    });
                }
                "queue" => {
                    let q = msf(v.parse().ok()?);
                    if let Some(c) = p.capacity.as_mut() {
                        c.queue = q;
                    }
                }
                _ => return None,
            }
        }
        Some(p)
    }
}

/// What happened to one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Arrives at this time (on the model's clock).
    Deliver(Duration),
    /// The access point's queue was full.
    QueueDrop,
    /// Lost on the air.
    RadioLoss,
}

/// Counters, for the test to report what the model actually did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SimStats {
    pub packets: u64,
    pub queue_drops: u64,
    pub radio_losses: u64,
    pub stalls: u64,
    pub spikes: u64,
    /// Largest delay the model added to a delivered packet.
    pub max_added: Duration,
}

/// The running model. Feed packets in arrival order with [`Link::on_packet`].
pub struct Link {
    p: Profile,
    rng: Rng,
    bad: bool,
    /// When the bottleneck finishes sending what is queued.
    link_free: Duration,
    /// The radio is held (spike or stall) until this time.
    held_until: Duration,
    next_spike: Duration,
    next_stall: Duration,
    /// Release times never go backwards: 802.11 delivers in order.
    last_release: Duration,
    pub stats: SimStats,
}

impl Link {
    pub fn new(p: Profile) -> Self {
        let mut rng = Rng::new(p.seed);
        let next_spike = exp_interval(&mut rng, p.jitter.spikes_per_sec);
        let next_stall =
            p.stalls.map_or(Duration::MAX, |s| uniform(&mut rng, s.every_min, s.every_max));
        Self {
            p,
            rng,
            bad: false,
            link_free: Duration::ZERO,
            held_until: Duration::ZERO,
            next_spike,
            next_stall,
            last_release: Duration::ZERO,
            stats: SimStats::default(),
        }
    }

    pub fn profile(&self) -> &Profile {
        &self.p
    }

    /// One packet of `len` bytes arriving at the link at `now`.
    pub fn on_packet(&mut self, now: Duration, len: usize) -> Fate {
        self.stats.packets += 1;
        // Holds and stalls that began since the last packet.
        while self.next_spike <= now {
            let d = uniform(&mut self.rng, self.p.jitter.spike_min, self.p.jitter.spike_max);
            self.held_until = self.held_until.max(self.next_spike + d);
            self.stats.spikes += 1;
            self.next_spike += exp_interval(&mut self.rng, self.p.jitter.spikes_per_sec).max(ms(1));
        }
        if let Some(s) = self.p.stalls {
            while self.next_stall <= now {
                let d = uniform(&mut self.rng, s.min, s.max);
                self.held_until = self.held_until.max(self.next_stall + d);
                self.stats.stalls += 1;
                self.next_stall += uniform(&mut self.rng, s.every_min, s.every_max);
            }
        }

        // The bottleneck queue. A hold stops the radio, so the queue stops
        // draining too: what arrives during a stall is queued behind it, and
        // can overflow the access point's buffer.
        let mut depart = now;
        if let Some(c) = self.p.capacity {
            let rate = c.rate_at(now).max(1);
            let start = now.max(self.link_free).max(self.held_until);
            let tx = Duration::from_nanos((len as u64 * 8).saturating_mul(1_000_000_000) / rate);
            if start.saturating_sub(now) > c.queue {
                self.stats.queue_drops += 1;
                return Fate::QueueDrop;
            }
            depart = start + tx;
            self.link_free = depart;
        } else {
            depart = depart.max(self.held_until);
        }

        let jitter = uniform(&mut self.rng, Duration::ZERO, self.p.jitter.base);
        let release = (depart + jitter).max(self.last_release);
        self.last_release = release;

        // Radio loss last: a lost packet still used its airtime.
        let g = self.p.loss;
        let flip = self.rng.f64();
        self.bad = if self.bad { flip >= g.p_bg } else { flip < g.p_gb };
        let loss = if self.bad { g.loss_bad } else { g.loss_good };
        if self.rng.f64() < loss {
            self.stats.radio_losses += 1;
            return Fate::RadioLoss;
        }
        self.stats.max_added = self.stats.max_added.max(release.saturating_sub(now));
        Fate::Deliver(release)
    }
}

/// xorshift64*: deterministic, tiny, good enough for a link model.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn uniform(rng: &mut Rng, lo: Duration, hi: Duration) -> Duration {
    if hi <= lo {
        return lo;
    }
    lo + (hi - lo).mul_f64(rng.f64())
}

fn exp_interval(rng: &mut Rng, per_sec: f64) -> Duration {
    if per_sec <= 0.0 {
        return Duration::MAX / 4;
    }
    let u = rng.f64().max(1e-12);
    Duration::from_secs_f64((-u.ln() / per_sec).min(1e6))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1200-byte packets at `mbps` for `secs`, returning every fate.
    fn run(p: Profile, mbps: f64, secs: f64) -> (Vec<(Duration, Fate)>, SimStats) {
        let mut l = Link::new(p);
        let gap = Duration::from_secs_f64(1200.0 * 8.0 / (mbps * 1e6));
        let n = (secs / gap.as_secs_f64()) as usize;
        let out = (0..n)
            .map(|i| {
                let t = gap * i as u32;
                (t, l.on_packet(t, 1200))
            })
            .collect();
        (out, l.stats)
    }

    #[test]
    fn wired_does_nothing() {
        let (out, s) = run(Profile::wired(), 40.0, 5.0);
        assert!(out.iter().all(|(t, f)| *f == Fate::Deliver(*t)));
        assert_eq!((s.queue_drops, s.radio_losses, s.stalls, s.spikes), (0, 0, 0, 0));
    }

    #[test]
    fn delivery_is_in_order() {
        let (out, _) = run(Profile::wifi_bad(), 30.0, 30.0);
        let mut last = Duration::ZERO;
        for (_, f) in out {
            if let Fate::Deliver(at) = f {
                assert!(at >= last);
                last = at;
            }
        }
    }

    #[test]
    fn gilbert_elliott_loss_matches_its_mean_and_comes_in_bursts() {
        let ge = GilbertElliott { p_gb: 0.003, p_bg: 0.2, loss_good: 0.001, loss_bad: 0.5 };
        let p = Profile { loss: ge, ..Profile::wired() };
        let (out, s) = run(p, 40.0, 60.0);
        let rate = s.radio_losses as f64 / s.packets as f64;
        assert!(
            (rate - ge.mean_loss()).abs() < ge.mean_loss() * 0.25,
            "{rate} vs {}",
            ge.mean_loss()
        );
        // Bursty: far more back-to-back losses than independent loss at the
        // same rate would give (rate^2 per pair).
        let lost: Vec<bool> = out.iter().map(|(_, f)| *f == Fate::RadioLoss).collect();
        let pairs = lost.windows(2).filter(|w| w[0] && w[1]).count() as f64;
        let independent = rate * rate * (lost.len() - 1) as f64;
        assert!(pairs > independent * 10.0, "pairs {pairs} vs independent {independent}");
    }

    #[test]
    fn stalls_hold_everything_then_release_in_a_burst() {
        let p = Profile {
            stalls: Some(Stalls {
                every_min: ms(1000),
                every_max: ms(1000),
                min: ms(200),
                max: ms(200),
            }),
            ..Profile::wired()
        };
        let (out, s) = run(p, 20.0, 2.5);
        assert_eq!(s.stalls, 2);
        // A packet arriving 50 ms into the first stall waits out the other 150.
        let (_, f) = out.iter().find(|(t, _)| *t >= ms(1050)).unwrap();
        assert_eq!(*f, Fate::Deliver(ms(1200)));
        assert!(s.max_added <= ms(200) && s.max_added > ms(195), "{:?}", s.max_added);
    }

    #[test]
    fn spikes_add_bounded_holds() {
        let p = Profile {
            jitter: Jitter {
                base: ms(2),
                spikes_per_sec: 3.0,
                spike_min: ms(5),
                spike_max: ms(80),
            },
            ..Profile::wired()
        };
        let (_, s) = run(p, 20.0, 20.0);
        assert!((40..=80).contains(&s.spikes), "{} spikes in 20 s at 3/s", s.spikes);
        assert!(s.max_added <= ms(82) && s.max_added >= ms(40), "{:?}", s.max_added);
    }

    #[test]
    fn a_capacity_drop_builds_queue_delay_then_tail_drops() {
        // 40 Mb/s into a link that drops to 20 for the last 5 s of 10.
        let p = Profile {
            capacity: Some(Capacity {
                bps: 100_000_000,
                low_bps: 20_000_000,
                period: ms(10_000),
                low_for: ms(5_000),
                queue: ms(150),
            }),
            ..Profile::wired()
        };
        let (out, s) = run(p, 40.0, 10.0);
        let delay_at = |secs: f64| {
            out.iter()
                .filter(|(t, _)| t.as_secs_f64() >= secs)
                .find_map(|(t, f)| if let Fate::Deliver(at) = f { Some(*at - *t) } else { None })
                .unwrap()
        };
        assert!(delay_at(2.0) < ms(1), "plenty of rate: no queue");
        let a = delay_at(5.05);
        let b = delay_at(5.15);
        assert!(b > a, "queue delay grows while the rate is short: {a:?} then {b:?}");
        assert!(s.queue_drops > 0, "and once it reaches 150 ms the queue drops");
        // About half of what is sent during the drop cannot fit.
        let sent_low = out.iter().filter(|(t, _)| *t >= ms(5_000)).count() as f64;
        let ratio = s.queue_drops as f64 / sent_low;
        assert!((0.4..0.6).contains(&ratio), "{ratio}");
    }

    #[test]
    fn the_same_seed_gives_the_same_link() {
        let a = run(Profile::wifi_busy(), 30.0, 10.0).0;
        let b = run(Profile::wifi_busy(), 30.0, 10.0).0;
        assert_eq!(a, b);
        let c = run(Profile::parse("wifi-busy,seed=99").unwrap(), 30.0, 10.0).0;
        assert_ne!(a, c);
    }

    #[test]
    fn specs_parse_with_overrides() {
        let p = Profile::parse("wifi-bad,seed=7,stall=off,cap=100:15:20:6,queue=120").unwrap();
        assert_eq!(p.seed, 7);
        assert!(p.stalls.is_none());
        let c = p.capacity.unwrap();
        assert_eq!(
            (c.bps, c.low_bps, c.period, c.low_for, c.queue),
            (100_000_000, 15_000_000, ms(20_000), ms(6_000), ms(120))
        );
        let p = Profile::parse("wired,ge=0.01:0.3:0:0.5,jitter=2:1:5:80").unwrap();
        assert_eq!(p.loss.p_gb, 0.01);
        assert_eq!(p.jitter.spike_max, ms(80));
        assert!(Profile::parse("nonsense").is_none());
        assert!(Profile::parse("wired,ge=1:2").is_none());
        assert!(Profile::parse("wired,bogus=1").is_none());
        for n in ["wired", "wifi-good", "wifi-busy", "wifi-bad", "capacity-drop"] {
            assert_eq!(Profile::parse(n).unwrap().name, n);
        }
    }
}
