/**
 * Games: the per-game audio and display editors.
 *
 * The thing worth pinning here is the shape that reaches `save_profile`. The
 * five sliders are a UI convenience over a free-form band list, and an
 * untouched profile must serialise to *no* bands at all — that is what makes
 * the core skip the exclusive-mode watcher and leave the chain in bypass.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, screen, within } from "@testing-library/react";
import { push, renderScreen, settle } from "../test/render";
import { card, inCard, kv, slider } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Games } from "./Games";
import type { Profile } from "../lib/ipc";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount(section: "audio" | "display" | "sharing" = "audio") {
  const h = renderScreen(<Games section={section} onSection={() => {}} />);
  await settle();
  return h;
}

/** Sliders are range inputs; `fireEvent.change` is how a controlled range is
 *  driven. Still a DOM event in this document — nothing reaches the host. */
function setSlider(label: string, value: number, scope?: HTMLElement) {
  const s = slider(label, scope);
  if (!s.input) throw new Error(`slider "${label}" is disabled`);
  fireEvent.change(s.input, { target: { value: String(value) } });
}

const saved = (): Profile => {
  const p = core.profiles.get("1");
  if (!p) throw new Error("profile 1 vanished");
  return p;
};

describe("the audio editor", () => {
  it("edits the first profile when no game is in focus", async () => {
    await mount();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Call of Duty — Audio");
    expect(screen.getByText("Not in focus")).toBeInTheDocument();
    expect(tauri.lastCall("get_profile")?.args).toEqual({ id: "1" });
  });

  it("follows the active profile when one is applied", async () => {
    core.state.active_profile = { id: "2", name: "Valorant", note: "", exe: "valorant.exe", headset: "hd560s", monitor: null, share: "game", status: "ready" };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Valorant — Audio");
    expect(screen.getByText("Active · in focus")).toBeInTheDocument();
  });

  it("writes one band per moved slider and nothing for the flat ones", async () => {
    const h = await mount();
    expect(within(card("Bands")).getByText("Sub · 60 Hz")).toBeInTheDocument();

    setSlider("Presence · 3 kHz", 4.5, card("Bands"));
    setSlider("Sub · 60 Hz", -2, card("Bands"));
    await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
    await settle();

    expect(saved().audio.bands).toEqual([
      { freq_hz: 60, gain_db: -2, q: 0.9 },
      { freq_hz: 3000, gain_db: 4.5, q: 0.9 },
    ]);
    h.expectClean();
  });

  it("drops a band returned to 0 dB rather than storing a flat filter", async () => {
    const h = await mount();
    setSlider("Mid · 1 kHz", 3, card("Bands"));
    setSlider("Mid · 1 kHz", 0, card("Bands"));
    await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
    await settle();
    expect(saved().audio.bands).toEqual([]);
  });

  it("Reset clears every band", async () => {
    const h = await mount();
    setSlider("Air · 10 kHz", 6, card("Bands"));
    await h.user.click(inCard("Bands").getByRole("button", { name: "Reset" }));
    await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
    await settle();
    expect(saved().audio.bands).toEqual([]);
    expect(slider("Air · 10 kHz", card("Bands")).value).toBe("0.0 dB");
  });

  it("stores the tuned limiter behind the explosion tamer, not a bare flag", async () => {
    const h = await mount();
    await h.user.click(screen.getByText("Explosion tamer"));
    await h.user.click(screen.getByText("Spatial audio"));
    await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
    await settle();

    expect(saved().audio.limiter).toEqual({ below_hz: 120, threshold_db: -10 });
    expect(saved().audio.hrtf).toBe(true);
  });

  it("only offers to save once something changed", async () => {
    const h = await mount();
    expect(screen.getByRole("button", { name: "Saved" })).toBeDisabled();
    await h.user.click(screen.getByText("Apply to share feed"));
    expect(screen.getByRole("button", { name: "Save to profile" })).toBeEnabled();
  });

  it("quotes the real chain latency for what is switched on", async () => {
    const h = await mount();
    expect(kv("Processing")).toBe("0 ms");
    core.state.audio_chain = "active";
    await push(() => tauri.emit("core://state", structuredClone(core.state)));
    expect(kv("Chain")).toBe("Active");
    expect(kv("Processing")).toBe("0.0 ms");

    await h.user.click(screen.getByText("Spatial audio"));
    expect(kv("Processing")).toBe("2.7 ms");
    await h.user.click(screen.getByText("Explosion tamer"));
    expect(kv("Processing")).toBe("3.7 ms");
  });

  it("says the profile is preview-only while the APO is not registered", async () => {
    await mount();
    expect(kv("Route")).toBe("Preview only · APO not installed");
  });

  it("warns when the game holds the endpoint in exclusive mode", async () => {
    core.state.audio_chain = "exclusivebypassed";
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByText(/This game opens the headset exclusively/)).toBeInTheDocument();
    expect(kv("Chain")).toBe("Bypassed by game (exclusive)");
  });

  it("names the headset whose curve is correcting, and how many points it has", async () => {
    await mount();
    expect(card("Headset correction")).toHaveTextContent(
      "HD 560S · 3 measured points, fitted to at most 8 filters ahead of your own bands.",
    );
  });

  it("will not let correction be switched on for a headset with no curve", async () => {
    core.profiles.get("1")!.headset = "blessing3";
    tauri.useFakeCore(core.handler);
    const h = await mount();
    expect(card("Headset correction")).toHaveTextContent(/has no measured curve yet/);
    await h.user.click(screen.getByText("Correct this headset's measured response"));
    expect(screen.getByRole("button", { name: "Saved" })).toBeDisabled();
  });

  it("renders the A/B pair through the core and reports the chain it used", async () => {
    const h = await mount();
    const ab = card("A/B listening test");
    expect(within(ab).getByRole("button", { name: "▶ Processed" })).toBeDisabled();

    await h.user.click(within(ab).getByRole("button", { name: "Render A/B" }));
    await settle();

    expect(tauri.lastCall("render_preview")?.args).toEqual({ id: "1", wav: null });
    expect(within(ab).getByText("48 kHz · EQ + limiter + HRTF")).toBeInTheDocument();
    expect(within(ab).getByRole("button", { name: "▶ Processed" })).toBeEnabled();
    h.expectClean();
  });

  it("explains a failed render instead of leaving the buttons dead", async () => {
    core.fail.set("render_preview", "no demo clip in the data folder");
    const h = await mount();
    await h.user.click(within(card("A/B listening test")).getByRole("button", { name: "Render A/B" }));
    await settle();
    expect(screen.getByText("no demo clip in the data folder")).toBeInTheDocument();
  });
});

describe("the display editor", () => {
  it("saves GPU colour to the profile", async () => {
    const h = await mount("display");
    setSlider("Vibrance", 72, card("GPU color"));
    setSlider("Gamma", 1.1, card("GPU color"));
    await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
    await settle();

    expect(saved().display.gpu).toMatchObject({ vibrance: 72, gamma: 1.1 });
    h.expectClean();
  });

  it("keeps hue in −180…180 on the way out and 0…360 on the wire", async () => {
    const h = await mount("display");
    setSlider("Hue", -30, card("GPU color"));
    await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
    await settle();
    expect(saved().display.gpu.hue_deg).toBe(330);
    expect(slider("Hue", card("GPU color")).value).toBe("-30°");
  });

  it("disables the DDC/CI controls this panel does not advertise", async () => {
    await mount("display");
    const mon = card("Monitor");
    expect(slider("Brightness", mon).disabled).toBe(false);
    expect(slider("Contrast", mon).disabled).toBe(false);
    // 0x87 is absent from the panel's capability string.
    expect(slider("Sharpness", mon).disabled).toBe(true);
    // Vendor-private codes stay off for every panel until one is verified.
    expect(slider("Black equalizer", mon).disabled).toBe(true);
    expect(slider("Response", mon).disabled).toBe(true);
    expect(mon).toHaveTextContent(/Relay will not guess at one/);
  });

  it("warns that vibrance clips on a wide-gamut panel", async () => {
    core.hardware.monitors[0].color = {
      digital: true,
      colorimetry: { bt2020_rgb: false, bt2020_ycc: false, bt2020_cycc: false, adobe_rgb: false, adobe_ycc: false, s_ycc601: false, xv_ycc709: false, xv_ycc601: false },
      hdr: { hdr10: true, hlg: false, hdr_gamma: false, dolby_vision: false, hdr10_plus: false, max_nits: 600 },
      coverage: { srgb: 1, dci_p3: 0.97, bt2020: 0.72 },
      bit_depth: 10,
    };
    tauri.useFakeCore(core.handler);
    const h = await mount("display");
    expect(card("GPU color")).toHaveTextContent("Panel reports: P3 97% · BT.2020 72% · HDR10 · 600 nits · 10-bit");
    expect(card("GPU color")).not.toHaveTextContent(/wide-gamut panel/);

    setSlider("Vibrance", 80, card("GPU color"));
    expect(card("GPU color")).toHaveTextContent(/This is a wide-gamut panel/);
    h.expectClean();
  });

  it("says the colour is open-loop when the panel reported no EDID data", async () => {
    await mount("display");
    expect(card("GPU color")).toHaveTextContent(/No EDID colour data from this monitor/);
  });

  it("reports which paths carried the apply, and what the hardware refused", async () => {
    core.state.display_state = "applied";
    core.state.display_via = { nvapi: true, gamma: true, ddcci: false, unsupported: ["black equalizer"] };
    tauri.useFakeCore(core.handler);
    await mount("display");
    expect(kv("Applied via")).toBe("NvAPI + gamma ramp");
    expect(kv("Not on this hardware")).toBe("black equalizer");
    expect(kv("In-game hooks")).toBe("None");
    expect(kv("Backup")).toBe("Saved before change");
  });
});

describe("the per-game share preset", () => {
  it("saves the chosen preset and reads its numbers from presets.json", async () => {
    const h = await mount("sharing");
    await h.user.click(inCard("Share preset for this game").getByRole("button", { name: "DAW" }));
    await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
    await settle();

    expect(saved().share).toBe("daw");
    expect(kv("Bitrate")).toBe("40 Mb/s");
    expect(kv("Size")).toBe("2560×1440");
    expect(kv("Audio")).toBe("System mix");
    h.expectClean();
  });

  it("says plainly that Off only means no automatic share", async () => {
    core.profiles.get("1")!.share = "off";
    tauri.useFakeCore(core.handler);
    await mount("sharing");
    expect(screen.getByText(/Sharing is off for this game/)).toBeInTheDocument();
  });
});
