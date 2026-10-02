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
pub const OUTLIER_CLIP_FRAC: f32 = 0.9;
/// No keyboard/mouse/pad input anywhere on the system for longer than this
/// means nobody is playing: a cutscene, a menu left open, AFK. Read from
/// `GetLastInputInfo` — one system-wide timestamp, no hooks, no keylogging.
pub const INPUT_IDLE_MAX_MS: u64 = 20_000;
/// Near-uniform also needs the spread to be small *relative to the mean*: a
/// night scene at mean 0.05 with std 0.02 is textured (ratio 0.4); a black
/// or grey loading screen is not.
pub const LOADING_MAX_CV: f32 = 0.25;
/// A near-uniform frame only counts as Loading when it also has no texture:
/// mean local gradient over the whole picture below this. A spinner or a
/// fade has none; a night raid's ground and foliage do, however dark.
/// (Motion is no test: a night scene can move less than a spinner.)
pub const LOADING_MIN_DETAIL: f32 = 0.005;
/// Recent block grids kept to find the active content region.
pub const ACTIVE_HISTORY: usize = 8;
/// A block whose mean changed by more than this across the history is
/// active content. Borders, desktop and browser chrome do not change at all,
/// so the bar is low: under one 8-bit code, which still catches a game's
/// slowly shimmering dark sky.
pub const ACTIVE_BLOCK_DELTA: f32 = 0.003;
/// A frame where at least this fraction of pixels sits within
/// [`LOADING_NEAR`] of the median is a uniform field with something small
/// on it: a spinner, a logo, a progress dot.
pub const LOADING_UNIFORM_FRAC: f32 = 0.95;
pub const LOADING_NEAR: f32 = 0.02;
/// Below this fraction of the frame, the "active region" is too small to
/// trust (a cursor blink, a clock): analyse the whole frame instead.
pub const MIN_ACTIVE_FRAC: f32 = 0.05;
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
    /// Fraction of the frame the statistics came from (the active content
    /// region; 1 = whole frame).
    #[serde(default = "one")]
    pub content_frac: f32,
    /// Fraction of pixels within [`LOADING_NEAR`] of the median.
    #[serde(default)]
    pub uniform_frac: f32,
    /// Mean local luma gradient over the whole picture (texture).
    #[serde(default)]
    pub detail: f32,
}

fn one() -> f32 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameReport {
    pub class: FrameClass,
    pub stats: FrameStats,
}

/// Stateful only for motion and the active region: the last few 64×36
/// block-mean grids (never pixels). Create one per sampling session.
#[derive(Debug, Default)]
pub struct Analyser {
    grids: std::collections::VecDeque<Vec<f32>>,
}

/// Block rectangle `[x0, x1) × [y0, y1)` in grid units.
type BlockRect = (usize, usize, usize, usize);

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
        if self.grids.back().is_some_and(|g| g.len() != grid.len()) {
            self.grids.clear();
        }
        self.grids.push_back(grid);
        if self.grids.len() > ACTIVE_HISTORY {
            self.grids.pop_front();
        }
        let rect = self.active_rect();
        let warmup = self.grids.len() < 2;
        let motion = if warmup { -1.0 } else { self.motion_in(rect) };

        // Crop to the active content: a windowed game or a video inside a
        // static page is analysed on its own pixels, not the page's.
        let (gw, gh) = MOTION_GRID;
        let (px0, px1) = (rect.0 * w / gw, (rect.1 * w).div_ceil(gw).min(w));
        let (py0, py1) = (rect.2 * h / gh, (rect.3 * h).div_ceil(gh).min(h));
        let (cw, ch) = (px1 - px0, py1 - py0);
        let mut data = Vec::with_capacity(cw * ch * 3);
        let mut cl = Vec::with_capacity(cw * ch);
        for y in py0..py1 {
            data.extend_from_slice(&f.data[(y * w + px0) * 3..(y * w + px1) * 3]);
            cl.extend_from_slice(&luma[y * w + px0..y * w + px1]);
        }
        let crop = Frame { width: cw, height: ch, data: &data, order: f.order };

        let bar = letterbox_rows(&cl, cw, ch);
        let mut stats = stats_for(&crop, &cl, bar);
        stats.motion = motion.max(0.0);
        // Static black bars are cropped away with the rest of the border, so
        // look for them on the whole frame too.
        let full_bar = letterbox_rows(&luma, w, h) as f32 / h as f32;
        stats.letterbox = (bar as f32 / ch as f32).max(full_bar);
        stats.content_frac = (cw * ch) as f32 / (w * h) as f32;
        let class = classify(&stats, warmup);
        FrameReport { class, stats }
    }

    /// Bounding box of blocks that changed across the history; the whole
    /// grid until there is history or when too little changed to trust.
    fn active_rect(&self) -> BlockRect {
        let (gw, gh) = MOTION_GRID;
        let full = (0, gw, 0, gh);
        if self.grids.len() < 2 {
            return full;
        }
        let (mut x0, mut x1, mut y0, mut y1) = (gw, 0, gh, 0);
        let mut active = 0usize;
        for i in 0..gw * gh {
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for g in &self.grids {
                lo = lo.min(g[i]);
                hi = hi.max(g[i]);
            }
            if hi - lo > ACTIVE_BLOCK_DELTA {
                active += 1;
                let (x, y) = (i % gw, i / gw);
                x0 = x0.min(x);
                x1 = x1.max(x + 1);
                y0 = y0.min(y);
                y1 = y1.max(y + 1);
            }
        }
        if active == 0 || ((x1 - x0) * (y1 - y0)) as f32 / ((gw * gh) as f32) < MIN_ACTIVE_FRAC {
            return full;
        }
        // An edge block of the box usually straddles the static border:
        // drop one block on every side that is not the frame edge.
        let (x0, x1) = if x1 - x0 > 2 {
            (if x0 > 0 { x0 + 1 } else { 0 }, if x1 < gw { x1 - 1 } else { gw })
        } else {
            (x0, x1)
        };
        let (y0, y1) = if y1 - y0 > 2 {
            (if y0 > 0 { y0 + 1 } else { 0 }, if y1 < gh { y1 - 1 } else { gh })
        } else {
            (y0, y1)
        };
        (x0, x1, y0, y1)
    }

    /// Mean block change between the last two grids, inside `rect` only, so
    /// a small window of motion is not diluted by a static page around it.
    fn motion_in(&self, r: BlockRect) -> f32 {
        let n = self.grids.len();
        let (a, b) = (&self.grids[n - 2], &self.grids[n - 1]);
        let gw = MOTION_GRID.0;
        let mut sum = 0.0;
        let mut count = 0;
        for y in r.2..r.3 {
            for x in r.0..r.1 {
                sum += (a[y * gw + x] - b[y * gw + x]).abs();
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            sum / count as f32
        }
    }
}

/// The classification rules, in priority order. Pure; tested directly.
pub fn classify(s: &FrameStats, warmup: bool) -> FrameClass {
    if s.clip_frac > OUTLIER_CLIP_FRAC {
        FrameClass::Outlier
    } else if is_loading(s) {
        FrameClass::Loading
    } else if s.letterbox >= LETTERBOX_BAR_FRAC {
        FrameClass::Cutscene
    } else if warmup {
        FrameClass::Warmup
    } else if s.motion < MOTION_STATIC {
        FrameClass::Static
    } else {
        FrameClass::Gameplay
    }
}

/// Near-uniform (small spread absolutely and relative to the mean, or almost
/// every pixel at the median) *and* textureless. Dark gameplay is
/// low-spread but textured, so it is not Loading.
pub fn is_loading(s: &FrameStats) -> bool {
    let uniform = (s.luma_std < LOADING_MAX_STDDEV
        && s.luma_std / s.mean_luma.max(0.01) < LOADING_MAX_CV)
        || s.uniform_frac >= LOADING_UNIFORM_FRAC;
    uniform && s.detail < LOADING_MIN_DETAIL
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
    let (mut crush_grad, mut dark_grad, mut all_grad) = (0f32, 0f32, 0f32);
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
            all_grad += grad;
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
    let near = (LOADING_NEAR * 255.0).round() as i32;
    let mid = (s.median_luma * 255.0).round() as i32;
    let close: u32 = hist
        .iter()
        .enumerate()
        .filter(|(v, _)| (*v as i32 - mid).abs() <= near)
        .map(|(_, c)| *c)
        .sum();
    s.uniform_frac = close as f32 / n;
    s.crush_frac = crush as f32 / n;
    s.detail = all_grad / n;
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
