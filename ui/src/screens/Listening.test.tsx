/**
 * S41: "What are you listening on?" per output, the quick switch, and the
 * other-processing notices.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Profiles } from "./Profiles";
import { processingLine } from "./Listening";
import { activeListening, listeningKey, type EndpointInfo } from "../lib/ipc";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Profiles />);
  await settle();
  return h;
}

function output(name: string): ReturnType<typeof within> {
  const el = document.querySelector(`.listen[data-output="${name}"]`);
  if (!el) throw new Error(`no output block for ${name}`);
  return within(el as HTMLElement);
}

describe("listening devices", () => {
  it("asks per output and adds a headset from the library search", async () => {
    const h = await mount();
    expect(screen.getByText("What are you listening on?")).toBeInTheDocument();
    const dac = output("USB Audio 2.0");
    expect(dac.getByText(/Nothing listed/)).toBeInTheDocument();
    await h.user.type(dac.getByRole("searchbox"), "bless");
    await h.user.click(dac.getByRole("button", { name: "Add Moondrop Blessing 3" }));
    await settle();
    expect(tauri.lastCall("set_listening_devices")?.args).toEqual({
      endpoint: "ep:dac",
      devices: [{ kind: "headset", id: "blessing3" }],
    });
  });

  it("offers speakers as the no-correction entry", async () => {
    const h = await mount();
    const dac = output("USB Audio 2.0");
    await h.user.type(dac.getByRole("searchbox"), "speak");
    await h.user.click(dac.getByRole("button", { name: /Add Speakers \/ home theater/ }));
    await settle();
    expect(tauri.lastCall("set_listening_devices")?.args).toEqual({
      endpoint: "ep:dac",
      devices: [{ kind: "speakers" }],
    });
  });

  it("switches the active device and says correction waits for a pick", async () => {
    core.hardware.connected.listening = [{
      endpoint: "ep:dac",
      devices: [{ kind: "headset", id: "hd560s" }, { kind: "headset", id: "blessing3" }],
      active: null,
    }];
    const h = await mount();
    const dac = output("USB Audio 2.0");
    expect(dac.getByText(/Until then Relay applies no headphone correction/)).toBeInTheDocument();
    await h.user.click(dac.getByRole("button", { name: "Use Moondrop Blessing 3" }));
    await settle();
    expect(tauri.lastCall("set_active_listening")?.args).toEqual({
      endpoint: "ep:dac",
      device: { kind: "headset", id: "blessing3" },
    });
    expect(output("USB Audio 2.0").getByText("In use")).toBeInTheDocument();
  });

  it("removes one entry and keeps the rest", async () => {
    core.hardware.connected.listening = [{
      endpoint: "ep:dac",
      devices: [{ kind: "headset", id: "hd560s" }, { kind: "speakers" }],
      active: { kind: "speakers" },
    }];
    const h = await mount();
    await h.user.click(output("USB Audio 2.0").getByRole("button", { name: "Remove HD 560S from USB Audio 2.0" }));
    await settle();
    expect(tauri.lastCall("set_listening_devices")?.args).toEqual({
      endpoint: "ep:dac",
      devices: [{ kind: "speakers" }],
    });
  });

  it("names other processing on the output and never offers to change it", async () => {
    core.hardware.connected.other_processing = [{
      endpoint: "ep:dac",
      processors: [
        { name: "SteelSeries Sonar", kind: "software", advice: "set Sonar's EQ flat" },
        { name: "Nahimic APO", kind: "apo", advice: "turn its effects off or set them flat in its own app for accurate correction", clsid: "{x}" },
      ],
    }];
    await mount();
    const dac = output("USB Audio 2.0");
    expect(dac.getByText(/SteelSeries Sonar is running and may also be processing this output\. Relay's correction adds to it; set Sonar's EQ flat for accurate correction\./)).toBeInTheDocument();
    expect(dac.getByText(/Nahimic APO is also processing this output\./)).toBeInTheDocument();
    expect(dac.queryByRole("button", { name: /disable|turn off/i })).toBeNull();
    expect(screen.getByText(/RODECaster's own EQ/)).toBeInTheDocument();
    expect(screen.getByText(/changes nothing in Windows/)).toBeInTheDocument();
  });
});

describe("listening helpers mirror the core", () => {
  const ep = (key: string, name: string): EndpointInfo => ({ key, name, default: false });

  it("splits endpoints that share a container key by name", () => {
    const all = [ep("ep:c:rode", "System"), ep("ep:c:rode", "Chat"), ep("ep:c:dac", "DAC")];
    expect(listeningKey(all, all[0])).toBe("ep:c:rode#System");
    expect(listeningKey(all, all[2])).toBe("ep:c:dac");
  });

  it("one entry is active by itself; several need a listed pick", () => {
    const hd = { kind: "headset", id: "hd560s" } as const;
    expect(activeListening({ endpoint: "e", devices: [hd] })).toEqual(hd);
    expect(activeListening({ endpoint: "e", devices: [hd, { kind: "speakers" }], active: null })).toBeNull();
    expect(activeListening({ endpoint: "e", devices: [hd, { kind: "speakers" }], active: { kind: "speakers" } }))
      .toEqual({ kind: "speakers" });
  });

  it("words software findings as possible and APO findings as certain", () => {
    expect(processingLine({ name: "G HUB", kind: "software", advice: "set G HUB's EQ flat" })).toMatch(/may also be processing/);
    expect(processingLine({ name: "X", kind: "apo", advice: "turn it off" })).toMatch(/^X is also processing this output/);
  });
});
