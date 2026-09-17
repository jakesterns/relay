/**
 * Share: the start/stop path and the preset editor behind it.
 *
 * Both were mock-only until recently — the buttons existed and drew the right
 * thing without ever reaching the core. These tests assert the command that
 * actually leaves the webview, with its arguments.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { push, renderScreen, settle } from "../test/render";
import { card, field, inCard, kv, readout } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Share } from "./Share";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

/** The core pushes state on every change; the provider has no other way to
 *  learn that a share started. */
const pushState = () => push(() => tauri.emit("core://state", structuredClone(core.state)));

async function mount() {
  const h = renderScreen(<Share />);
  await settle();
  return h;
}

/** Read-only it is titled "<Name> preset"; in edit mode "Edit <Name>". */
const presetCard = (): HTMLElement => {
  const h = [...document.querySelectorAll<HTMLElement>(".card h3")]
    .find((e) => /preset/i.test(e.textContent ?? "") || /^Edit /.test(e.textContent ?? ""));
  const el = h?.closest(".card");
  if (!el) throw new Error("no preset card");
  return el as HTMLElement;
};

const codeBox = () => screen.getByPlaceholderText(/6 digits/);

describe("starting a share", () => {
  it("refuses to start without the receiver's six-digit code", async () => {
    const h = await mount();
    const start = screen.getByRole("button", { name: "Start sharing" });
    expect(start).toBeDisabled();

    await h.user.type(codeBox(), "12345");
    expect(start).toBeDisabled();

    await h.user.type(codeBox(), "6");
    expect(start).toBeEnabled();
    expect(tauri.lastCall("start_share_preset")).toBeUndefined();
  });

  it("keeps only digits, so a code typed with separators still works", async () => {
    const h = await mount();
    await h.user.type(codeBox(), "12-34 56");
    expect(codeBox()).toHaveValue("123456");
    expect(screen.getByRole("button", { name: "Start sharing" })).toBeEnabled();
  });

  it("sends the chosen preset, the code and the picked receiver", async () => {
    const h = await mount();

    await h.user.click(screen.getByRole("button", { name: "Scan for receivers" }));
    await settle();
    expect(screen.getByText("192.168.1.42")).toBeInTheDocument();
    await h.user.click(screen.getByRole("radio", { name: /studio-pc/ }));

    await h.user.click(screen.getByRole("button", { name: "Desktop" }));
    await h.user.type(codeBox(), "445566");
    await h.user.click(screen.getByRole("button", { name: "Start sharing" }));
    await settle();

    expect(tauri.lastCall("start_share_preset")?.args).toEqual({
      preset: "desktop", code: "445566", peer: "studio-pc",
    });
    expect(core.state.sharing).toEqual({ kind: "sharing", peer: "studio-pc" });
    h.expectClean();
  });

  it("surfaces the core's refusal instead of pretending it started", async () => {
    core.fail.set("start_share_preset", "no hardware HEVC encoder on this GPU");
    const h = await mount();
    await h.user.type(codeBox(), "111111");
    await h.user.click(screen.getByRole("button", { name: "Start sharing" }));
    await settle();

    expect(screen.getByText(/no hardware HEVC encoder/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start sharing" })).toBeInTheDocument();
    expect(kv("Peer")).toBe("—");
  });

  it("swaps to Stop once the core reports it is sharing, and stops on click", async () => {
    const h = await mount();
    await h.user.type(codeBox(), "123456");
    await h.user.click(screen.getByRole("button", { name: "Start sharing" }));
    await settle();
    await pushState();

    expect(screen.getByText("Sharing")).toBeInTheDocument();
    expect(kv("Peer")).toBe("living-room-pc");
    expect(screen.queryByRole("button", { name: "Start sharing" })).not.toBeInTheDocument();

    await h.user.click(screen.getByRole("button", { name: "Stop sharing" }));
    await settle();
    await pushState();

    expect(tauri.lastCall("stop_share")).toBeDefined();
    expect(core.state.sharing).toEqual({ kind: "off" });
    expect(screen.getByRole("button", { name: "Start sharing" })).toBeInTheDocument();
    expect(screen.getByText("Not sharing")).toBeInTheDocument();
    expect(kv("Peer")).toBe("—");
    h.expectClean();
  });
});

describe("while sharing", () => {
  async function sharing() {
    const h = await mount();
    core.state.sharing = { kind: "sharing", peer: "living-room-pc" };
    await pushState();
    return h;
  }

  it("shows live instrument readings from the engine's stats", async () => {
    const h = await sharing();
    await push(() =>
      tauri.emit("core://share-stats", {
        event: "stats", bitrate_mbps: 58.4, capture_to_send_ms: 21.6,
        dropped: 3, frames: 4210, cpu_percent: 4.2, encode_ms: 6,
        recording: true, rec_mb: 812, replay_fill: 0.5,
      }),
    );

    expect(readout("Bitrate").value).toBe("58.4Mb/s");
    expect(readout("Latency").value).toBe("22ms");
    expect(readout("Dropped frames")).toEqual({ value: "3", hint: "of 4,210 sent" });
    expect(readout("Rec").value).toBe("812MB");
    expect(screen.getByRole("meter", { name: "Replay buffer" })).toHaveAttribute("aria-valuenow", "50");
    h.expectClean();
  });

  it("reads em-dashes, not zeroes, before the first stats line arrives", async () => {
    await mount();
    expect(readout("Bitrate").value).toBe("—Mb/s");
    expect(readout("Dropped frames")).toEqual({ value: "—", hint: "Nothing sent" });
  });

  it("warns when the recorder hit the disk floor", async () => {
    await sharing();
    await push(() =>
      tauri.emit("core://share-stats", {
        event: "stats", bitrate_mbps: 60, recording: true, rec_stopped_disk: true,
      }),
    );
    expect(readout("Rec").hint).toBe("Stopped — disk floor");
  });

  it("switches capture source and asks the core for the same target", async () => {
    const h = await sharing();
    await h.user.click(screen.getByRole("button", { name: "Region…" }));
    await h.user.click(screen.getByRole("button", { name: "Apply region" }));
    await settle();

    expect(tauri.lastCall("switch_source")?.args).toEqual({
      target: { kind: "region", display: 0, x: 0, y: 0, w: 1920, h: 1080 },
    });
    h.expectClean();
  });

  it("lists running windows only when the window picker is opened", async () => {
    const h = await sharing();
    expect(tauri.lastCall("list_processes")).toBeUndefined();
    await h.user.click(screen.getByRole("button", { name: "Window…" }));
    await settle();
    expect(screen.getByRole("option", { name: /cod\.exe/ })).toBeInTheDocument();
  });

  it("toggles recording and saves a replay through the core", async () => {
    const h = await sharing();
    await h.user.click(screen.getByRole("button", { name: "Record" }));
    await settle();
    expect(tauri.lastCall("record")?.args).toEqual({ on: true });

    await push(() => tauri.emit("core://recording-status", { on: true, path: "C:\\clips\\a.mp4" }));
    expect(screen.getByRole("button", { name: "Stop recording" })).toBeInTheDocument();

    await h.user.click(screen.getByRole("button", { name: /Save replay/ }));
    await settle();
    expect(tauri.lastCall("save_replay")).toBeDefined();

    await push(() => tauri.emit("core://replay-saved", { path: "C:\\clips\\replay.mp4", ms: 30000 }));
    expect(screen.getByText(/Replay saved · 30\.0 s/)).toBeInTheDocument();
    h.expectClean();
  });

  it("shows the engine's thumbnail once one arrives", async () => {
    await sharing();
    expect(screen.getByText("Waiting for the first frame…")).toBeInTheDocument();
    await push(() => tauri.emit("core://share-preview", { width: 3840, height: 2160, jpeg: "AAAA" }));
    expect(screen.getByAltText("What is being shared")).toHaveAttribute("src", "data:image/jpeg;base64,AAAA");
    expect(screen.getByText("Live · 3840×2160 thumbnail")).toBeInTheDocument();
  });

  it("locks the preset card, because the engine already read those numbers", async () => {
    await sharing();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.getByText("Stop sharing to change the preset.")).toBeInTheDocument();
  });
});

describe("the preset editor", () => {
  it("saves edited numbers to the core and reads them back", async () => {
    const h = await mount();
    expect(kv("Bitrate", presetCard())).toBe("60 Mb/s");

    await h.user.click(inCard(/preset/i).getByRole("button", { name: "Edit" }));
    const bitrate = field("Bitrate (Mb/s)", presetCard());
    await h.user.clear(bitrate);
    await h.user.type(bitrate, "35");
    await h.user.click(screen.getByRole("button", { name: "Save preset" }));
    await settle();

    expect(core.presets.find((p) => p.id === "game")?.bitrate_mbps).toBe(35);
    expect(kv("Bitrate", presetCard())).toBe("35 Mb/s");
    h.expectClean();
  });

  it("will not let a built-in preset be deleted, but a duplicate can be", async () => {
    const h = await mount();
    await h.user.click(inCard(/preset/i).getByRole("button", { name: "Edit" }));
    expect(screen.queryByRole("button", { name: "Delete this preset" })).not.toBeInTheDocument();
    expect(screen.getByText(/Built-in presets can be edited but not deleted/)).toBeInTheDocument();

    await h.user.click(screen.getByRole("button", { name: "Duplicate" }));
    await h.user.click(screen.getByRole("button", { name: "Save preset" }));
    await settle();

    const copy = core.presets.find((p) => p.name === "Game copy");
    expect(copy).toBeDefined();
    expect(copy?.id.startsWith("custom-")).toBe(true);
    expect(core.presets.find((p) => p.id === "game")).toBeDefined();

    await h.user.click(inCard(/preset/i).getByRole("button", { name: "Edit" }));
    await h.user.click(screen.getByRole("button", { name: "Delete this preset" }));
    await h.user.click(screen.getByRole("button", { name: "Confirm delete" }));
    await settle();

    expect(core.presets.some((p) => p.name === "Game copy")).toBe(false);
    expect(core.presets.map((p) => p.id)).toEqual(["game", "daw", "desktop"]);
    h.expectClean();
  });

  it("reads a blank encode size as native rather than 0×0", async () => {
    const h = await mount();
    await h.user.click(inCard(/preset/i).getByRole("button", { name: "Edit" }));
    await h.user.type(field("Encode size", presetCard()), "2560x1440");
    await h.user.click(screen.getByRole("button", { name: "Save preset" }));
    await settle();
    expect(core.presets.find((p) => p.id === "game")?.size).toEqual([2560, 1440]);
    expect(kv("Size", presetCard())).toBe("2560×1440");

    await h.user.click(inCard(/preset/i).getByRole("button", { name: "Edit" }));
    await h.user.clear(field("Encode size", presetCard()));
    await h.user.click(screen.getByRole("button", { name: "Save preset" }));
    await settle();
    expect(core.presets.find((p) => p.id === "game")?.size).toBeUndefined();
    expect(kv("Size", presetCard())).toBe("Native");
  });

  it("reports a core that refused the save, and keeps the draft open", async () => {
    core.fail.set("save_preset", "presets.json is read-only");
    const h = await mount();
    await h.user.click(inCard(/preset/i).getByRole("button", { name: "Edit" }));
    await h.user.click(screen.getByRole("button", { name: "Save preset" }));
    await settle();
    expect(screen.getByText("presets.json is read-only")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save preset" })).toBeInTheDocument();
  });
});

describe("codec capability", () => {
  it("says this PC cannot send before the user tries", async () => {
    core.capabilities = { can_share: false, can_receive: true, adapters: ["Intel(R) UHD Graphics 630"], encoders: [], decoders: ["x"] };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByText(/No hardware HEVC or H\.264 encoder on Intel\(R\) UHD Graphics 630/)).toBeInTheDocument();
  });

  it("stays quiet when the PC can send", async () => {
    await mount();
    expect(screen.queryByText(/No hardware HEVC or H\.264 encoder/)).not.toBeInTheDocument();
    expect(card(/Send to/)).toBeInTheDocument();
  });
});

/* The overlay used to read "Up to 3840×2160 / 60 fps / HEVC" and the load
 * hint "NVENC" on every PC with every preset. */
describe("the overlay and encoder labels", () => {
  const tags = () => [...screen.getByTestId("share-tags").querySelectorAll("span")].map((s) => s.textContent);

  it("describes the selected preset while idle", async () => {
    core.presets = core.presets.map((p) => (p.id === "daw" ? { ...p, fps: 30 } : p));
    const h = await mount();
    // No codec while idle: it is negotiated with the receiver at start.
    expect(tags()).toEqual(["Native size", "60 fps"]);
    await h.user.click(screen.getByRole("button", { name: "DAW" }));
    expect(tags()).toEqual(["Up to 2560×1440", "30 fps"]);
  });

  it("while sharing, shows the started preset, the measured rate and this PC's encoder", async () => {
    core.presets = core.presets.map((p) => (p.id === "daw" ? { ...p, fps: 30 } : p));
    core.capabilities = { ...core.capabilities, adapters: ["Intel(R) Arc(TM) A770"], encoders: ["Intel® Hardware H265 Encoder MFT"] };
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "DAW" }));
    await h.user.type(codeBox(), "123456");
    await h.user.click(screen.getByRole("button", { name: "Start sharing" }));
    await settle();
    await pushState();

    expect(tags()).toEqual(["Up to 2560×1440"]);
    await push(() =>
      tauri.emit("core://share-stats", { event: "stats", codec: "h264", bitrate_mbps: 38, fps: 29.8, encode_ms: 16.7, cpu_percent: 3.1 }),
    );
    expect(tags()).toEqual(["Up to 2560×1440", "30 fps", "H.264"]);
    // 16.7 ms against a 30 fps budget, not a 60 fps one.
    expect(readout("Load")).toEqual({ value: "50% enc", hint: "Quick Sync · CPU 3.1%" });

    // The engine read the preset at start; the chips cannot pretend otherwise.
    await h.user.click(screen.getByRole("button", { name: "Game" }));
    expect(tags()).toEqual(["Up to 2560×1440", "30 fps", "H.264"]);
  });

  it("does not name a preset or an encoder it cannot back", async () => {
    core.capabilities = { ...core.capabilities, encoders: ["NVIDIA HEVC Encoder MFT", "AMDh265Encoder"] };
    await mount();
    // A share this screen did not start: the core does not say which preset.
    core.state.sharing = { kind: "sharing", peer: "living-room-pc" };
    await pushState();
    await push(() => tauri.emit("core://share-stats", { event: "stats", codec: "hevc", bitrate_mbps: 50, fps: 60, cpu_percent: 2 }));
    expect(tags()).toEqual(["60 fps", "HEVC"]);
    expect(readout("Load").hint).toBe("Hardware encoder · CPU 2%");
  });
});
