/**
 * The bundled VB-CABLE (the virtual microphone): off by default, credited,
 * honest that this build does not carry it yet, and removable only when
 * Relay installed it. First run records the opt-in and installs nothing.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Settings, vbcableNote } from "./Settings";
import { FirstRun } from "./FirstRun";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

const ROW = "Install the virtual microphone (VB-CABLE by VB-Audio)";
const SWITCH = /^Install the virtual microphone \(VB-CABLE by VB-Audio\)/;

function row(): HTMLElement {
  const r = [...card("What Relay installs").querySelectorAll<HTMLElement>(".tog")]
    .find((x) => (x.textContent ?? "").includes(ROW));
  if (!r) throw new Error("no VB-CABLE row");
  return r;
}

function setVb(patch: Partial<NonNullable<FakeCore["vdevice"]["vbcable"]>>) {
  core.vdevice = { ...core.vdevice, vbcable: { ...core.vdevice.vbcable!, ...patch } };
  tauri.useFakeCore(core.handler);
}

async function mountSettings() {
  const h = renderScreen(<Settings />);
  await settle();
  return h;
}

describe("the Settings row", () => {
  it("credits VB-Audio and links vb-cable.com", async () => {
    await mountSettings();
    expect(row()).toHaveTextContent("VB-CABLE by VB-Audio Software — donationware");
    expect(within(row()).getByRole("link", { name: "vb-cable.com" })).toHaveAttribute("href", "https://vb-cable.com");
  });

  it("uses a VB-CABLE the user already has and offers nothing to install or remove", async () => {
    await mountSettings();
    expect(row()).toHaveTextContent(/already on this PC/);
    expect(within(row()).queryByRole("button")).toBeNull();
  });

  it("says plainly this build does not carry it yet", async () => {
    setVb({ present: false });
    await mountSettings();
    expect(row()).toHaveTextContent(/waiting on VB-Audio's permission/);
    expect(within(row()).queryByRole("button", { name: "Install…" })).toBeNull();
  });

  it("records the opt-in before the prompt, then installs through the helper", async () => {
    setVb({ present: false, payload: { state: "ready" } });
    const h = await mountSettings();
    await h.user.click(within(row()).getByRole("button", { name: "Install…" }));
    await settle();
    expect(tauri.lastCall("elevation_plan")?.args).toEqual({ op: "install_vbcable" });
    expect(screen.getByText(/VBCABLE_Setup_x64\.exe -i -h/)).toBeInTheDocument();
    expect(tauri.lastCall("run_elevated")).toBeUndefined();
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();
    const order = tauri.calls.map((c) => c.cmd);
    expect(order.indexOf("set_vbcable_consent")).toBeLessThan(order.indexOf("run_elevated"));
    expect(tauri.lastCall("set_vbcable_consent")?.args).toEqual({ on: true });
    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: "install_vbcable" });
    expect(core.vdevice.vbcable?.installed_by_relay).toBe(true);
    expect(row()).toHaveTextContent(/Installed by Relay/);
    h.expectClean();
  });

  it("installs nothing when the prompt is declined", async () => {
    setVb({ present: false, payload: { state: "ready" } });
    core.elevation.decline = true;
    const h = await mountSettings();
    await h.user.click(within(row()).getByRole("button", { name: "Install…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();
    expect(core.vdevice.vbcable?.installed_by_relay).toBe(false);
    expect(screen.getByText(/Nothing on this PC was changed/)).toBeInTheDocument();
    h.expectClean();
  });

  it("offers removal only for the VB-CABLE Relay installed", async () => {
    setVb({ present: true, installed_by_relay: true, consent: true, payload: { state: "ready" } });
    const h = await mountSettings();
    await h.user.click(within(row()).getByRole("button", { name: "Remove…" }));
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Remove now" }));
    await settle();
    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: "uninstall_vbcable" });
    expect(core.vdevice.vbcable?.installed_by_relay).toBe(false);
    h.expectClean();
  });

  it("is absent against a core that predates the bundle", async () => {
    core.vdevice = { ...core.vdevice, vbcable: undefined };
    tauri.useFakeCore(core.handler);
    await mountSettings();
    expect(card("What Relay installs")).not.toHaveTextContent(ROW);
  });

  it("explains a pack that fails its check", () => {
    const base = core.vdevice.vbcable!;
    expect(vbcableNote({ ...base, present: false, payload: { state: "mismatch", file: "x" } }))
      .toMatch(/integrity check/);
    expect(vbcableNote({ ...base, present: false, payload: { state: "incomplete", missing: "x" } }))
      .toMatch(/integrity check/);
  });
});

describe("first run", () => {
  beforeEach(() => {
    core.vdevice.consent = null;
    tauri.useFakeCore(core.handler);
  });

  async function mountFirstRun() {
    const h = renderScreen(<FirstRun onDone={vi.fn()} />);
    await settle();
    return h;
  }

  it("starts off and records nothing about VB-CABLE on a flat no", async () => {
    const h = await mountFirstRun();
    expect(screen.getByRole("switch", { name: SWITCH })).toHaveAttribute("aria-checked", "false");
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();
    expect(tauri.lastCall("set_vbcable_consent")).toBeUndefined();
    expect(tauri.commandNames()).not.toContain("run_elevated");
  });

  it("records the opt-in (and the mic route) but installs nothing", async () => {
    const h = await mountFirstRun();
    await h.user.click(screen.getByRole("switch", { name: SWITCH }));
    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();
    expect(tauri.lastCall("set_vbcable_consent")?.args).toEqual({ on: true });
    expect(tauri.lastCall("set_vdevice_consent")?.args).toEqual({ apo: false, camera: false, microphone: true });
    expect(tauri.commandNames()).not.toContain("run_elevated");
    expect(card("Virtual microphone (VB-CABLE)")).toHaveTextContent(/vb-cable\.com/);
  });
});
