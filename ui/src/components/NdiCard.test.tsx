/**
 * S51: the NDI® output card on Receive and Share. Off by default, one saved
 * switch per side, honest about a missing runtime, and carrying the link and
 * trademark line NDI's licence asks for.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { push, renderScreen, settle } from "../test/render";
import { card, inCard, kv } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Receive } from "../screens/Receive";
import { Share } from "../screens/Share";
import { Settings } from "../screens/Settings";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount(ui: React.ReactElement) {
  const h = renderScreen(ui);
  await settle();
  return h;
}

const ndiSwitch = (name: RegExp) => inCard("NDI® output").getByRole("switch", { name });

describe("Receive", () => {
  it("is off by default and says the runtime is missing, with NDI's own link", async () => {
    const h = await mount(<Receive />);
    const sw = ndiSwitch(/Publish as an NDI source/);
    expect(sw).toHaveAttribute("aria-checked", "false");
    expect(screen.getByTestId("ndi-runtime-missing")).toHaveTextContent(
      "NDI output needs the NDI runtime",
    );
    await h.user.click(inCard("NDI® output").getByRole("button", { name: "Get the NDI runtime" }));
    await settle();
    expect(core.ndiOpened).toEqual(["runtime"]);
    // The licence's two asks, next to the switch.
    expect(card("NDI® output")).toHaveTextContent("NDI® is a registered trademark of Vizrt NDI AB.");
    await h.user.click(inCard("NDI® output").getByRole("button", { name: "ndi.video" }));
    await settle();
    expect(core.ndiOpened).toEqual(["runtime", "ndi"]);
    h.expectClean();
  });

  it("saves the Receive setting, and only that one", async () => {
    const h = await mount(<Receive />);
    await h.user.click(ndiSwitch(/Publish as an NDI source/));
    await settle();
    expect(core.prefs.ndi_receive).toBe(true);
    expect(core.prefs.ndi_share).toBe(false);
    expect(ndiSwitch(/Publish as an NDI source/)).toHaveAttribute("aria-checked", "true");
    expect(card("NDI® output")).toHaveTextContent('Published on this network as "Relay (from the sending PC)"');
    h.expectClean();
  });

  it("does not write back a stale copy of the other settings", async () => {
    const h = await mount(<Receive />);
    // A device pick lands in the core after the card loaded its copy.
    core.prefs.audio_devices = { receive_output: "{hdmi}" };
    await h.user.click(ndiSwitch(/Publish as an NDI source/));
    await settle();
    expect(core.prefs.ndi_receive).toBe(true);
    expect(core.prefs.audio_devices).toEqual({ receive_output: "{hdmi}" });
    h.expectClean();
  });

  it("shows the live source and its receivers while receiving", async () => {
    core.ndi = { ...core.ndi, present: true, path: "C:\\NDI\\Processing.NDI.Lib.x64.dll" };
    core.prefs.ndi_receive = true;
    const h = await mount(<Receive />);
    expect(screen.queryByTestId("ndi-runtime-missing")).not.toBeInTheDocument();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254", sender: "studio-pc" }));
    await push(() => tauri.emit("core://share-stats", {
      event: "stats", aus: 10, presented: 10,
      ndi: { on: true, name: "Relay (from studio-pc)", connections: 2,
        video: { sent: 9, dropped: 0 }, audio: { sent: 30, dropped: 0 } },
    }));
    const c = card("NDI® output");
    expect(kv("Source", c)).toBe("Relay (from studio-pc)");
    expect(kv("NDI receivers", c)).toBe("2");
    h.expectClean();
  });

  it("believes the engine when it could not load the runtime", async () => {
    // The file check found a DLL, but the engine could not use it.
    core.ndi = { ...core.ndi, present: true, path: "C:\\NDI\\Processing.NDI.Lib.x64.dll" };
    core.prefs.ndi_receive = true;
    const h = await mount(<Receive />);
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, sender: "studio-pc" }));
    await push(() => tauri.emit("core://share-stats", {
      event: "stats", aus: 1, presented: 1,
      ndi: { on: false, name: "Relay (from studio-pc)", error: "the NDI runtime at C:\\NDI would not load: bad image", runtime_missing: false },
    }));
    expect(card("NDI® output")).toHaveTextContent("would not load");
    expect(screen.queryByTestId("ndi-runtime-missing")).not.toBeInTheDocument();
    h.expectClean();
  });
});

describe("Share", () => {
  it("has its own switch, saved separately from Receive's", async () => {
    const h = await mount(<Share />);
    const sw = ndiSwitch(/Publish my share as NDI too/);
    expect(sw).toHaveAttribute("aria-checked", "false");
    await h.user.click(sw);
    await settle();
    expect(core.prefs.ndi_share).toBe(true);
    expect(core.prefs.ndi_receive).toBe(false);
    expect(card("NDI® output")).toHaveTextContent('"Relay share"');
    h.expectClean();
  });
});

describe("Settings", () => {
  it("carries the trademark line in the About card", async () => {
    const h = await mount(<Settings />);
    expect(screen.getByTestId("ndi-attribution")).toHaveTextContent(
      "NDI® is a registered trademark of Vizrt NDI AB.",
    );
    h.expectClean();
  });
});
