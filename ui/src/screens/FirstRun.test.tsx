/**
 * First run: the two opt-ins, in the one place the user meets them before
 * anything exists on disk.
 *
 * The rule this screen has to keep is narrow and absolute — answering here
 * records a decision and installs nothing. So every test checks both what was
 * recorded and what was *not* called.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card, monoLines } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { FirstRun } from "./FirstRun";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  core.vdevice.consent = null;
  tauri.useFakeCore(core.handler);
});

async function mount(onDone = vi.fn()) {
  const h = renderScreen(<FirstRun onDone={onDone} />);
  await settle();
  return { ...h, onDone };
}

describe("what the screen says", () => {
  it("opens with both components off and nothing installed", async () => {
    const h = await mount();
    for (const sw of screen.getAllByRole("switch")) {
      expect(sw).toHaveAttribute("aria-checked", "false");
    }
    expect(screen.getByText(/Nothing on your PC has been changed/)).toBeInTheDocument();
    h.expectClean();
  });

  it("prints the camera's registry keys verbatim, from the core's dry run", async () => {
    await mount();
    expect(tauri.lastCall("vdevice_dry_run")).toBeDefined();
    expect(monoLines(card("Virtual camera & microphone"))).toEqual([
      "HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}",
      "HKLM\\SOFTWARE\\Classes\\CLSID\\{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}\\InprocServer32",
      "file: <install dir>\\relay_vdevice.dll (stays in place; only registered)",
    ]);
  });

  it("names the APO backup location and says removal restores it", async () => {
    await mount();
    expect(card("Endpoint audio processor (APO)")).toHaveTextContent(/%LOCALAPPDATA%\\Relay\\apo-backup/);
    expect(card("Endpoint audio processor (APO)")).toHaveTextContent(/removal restores it byte-for-byte/);
  });

  it("is honest that the signed mic driver does not exist yet", async () => {
    await mount();
    expect(card("Virtual camera & microphone")).toHaveTextContent(/the signed Relay driver is\s+not included yet/);
  });

  it("renders without the listing when the core cannot be asked", async () => {
    tauri.useOfflineCore();
    const h = await mount();
    expect(monoLines(card("Virtual camera & microphone"))).toEqual([]);
    expect(screen.getByRole("button", { name: "Continue" })).toBeEnabled();
    h.expectClean();
  });
});

describe("recording the decision", () => {
  it("records a flat no, installs nothing, and moves on", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();

    expect(tauri.lastCall("set_vdevice_consent")?.args).toEqual({ apo: false, camera: false, microphone: false });
    expect(tauri.commandNames()).not.toContain("install_vcam");
    expect(tauri.commandNames()).not.toContain("install_apo");
    expect(tauri.commandNames()).not.toContain("set_autostart");
    expect(core.vdevice.camera_registered).toBe(false);
    expect(h.onDone).toHaveBeenCalledOnce();
    h.expectClean();
  });

  it("records a yes to both without installing either", async () => {
    const h = await mount();
    await h.user.click(screen.getByText("Allow the audio processor"));
    await h.user.click(screen.getByText("Allow the virtual camera & microphone"));
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();

    expect(tauri.lastCall("set_vdevice_consent")?.args).toEqual({ apo: true, camera: true, microphone: true });
    expect(tauri.commandNames()).not.toContain("install_vcam");
    expect(tauri.commandNames()).not.toContain("install_apo");
    expect(core.vdevice.consent).toMatchObject({ apo: true, camera: true, microphone: true });
    h.expectClean();
  });

  it("asks the camera and the microphone as one answer — the driver ships as one", async () => {
    const h = await mount();
    await h.user.click(screen.getByText("Allow the virtual camera & microphone"));
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();
    const { camera, microphone } = tauri.lastCall("set_vdevice_consent")?.args as { camera: boolean; microphone: boolean };
    expect(camera).toBe(microphone);
  });

  it("writes the Run key only when start-at-login was actually ticked", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();
    expect(tauri.lastCall("set_autostart")).toBeUndefined();
    expect(core.autostart).toBe(false);
  });

  it("writes the Run key when it was", async () => {
    const h = await mount();
    await h.user.click(screen.getByText("Start Relay when I sign in"));
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();
    expect(tauri.lastCall("set_autostart")?.args).toEqual({ enabled: true });
    expect(core.autostart).toBe(true);
  });

  it("does not move on if the core could not record the answer", async () => {
    core.fail.set("set_vdevice_consent", "installed.json is not writable");
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();

    expect(screen.getByText("installed.json is not writable")).toBeInTheDocument();
    expect(h.onDone).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Continue" })).toBeEnabled();
  });

  it("says that a yes here is still not an install", async () => {
    await mount();
    expect(screen.getByText(/each component still shows an explicit install step/)).toBeInTheDocument();
  });
});
