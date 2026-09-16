/**
 * The maths behind the EQ graph, the Display A/B and the Share overlay.
 *
 * These used to be drawings. Each case here pins a number to its source, so a
 * later change cannot swap real data back for a hard-coded picture unnoticed.
 */
import { describe, expect, it } from "vitest";
import { buildRamp, cascadeDb, encoderBrand, peakingDb, shareTags } from "./honest";
import type { ShareCapabilities, SharePresetDef } from "./ipc";

describe("the EQ response", () => {
  it("peaks at exactly the band's gain at its centre, and is flat far away", () => {
    const band = { freq_hz: 3000, gain_db: 6, q: 0.9 };
    expect(peakingDb(band, 3000)).toBeCloseTo(6, 3);
    expect(Math.abs(peakingDb(band, 30))).toBeLessThan(0.05);
    expect(peakingDb({ ...band, gain_db: -4 }, 3000)).toBeCloseTo(-4, 3);
  });

  it("adds the bands of a cascade", () => {
    const a = { freq_hz: 60, gain_db: 3, q: 0.9 };
    const b = { freq_hz: 10000, gain_db: -2, q: 0.9 };
    expect(cascadeDb([a, b], 1000)).toBeCloseTo(peakingDb(a, 1000) + peakingDb(b, 1000), 9);
    expect(cascadeDb([], 1000)).toBe(0);
  });
});

describe("the gamma ramp port", () => {
  it("is the identity for neutral settings", () => {
    const r = buildRamp({ gamma: 1, contrast: 0, shadow_lift: 0 });
    r.forEach((v, i) => expect(v).toBeCloseTo(i / 255, 9));
  });

  it("matches the golden samples relay-display asserts for build_ramp", () => {
    // Same params and values as gamma.rs `ramp_golden_samples_shared_with_the_ui`.
    const r = buildRamp({ gamma: 1.2, contrast: 30, shadow_lift: 40 });
    const got = [0, 32, 64, 128, 192, 255].map((i) => Math.round(r[i] * 65535));
    const want = [6554, 12782, 21262, 38032, 54609, 65535];
    got.forEach((g, i) => expect(Math.abs(g - want[i])).toBeLessThanOrEqual(1));
  });
});

describe("the encoder name", () => {
  const caps = (encoders: string[], adapters: string[] = []): ShareCapabilities =>
    ({ can_share: true, can_receive: true, adapters, encoders, decoders: [] });

  it("names the vendor encoder the probe found", () => {
    expect(encoderBrand(caps(["NVIDIA HEVC Encoder MFT"]))).toBe("NVENC");
    expect(encoderBrand(caps(["Intel® Hardware H265 Encoder MFT"]))).toBe("Quick Sync");
    expect(encoderBrand(caps(["AMDh265Encoder"]))).toBe("AMF");
  });

  it("falls back to the adapter name when the encoder's is not telling", () => {
    expect(encoderBrand(caps([], ["AMD Radeon RX 7800 XT"]))).toBe("AMF");
  });

  it("does not guess when two vendors could encode, or when nothing is known", () => {
    expect(encoderBrand(caps(["NVIDIA HEVC Encoder MFT", "Intel® Hardware H265 Encoder MFT"]))).toBeNull();
    expect(encoderBrand(caps([], []))).toBeNull();
    expect(encoderBrand(null)).toBeNull();
  });
});

describe("the share overlay labels", () => {
  const def = (patch: Partial<SharePresetDef>): SharePresetDef => ({
    id: "x", name: "X", bitrate_mbps: 40, fps: 30, audio: { desktop: "system", mic: false },
    cursor: false, record: false, replay_secs: 0, container: "mp4", ...patch,
  });

  it("describes the selected preset while idle", () => {
    expect(shareTags(def({ size: [2560, 1440] }))).toEqual(["Up to 2560×1440", "30 fps", "HEVC"]);
    expect(shareTags(def({ fps: 120 }))).toEqual(["Native size", "120 fps", "HEVC"]);
  });

  it("shows the measured frame rate while sharing, and omits what is not known", () => {
    expect(shareTags(def({}), 29.6)).toEqual(["Native size", "30 fps", "HEVC"]);
    expect(shareTags(def({}), 0)).toEqual(["Native size", "HEVC"]);
    expect(shareTags(undefined, 59.9)).toEqual(["60 fps", "HEVC"]);
  });
});
