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
    expect(screen.getByText(/Core offline — cannot read the plan/)).toBeInTheDocument();
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
  it("shows an explicit confirmation before installing, naming the backup path", async () => {
    const h = await mount();
    expect(within(installRow(APO)).getByText(/Not installed/)).toBeInTheDocument();

    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Install…" }));
    expect(screen.getByText(/apo-backup/)).toBeInTheDocument();
    expect(tauri.lastCall("install_apo")).toBeUndefined();

    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    expect(tauri.lastCall("install_apo")).toBeDefined();
    expect(within(installRow(APO)).getByText(/Installed on your headset endpoint · active/)).toBeInTheDocument();
    expect(within(installRow(APO)).getByRole("button", { name: "Remove" })).toBeInTheDocument();
    h.expectClean();
  });

  it("reports the core's gate instead of appearing to have installed", async () => {
    core.fail.set("install_apo", "live APO writes need RELAY_APO_ALLOW_LIVE_WRITE and an elevated core");
    const h = await mount();
    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Install…" }));
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    expect(screen.getByText(/RELAY_APO_ALLOW_LIVE_WRITE/)).toBeInTheDocument();
    expect(core.apo.installed).toBe(false);
    expect(within(installRow(APO)).getByRole("button", { name: "Install…" })).toBeInTheDocument();
  });

  it("removes it again, and the row goes back to not installed", async () => {
    core.apo = { installed: true, endpoint: "ep:dac", running: false };
    tauri.useFakeCore(core.handler);
    const h = await mount();
    await h.user.click(within(installRow(APO)).getByRole("button", { name: "Remove" }));
    await settle();
    expect(tauri.lastCall("uninstall_apo")).toBeDefined();
    expect(within(installRow(APO)).getByText(/Not installed/)).toBeInTheDocument();
  });
});

describe("the virtual camera opt-in", () => {
  it("lists every registry key before asking, then records consent and installs", async () => {
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Install…" }));
    await settle();

    expect(monoLines(card("What Relay installs")).filter((l) => l.startsWith("HKLM"))).toHaveLength(2);
    expect(screen.getByText(/installed\.json/)).toBeInTheDocument();

    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();

    expect(tauri.lastCall("set_vdevice_consent")?.args).toEqual({ apo: false, camera: true, microphone: true });
    expect(tauri.lastCall("install_vcam")).toBeDefined();
    expect(core.vdevice.camera_registered).toBe(true);
    expect(within(installRow(VDEV)).getByText(/"Relay Camera" registered/)).toBeInTheDocument();
    h.expectClean();
  });

  it("warns that installing needs an elevated core", async () => {
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Install…" }));
    await settle();
    expect(screen.getByText(/Installing needs the core running as administrator/)).toBeInTheDocument();
  });

  it("will not offer an install on a Windows build that cannot host one", async () => {
    core.vdevice = { ...core.vdevice, camera_supported: false, windows_build: 19045, obs_virtualcam: "OBS Virtual Camera" };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(within(installRow(VDEV)).getByRole("button", { name: "Install…" })).toBeDisabled();
    expect(within(installRow(VDEV)).getByText(/Needs Windows 11 22H2\+ \(this PC: build 19045\)/)).toBeInTheDocument();
    expect(within(installRow(VDEV)).getByText(/OBS VirtualCam detected as a fallback/)).toBeInTheDocument();
  });

  it("withdraws the camera consent when the camera is removed", async () => {
    core.vdevice = { ...core.vdevice, camera_registered: true, consent: { decided_at: "x", apo: true, camera: true, microphone: true } };
    tauri.useFakeCore(core.handler);
    const h = await mount();
    await h.user.click(within(installRow(VDEV)).getByRole("button", { name: "Remove" }));
    await settle();

    expect(tauri.lastCall("uninstall_vcam")).toBeDefined();
    // The APO answer is left alone; only the camera/mic answer is withdrawn.
    expect(tauri.lastCall("set_vdevice_consent")?.args).toEqual({ apo: true, camera: false, microphone: false });
    expect(core.vdevice.camera_registered).toBe(false);
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
    await settle();

    expect(tauri.lastCall("restore_all")).toBeDefined();
    expect(screen.getByText("Nothing applied")).toBeInTheDocument();
  });

  it("reports the paths this core is really using", async () => {
    await mount();
    expect(kv("Core service")).toBe("Running");
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
