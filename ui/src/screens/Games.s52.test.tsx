/**
 * S52: per-game display mode, HDR, NVIDIA output colour and the monitor's
 * colour preset, each beside its recommendation. What the monitor offers
 * comes from the core (`display_options`); a recommendation is applied only
 * by the user's Use click, and only reaches the PC through Save + focus.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card, kv } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Games } from "./Games";
import { MODE_BLANK_NOTE, recText } from "../components/DisplayOutputCard";
import type { Profile } from "../lib/ipc";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Games section="display" onSection={() => {}} />);
  await settle();
  return h;
}

const out = () => card("Output & HDR");
const saved = (): Profile => {
  const p = core.profiles.get("1");
  if (!p) throw new Error("no saved profile");
  return p;
};
const save = async (h: Awaited<ReturnType<typeof mount>>) => {
  await h.user.click(screen.getByRole("button", { name: "Save to profile" }));
  await settle();
};

describe("the output & HDR card", () => {
  it("reads what the monitor offers, read-only, and shows it as it is now", async () => {
    await mount();
    expect(tauri.lastCall("display_options")).toBeDefined();
    const c = out();
    expect(c).toHaveTextContent("now 2560×1440 @ 240 Hz");
    expect(c).toHaveTextContent("HDR · now on");
    expect(within(c).getByRole("combobox", { name: "Display mode" })).toHaveValue("");
    expect(within(c).getAllByRole("option").map((o) => o.textContent)).toContain("3840×2160 @ 240 Hz");
    expect(within(c).getByRole("combobox", { name: "NVIDIA dynamic range" })).toHaveDisplayValue("Leave (Full)");
    // The model guess is said to be a guess.
    expect(within(c).getByTestId("panel-advice")).toHaveTextContent(/OLED \(guessed from the model/);
    // Nothing was written: reading options is not an apply.
    expect(tauri.lastCall("save_profile")).toBeUndefined();
  });

  it("saves a chosen mode, HDR off, NVIDIA range and colour preset to the profile", async () => {
    const h = await mount();
    const c = out();
    await h.user.selectOptions(within(c).getByRole("combobox", { name: "Display mode" }), "3840x2160@240");
    expect(within(c).getByTestId("mode-blank")).toHaveTextContent(MODE_BLANK_NOTE);
    await h.user.selectOptions(within(c).getByRole("combobox", { name: "HDR" }), "off");
    await h.user.selectOptions(within(c).getByRole("combobox", { name: "NVIDIA dynamic range" }), "limited");
    await h.user.selectOptions(within(c).getByRole("combobox", { name: "NVIDIA colour depth" }), "10");
    await h.user.selectOptions(within(c).getByRole("combobox", { name: "Monitor colour preset" }), "5");
    await save(h);
    const d = saved().display;
    expect(d.mode).toEqual({ width: 3840, height: 2160, refresh_hz: 240 });
    expect(d.hdr).toBe(false);
    expect(d.nvidia_color).toEqual({ range: "limited", bpc: 10 });
    expect(d.monitor.color_preset).toBe(5);
  });

  it("puts a recommendation in only on Use, and then says it is in use", async () => {
    const h = await mount();
    const rec = within(out()).getByTestId("rec-nvidia_color.bpc");
    expect(rec).toHaveTextContent("Recommended: 10-bit — The panel takes 10-bit.");
    await h.user.click(within(rec).getByRole("button", { name: /Use the recommended bpc: 10-bit/ }));
    expect(within(out()).getByTestId("rec-nvidia_color.bpc")).toHaveTextContent("In use");
    const mode = within(out()).getByTestId("rec-mode");
    await h.user.click(within(mode).getByRole("button", { name: /^Use/ }));
    await save(h);
    expect(saved().display.nvidia_color?.bpc).toBe(10);
    expect(saved().display.mode).toEqual({ width: 3840, height: 2160, refresh_hz: 240 });
    // Advice that was not used was not applied.
    expect(saved().display.hdr).toBeUndefined();
  });

  it("Leave as it is clears a field, and Reset clears them all", async () => {
    core.profiles.get("1")!.display.hdr = true;
    core.profiles.get("1")!.display.mode = { width: 2560, height: 1440, refresh_hz: 240 };
    const h = await mount();
    await h.user.selectOptions(within(out()).getByRole("combobox", { name: "HDR" }), "");
    await save(h);
    expect(saved().display.hdr).toBeUndefined();
    expect(saved().display.mode).toBeDefined();
    await h.user.click(within(out()).getByRole("button", { name: "Reset" }));
    await save(h);
    expect(saved().display.mode).toBeUndefined();
  });

  it("offers no NVIDIA colour on another GPU and no HDR where Windows has none", async () => {
    core.displayOptions = {
      ...core.displayOptions, vendor: "amd", nvidia_color: undefined, hdr_supported: false,
      hdr: { supported: false, enabled: false },
      recommendations: core.displayOptions.recommendations.filter((r) => !r.field.startsWith("nvidia") && r.field !== "hdr"),
    };
    await mount();
    const c = out();
    expect(within(c).queryByRole("combobox", { name: "NVIDIA dynamic range" })).toBeNull();
    expect(within(c).getByTestId("nv-unavailable")).toHaveTextContent(/not on an NVIDIA GPU/);
    expect(within(c).getByRole("combobox", { name: "HDR" })).toBeDisabled();
    expect(c).toHaveTextContent("Windows does not offer HDR on this monitor.");
  });

  it("says the options are unavailable when the core cannot read them", async () => {
    core.fail.set("display_options", "that monitor is not connected");
    await mount();
    expect(out()).toHaveTextContent("Display options are not available: that monitor is not connected");
  });

  it("names Windows display settings in Applied via", async () => {
    core.state.display_state = "applied";
    core.state.display_via = { nvapi: true, amd: false, gamma: false, ddcci: true, windows: true };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(kv("Applied via")).toBe("Windows display settings + NvAPI + DDC/CI");
  });

  it("words each recommendation in its control's own terms", () => {
    expect(recText({ field: "mode", value: { width: 2560, height: 1440, refresh_hz: 165 }, reason: "" })).toBe("2560×1440 @ 165 Hz");
    expect(recText({ field: "hdr", value: false, reason: "" })).toBe("Off");
    expect(recText({ field: "nvidia_color.range", value: "full", reason: "" })).toBe("Full");
    expect(recText({ field: "color_preset", value: 0x05, reason: "" })).toBe("6500 K");
    expect(recText({ field: "shadow_lift", value: 10, reason: "" })).toBe("+10");
  });
});
