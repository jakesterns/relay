//! Pure pixel work for the DirectShow camera: NV12 scaling, NV12 → YUY2 /
//! RGB24 conversion and the "waiting for a stream" still.
//!
//! No COM, no allocation on the per-frame paths except where noted; every
//! function here is unit-tested without Windows APIs.

/// A pixel format the DirectShow pin offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixFmt {
    /// 4:2:0, Y plane then interleaved UV — the ring's own format.
    Nv12,
    /// 4:2:2 packed Y0 U Y1 V — for apps that only take YUY2.
    Yuy2,
    /// Bottom-up BGR, rows padded to 4 bytes (`BI_RGB`, positive height).
    Rgb24,
}

impl PixFmt {
    pub const ALL: [PixFmt; 3] = [PixFmt::Nv12, PixFmt::Yuy2, PixFmt::Rgb24];

    /// The FourCC-style name testers see in the camera log.
    pub fn name(self) -> &'static str {
        match self {
            PixFmt::Nv12 => "NV12",
            PixFmt::Yuy2 => "YUY2",
            PixFmt::Rgb24 => "RGB24",
        }
    }

    pub fn bits_per_pixel(self) -> u16 {
        match self {
            PixFmt::Nv12 => 12,
            PixFmt::Yuy2 => 16,
            PixFmt::Rgb24 => 24,
        }
    }

    /// Bytes per row (RGB24 rows are DWORD aligned; the YUV formats are
    /// tightly packed at even widths).
    pub fn stride(self, width: u32) -> usize {
        let w = width as usize;
        match self {
            PixFmt::Nv12 => w,
            PixFmt::Yuy2 => w * 2,
            PixFmt::Rgb24 => (w * 3 + 3) & !3,
        }
    }

    /// Bytes for one whole frame.
    pub fn frame_bytes(self, width: u32, height: u32) -> usize {
        let h = height as usize;
        match self {
            PixFmt::Nv12 => self.stride(width) * h * 3 / 2,
            PixFmt::Yuy2 | PixFmt::Rgb24 => self.stride(width) * h,
        }
    }
}

/// NV12 bytes for a `w`×`h` frame.
pub fn nv12_bytes(w: u32, h: u32) -> usize {
    PixFmt::Nv12.frame_bytes(w, h)
}

// ---------------------------------------------------------------------------
// Scaling
// ---------------------------------------------------------------------------

/// Black in limited-range NV12: what the bars around a fitted picture are.
pub const BLACK_Y: u8 = 16;
pub const BLACK_C: u8 = 128;

/// Fixed-point weight scale: every output sample's taps sum to this.
const W_ONE: u32 = 1 << 14;

/// The largest `src`-shaped rectangle centred in `out`, as (x, y, w, h), on
/// even coordinates (NV12 chroma is 2x2). The same rule as the share
/// engine's GPU converter, so a GPU-scaled and a CPU-scaled frame agree.
pub fn fit_rect(src: (u32, u32), out: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sw, sh) = (u64::from(src.0.max(1)), u64::from(src.1.max(1)));
    let (ow, oh) = (u64::from(out.0), u64::from(out.1));
    let (w, h) = if sw * oh >= sh * ow { (ow, ow * sh / sw) } else { (oh * sw / sh, oh) };
    let (w, h) = (((w as u32) & !1).max(2), ((h as u32) & !1).max(2));
    let (w, h) = (w.min(out.0), h.min(out.1));
    let x = ((out.0 - w) / 2) & !1;
    let y = ((out.1 - h) / 2) & !1;
    (x, y, w, h)
}

/// One axis of a resample: for every output sample, the first source
/// sample it reads and its weights (summing to [`W_ONE`]).
#[derive(Debug, Clone, Default)]
struct Axis {
    start: Vec<u32>,
    /// `taps[i]` = offset into `weights` of output `i`'s taps; `taps[n]` = end.
    taps: Vec<u32>,
    weights: Vec<u16>,
}

impl Axis {
    /// Area (box) averaging when shrinking — each output sample is the mean
    /// of exactly the source area it covers, which is what keeps thin text
    /// strokes from vanishing or doubling the way point sampling does —
    /// and bilinear when growing.
    fn new(sn: usize, dn: usize) -> Self {
        let mut ax = Axis { start: Vec::with_capacity(dn), taps: vec![0], weights: Vec::new() };
        let scale = sn as f64 / dn as f64;
        for o in 0..dn {
            let mut taps: Vec<(usize, f64)> = Vec::with_capacity(4);
            if sn >= dn {
                let lo = o as f64 * scale;
                let hi = (o + 1) as f64 * scale;
                let mut i = lo.floor() as usize;
                while (i as f64) < hi && i < sn {
                    let overlap = hi.min(i as f64 + 1.0) - lo.max(i as f64);
                    if overlap > 1e-9 {
                        taps.push((i, overlap));
                    }
                    i += 1;
                }
            } else {
                let c = ((o as f64 + 0.5) * scale - 0.5).clamp(0.0, (sn - 1) as f64);
                let i0 = c.floor() as usize;
                let f = c - i0 as f64;
                taps.push((i0, 1.0 - f));
                if i0 + 1 < sn && f > 1e-9 {
                    taps.push((i0 + 1, f));
                }
            }
            let total: f64 = taps.iter().map(|t| t.1).sum();
            let mut q: Vec<u16> =
                taps.iter().map(|t| (t.1 / total * W_ONE as f64).round() as u16).collect();
            // Make the fixed-point weights sum exactly to one: flat areas
            // stay exactly flat.
            let sum: i64 = q.iter().map(|&w| w as i64).sum();
            let big = (0..q.len()).max_by_key(|&k| q[k]).unwrap_or(0);
            q[big] = (q[big] as i64 + W_ONE as i64 - sum) as u16;
            ax.start.push(taps[0].0 as u32);
            ax.weights.extend_from_slice(&q);
            ax.taps.push(ax.weights.len() as u32);
        }
        ax
    }

    fn of(&self, o: usize) -> (usize, &[u16]) {
        let (a, b) = (self.taps[o] as usize, self.taps[o + 1] as usize);
        (self.start[o] as usize, &self.weights[a..b])
    }
}

/// The resample of one plane into a rectangle of the destination plane.
#[derive(Debug, Clone, Default)]
struct PlanePlan {
    /// Destination rectangle, in plane samples (not bytes).
    rect: (usize, usize, usize, usize),
    h: Axis,
    v: Axis,
}

/// A reusable NV12 resizer: area-averaged when shrinking, bilinear when
/// growing, aspect kept (black bars, never a stretch). The plan is built
/// once per size pair; the per-frame work allocates nothing after the
/// first frame.
#[derive(Debug, Clone, Default)]
pub struct Scaler {
    key: (u32, u32, u32, u32),
    y: PlanePlan,
    uv: PlanePlan,
    /// Horizontal pass output, 8.8 fixed point.
    tmp: Vec<u16>,
    /// Vertical accumulator for one output row.
    acc: Vec<u32>,
}

impl Scaler {
    pub fn new() -> Self {
        Self::default()
    }

    fn plan(&mut self, sw: u32, sh: u32, dw: u32, dh: u32) {
        if self.key == (sw, sh, dw, dh) && !self.y.h.start.is_empty() {
            return;
        }
        let (x, y, w, h) = fit_rect((sw, sh), (dw, dh));
        let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
        let (sw, sh) = (sw as usize, sh as usize);
        self.y = PlanePlan { rect: (x, y, w, h), h: Axis::new(sw, w), v: Axis::new(sh, h) };
        self.uv = PlanePlan {
            rect: (x / 2, y / 2, w / 2, h / 2),
            h: Axis::new(sw / 2, w / 2),
            v: Axis::new(sh / 2, h / 2),
        };
        self.key = (sw as u32, sh as u32, dw, dh);
    }

    /// Resize a tightly packed NV12 `sw`×`sh` frame into a tightly packed
    /// `dw`×`dh` one. Orientation is untouched: row 0 stays the top row and
    /// column 0 the left column.
    pub fn scale(&mut self, src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
        let (swu, shu, dwu, dhu) = (sw as usize, sh as usize, dw as usize, dh as usize);
        assert!(src.len() >= swu * shu * 3 / 2 && dst.len() >= dwu * dhu * 3 / 2);
        self.plan(sw, sh, dw, dh);
        let (dy, duv) = dst[..dwu * dhu * 3 / 2].split_at_mut(dwu * dhu);
        let (sy, suv) = src[..swu * shu * 3 / 2].split_at(swu * shu);
        let full = self.y.rect == (0, 0, dwu, dhu);
        if !full {
            dy.fill(BLACK_Y);
            duv.fill(BLACK_C);
        }
        let (yp, uvp) = (std::mem::take(&mut self.y), std::mem::take(&mut self.uv));
        self.plane(sy, swu, shu, 1, dy, dwu, &yp);
        self.plane(suv, swu / 2, shu / 2, 2, duv, dwu, &uvp);
        (self.y, self.uv) = (yp, uvp);
    }

    /// Resample one plane of `ch` interleaved channels. `sw` is in samples;
    /// rows are `sw * ch` bytes in the source and `dstride` in the target.
    #[allow(clippy::too_many_arguments)]
    fn plane(
        &mut self,
        src: &[u8],
        sw: usize,
        sh: usize,
        ch: usize,
        dst: &mut [u8],
        dstride: usize,
        p: &PlanePlan,
    ) {
        let (rx, ry, rw, rh) = p.rect;
        let row = rw * ch;
        self.tmp.resize(sh * row, 0);
        // Horizontal: every source row into `rw` samples.
        for y in 0..sh {
            let s = &src[y * sw * ch..(y + 1) * sw * ch];
            let t = &mut self.tmp[y * row..(y + 1) * row];
            for x in 0..rw {
                let (start, w) = p.h.of(x);
                for c in 0..ch {
                    let mut a = 0u32;
                    for (k, &wk) in w.iter().enumerate() {
                        a += wk as u32 * s[(start + k) * ch + c] as u32;
                    }
                    t[x * ch + c] = ((a + 32) >> 6) as u16;
                }
            }
        }
        // Vertical: weighted rows, accumulated a whole row at a time.
        self.acc.resize(row, 0);
        for y in 0..rh {
            let (start, w) = p.v.of(y);
            self.acc.fill(0);
            for (k, &wk) in w.iter().enumerate() {
                let t = &self.tmp[(start + k) * row..(start + k + 1) * row];
                for (a, &v) in self.acc.iter_mut().zip(t) {
                    *a += wk as u32 * v as u32;
                }
            }
            let out = &mut dst[(ry + y) * dstride + rx * ch..(ry + y) * dstride + rx * ch + row];
            for (o, &a) in out.iter_mut().zip(&self.acc) {
                *o = ((a + (1 << 21)) >> 22).min(255) as u8;
            }
        }
    }
}

/// One-off NV12 resize with a fresh [`Scaler`] (tests, rare paths). Area
/// averaging when shrinking, bilinear when growing, aspect kept.
pub fn scale_nv12(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    Scaler::new().scale(src, sw, sh, dst, dw, dh);
}

/// The old nearest-neighbour resize (r54 made it the baseline the area
/// filter is measured against; nothing ships it). Point sampling drops or
/// doubles whole columns of a 1-2 px text stroke, which is the "blurry,
/// hard to read" picture the call saw at 2560x1440 → 1280x720.
#[doc(hidden)]
pub fn scale_nv12_nearest(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    for y in 0..dh {
        let sy = y * sh / dh;
        for x in 0..dw {
            dst[y * dw + x] = src[sy * sw + x * sw / dw];
        }
    }
    let (suv, duv) = (&src[sw * sh..], &mut dst[dw * dh..]);
    let (scw, dcw) = (sw / 2, dw / 2);
    for y in 0..dh / 2 {
        let sy = (y * (sh / 2) / (dh / 2).max(1)).min(sh / 2 - 1);
        for x in 0..dcw {
            let sx = (x * scw / dcw.max(1)).min(scw - 1);
            duv[y * dw + 2 * x] = suv[sy * sw + 2 * sx];
            duv[y * dw + 2 * x + 1] = suv[sy * sw + 2 * sx + 1];
        }
    }
}

/// Convert a tightly packed NV12 frame into `fmt` (same size). `dst` must
/// hold `fmt.frame_bytes(w, h)`.
pub fn convert_nv12(fmt: PixFmt, src: &[u8], w: u32, h: u32, dst: &mut [u8]) {
    let (wu, hu) = (w as usize, h as usize);
    let uv = &src[wu * hu..];
    match fmt {
        PixFmt::Nv12 => dst[..wu * hu * 3 / 2].copy_from_slice(&src[..wu * hu * 3 / 2]),
        PixFmt::Yuy2 => {
            for y in 0..hu {
                let yrow = &src[y * wu..y * wu + wu];
                let crow = &uv[(y / 2) * wu..(y / 2) * wu + wu];
                let out = &mut dst[y * wu * 2..y * wu * 2 + wu * 2];
                for x in 0..wu / 2 {
                    out[4 * x] = yrow[2 * x];
                    out[4 * x + 1] = crow[2 * x];
                    out[4 * x + 2] = yrow[2 * x + 1];
                    out[4 * x + 3] = crow[2 * x + 1];
                }
            }
        }
        PixFmt::Rgb24 => {
            let stride = fmt.stride(w);
            for y in 0..hu {
                let yrow = &src[y * wu..y * wu + wu];
                let crow = &uv[(y / 2) * wu..(y / 2) * wu + wu];
                // Bottom-up: source row 0 is the last row in memory.
                let out = &mut dst[(hu - 1 - y) * stride..(hu - 1 - y) * stride + wu * 3];
                for x in 0..wu {
                    let (b, g, r) = yuv_to_bgr(yrow[x], crow[x & !1], crow[x | 1]);
                    out[3 * x] = b;
                    out[3 * x + 1] = g;
                    out[3 * x + 2] = r;
                }
            }
        }
    }
}

/// BT.709 limited range → 8-bit BGR, integer only.
pub fn yuv_to_bgr(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let c = 298 * (y as i32 - 16);
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clamp = |x: i32| ((x + 128) >> 8).clamp(0, 255) as u8;
    (clamp(c + 541 * d), clamp(c - 55 * d - 137 * e), clamp(c + 459 * e))
}

// ---------------------------------------------------------------------------
// The waiting still
// ---------------------------------------------------------------------------

const BG_Y: u8 = 0x1A; // warm near-black, not a "no signal" pure black
const FG_Y: u8 = 0xD8; // ivory-ish text
const DIM_Y: u8 = 0x70;

/// 5×7 glyphs for the two lines we draw; unknown characters draw blank.
fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        _ => [0; 7],
    }
}

/// Top line of the still.
pub const WAITING_TITLE: &str = "RELAY CAMERA";
/// Second line of the still.
pub const WAITING_LINE: &str = "WAITING FOR A STREAM";

fn draw_text(yplane: &mut [u8], w: usize, h: usize, text: &str, cy: usize, scale: usize, luma: u8) {
    let cell = 6 * scale;
    let tw = text.chars().count() * cell;
    let x0 = w.saturating_sub(tw) / 2;
    for (i, c) in text.chars().enumerate() {
        let g = glyph(c);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5 {
                if bits & (0x10 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    let y = cy + row * scale + dy;
                    if y >= h {
                        continue;
                    }
                    let xs = x0 + i * cell + col * scale;
                    let xe = (xs + scale).min(w);
                    if xs < xe {
                        yplane[y * w + xs..y * w + xe].fill(luma);
                    }
                }
            }
        }
    }
}

/// The still an app sees when "Relay Camera" is open but nothing is
/// feeding the ring: warm black, "RELAY CAMERA" and "WAITING FOR A STREAM"
/// centred. NV12, `w`×`h` (both even).
pub fn waiting_frame_nv12(w: u32, h: u32) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut out = vec![0x80u8; nv12_bytes(w, h)];
    let (yplane, _) = out.split_at_mut(wu * hu);
    yplane.fill(BG_Y);
    let big = (hu / 90).max(1);
    let small = (hu / 180).max(1);
    let block = 7 * big + 5 * small + 7 * small;
    let top = hu.saturating_sub(block) / 2;
    draw_text(yplane, wu, hu, WAITING_TITLE, top, big, FG_Y);
    draw_text(yplane, wu, hu, WAITING_LINE, top + 7 * big + 5 * small, small, DIM_Y);
    out
}

/// Asymmetric test pictures and a per-spec reader for every offered
/// format, shared by the unit tests and the in-process camera tests (r54:
/// the call saw the picture mirrored; these prove left stays left and top
/// stays top in each format).
#[doc(hidden)]
pub mod testpat {
    use super::*;

    /// Limited-range white.
    pub const WHITE_Y: u8 = 235;

    /// NV12 `w`×`h`: dark background; a bright block in the top-left
    /// corner (luma); a red block in the top-right corner (chroma, V high,
    /// U low); text-like vertical and horizontal 2 px strokes across the
    /// middle. Nothing is left/right or top/bottom symmetric.
    pub fn marker_nv12(w: u32, h: u32) -> Vec<u8> {
        let (wu, hu) = (w as usize, h as usize);
        let mut f = vec![BLACK_C; nv12_bytes(w, h)];
        let (y, uv) = f.split_at_mut(wu * hu);
        y.fill(BLACK_Y);
        let (bw, bh) = ((wu / 8).max(2) & !1, (hu / 8).max(2) & !1);
        for r in 0..bh {
            y[r * wu..r * wu + bw].fill(WHITE_Y);
        }
        // Red top-right: in chroma samples (2x2 subsampled).
        for r in 0..bh / 2 {
            for c in (wu - bw) / 2..wu / 2 {
                uv[r * wu + 2 * c] = 90; // U
                uv[r * wu + 2 * c + 1] = 240; // V
            }
        }
        // Text-like strokes in the middle band.
        for r in hu * 3 / 8..hu * 5 / 8 {
            for c in wu / 4..wu * 3 / 4 {
                let stroke = (c / 2) % 3 == 0 || (r / 2) % 5 == 0;
                if stroke {
                    y[r * wu + c] = WHITE_Y;
                }
            }
        }
        // Real glyphs with 1, 2 and 3 px strokes below it: UI text.
        let line = hu * 5 / 8 + 8;
        for (i, scale) in [1usize, 2, 3].iter().enumerate() {
            let top = line + i * 10 * scale;
            if top + 7 * scale < hu * 7 / 8 {
                draw_text(y, wu, hu, "RELAY CAMERA WAITING FOR A STREAM", top, *scale, WHITE_Y);
            }
        }
        f
    }

    /// Displayed (luma, red-ness) at picture coordinates (`x`, `y`) of a
    /// buffer in `fmt`, read the way the format's spec says to: NV12 and
    /// YUY2 top-down, RGB24 bottom-up BGR (positive `biHeight`). Red-ness
    /// is V-U for YUV and R-B for RGB.
    pub fn probe(fmt: PixFmt, buf: &[u8], w: u32, h: u32, x: u32, y: u32) -> (i32, i32) {
        let (wu, hu, x, y) = (w as usize, h as usize, x as usize, y as usize);
        match fmt {
            PixFmt::Nv12 => {
                let uv = &buf[wu * hu..];
                let c = (y / 2) * wu + (x & !1);
                (buf[y * wu + x] as i32, uv[c + 1] as i32 - uv[c] as i32)
            }
            PixFmt::Yuy2 => {
                let row = &buf[y * wu * 2..];
                let pair = (x / 2) * 4;
                (row[x * 2] as i32, row[pair + 3] as i32 - row[pair + 1] as i32)
            }
            PixFmt::Rgb24 => {
                let stride = PixFmt::Rgb24.stride(w);
                let row = &buf[(hu - 1 - y) * stride..];
                let (b, g, r) = (row[x * 3] as i32, row[x * 3 + 1] as i32, row[x * 3 + 2] as i32);
                ((r + g + b) / 3, r - b)
            }
        }
    }

    /// Assert the marker picture reads the right way round in `fmt`.
    pub fn assert_oriented(fmt: PixFmt, buf: &[u8], w: u32, h: u32, what: &str) {
        let (x0, x1, y0, y1) = (w / 32, w - 1 - w / 32, h / 32, h - 1 - h / 32);
        let tl = probe(fmt, buf, w, h, x0, y0);
        let tr = probe(fmt, buf, w, h, x1, y0);
        let bl = probe(fmt, buf, w, h, x0, y1);
        let br = probe(fmt, buf, w, h, x1, y1);
        assert!(tl.0 > 150, "{what} {fmt:?}: bright marker must be top-left: TL {tl:?}");
        assert!(
            tr.0 < 120 && bl.0 < 100 && br.0 < 100,
            "{what} {fmt:?}: not mirrored or flipped: TR {tr:?} BL {bl:?} BR {br:?}"
        );
        assert!(tr.1 > 60, "{what} {fmt:?}: red marker must be top-right: TR {tr:?}");
        assert!(
            tl.1.abs() < 40 && bl.1.abs() < 40,
            "{what} {fmt:?}: red only top-right: TL {tl:?} BL {bl:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes() {
        assert_eq!(PixFmt::Nv12.frame_bytes(1920, 1080), 1920 * 1080 * 3 / 2);
        assert_eq!(PixFmt::Yuy2.frame_bytes(1920, 1080), 1920 * 1080 * 2);
        // 3 * 2 = 6 → padded to 8 per row.
        assert_eq!(PixFmt::Rgb24.stride(2), 8);
        assert_eq!(PixFmt::Rgb24.frame_bytes(2, 2), 16);
    }

    #[test]
    fn yuy2_interleaves_y_and_shared_chroma() {
        // 2×2 NV12: Y = 1 2 / 3 4, UV = (10, 20).
        let src = [1, 2, 3, 4, 10, 20];
        let mut dst = [0u8; 8];
        convert_nv12(PixFmt::Yuy2, &src, 2, 2, &mut dst);
        assert_eq!(dst, [1, 10, 2, 20, 3, 10, 4, 20]);
    }

    #[test]
    fn rgb24_is_bottom_up_bgr_with_limited_range() {
        // Top row white (Y 235), bottom row black (Y 16), neutral chroma.
        let src = [235, 235, 16, 16, 128, 128];
        let mut dst = [0u8; 16];
        convert_nv12(PixFmt::Rgb24, &src, 2, 2, &mut dst);
        // Memory row 0 is the bottom (black) row.
        assert_eq!(&dst[..6], &[0, 0, 0, 0, 0, 0]);
        assert_eq!(&dst[8..14], &[255, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn saturated_red_converts_to_red() {
        // BT.709 limited red: Y 63, U 102, V 240.
        let (b, g, r) = yuv_to_bgr(63, 102, 240);
        assert!(r > 240 && g < 15 && b < 15, "{r} {g} {b}");
    }

    #[test]
    fn scale_identity_and_halving() {
        let (w, h) = (4u32, 4u32);
        let src: Vec<u8> = (0..nv12_bytes(w, h) as u8).collect();
        let mut same = vec![0; src.len()];
        scale_nv12(&src, w, h, &mut same, w, h);
        assert_eq!(same, src);
        let mut half = vec![0; nv12_bytes(2, 2)];
        scale_nv12(&src, w, h, &mut half, 2, 2);
        // Area means: (0+1+4+5)/4 = 2.5 → 3, and so on; chroma averages
        // its own channel only (U with U, V with V).
        assert_eq!(&half[..4], &[3, 5, 11, 13]);
        assert_eq!(&half[4..], &[19, 20]);
    }

    #[test]
    fn flat_stays_exactly_flat_at_every_ratio() {
        let src = {
            let mut f = vec![77u8; nv12_bytes(2560, 1440)];
            f[2560 * 1440..].fill(140);
            f
        };
        for (dw, dh) in [(1920, 1080), (1280, 720), (640, 360), (3840, 2160), (2048, 1152)] {
            let mut out = vec![0u8; nv12_bytes(dw, dh)];
            scale_nv12(&src, 2560, 1440, &mut out, dw, dh);
            let n = (dw * dh) as usize;
            assert!(out[..n].iter().all(|&v| v == 77), "{dw}x{dh} luma");
            assert!(out[n..].iter().all(|&v| v == 140), "{dw}x{dh} chroma");
        }
    }

    #[test]
    fn another_aspect_is_fitted_with_black_bars_not_stretched() {
        // 4:3 into 16:9: pillarboxed.
        let mut src = vec![200u8; nv12_bytes(640, 480)];
        src[640 * 480..].fill(128);
        let mut out = vec![0u8; nv12_bytes(1280, 720)];
        scale_nv12(&src, 640, 480, &mut out, 1280, 720);
        assert_eq!(fit_rect((640, 480), (1280, 720)), (160, 0, 960, 720));
        assert_eq!(out[10], BLACK_Y, "left bar");
        assert_eq!(out[1279], BLACK_Y, "right bar");
        assert_eq!(out[640], 200, "picture in the middle");
        assert_eq!(fit_rect((2560, 1440), (1920, 1080)), (0, 0, 1920, 1080));
    }

    #[test]
    fn scaling_keeps_left_left_and_top_top() {
        let src = testpat::marker_nv12(2560, 1440);
        testpat::assert_oriented(PixFmt::Nv12, &src, 2560, 1440, "source");
        for (dw, dh) in [(1920, 1080), (1280, 720), (640, 360), (3840, 2160)] {
            let mut out = vec![0u8; nv12_bytes(dw, dh)];
            scale_nv12(&src, 2560, 1440, &mut out, dw, dh);
            for fmt in PixFmt::ALL {
                let mut conv = vec![0u8; fmt.frame_bytes(dw, dh)];
                convert_nv12(fmt, &out, dw, dh, &mut conv);
                testpat::assert_oriented(fmt, &conv, dw, dh, &format!("{dw}x{dh}"));
            }
        }
    }

    #[test]
    fn every_format_is_oriented_per_its_spec() {
        let (w, h) = (320, 180);
        let src = testpat::marker_nv12(w, h);
        for fmt in PixFmt::ALL {
            let mut out = vec![0u8; fmt.frame_bytes(w, h)];
            convert_nv12(fmt, &src, w, h, &mut out);
            testpat::assert_oriented(fmt, &out, w, h, "native");
        }
    }

    // ---- quality: area filter vs the old nearest-neighbour baseline ----

    /// The exact area average of a Y plane over each destination pixel's
    /// footprint, in floating point: the ideal a pixel image downscales to.
    /// Independent of the fixed-point code under test.
    fn ideal_area(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f64> {
        let (fx, fy) = (sw as f64 / dw as f64, sh as f64 / dh as f64);
        let cover = |o: usize, f: f64, n: usize| -> Vec<(usize, f64)> {
            let (lo, hi) = (o as f64 * f, (o + 1) as f64 * f);
            (lo.floor() as usize..(hi.ceil() as usize).min(n))
                .map(|i| (i, hi.min(i as f64 + 1.0) - lo.max(i as f64)))
                .filter(|t| t.1 > 0.0)
                .collect()
        };
        let mut out = vec![0.0; dw * dh];
        for y in 0..dh {
            let ys = cover(y, fy, sh);
            for x in 0..dw {
                let xs = cover(x, fx, sw);
                let (mut a, mut wsum) = (0.0, 0.0);
                for &(yi, wy) in &ys {
                    for &(xi, wx) in &xs {
                        a += wx * wy * src[yi * sw + xi] as f64;
                        wsum += wx * wy;
                    }
                }
                out[y * dw + x] = a / wsum;
            }
        }
        out
    }

    fn rmse(a: &[u8], ideal: &[f64]) -> f64 {
        (a.iter().zip(ideal).map(|(&v, &r)| (v as f64 - r).powi(2)).sum::<f64>()
            / ideal.len() as f64)
            .sqrt()
    }

    fn stddev(a: &[u8]) -> f64 {
        let m = a.iter().map(|&v| v as f64).sum::<f64>() / a.len() as f64;
        (a.iter().map(|&v| (v as f64 - m).powi(2)).sum::<f64>() / a.len() as f64).sqrt()
    }

    /// Michelson contrast of the luma above black (so 16 reads as zero).
    fn contrast(a: &[u8]) -> f64 {
        let (lo, hi) = a.iter().fold((255u8, 0u8), |(l, h), &v| (l.min(v), h.max(v)));
        let (lo, hi) = (lo.saturating_sub(16) as f64, hi.saturating_sub(16) as f64);
        if hi + lo == 0.0 {
            0.0
        } else {
            (hi - lo) / (hi + lo)
        }
    }

    /// NV12 with `y_of(x, y)` as luma and neutral chroma.
    fn luma_frame(w: usize, h: usize, y_of: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let mut f = vec![128u8; w * h * 3 / 2];
        for y in 0..h {
            for x in 0..w {
                f[y * w + x] = y_of(x, y);
            }
        }
        f
    }

    /// Text at 2560x1440: the waiting still's 5x7 glyphs with 2 px and
    /// 3 px strokes, the thin UI text a shared screen is full of.
    fn text_frame() -> Vec<u8> {
        let (w, h) = (2560usize, 1440usize);
        let mut f = vec![128u8; w * h * 3 / 2];
        let y = &mut f[..w * h];
        y.fill(BLACK_Y);
        for (i, scale) in [2usize, 3, 2, 3].iter().enumerate() {
            draw_text(y, w, h, "RELAY CAMERA WAITING FOR A STREAM", 200 + i * 300, *scale, 235);
        }
        f
    }

    fn both(src: &[u8], dw: u32, dh: u32) -> (Vec<u8>, Vec<u8>) {
        let mut area = vec![0u8; nv12_bytes(dw, dh)];
        let mut near = vec![0u8; nv12_bytes(dw, dh)];
        scale_nv12(src, 2560, 1440, &mut area, dw, dh);
        scale_nv12_nearest(src, 2560, 1440, &mut near, dw, dh);
        let n = (dw * dh) as usize;
        area.truncate(n);
        near.truncate(n);
        (area, near)
    }

    #[test]
    fn downscaled_text_is_closer_to_ideal_than_nearest_neighbour() {
        let src = text_frame();
        for (dw, dh) in [(1920u32, 1080u32), (1280, 720)] {
            let (area, near) = both(&src, dw, dh);
            let ideal = ideal_area(&src[..2560 * 1440], 2560, 1440, dw as usize, dh as usize);
            let (ea, en) = (rmse(&area, &ideal), rmse(&near, &ideal));
            eprintln!("{dw}x{dh} text RMSE vs ideal: area {ea:.2}, nearest {en:.2}");
            assert!(ea < 1.0, "{dw}x{dh}: area filter matches the ideal: {ea:.2}");
            assert!(ea * 4.0 < en, "{dw}x{dh}: text error area {ea:.2} vs nearest {en:.2}");
        }
    }

    #[test]
    fn above_nyquist_strokes_do_not_alias() {
        // 1 px on / 1 px off: finer than either target can show. The right
        // answer is an even grey; point sampling turns it into full-contrast
        // moire (1920) or a solid colour of the wrong level (1280).
        let src = luma_frame(2560, 1440, |x, _| if x % 2 == 0 { 235 } else { 16 });
        let mid = (235.0 + 16.0) / 2.0;
        for (dw, dh) in [(1920u32, 1080u32), (1280, 720)] {
            let (area, near) = both(&src, dw, dh);
            let mean = |a: &[u8]| a.iter().map(|&v| v as f64).sum::<f64>() / a.len() as f64;
            let (sa, sn) = (stddev(&area), stddev(&near));
            let (da, dn) = ((mean(&area) - mid).abs(), (mean(&near) - mid).abs());
            eprintln!("{dw}x{dh} 1px grating: area sd {sa:.1} mean err {da:.1}; nearest sd {sn:.1} mean err {dn:.1}");
            // Residual ripple from the box filter at 0.75x is real but small.
            assert!(sa * 2.0 < sn.max(1.0) || (dn > 50.0 && da < 2.0),
                "{dw}x{dh}: aliasing area sd {sa:.1} mean err {da:.1} vs nearest sd {sn:.1} mean err {dn:.1}");
            assert!(da < 2.0, "{dw}x{dh}: area keeps the mean level: {da:.1}");
        }
    }

    #[test]
    fn below_nyquist_strokes_keep_their_contrast() {
        // 4 px on / 4 px off (a bold stroke) survives the shrink.
        let src = luma_frame(2560, 1440, |x, _| if (x / 4) % 2 == 0 { 235 } else { 16 });
        for (dw, dh) in [(1920u32, 1080u32), (1280, 720)] {
            let (area, near) = both(&src, dw, dh);
            let row = &area[dw as usize * 10..dw as usize * 11];
            assert!(contrast(row) > 0.9, "{dw}x{dh}: contrast {:.2}", contrast(row));
            // And it is the ideal, where point sampling jitters stroke widths.
            let ideal = ideal_area(&src[..2560 * 1440], 2560, 1440, dw as usize, dh as usize);
            assert!(rmse(&area, &ideal) * 4.0 < rmse(&near, &ideal).max(1.0));
        }
    }

    #[test]
    fn waiting_frame_has_text_on_dark_background() {
        let (w, h) = (640u32, 360u32);
        let f = waiting_frame_nv12(w, h);
        assert_eq!(f.len(), nv12_bytes(w, h));
        let y = &f[..(w * h) as usize];
        let bright = y.iter().filter(|&&v| v == FG_Y).count();
        let dim = y.iter().filter(|&&v| v == DIM_Y).count();
        assert!(bright > 500 && dim > 200, "text drawn: {bright} {dim}");
        assert!(y.iter().filter(|&&v| v == BG_Y).count() > y.len() * 9 / 10);
        // Corners are background, chroma neutral.
        assert_eq!(y[0], BG_Y);
        assert!(f[(w * h) as usize..].iter().all(|&c| c == 0x80));
    }

    #[test]
    fn waiting_frame_survives_tiny_sizes() {
        let f = waiting_frame_nv12(16, 8);
        assert_eq!(f.len(), nv12_bytes(16, 8));
    }
}
