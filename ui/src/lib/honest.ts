/**
 * The numbers behind the three places the UI used to draw invented content:
 * the EQ graph, the Display A/B and the Share overlay. Pure functions, so a
 * test can pin each against the Rust it mirrors.
 */
import type { EqBand, GpuColor, ShareCapabilities, SharePresetDef } from "./ipc";

/* ---------- EQ ---------- */

/** Graph rate. The APO runs at the endpoint's rate, which the UI does not
 *  know; below 20 kHz a peaking band's response at 44.1, 48 or 96 kHz differs
 *  by well under a pixel, so one rate is honest for drawing. */
const FS = 48000;

/** Magnitude in dB of one RBJ peaking biquad at `hz`. The same design as
 *  `relay_audio::coeffs` (`FilterKind::Peaking`). */
export function peakingDb(band: EqBand, hz: number, fs = FS): number {
  if (band.gain_db === 0 || band.freq_hz <= 0 || band.freq_hz >= fs / 2 || band.q <= 0) return 0;
  const a = 10 ** (band.gain_db / 40);
  const w0 = (2 * Math.PI * band.freq_hz) / fs;
  const alpha = Math.sin(w0) / (2 * band.q);
  const cw = Math.cos(w0);
  const b0 = 1 + alpha * a, b1 = -2 * cw, b2 = 1 - alpha * a;
  const a0 = 1 + alpha / a, a1 = -2 * cw, a2 = 1 - alpha / a;
  const w = (2 * Math.PI * hz) / fs;
  // |H(e^jw)|^2 for a biquad, expanded so no complex type is needed.
  const mag2 = (c0: number, c1: number, c2: number) =>
    c0 * c0 + c1 * c1 + c2 * c2 + 2 * (c0 * c1 + c1 * c2) * Math.cos(w) + 2 * c0 * c2 * Math.cos(2 * w);
  return 10 * Math.log10(mag2(b0, b1, b2) / mag2(a0, a1, a2));
}

/** The cascade's response: the bands' dB responses add. */
export function cascadeDb(bands: EqBand[], hz: number, fs = FS): number {
  return bands.reduce((sum, b) => sum + peakingDb(b, hz, fs), 0);
}

/* ---------- Display ---------- */

/** Port of `relay_display::gamma::build_ramp`, normalised to 0..1.
 *
 *  Gamma, contrast and shadow lift reach the monitor as exactly this curve, so
 *  a preview drawn through it is the real transform, not an impression of it.
 *  Vibrance and hue are not in it: they go through the GPU vendor's driver,
 *  whose maths Relay does not have, so nothing here pretends to show them. */
export function buildRamp(gpu: Pick<GpuColor, "gamma" | "contrast" | "shadow_lift">, size = 256): number[] {
  const gamma = Math.min(2, Math.max(0.5, gpu.gamma));
  const contrast = 1 + (Math.min(100, Math.max(-100, gpu.contrast)) / 100) * 0.5;
  const lift = (Math.min(100, Math.max(0, gpu.shadow_lift)) / 100) * 0.25;
  const clamp01 = (v: number) => Math.min(1, Math.max(0, v));
  return Array.from({ length: size }, (_, i) => {
    const x = i / (size - 1);
    let y = x ** (1 / gamma);
    y = clamp01(0.5 + (y - 0.5) * contrast);
    return clamp01(y + lift * (1 - y) ** 3);
  });
}

/* ---------- Share ---------- */

/** "NVENC", "Quick Sync" or "AMF", from the hardware HEVC encoders the probe
 *  found (falling back to the adapter names). `null` when there is none, or
 *  when two vendors could encode: the engine picks the one on the adapter that
 *  drives the captured display, which the UI cannot know in advance. */
export function encoderBrand(caps: ShareCapabilities | null): string | null {
  if (!caps) return null;
  const brandOf = (name: string): string | null => {
    const n = name.toLowerCase();
    if (n.includes("nvidia")) return "NVENC";
    if (n.includes("intel")) return "Quick Sync";
    if (n.includes("amd") || n.includes("radeon")) return "AMF";
    return null;
  };
  const source = caps.encoders.length ? caps.encoders : caps.adapters;
  const brands = new Set(source.map(brandOf));
  if (brands.size !== 1) return null;
  return [...brands][0];
}

/** The preview overlay's labels.
 *
 *  Idle, they describe the selected preset: what a share *would* send. While
 *  sharing, pass `measuredFps` (the engine's own `stats` figure, or null
 *  before the first one) and `def` only if it is the preset the running share
 *  was started with; a label that cannot be backed is left out, not guessed.
 *  HEVC is the one codec the engine has, so that one may be stated flat. */
export function shareTags(def: SharePresetDef | undefined, measuredFps?: number | null): string[] {
  const size = def && (def.size ? `Up to ${def.size[0]}×${def.size[1]}` : "Native size");
  const fps = measuredFps === undefined
    ? def && `${def.fps} fps`
    : measuredFps && measuredFps > 0 ? `${Math.round(measuredFps)} fps` : null;
  return [size, fps, "HEVC"].filter((t): t is string => !!t);
}
