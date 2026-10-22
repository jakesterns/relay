/**
 * The APO update path: a Relay update cannot replace the DLL Windows audio
 * loads (it lives under Program Files), so Settings offers Update — an
 * install over the install, listed before Windows asks, then an audio
 * restart. Nothing shows while the loaded APO is this build's.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card } from "../test/dom";
import { apoStatus, makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Settings } from "./Settings";

let core: FakeCore;
const DAC_NAME = "Headphones (USB DAC)";

function installed(update: boolean) {
  const list = (core.apo.endpoints ?? []).map((e) =>
    e.is_default ? { ...e, installed: true, backed_up: true, running: true } : e);
  core.apo = { ...apoStatus(list), update_needed: update };
  core.audioEffects = { value: 1, allowed: true, changed_by_relay: true, prior: null, set_elsewhere: false, unknown: false };
}

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Settings />);
  await settle();
  return h;
}

describe("the APO update", () => {
  it("is not offered while the loaded APO is this build's", async () => {
    installed(false);
    await mount();
    expect(screen.queryByRole("button", { name: "Update the audio processor" })).toBeNull();
    expect(card("What Relay installs").textContent).not.toContain("Update available");
  });

  it("lists the file swap and the audio restart before Windows asks, then updates", async () => {
    installed(true);
    const h = await mount();
    expect(card("What Relay installs").textContent).toContain("per-game EQ is paused until it is updated");
    await h.user.click(screen.getByRole("button", { name: "Update the audio processor" }));
    await settle();
    const dac = core.apo.endpoints!.find((e) => e.name === DAC_NAME)!.endpoint;
    expect(tauri.lastCall("elevation_plan")?.args).toEqual({ op: { install_apo: { endpoint: dac } } });
    const text = card("What Relay installs").textContent ?? "";
    expect(text).toContain("replaces C:\\Program Files\\Relay\\audio\\relay_apo.dll");
    expect(text).toContain("No registry value changes.");
    expect(tauri.lastCall("run_elevated")).toBeUndefined();

    await h.user.click(screen.getByRole("button", { name: "Update now" }));
    await settle();
    expect(tauri.lastCall("run_elevated")?.args).toEqual({ op: { install_apo: { endpoint: dac } } });
    // The fake's install clears the flag; the re-read status hides the row.
    expect(screen.queryByRole("button", { name: "Update the audio processor" })).toBeNull();
  });
});
