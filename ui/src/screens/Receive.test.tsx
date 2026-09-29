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

  /** Two-PC finding: a receiver started elsewhere (a resume, another window)
   *  showed "Call app: None" while it was returning audio. */
  it("shows the running receiver's call app, not only this window's pick", async () => {
    localStorage.clear();
    await mount();
    expect(kv("Call app")).toBe("None");
    await push(() => tauri.emit("core://receive-status",
      { receiving: true, code: "418254", return_pid: 1004 }));
    await settle();
    expect(kv("Call app")).toBe("discord.exe");
  });

  /** r32 two-PC finding: after the UI was closed and reopened mid-receive
   *  the card read "None" -- the replayed event went past before the page
   *  listened, and the shell's stream status did not carry the call app. */
  it("names the call app when the window opens mid-receive", async () => {
    localStorage.clear();
    core.stream = { live: false, mode: "none", width: 0, height: 0, excluded_from_capture: true,
      receiving: true, code: "418254", return_pid: 1004, return_exe: "discord.exe" };
    await mount();
    expect(kv("Call app")).toBe("discord.exe");
  });

  it("never shows a PID for a call app that has closed", async () => {
    localStorage.clear();
    core.stream = { live: false, mode: "none", width: 0, height: 0, excluded_from_capture: true,
      receiving: true, code: "418254", return_pid: 99999 };
    await mount();
    expect(kv("Call app")).not.toMatch(/process|99999/);
  });

  /** S19: the return route. Off until a call app is picked; then its PID
   *  rides on the receive request, the choice survives a revisit by exe
   *  name, and it is locked while receiving. */
  it("sends the picked call app's PID with Start receiving, and remembers the app", async () => {
    localStorage.clear();
    const h = await mount();
    expect(kv("Call app")).toBe("None");
    await h.user.click(screen.getByRole("button", { name: "Pick the call app…" }));
    await settle();
    await h.user.selectOptions(screen.getByRole("combobox", { name: "Call app" }), "1004");
    expect(kv("Call app")).toBe("discord.exe");

    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await settle();
    expect(tauri.lastCall("start_receive")?.args).toEqual({ request: { return_pid: 1004 } });
    // Locked while receiving: the engine read the choice when it started.
    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254" }));
    expect(screen.queryByRole("button", { name: "Change…" })).not.toBeInTheDocument();

    // A fresh visit finds the same program again by name, not by PID.
    expect(localStorage.getItem("relay.callApp.exe")).toBe("discord.exe");
    // (The first screen is still mounted and locked, so the buttons below
    // can only belong to the fresh one.)
    const again = await mount();
    await settle();
    expect(screen.getByRole("button", { name: "Change…" })).toBeInTheDocument();
    await again.user.click(screen.getByRole("button", { name: "Off" }));
    expect(screen.getByRole("button", { name: "Pick the call app…" })).toBeInTheDocument();
    expect(localStorage.getItem("relay.callApp.exe")).toBeNull();
    localStorage.clear();
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

/** Stream health (S31).
 *
 *  These assert the two rules that decide whether the indicator is useful or
 *  is trained away: it must not appear for a single lost packet, and it must
 *  never appear for repair alone. Both come from real runs — S30 measured
 *  2,242 repaired packets with zero lost and a clean picture. */
describe("stream health", () => {
  const receiving = () =>
    push(() => tauri.emit("core://receive-status",
      { receiving: true, code: "418254", sender: "studio-pc" }));

  /** One receiver `stats` line with cumulative counters. */
  const stats = (o: Record<string, unknown>) =>
    push(() => tauri.emit("core://share-stats", { event: "stats", ...o }));

  const chip = () => screen.queryByTestId("health-chip");

  it("shows nothing while the stream is healthy", async () => {
    await mount();
    await receiving();
    await stats({ fps: 60, bitrate_mbps: 40, rtp_lost: 0, rtp_recovered: 0 });
    await stats({ fps: 60, bitrate_mbps: 40, rtp_lost: 0, rtp_recovered: 0 });
    expect(chip()).not.toBeInTheDocument();
  });

  it("does not flash for a single lost packet", async () => {
    await mount();
    await receiving();
    await stats({ rtp_lost: 0 });
    await stats({ rtp_lost: 1 });
    expect(chip()).not.toBeInTheDocument();
  });

  it("never warns about repaired packets, however many", async () => {
    await mount();
    await receiving();
    // S30's acceptance run, in miniature: a lossy link Relay is handling.
    await stats({ rtp_lost: 0, rtp_recovered: 0 });
    for (let i = 1; i <= 8; i++) await stats({ rtp_lost: 0, rtp_recovered: i * 280 });
    expect(chip()).not.toBeInTheDocument();
    // It is still worth saying, just not as a warning.
    expect(screen.getByTestId("health-coping")).toBeInTheDocument();
    expect(kv("Repaired")).toBe("2,240");
    expect(kv("Lost")).toBe("0");
  });

  it("warns once damage is sustained, and says so without blaming the network", async () => {
    await mount();
    await receiving();
    await stats({ rtp_lost: 0 });
    await stats({ rtp_lost: 40 });
    await stats({ rtp_lost: 90 });
    const el = chip();
    expect(el).toBeInTheDocument();
    expect(el!.textContent ?? "").not.toMatch(/network|Wi-?Fi|router/i);
  });

  it("clears the warning when the stream recovers", async () => {
    await mount();
    await receiving();
    await stats({ rtp_lost: 0 });
    await stats({ rtp_lost: 40 });
    await stats({ rtp_lost: 90 });
    expect(chip()).toBeInTheDocument();
    // Clearing is deliberately slower than triggering.
    for (let i = 0; i < 6; i++) await stats({ rtp_lost: 90 });
    expect(chip()).not.toBeInTheDocument();
  });

  it("drops the warning and the readout when the share ends", async () => {
    await mount();
    await receiving();
    await stats({ rtp_lost: 0 });
    await stats({ rtp_lost: 40 });
    await stats({ rtp_lost: 90 });
    expect(chip()).toBeInTheDocument();

    await push(() => tauri.emit("core://receive-status", { receiving: false }));
    expect(chip()).not.toBeInTheDocument();
    // And no stale numbers from a dead engine left on screen: the whole card
    // goes, rather than freezing on the last counts it happened to see.
    expect(screen.queryByText("Stream health")).not.toBeInTheDocument();
  });

  it("shows latency, which B14 made trustworthy again", async () => {
    await mount();
    await receiving();
    await stats({ fps: 59.9, bitrate_mbps: 38.2, capture_to_present_ms: 1.3,
      audio: { buffered_ms: 44 }, rtp_lost: 0, rtp_recovered: 0 });
    expect(kv("Latency")).toBe("1.3 ms");
    expect(kv("Audio buffer")).toBe("44 ms");
    expect(kv("Frame rate")).toBe("59.9 fps");
  });
});

/** Remembered PCs (S35), from the receiving end.
 *
 *  Forget must be real, and the screen must say when a sender got in
 *  without a code -- that is the one visible proof the feature works. */
describe("remembered PCs", () => {
  const now = Math.floor(Date.now() / 1000);
  const remembered = () => [
    { id: "p1", name: "studio-pc", fingerprint: "sha-256 aa", first_paired_unix: now - 9000,
      last_seen_unix: now - 300, last_direction: "received_from" as const, favourite: false },
  ];

  it("shows nothing when none are remembered", async () => {
    await mount();
    expect(screen.queryByText("Remembered PCs")).not.toBeInTheDocument();
  });

  it("lists them, and Forget is real", async () => {
    core.peers = remembered();
    tauri.useFakeCore(core.handler);
    const h = await mount();
    expect(screen.getByText("Remembered PCs")).toBeInTheDocument();
    expect(screen.getAllByTestId("trusted-row")).toHaveLength(1);
    // Honest about the limit: no code, but only while Start receiving is on.
    expect(screen.getByText(/only while Start receiving is on/)).toBeInTheDocument();

    await h.user.click(screen.getByRole("button", { name: "Forget" }));
    await settle();
    expect(tauri.lastCall("forget_peer")?.args).toEqual({ id: "p1" });
    expect(core.peers).toHaveLength(0);
    expect(screen.queryAllByTestId("trusted-row")).toHaveLength(0);
    expect(screen.getByText(/studio-pc will need a code next time/)).toBeInTheDocument();
  });

  it("says when the sender got in without a code, and only then", async () => {
    await mount();
    await push(() => tauri.emit("core://receive-status",
      { receiving: true, sender: "studio-pc", trusted: true }));
    expect(kv("Status")).toBe("Paired with studio-pc · remembered, no code");

    await push(() => tauri.emit("core://receive-status", { receiving: false }));
    await push(() => tauri.emit("core://receive-status", { receiving: true, sender: "laptop" }));
    expect(kv("Status")).toBe("Paired with laptop");
  });

  it("picks up a PC remembered by the share that just happened", async () => {
    await mount();
    expect(screen.queryByText("Remembered PCs")).not.toBeInTheDocument();
    // A code-paired share adds the sender to the store; the card should
    // notice without a reload.
    core.peers = remembered();
    await push(() => tauri.emit("core://receive-status",
      { receiving: true, code: "418254", sender: "studio-pc" }));
    await settle();
    expect(screen.getByText("Remembered PCs")).toBeInTheDocument();
  });
});

/** S38: the core restarts a receiver whose sender vanished. The screen must
 *  say that, not pretend it is a fresh wait -- and must not say it after a
 *  wait the user started by hand. */
describe("a receiver brought back on its own", () => {
  const paired = () =>
    push(() => tauri.emit("core://receive-status",
      { receiving: true, code: "418254", sender: "studio-pc" }));
  const gone = () => push(() => tauri.emit("core://receive-status", { receiving: false }));
  const waiting = () =>
    push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254" }));

  it("says the share dropped and it is waiting for it", async () => {
    await mount();
    await paired();
    await gone();
    expect(screen.getByText("The share from studio-pc ended.")).toBeInTheDocument();
    // No hand on Start receiving: the core did this.
    await waiting();
    expect(screen.getByText(/The share from studio-pc dropped — waiting for it to come back/))
      .toBeInTheDocument();
  });

  it("says dropped at once, never ended, while the core restarts it", async () => {
    await mount();
    await paired();
    await push(() => tauri.emit("core://receive-status", { receiving: false, restarting: true }));
    expect(screen.getByText(/The share from studio-pc dropped — waiting for it to come back/))
      .toBeInTheDocument();
    expect(screen.queryByText(/ended/)).not.toBeInTheDocument();
  });

  it("says ended, not dropped, when the sender stopped it", async () => {
    await mount();
    await paired();
    await push(() => tauri.emit("core://receive-status", { receiving: false, ended_by_sender: true }));
    await waiting();
    expect(screen.getByText("The share from studio-pc ended. Waiting for a sender to pair…"))
      .toBeInTheDocument();
    expect(screen.queryByText(/dropped/)).not.toBeInTheDocument();
  });

  it("but a Start receiving by hand is a fresh wait", async () => {
    const h = await mount();
    await paired();
    await gone();
    await h.user.click(screen.getByRole("button", { name: "Start receiving" }));
    await settle();
    await waiting();
    expect(screen.getByText("Waiting for a sender to pair…")).toBeInTheDocument();
    expect(screen.queryByText(/dropped/)).not.toBeInTheDocument();
  });
});

/** The mixer on the receiving end (S37): a row per track that has actually
 *  arrived, and none of it before a sender is there. */
describe("the mixer", () => {
  const stats = (o: Record<string, unknown>) =>
    push(() => tauri.emit("core://share-stats", { event: "stats", ...o }));

  it("waits for a sender, then grows a row as each track arrives", async () => {
    await mount();
    await push(() => tauri.emit("core://receive-status", { receiving: true, code: "418254" }));
    expect(screen.queryByText("Mixer")).not.toBeInTheDocument();

    await push(() => tauri.emit("core://receive-status", { receiving: true, sender: "studio-pc" }));
    expect(screen.getByRole("slider", { name: "Their audio" })).toBeInTheDocument();
    expect(screen.queryByRole("slider", { name: "Their microphone" })).not.toBeInTheDocument();

    await stats({ audio_packets: 50, mic_packets: 12 });
    expect(screen.getByRole("slider", { name: "Their microphone" })).toBeInTheDocument();
    await stats({ audio_packets: 100, mic_packets: 24, rest_packets: 9 });
    expect(screen.getByRole("slider", { name: "Everything else on their PC" })).toBeInTheDocument();
  });

  it("sends a receive-side fader to the core", async () => {
    await mount();
    await push(() => tauri.emit("core://receive-status", { receiving: true, sender: "studio-pc" }));
    const s = screen.getByRole("slider", { name: "Their audio" });
    // A range input is driven by event, never by moving the real cursor.
    const { fireEvent } = await import("@testing-library/react");
    fireEvent.change(s, { target: { value: "-6" } });
    await new Promise((r) => setTimeout(r, 80));
    await settle();
    expect(tauri.lastCall("set_mixer")?.args).toMatchObject({ side: "receive" });
  });
});
