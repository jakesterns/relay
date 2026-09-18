/**
 * Receive: the pairing code, and the "In calls" card that answers the one
 * question this screen exists for — will Discord actually see this stream.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { push, renderScreen, settle } from "../test/render";
import { card, kv } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Receive } from "./Receive";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Receive />);
  await settle();
  return h;
}

/** The digits, or `null` while the placeholder slots are showing. */
const code = () => {
  if (screen.queryByRole("img", { name: "No code yet" })) return null;
  return (document.querySelector(".paircode")?.textContent ?? "").trim();
};

describe("pairing", () => {
  it("shows no code until the core hands one out", async () => {
    const h = await mount();
    expect(code()).toBeNull();
    // Empty slots, not six em-dashes at 34 px, which read as an error.
    const placeholder = screen.getByRole("img", { name: "No code yet" });
    expect(placeholder.textContent).toBe("");
    expect(placeholder.children).toHaveLength(6);
    expect(kv("Status")).toBe("Idle");
    expect(kv("Codec")).toBe("—");
    h.expectClean();
  });

  it("starts receiving and shows the code the core generated", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await settle();
    expect(tauri.lastCall("start_receive")?.args).toEqual({ request: {} });

    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254" }));
    expect(code()).toBe("418254");
    expect(kv("Status")).toBe("Advertising on the LAN");
    expect(screen.getByText("Waiting for a sender to pair…")).toBeInTheDocument();
    h.expectClean();
  });

  it("names the sender once paired and waits for the first frame in the video area", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254", sender: "studio-pc" }));

    expect(kv("Status")).toBe("Paired with studio-pc");
    // Not claimed until the stream names it: the codec is negotiated.
    expect(kv("Codec")).toBe("—");
    await push(() => tauri.emit("core://receive-status", { receiving: true, codec: "h264" }));
    expect(kv("Codec")).toBe("H.264");
    expect(screen.getByText(/Connected to studio-pc — waiting for the first frame/)).toBeInTheDocument();
    // S29: the stream plays inside the app. Nothing sends the user to look
    // for another window.
    expect(screen.queryByText(/separate window/)).not.toBeInTheDocument();
    expect(screen.getByText(/The stream plays here, in this window\. Nothing on this PC is changed\./)).toBeInTheDocument();
    h.expectClean();
  });

  it("clears the code and sender when receiving stops", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254", sender: "studio-pc" }));

    await h.user.click(screen.getByRole("button", { name: "Stop receiving" }));
    await settle();
    expect(tauri.lastCall("stop_receive")).toBeDefined();

    await push(() => tauri.emit("core://receive-status", { receiving: false }));
    expect(code()).toBeNull();
    expect(kv("Status")).toBe("Idle");
  });

  it("says the share ended, in the video area, rather than snapping back to idle", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254", sender: "studio-pc" }));
    await push(() => tauri.emit("core://receive-status", { receiving: false }));
    expect(screen.getByText("The share from studio-pc ended.")).toBeInTheDocument();
    expect(kv("Status")).toBe("Idle");
    // Starting again clears it.
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await settle();
    expect(screen.queryByText(/ended\./)).not.toBeInTheDocument();
  });

  it("shows the core's message when a start fails", async () => {
    core.fail.set("start_receive", "another receiver already holds the port");
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await settle();
    expect(screen.getByText(/another receiver already holds the port/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start receiving" })).toBeInTheDocument();
  });
});

/** S29: the stream is a native window the shell keeps over the video area.
 *  The page's job is to say where that area is, offer the pop-out, and keep
 *  the box empty while the picture covers it. */
describe("the stream inside the app", () => {
  const live = (mode: "embedded" | "popout", excluded = true) => ({
    live: true, mode, width: 2560, height: 1440, excluded_from_capture: excluded,
  });

  it("reports the video area to the shell on mount and clears it on unmount", async () => {
    const h = await mount();
    // jsdom lays nothing out, so the box measures 0x0 and reads as "no area".
    expect(tauri.lastCall("set_video_area")?.args).toEqual({ area: null });
    h.unmount();
    await settle();
    expect(tauri.lastCall("set_video_area")?.args).toEqual({ area: null });
    expect(tauri.calls.filter((c) => c.cmd === "set_video_area").length).toBeGreaterThanOrEqual(2);
  });

  it("keeps the video area empty while the stream is embedded, and offers the pop-out", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, sender: "studio-pc" }));
    await push(() => tauri.emit("core://stream", live("embedded")));
    const area = screen.getByTestId("video-area");
    expect(area.dataset.stream).toBe("embedded");
    expect(area.querySelector(".idlemsg")).toBeNull();
    expect(kv("Stream")).toBe("2560×1440");

    await h.user.click(screen.getByRole("button", { name: "Pop out into its own window" }));
    await settle();
    expect(tauri.lastCall("set_stream_mode")?.args).toEqual({ mode: "popout" });
    // Nothing changes until the engine confirms.
    expect(area.dataset.stream).toBe("embedded");

    await push(() => tauri.emit("core://stream", live("popout")));
    expect(area.dataset.stream).toBe("popout");
    expect(screen.getByText(/Playing in its own window · studio-pc/)).toBeInTheDocument();
    await h.user.click(screen.getByRole("button", { name: "Bring back into Relay" }));
    await settle();
    expect(tauri.lastCall("set_stream_mode")?.args).toEqual({ mode: "embedded" });
    h.expectClean();
  });

  it("learns about a stream that was already playing when the screen opened", async () => {
    core.stream = { ...live("embedded"), receiving: true, code: "418254", sender: "studio-pc", codec: "h264" };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByTestId("video-area").dataset.stream).toBe("embedded");
    expect(screen.getByRole("button", { name: "Pop out into its own window" })).toBeInTheDocument();
    // And the rest of the screen agrees with the picture: this was the
    // "Idle beside a playing stream" state after Settings-and-back.
    expect(kv("Status")).toBe("Paired with studio-pc");
    expect(kv("Codec")).toBe("H.264");
    expect(code()).toBe("418254");
    expect(screen.getByRole("button", { name: "Stop receiving" })).toBeInTheDocument();
  });

  it("learns about a receive that is still waiting for a sender when the screen opened", async () => {
    core.stream = { live: false, mode: "none", width: 0, height: 0, excluded_from_capture: true, receiving: true, code: "990011" };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(kv("Status")).toBe("Advertising on the LAN");
    expect(code()).toBe("990011");
    expect(screen.getByText("Waiting for a sender to pair…")).toBeInTheDocument();
  });

  it("says so when Windows could not hide the stream from capture", async () => {
    const h = await mount();
    await push(() => tauri.emit("core://stream", live("embedded", false)));
    expect(screen.getByText(/could not hide the stream from screen capture/)).toBeInTheDocument();
    await push(() => tauri.emit("core://stream", live("embedded", true)));
    expect(screen.queryByText(/could not hide the stream/)).not.toBeInTheDocument();
    h.expectClean();
  });

  it("returns to the idle prompt when the stream and the receive end", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, sender: "studio-pc" }));
    await push(() => tauri.emit("core://stream", live("embedded")));
    await push(() => tauri.emit("core://stream", { live: false, mode: "none", width: 0, height: 0, excluded_from_capture: true }));
    await push(() => tauri.emit("core://receive-status", { receiving: false }));
    expect(screen.getByTestId("video-area").dataset.stream).toBe("none");
    expect(screen.getByText("The share from studio-pc ended.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Pop out/ })).not.toBeInTheDocument();
  });
});

describe("whether a call will see the stream", () => {
  it("points at Settings when the camera was never enabled", async () => {
    await mount();
    expect(kv("Camera", card("In calls"))).toBe("Not enabled — turn it on in Settings");
    expect(card("In calls")).toHaveTextContent(/it just\s+cannot be picked as a webcam/);
  });

  it("distinguishes consented-but-not-installed from installed", async () => {
    core.vdevice.consent = { decided_at: "x", apo: false, camera: true, microphone: true };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(kv("Camera", card("In calls"))).toBe("Consented, not installed yet — finish in Settings");
  });

  it("names the camera exactly as a call will list it", async () => {
    core.vdevice.camera_registered = true;
    tauri.useFakeCore(core.handler);
    await mount();
    expect(kv("Camera", card("In calls"))).toBe('"Relay Camera" — pick it in Discord, Zoom or Meet');
    expect(card("In calls")).not.toHaveTextContent(/cannot be picked as a webcam/);
  });

  it("says which Windows build is needed when the camera cannot exist here", async () => {
    core.vdevice = { ...core.vdevice, camera_supported: false, windows_build: 19045 };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(kv("Camera", card("In calls"))).toBe("Needs Windows 11 22H2+ (this PC: build 19045)");
  });

  it("says Relay does not feed an OBS camera that happens to be installed", async () => {
    core.vdevice = { ...core.vdevice, obs_virtualcam: "OBS Virtual Camera" };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(card("In calls")).toHaveTextContent(/OBS VirtualCam is installed on this PC, but Relay does not feed it/);
  });

  it("routes call audio through VB-Cable while the signed mic driver is missing", async () => {
    await mount();
    expect(kv("Microphone", card("In calls"))).toBe("CABLE Input (VB-Audio Virtual Cable)");
  });

  it("says there is no mic route at all when nothing suitable is installed", async () => {
    core.vdevice = { ...core.vdevice, mic_targets: [] };
    tauri.useFakeCore(core.handler);
    const h = await mount();
    expect(kv("Microphone", card("In calls"))).toBe(
      "No route yet — the signed driver ships later; VB-Cable works meanwhile",
    );
    h.expectClean();
  });
});

describe("codec capability", () => {
  /* B6: the Windows 10 test PC has no HEVC decoder and cannot install one for
   * free. Since S27 its shares work over H.264, so a red "cannot show" error
   * there would claim a working feature is broken. */
  it("notes H.264 without an error when this PC has no HEVC decoder", async () => {
    core.capabilities = {
      can_share: true, can_receive: true, adapters: ["NVIDIA GeForce RTX 2080"],
      encoders: ["NVIDIA HEVC Encoder MFT"], decoders: ["Microsoft H264 Video Decoder MFT"],
      share_codecs: ["hevc", "h264"], receive_codecs: ["h264"],
    };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByTestId("codec-note")).toHaveTextContent(/use H\.264, because it has no HEVC decoder/);
    expect(screen.queryByText(/cannot show a shared screen/)).not.toBeInTheDocument();
    // Relay is free: nothing points anyone at a paid codec.
    expect(screen.queryByRole("link", { name: /HEVC/ })).not.toBeInTheDocument();
    expect(document.querySelector(".offline")).toBeNull();
  });

  it("errors only when no decoder for either codec exists", async () => {
    core.capabilities = {
      can_share: true, can_receive: false, adapters: ["NVIDIA GeForce RTX 3090"],
      encoders: ["x"], decoders: [], share_codecs: ["hevc"], receive_codecs: [],
    };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByText(/no H\.264 or HEVC video decoder/)).toBeInTheDocument();
    expect(screen.getByText(/Media Feature Pack/)).toBeInTheDocument();
  });

  it("stays quiet when this PC decodes both", async () => {
    await mount();
    expect(screen.queryByTestId("codec-note")).not.toBeInTheDocument();
    expect(screen.queryByText(/video decoder/)).not.toBeInTheDocument();
  });
});

/**
 * The state this whole feature exists for. A dismissed Windows prompt writes
 * a permanent Block rule, and every symptom afterwards points at the network:
 * the code is accepted, discovery finds nothing, the share just waits. These
 * pin that Relay names the real cause instead.
 */
describe("firewall capability", () => {
  const blocked = {
    state: "blocked" as const,
    program: "C:\Relay\relay-share.exe",
    rule_present: false, blocking_rules: 3, stale_rules: 0,
    policy: { active_profiles: 2, enabled: true, default_inbound_block: true },
    unknown: false,
  };

  it("stays quiet when Relay is already allowed through", async () => {
    const h = await mount();
    expect(screen.queryByText(/Windows Firewall/)).not.toBeInTheDocument();
    h.expectClean();
  });

  it("calls a block a block, not a network fault", async () => {
    core.firewall = blocked;
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByText("Windows Firewall is blocking Relay.")).toBeInTheDocument();
    expect(screen.getByText(/3 rules were created/)).toBeInTheDocument();
    expect(screen.getByText(/not a network fault/)).toBeInTheDocument();
  });

  it("warns before the prompt appears, because declining it is the trap", async () => {
    core.firewall = { ...blocked, state: "will_prompt", blocking_rules: 0 };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByText(/Windows will ask whether to allow Relay/)).toBeInTheDocument();
    expect(screen.getByText(/blocks Relay permanently/)).toBeInTheDocument();
  });

  it("fixes it through the elevated helper and then goes quiet", async () => {
    core.firewall = blocked;
    tauri.useFakeCore(core.handler);
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Allow Relay through Windows Firewall/ }));
    await settle();
    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: "allow_firewall" });
    // The banner re-probes and disappears — the fix is verified, not assumed.
    expect(screen.queryByText("Windows Firewall is blocking Relay.")).not.toBeInTheDocument();
  });

  it("treats a declined UAC prompt as an answer and says what still works", async () => {
    core.firewall = blocked;
    core.elevation = { decline: true };
    tauri.useFakeCore(core.handler);
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Allow Relay through Windows Firewall/ }));
    await settle();
    expect(screen.getByText(/You declined the Windows permission prompt, so nothing was changed/)).toBeInTheDocument();
    expect(screen.getByText(/Relay still works everywhere it can/)).toBeInTheDocument();
    // Still blocked, and still saying so.
    expect(screen.getByText("Windows Firewall is blocking Relay.")).toBeInTheDocument();
  });

  it("points at the Windows network setting on a public network, with no button", async () => {
    core.firewall = { ...blocked, state: "public_network", blocking_rules: 0, policy: { active_profiles: 4, enabled: true, default_inbound_block: true } };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByText("This network is set to Public.")).toBeInTheDocument();
    // Relay does not add public-profile rules, so offering the fix would lie.
    expect(screen.queryByRole("button", { name: /Allow Relay through/ })).not.toBeInTheDocument();
  });

  it("says nothing at all when the probe could not read the firewall", async () => {
    core.firewall = { ...blocked, unknown: true };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.queryByText(/Windows Firewall/)).not.toBeInTheDocument();
  });

  it("stays quiet on a permissive network even with no rule", async () => {
    core.firewall = { ...blocked, state: "permissive", blocking_rules: 0 };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.queryByText(/Windows Firewall/)).not.toBeInTheDocument();
  });
});
