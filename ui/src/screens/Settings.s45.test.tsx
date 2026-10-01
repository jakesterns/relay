/**
 * S45: the Updates card. Relay checks on its own (default on) and installs
 * only when the user says so (default off). The card shows the release and
 * its notes, and each button reaches the core with the right command.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import type { UpdateAvailable } from "../lib/ipc";
import { Settings } from "./Settings";

let core: FakeCore;

const release: UpdateAvailable = {
  version: "0.2.1",
  notes: "What's Changed\n- Faster reconnect",
  url: "https://github.com/jakesterns/relay/releases/tag/v0.2.1",
  prerelease: false,
  installer_name: "Relay_0.2.1_x64-setup.exe",
  installer_url: "https://github.com/jakesterns/relay/releases/download/v0.2.1/Relay_0.2.1_x64-setup.exe",
  installer_size: 31457280,
  sums_url: "https://github.com/jakesterns/relay/releases/download/v0.2.1/SHA256SUMS.txt",
};

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Settings />);
  await settle();
  return { h, updates: () => within(card("Updates")) };
}

const autoCheck = () => screen.getByRole("switch", { name: /Check for updates automatically/ });
const autoInstall = () => screen.getByRole("switch", { name: /Install updates automatically/ });

describe("the Updates card", () => {
  it("checks automatically by default and never installs on its own", async () => {
    await mount();
    expect(autoCheck()).toHaveAttribute("aria-checked", "true");
    expect(autoInstall()).toHaveAttribute("aria-checked", "false");
    expect(screen.queryByTestId("update-offer")).toBeNull();
    expect(screen.queryByRole("button", { name: "Install now" })).toBeNull();
  });

  it("shows the release and its notes when one is available", async () => {
    core.update.available = release;
    const { updates } = await mount();
    expect(updates().getByText(/Relay 0\.2\.1 is available/)).toBeInTheDocument();
    expect(updates().getByText(/Faster reconnect/)).toBeInTheDocument();
    expect(updates().getByRole("button", { name: "Install now" })).toBeEnabled();
    expect(updates().getByRole("button", { name: "Later" })).toBeEnabled();
    expect(updates().getByRole("button", { name: "Skip this version" })).toBeEnabled();
  });

  it("Install now asks the core to install, and only then", async () => {
    core.update.available = release;
    const { h } = await mount();
    expect(tauri.callsOf("install_update")).toHaveLength(0);
    await h.user.click(screen.getByRole("button", { name: "Install now" }));
    await settle();
    expect(tauri.callsOf("install_update")).toHaveLength(1);
    expect(screen.getByText(/Downloading and verifying/)).toBeInTheDocument();
  });

  it("Later hides the offer without skipping it", async () => {
    core.update.available = release;
    const { h } = await mount();
    await h.user.click(screen.getByRole("button", { name: "Later" }));
    await settle();
    expect(tauri.callsOf("update_later")).toHaveLength(1);
    expect(core.skippedUpdate).toBeNull();
    expect(screen.queryByTestId("update-offer")).toBeNull();
  });

  it("Skip this version records that version", async () => {
    core.update.available = release;
    const { h } = await mount();
    await h.user.click(screen.getByRole("button", { name: "Skip this version" }));
    await settle();
    expect(tauri.lastCall("skip_update")?.args).toEqual({ version: "0.2.1" });
    expect(core.skippedUpdate).toBe("0.2.1");
    expect(screen.queryByTestId("update-offer")).toBeNull();
  });

  it("Check now checks and says when", async () => {
    const { h, updates } = await mount();
    expect(updates().getByText(/not checked yet/)).toBeInTheDocument();
    await h.user.click(screen.getByRole("button", { name: "Check now" }));
    await settle();
    expect(tauri.callsOf("check_for_updates")).toHaveLength(1);
    expect(updates().getByText(/you have the latest version/)).toBeInTheDocument();
  });

  it("turning on automatic installs keeps every other preference", async () => {
    const { h } = await mount();
    await h.user.click(autoInstall());
    await settle();
    expect(tauri.lastCall("set_ui_prefs")?.args).toEqual({
      prefs: { ...makeFakeCore().prefs, auto_install_updates: true },
    });
    expect(core.prefs.auto_check_updates).toBe(true);
  });

  it("shows a refused install in plain words", async () => {
    core.update.last_result = {
      version: "0.2.1", ok: false, at: 1,
      message: "The download does not match the checksum published with the release, so it was not installed. Nothing on your PC was changed.",
    };
    const { updates } = await mount();
    expect(updates().getByRole("alert")).toHaveTextContent(/does not match the checksum/);
  });
});
