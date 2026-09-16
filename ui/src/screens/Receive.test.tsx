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

  it("names the sender once paired and says the stream is its own window", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254", sender: "studio-pc" }));

    expect(kv("Status")).toBe("Paired with studio-pc");
    expect(kv("Codec")).toBe("HEVC");
    expect(screen.getByText(/Playing in a separate window · studio-pc/)).toBeInTheDocument();
    expect(screen.getByText(/The stream appears as a normal window\. Nothing on this PC is changed\./)).toBeInTheDocument();
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

  it("shows the core's message when a start fails", async () => {
    core.fail.set("start_receive", "another receiver already holds the port");
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await settle();
    expect(screen.getByText(/another receiver already holds the port/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start receiving" })).toBeInTheDocument();
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
  it("names the Store download needed to decode, before the first frame fails", async () => {
    core.capabilities = { can_share: true, can_receive: false, adapters: ["NVIDIA GeForce RTX 3090"], encoders: ["x"], decoders: [] };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(screen.getByText(/HEVC Video Extensions from Device Manufacturer/)).toBeInTheDocument();
  });

  it("stays quiet when this PC can decode", async () => {
    await mount();
    expect(screen.queryByText(/No HEVC decoder on this PC/)).not.toBeInTheDocument();
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
