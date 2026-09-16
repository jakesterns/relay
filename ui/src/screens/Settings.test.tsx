/**
 * Settings: the uninstall plan and the two opt-in rows.
 *
 * The plan listing is the promise the whole project rests on — "nothing is
 * left behind" — and until this month it was a hard-coded list in the
 * webview. It now comes from the core's planner, so these tests check that
 * what is drawn is what the core said, including the parts that depend on
 * what is actually installed.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card, field, inCard, kv, monoLines } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Settings } from "./Settings";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Settings />);
  await settle();
  return h;
}

/** One `.tog` row of the "What Relay installs" card. */
function installRow(text: string): HTMLElement {
  const row = [...card("What Relay installs").querySelectorAll<HTMLElement>(".tog")]
    .find((r) => (r.textContent ?? "").includes(text));
  if (!row) throw new Error(`no install row for "${text}"`);
  return row;
}

const APO = "Endpoint audio processor (APO)";
const VDEV = "Virtual camera & microphone";

describe("the uninstall plan", () => {
  it("is not fetched until it is asked for", async () => {
    await mount();
    expect(tauri.lastCall("uninstall_plan")).toBeUndefined();
  });

  it("draws exactly the steps the core planned", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Show what will be removed/ }));
    await settle();

    expect(tauri.lastCall("uninstall_plan")?.args).toEqual({ keepData: true });
    expect(monoLines(card("Uninstall Relay"))).toEqual([
      "[ ] Close the Relay window — relay-ui.exe",
      "[x] Stop the core (restores your audio and display settings) — \\\\.\\pipe\\relay-core",
      "[ ] Restore the endpoint audio chain — not installed",
      "[ ] Unregister the virtual camera — not installed",
      "[ ] Remove the start-at-login entry — not set",
      "[x] Delete the program files — <install dir>",
      "[ ] Delete your profiles and settings — %LOCALAPPDATA%\\Relay",
    ]);
    expect(screen.getByText(/Your profiles and hardware library are kept/)).toBeInTheDocument();
    h.expectClean();
  });

  it("re-asks the core when the keep-my-data answer changes", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Show what will be removed/ }));
    await settle();

    await h.user.click(screen.getByText("Keep my profiles and hardware library"));
    await settle();

    expect(tauri.lastCall("uninstall_plan")?.args).toEqual({ keepData: false });
    expect(monoLines(card("Uninstall Relay"))).toContain(
      "[x] Delete your profiles and settings — %LOCALAPPDATA%\\Relay",
    );
    expect(screen.getByText(/Everything above is removed\./)).toBeInTheDocument();
    expect(screen.queryByText(/Your profiles and hardware library are kept/)).not.toBeInTheDocument();
  });

  it("reflects what is actually registered, not a fixed list", async () => {
    core.apo = { installed: true, endpoint: "ep:dac", running: true };
    core.vdevice.camera_registered = true;
    core.autostart = true;
    tauri.useFakeCore(core.handler);

    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Show what will be removed/ }));
    await settle();

    const lines = monoLines(card("Uninstall Relay"));
    expect(lines).toContain("[x] Restore the endpoint audio chain — ep:dac");
    expect(lines.some((l) => l.startsWith("[x] Unregister the virtual camera"))).toBe(true);
    expect(lines.some((l) => l.startsWith("[x] Remove the start-at-login entry"))).toBe(true);
  });

  it("says the plan is unreadable when the core is offline, rather than guessing one", async () => {
    tauri.useOfflineCore();
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Show what will be removed/ }));
    await settle();
    expect(screen.getByText(/Relay is not running — cannot read the plan/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Uninstall Relay" })).toBeDisabled();
  });

  it("hands over to the Windows uninstaller rather than removing anything itself", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Show what will be removed/ }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Uninstall Relay" }));
    await settle();
    expect(tauri.lastCall("launch_uninstaller")).toBeDefined();
  });

  it("can be closed again without doing anything", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: /Show what will be removed/ }));
    await settle();
    await h.user.click(within(card("Uninstall Relay")).getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("button", { name: /Show what will be removed/ })).toBeInTheDocument();
    expect(tauri.lastCall("launch_uninstaller")).toBeUndefined();
  });
});

describe("the endpoint APO opt-in", () => {
  it("shows what would change before Windows is ever asked", async () => {
    const h = await mount();
    expect(within(installRow(APO)).getByText(/Not installed/)).toBeInTheDocument();

    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Install…" }));
    await settle();

    // The plan is read-only, and it is on screen while nothing has run.
    expect(tauri.lastCall("elevation_plan")?.args).toEqual({ op: "install_apo" });
    expect(tauri.lastCall("run_elevated")).toBeUndefined();
    expect(core.apo.installed).toBe(false);

    const lines = monoLines(card("What Relay installs"));
    expect(lines.some((l) => l.includes("FxProperties"))).toBe(true);
    expect(lines.some((l) => l.startsWith("backup:") && l.includes("apo-backup"))).toBe(true);
    expect(screen.getByText(/Windows will ask for permission before any of this happens/)).toBeInTheDocument();
    h.expectClean();
  });

  it("installs only after the prompt is accepted, then reports it installed", async () => {
    const h = await mount();
    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Install…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: "install_apo" });
    expect(core.apo.installed).toBe(true);
    expect(within(installRow(APO)).getByText(/Installed on your headset endpoint · active/)).toBeInTheDocument();
    expect(within(installRow(APO)).getByRole("button", { name: "Remove…" })).toBeInTheDocument();
    h.expectClean();
  });

  it("leaves the machine untouched when the prompt is declined, and says so", async () => {
    core.elevation.decline = true;
    const h = await mount();
    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Install…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    // Declining is an answer, not an error: nothing installed, nothing broken,
    // and the user is told in plain words rather than shown a failure.
    expect(core.apo.installed).toBe(false);
    expect(screen.getByText(/Windows permission was declined\. Nothing on this PC was changed\./)).toBeInTheDocument();
    expect(within(installRow(APO)).getByText(/Not installed/)).toBeInTheDocument();
    h.expectClean();
  });

  it("surfaces a helper that failed, instead of appearing to have installed", async () => {
    core.fail.set("run_elevated", "the elevated helper exited with code 5 (access denied)");
    const h = await mount();
    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Install…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    expect(screen.getByText(/exited with code 5/)).toBeInTheDocument();
    expect(core.apo.installed).toBe(false);
    expect(within(installRow(APO)).getByText(/Not installed/)).toBeInTheDocument();
  });

  it("removes it through the same prompt, showing the restore plan first", async () => {
    core.apo = { installed: true, endpoint: "ep:dac", running: false };
    tauri.useFakeCore(core.handler);
    const h = await mount();

    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Remove…" }));
    await settle();
    expect(tauri.lastCall("elevation_plan")?.args).toEqual({ op: "uninstall_apo" });
    expect(monoLines(card("What Relay installs")).some((l) => l.includes("Restore the endpoint audio chain"))).toBe(true);
    expect(core.apo.installed).toBe(true);

    await h.user.click(screen.getByRole("button", { name: "Remove now" }));
    await settle();
    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: "uninstall_apo" });
    expect(within(installRow(APO)).getByText(/Not installed/)).toBeInTheDocument();
    h.expectClean();
  });

  it("will not offer the prompt until the core has answered with a status", async () => {
    tauri.useOfflineCore();
    await mount();
    expect(within(installRow(APO)).getByRole("button", { name: "Install…" })).toBeDisabled();
    expect(within(installRow(APO)).getByText(/Relay is not running — status unknown/)).toBeInTheDocument();
  });
});

describe("the virtual camera opt-in", () => {
  it("lists every registry key before asking, and asks nothing yet", async () => {
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Install…" }));
    await settle();

    expect(tauri.lastCall("elevation_plan")?.args).toEqual({ op: "install_camera" });
    expect(monoLines(card("What Relay installs")).filter((l) => l.startsWith("HKLM"))).toHaveLength(2);
    expect(tauri.lastCall("run_elevated")).toBeUndefined();
    expect(tauri.lastCall("set_vdevice_consent")).toBeUndefined();
    expect(core.vdevice.camera_registered).toBe(false);
    h.expectClean();
  });

  it("records consent before the prompt, never after", async () => {
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Install…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    // The helper refuses to register the camera without a recorded consent,
    // so the order is the invariant, not just the pair of calls.
    const order = tauri.calls.map((c) => c.cmd);
    expect(order.indexOf("set_vdevice_consent")).toBeLessThan(order.indexOf("run_elevated"));
    expect(tauri.lastCall("set_vdevice_consent")?.args).toEqual({ apo: false, camera: true, microphone: true });
    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: "install_camera" });
    expect(core.vdevice.camera_registered).toBe(true);
    expect(within(installRow(VDEV)).getByText(/"Relay Camera" registered/)).toBeInTheDocument();
    h.expectClean();
  });

  it("registers nothing when the prompt is declined", async () => {
    core.elevation.decline = true;
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Install…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    expect(core.vdevice.camera_registered).toBe(false);
    expect(screen.getByText(/Nothing on this PC was changed/)).toBeInTheDocument();
    expect(within(installRow(VDEV)).getByText(/Not installed/)).toBeInTheDocument();
    h.expectClean();
  });

  it("says Windows will ask before anything happens", async () => {
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Install…" }));
    await settle();
    expect(screen.getByText(/Windows will ask for permission before any of this happens/)).toBeInTheDocument();
    h.expectClean();
  });

  it("will not offer an install on a Windows build that cannot host one", async () => {
    core.vdevice = { ...core.vdevice, camera_supported: false, windows_build: 19045, obs_virtualcam: "OBS Virtual Camera" };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(within(installRow(VDEV)).getByRole("button", { name: "Install…" })).toBeDisabled();
    expect(within(installRow(VDEV)).getByText(/Needs Windows 11 22H2\+ \(this PC: build 19045\)/)).toBeInTheDocument();
    expect(within(installRow(VDEV)).getByText(/OBS VirtualCam detected as a fallback/)).toBeInTheDocument();
  });

  it("withdraws the camera consent after the keys are actually gone", async () => {
    core.vdevice = { ...core.vdevice, camera_registered: true, consent: { decided_at: "x", apo: true, camera: true, microphone: true } };
    tauri.useFakeCore(core.handler);
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Remove…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Remove now" }));
    await settle();

    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: "uninstall_camera" });
    // Mirror image of install: consent is withdrawn only once the removal
    // succeeded, so a declined prompt cannot leave a registered camera with
    // no recorded consent behind it.
    const order = tauri.calls.map((c) => c.cmd);
    expect(order.lastIndexOf("set_vdevice_consent")).toBeGreaterThan(order.lastIndexOf("run_elevated"));
    // The APO answer is left alone; only the camera/mic answer is withdrawn.
    expect(tauri.lastCall("set_vdevice_consent")?.args).toEqual({ apo: true, camera: false, microphone: false });
    expect(core.vdevice.camera_registered).toBe(false);
    h.expectClean();
  });

  it("keeps the recorded consent when a removal prompt is declined", async () => {
    // Regression: the panel used to run its `after` hook unconditionally, so
    // declining the prompt withdrew consent while the camera stayed
    // registered — a registration with nothing consenting to it, which is
    // precisely the state installed.json exists to rule out.
    core.elevation.decline = true;
    core.vdevice = { ...core.vdevice, camera_registered: true, consent: { decided_at: "x", apo: true, camera: true, microphone: true } };
    tauri.useFakeCore(core.handler);
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Remove…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Remove now" }));
    await settle();

    expect(core.vdevice.camera_registered).toBe(true);
    expect(core.vdevice.consent).toMatchObject({ camera: true, microphone: true });
    expect(screen.getByText(/Nothing on this PC was changed/)).toBeInTheDocument();
    h.expectClean();
  });

  it("keeps the recorded consent when the removal helper fails", async () => {
    core.fail.set("run_elevated", "the elevated helper exited with code 5 (access denied)");
    core.vdevice = { ...core.vdevice, camera_registered: true, consent: { decided_at: "x", apo: true, camera: true, microphone: true } };
    tauri.useFakeCore(core.handler);
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Remove…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Remove now" }));
    await settle();

    expect(core.vdevice.camera_registered).toBe(true);
    expect(core.vdevice.consent).toMatchObject({ camera: true, microphone: true });
    expect(screen.getByText(/exited with code 5/)).toBeInTheDocument();
  });

  it("names the interim VB-Cable route while the signed driver is missing", async () => {
    await mount();
    expect(within(installRow(VDEV)).getByText(/Mic route: CABLE Input \(VB-Audio Virtual Cable\)/)).toBeInTheDocument();
  });
});

describe("startup, recording and restore", () => {
  it("writes the one Run-key value on request", async () => {
    const h = await mount();
    await h.user.click(screen.getByText("Start Relay at login"));
    await settle();
    expect(tauri.lastCall("set_autostart")?.args).toEqual({ enabled: true });
    expect(core.autostart).toBe(true);
  });

  it("drops a blank recording folder rather than sending an empty path", async () => {
    const h = await mount();
    const rec = card("Recording");
    await h.user.clear(field("Disk cap (GB)", rec));
    await h.user.type(field("Disk cap (GB)", rec), "80");
    await h.user.click(within(rec).getByRole("button", { name: "Save recording settings" }));
    await settle();

    expect(tauri.lastCall("set_recording_settings")?.args).toEqual({
      settings: { cap_gb: 80, free_floor_gb: 10 },
    });
    expect(within(rec).getByRole("button", { name: "Saved" })).toBeDisabled();
  });

  it("passes a folder through when one is given", async () => {
    const h = await mount();
    const rec = card("Recording");
    await h.user.type(field("Folder", rec), "D:\\clips");
    await h.user.click(within(rec).getByRole("button", { name: "Save recording settings" }));
    await settle();
    expect(core.recording.dir).toBe("D:\\clips");
  });

  it("restores everything on request and shows nothing applied", async () => {
    core.state.active_profile = { id: "1", name: "Call of Duty", note: "", exe: "cod.exe", headset: null, monitor: null, share: "game", status: "ready" };
    core.state.display_state = "applied";
    tauri.useFakeCore(core.handler);
    const h = await mount();
    expect(screen.getByText("Profile applied")).toBeInTheDocument();

    await h.user.click(screen.getByRole("button", { name: "Restore original state now" }));
    expect(tauri.lastCall("restore_all")).toBeUndefined();
    await h.user.click(screen.getByRole("button", { name: "Confirm restore" }));
    await settle();

    expect(tauri.lastCall("restore_all")).toBeDefined();
    expect(screen.getByText("Nothing applied")).toBeInTheDocument();
  });

  it("reports the paths this core is really using", async () => {
    await mount();
    expect(kv("Relay")).toBe("Running");
    expect(kv("Data folder")).toBe("C:\\Users\\test\\AppData\\Local\\Relay");
    expect(kv("Log file")).toBe("C:\\Users\\test\\AppData\\Local\\Relay\\logs\\core.log");
    expect(kv("Version")).toBe("0.1.0-test");
  });

  it("lists the hotkeys the core registered", async () => {
    await mount();
    const hk = card("Hotkeys");
    expect(kv("Toggle share", hk)).toBe("Ctrl + Alt + S");
    expect(kv("Save replay clip", hk)).toBe("Ctrl + Alt + R");
  });

  it("surfaces a Run-key write that Windows refused", async () => {
    core.fail.set("set_autostart", "Access is denied. (os error 5)");
    const h = await mount();
    await h.user.click(screen.getByText("Start Relay at login"));
    await settle();
    expect(screen.getByText(/Access is denied/)).toBeInTheDocument();
  });

  it("keeps the toggle inert while the setting is unknown", async () => {
    tauri.useOfflineCore();
    const h = await mount();
    await h.user.click(screen.getByText("Start Relay at login"));
    await settle();
    expect(tauri.lastCall("set_autostart")).toBeUndefined();
    expect(inCard("Startup").getByRole("switch")).toHaveAttribute("aria-checked", "false");
  });
});
