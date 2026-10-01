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

/// Nearest-neighbour NV12 resize. Used only when the stream changed size
/// under a pin that already negotiated another one: the call keeps getting
/// a picture of the size it asked for instead of a black frame.
pub fn scale_nv12(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    debug_assert!(src.len() >= sw * sh * 3 / 2 && dst.len() >= dw * dh * 3 / 2);
    for y in 0..dh {
        let sy = y * sh / dh;
        let (srow, drow) = (&src[sy * sw..sy * sw + sw], &mut dst[y * dw..y * dw + dw]);
        for (x, d) in drow.iter_mut().enumerate() {
            *d = srow[x * sw / dw];
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
        assert_eq!(&half[..4], &[0, 2, 8, 10]);
        assert_eq!(&half[4..], &[16, 17]);
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
