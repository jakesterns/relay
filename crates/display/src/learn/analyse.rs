//! Per-frame analysis of a small (~480×270) game frame.
//!
//! Input is packed 8-bit RGB or BGR, already scaled down on the GPU. Output
//! is a handful of numbers ([`FrameStats`]) and a class ([`FrameClass`]);
//! the pixels are never stored. The one piece of state kept between frames
//! is a 64×36 block-mean luma grid, used only to measure motion against the
//! next frame — it is overwritten every frame and never leaves the process.
//!
//! All values are on gamma-encoded (display) luma Y' = BT.709 weights over
//! R'G'B', 0..1. That is deliberate: "crushed" and "clipped" are about the
//! code values the panel receives, not scene-linear light.

use serde::{Deserialize, Serialize};

/// Luma below this is "crushed" shadow (5 % of full scale).
pub const LUMA_CRUSH: f32 = 0.05;
/// Luma at or above this is clipped highlight.
pub const LUMA_CLIP: f32 = 0.98;
/// Luma below this counts as a dark region for local-contrast measurement.
pub const DARK_REGION: f32 = 0.15;
/// Hue histogram resolution (30° bins).
pub const HUE_BINS: usize = 12;
/// Pixels darker than this (max channel) have no meaningful hue/saturation.
pub const CHROMA_FLOOR: f32 = 0.04;
/// Mean absolute block-luma change between consecutive samples below which
/// the frame is static: a menu, a pause screen, a HUD over a frozen world.
pub const MOTION_STATIC: f32 = 0.004;
/// A letterbox bar must be at least this fraction of the frame height, top
/// and bottom, for the frame to read as a cutscene.
pub const LETTERBOX_BAR_FRAC: f32 = 0.08;
/// A row whose mean luma is below this, with almost no variation, is bar.
pub const LETTERBOX_BLACK: f32 = 0.03;
/// A frame whose luma standard deviation is below this is near-uniform: a
/// loading screen, a fade, a black transition.
pub const LOADING_MAX_STDDEV: f32 = 0.03;
/// More than this fraction clipped is a flash / white-out: an outlier.
pub const OUTLIER_CLIP_FRAC: f32 = 0.6;
/// No keyboard/mouse/pad input anywhere on the system for longer than this
/// means nobody is playing: a cutscene, a menu left open, AFK. Read from
/// `GetLastInputInfo` — one system-wide timestamp, no hooks, no keylogging.
pub const INPUT_IDLE_MAX_MS: u64 = 20_000;
/// Motion grid (block means), small enough to be free to keep and compare.
pub const MOTION_GRID: (usize, usize) = (64, 36);

/// Byte order of the packed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Rgb,
    Bgr,
}

/// A borrowed, packed 3-bytes-per-pixel frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    pub width: usize,
    pub height: usize,
    pub data: &'a [u8],
    pub order: Order,
}

/// What kind of frame this was. Only `Gameplay` feeds the learner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameClass {
    Gameplay,
    /// No previous frame to measure motion against yet.
    Warmup,
    /// Low motion: menu, pause, map screen, HUD-only.
    Static,
    /// Near-uniform: loading screen, fade, black transition.
    Loading,
    /// Letterboxed: a cutscene.
    Cutscene,
    /// Flash / white-out, or an unusable frame.
    Outlier,
    /// Moving picture but no input for [`INPUT_IDLE_MAX_MS`]: a cutscene
    /// without bars, an attract loop, AFK.
    Idle,
}

/// Fold the system input-idle time into a report: gameplay with no input for
/// longer than [`INPUT_IDLE_MAX_MS`] is not gameplay. `None` = unknown (the
/// classification stands).
pub fn with_input_idle(mut r: FrameReport, idle_ms: Option<u64>) -> FrameReport {
    if r.class == FrameClass::Gameplay && idle_ms.is_some_and(|ms| ms > INPUT_IDLE_MAX_MS) {
        r.class = FrameClass::Idle;
    }
    r
}

/// The only thing that leaves the analyser. No pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FrameStats {
    pub mean_luma: f32,
    pub median_luma: f32,
    pub luma_std: f32,
    /// Fraction of pixels below [`LUMA_CRUSH`].
    pub crush_frac: f32,
    /// Fraction of pixels at or above [`LUMA_CLIP`].
    pub clip_frac: f32,
    /// Fraction of pixels below [`DARK_REGION`].
    pub dark_frac: f32,
    /// Mean local luma gradient among crushed pixels. Non-zero means there is
    /// texture down there — detail that exists but is crushed.
    pub crushed_detail: f32,
    /// Mean local luma gradient among dark-region pixels.
    pub dark_detail: f32,
    /// Mean HSV-style saturation over pixels above [`CHROMA_FLOOR`].
    pub sat_mean: f32,
    /// 90th percentile of the same.
    pub sat_p90: f32,
    /// Saturation-weighted hue histogram, normalised to sum 1 (all zero for
    /// a grey frame).
    pub hue_hist: [f32; HUE_BINS],
    /// Mean absolute block-luma change from the previous sample (0 on warmup).
    pub motion: f32,
    /// Letterbox bar height as a fraction of the frame, top and bottom (min).
    pub letterbox: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameReport {
    pub class: FrameClass,
    pub stats: FrameStats,
}

/// Stateful only for motion. Create one per sampling session.
#[derive(Debug, Default)]
pub struct Analyser {
    prev_grid: Option<Vec<f32>>,
}

impl Analyser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn analyse(&mut self, f: &Frame) -> FrameReport {
        let (w, h) = (f.width, f.height);
        if w < 2 || h < 2 || f.data.len() < w * h * 3 {
            return FrameReport { class: FrameClass::Outlier, stats: FrameStats::default() };
        }
        let luma = luma_plane(f);
        let grid = block_grid(&luma, w, h);
        let motion = match &self.prev_grid {
            Some(prev) => mean_abs_diff(prev, &grid),
            None => -1.0,
        };
        self.prev_grid = Some(grid);

        let bar = letterbox_rows(&luma, w, h);
        let mut stats = stats_for(f, &luma, bar);
        stats.motion = motion.max(0.0);
        stats.letterbox = bar as f32 / h as f32;
        let class = classify(&stats, motion < 0.0);
        FrameReport { class, stats }
    }
}

/// The classification rules, in priority order. Pure; tested directly.
pub fn classify(s: &FrameStats, warmup: bool) -> FrameClass {
    if s.luma_std < LOADING_MAX_STDDEV {
        FrameClass::Loading
    } else if s.letterbox >= LETTERBOX_BAR_FRAC {
        FrameClass::Cutscene
    } else if s.clip_frac > OUTLIER_CLIP_FRAC {
        FrameClass::Outlier
    } else if warmup {
        FrameClass::Warmup
    } else if s.motion < MOTION_STATIC {
        FrameClass::Static
    } else {
        FrameClass::Gameplay
    }
}

fn rgb_at(f: &Frame, i: usize) -> (f32, f32, f32) {
    let p = &f.data[i * 3..i * 3 + 3];
    let (r, g, b) = match f.order {
        Order::Rgb => (p[0], p[1], p[2]),
        Order::Bgr => (p[2], p[1], p[0]),
    };
    (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

fn luma_plane(f: &Frame) -> Vec<f32> {
    (0..f.width * f.height)
        .map(|i| {
            let (r, g, b) = rgb_at(f, i);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        })
        .collect()
}

fn block_grid(luma: &[f32], w: usize, h: usize) -> Vec<f32> {
    let (gw, gh) = MOTION_GRID;
    let mut sums = vec![0f32; gw * gh];
    let mut counts = vec![0u32; gw * gh];
    for y in 0..h {
        let gy = y * gh / h;
        for x in 0..w {
            let gx = x * gw / w;
            sums[gy * gw + gx] += luma[y * w + x];
            counts[gy * gw + gx] += 1;
        }
    }
    sums.iter().zip(&counts).map(|(s, c)| if *c == 0 { 0.0 } else { s / *c as f32 }).collect()
}

fn mean_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0; // a size change is not "static"
    }
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

/// Height of the shorter of the top and bottom black bars, in rows.
fn letterbox_rows(luma: &[f32], w: usize, h: usize) -> usize {
    let row_black = |y: usize| {
        let row = &luma[y * w..(y + 1) * w];
        let mean = row.iter().sum::<f32>() / w as f32;
        let max = row.iter().cloned().fold(0.0, f32::max);
        mean < LETTERBOX_BLACK && max < LETTERBOX_BLACK * 2.0
    };
    let top = (0..h).take_while(|&y| row_black(y)).count();
    if top >= h {
        return 0; // all black: that is Loading, not letterbox
    }
    let bottom = (0..h).rev().take_while(|&y| row_black(y)).count();
    top.min(bottom)
}

fn stats_for(f: &Frame, luma: &[f32], bar: usize) -> FrameStats {
    let (w, h) = (f.width, f.height);
    // Analyse the picture area only: bars would read as crushed shadow.
    let (y0, y1) = (bar, h - bar);
    let n = ((y1 - y0) * w) as f32;
    let mut s = FrameStats::default();
    let mut hist = [0u32; 256];
    let mut sum = 0f32;
    let mut sum_sq = 0f32;
    let (mut crush, mut clip, mut dark) = (0u32, 0u32, 0u32);
    let (mut crush_grad, mut dark_grad) = (0f32, 0f32);
    let mut sats: Vec<f32> = Vec::with_capacity((y1 - y0) * w);
    let mut hue = [0f32; HUE_BINS];

    for y in y0..y1 {
        for x in 0..w {
            let i = y * w + x;
            let l = luma[i];
            sum += l;
            sum_sq += l * l;
            hist[(l * 255.0).round().clamp(0.0, 255.0) as usize] += 1;
            // Forward differences, clamped at the picture edge.
            let right = if x + 1 < w { luma[i + 1] } else { l };
            let down = if y + 1 < y1 { luma[i + w] } else { l };
            let grad = (l - right).abs() + (l - down).abs();
            if l < LUMA_CRUSH {
                crush += 1;
                crush_grad += grad;
            }
            if l < DARK_REGION {
                dark += 1;
                dark_grad += grad;
            }
            if l >= LUMA_CLIP {
                clip += 1;
            }
            let (r, g, b) = rgb_at(f, i);
            let max = r.max(g).max(b);
            if max > CHROMA_FLOOR {
                let min = r.min(g).min(b);
                let sat = (max - min) / max;
                sats.push(sat);
                if sat > 0.0 {
                    hue[hue_bin(r, g, b, max, min)] += sat;
                }
            }
        }
    }

    s.mean_luma = sum / n;
    s.luma_std = (sum_sq / n - s.mean_luma * s.mean_luma).max(0.0).sqrt();
    s.median_luma = median_from_hist(&hist, n as u32);
    s.crush_frac = crush as f32 / n;
    s.clip_frac = clip as f32 / n;
    s.dark_frac = dark as f32 / n;
    s.crushed_detail = if crush > 0 { crush_grad / crush as f32 } else { 0.0 };
    s.dark_detail = if dark > 0 { dark_grad / dark as f32 } else { 0.0 };
    if !sats.is_empty() {
        s.sat_mean = sats.iter().sum::<f32>() / sats.len() as f32;
        let k = ((sats.len() as f32 * 0.9) as usize).min(sats.len() - 1);
        let (_, p90, _) = sats.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
        s.sat_p90 = *p90;
    }
    let total: f32 = hue.iter().sum();
    if total > 0.0 {
        for (o, v) in s.hue_hist.iter_mut().zip(hue) {
            *o = v / total;
        }
    }
    s
}

fn hue_bin(r: f32, g: f32, b: f32, max: f32, min: f32) -> usize {
    let d = max - min;
    let deg = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    ((deg / (360.0 / HUE_BINS as f32)) as usize).min(HUE_BINS - 1)
}

fn median_from_hist(hist: &[u32; 256], n: u32) -> f32 {
    let half = n.div_ceil(2);
    let mut acc = 0;
    for (v, c) in hist.iter().enumerate() {
        acc += c;
        if acc >= half {
            return v as f32 / 255.0;
        }
    }
    1.0
}
