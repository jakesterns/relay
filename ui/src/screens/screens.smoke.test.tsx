/**
 * Every screen, rendered in each of the three situations it actually meets:
 * browser mock data, a core that is not there, and a core that answers.
 *
 * The bar is deliberately low and broad — nothing throws, nothing logs an
 * error, and the screen says something true about the mode it is in. The
 * narrow behavioural tests live beside this file.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { renderApp, renderScreen, settle } from "../test/render";
import { makeFakeCore } from "../test/fakeCore";
import { card } from "../test/dom";
import * as tauri from "../test/tauriMock";
import { Games } from "./Games";
import { Profiles } from "./Profiles";
import { Receive } from "./Receive";
import { Settings } from "./Settings";
import { Share } from "./Share";
import { FirstRun } from "./FirstRun";

const SCREENS = [
  { name: "Share", node: <Share />, heading: /Display 1/ },
  { name: "Receive", node: <Receive />, heading: /Receive/ },
  { name: "Games · Audio", node: <Games section="audio" onSection={() => {}} />, heading: /Audio/ },
  { name: "Games · Display", node: <Games section="display" onSection={() => {}} />, heading: /Display/ },
  { name: "Games · Sharing", node: <Games section="sharing" onSection={() => {}} />, heading: /Sharing/ },
  { name: "Profiles", node: <Profiles />, heading: /Profiles/ },
  { name: "Settings", node: <Settings />, heading: /Settings/ },
  { name: "First run", node: <FirstRun onDone={() => {}} />, heading: /Before anything is installed/ },
] as const;

describe("mock data (browser, no Tauri)", () => {
  beforeEach(() => tauri.useMockData());

  for (const s of SCREENS) {
    it(`${s.name} renders`, async () => {
      const h = renderScreen(s.node);
      await settle();
      expect(screen.getByRole("heading", { level: 1, name: s.heading })).toBeInTheDocument();
      h.expectClean();
    });
  }

  it("issues no Tauri commands at all", async () => {
    renderScreen(<Profiles />);
    await settle();
    expect(tauri.calls).toEqual([]);
  });

  it("never shows the offline banner — there is no core to be offline", async () => {
    renderScreen(<Settings />);
    await settle();
    expect(screen.queryByText(/Relay is not running/)).not.toBeInTheDocument();
    expect(screen.getByText("Running")).toBeInTheDocument();
  });

  it("opens on the first-run gate, because consent is undecided", async () => {
    const h = renderApp();
    await settle();
    expect(screen.getByRole("heading", { level: 1, name: /Before anything is installed/ })).toBeInTheDocument();
    h.expectClean();
  });
});

describe("offline core (inside Tauri, nothing behind the pipe)", () => {
  beforeEach(() => tauri.useOfflineCore());

  for (const s of SCREENS) {
    it(`${s.name} renders`, async () => {
      const h = renderScreen(s.node);
      await settle();
      expect(screen.getByRole("heading", { level: 1, name: s.heading })).toBeInTheDocument();
      h.expectClean();
    });
  }

  it("says Relay is not running and offers a button, never a command to type", async () => {
    renderScreen(<Profiles />);
    await settle();
    expect(screen.getByText(/Relay is not running/)).toBeInTheDocument();
    // The remedy is a control, not instructions: this is an installed app.
    expect(screen.getByRole("button", { name: "Start Relay" })).toBeInTheDocument();
    expect(screen.queryByText(/relay-core/)).not.toBeInTheDocument();
    expect(document.querySelector(".offline code")).toBeNull();
  });

  it("Settings reports Offline and blanks the paths rather than inventing them", async () => {
    renderScreen(<Settings />);
    await settle();
    expect(screen.getByText("Offline")).toBeInTheDocument();
    // Startup, close-behaviour and Recording each say so in their own words,
    // and none of them offers a command to type.
    expect(screen.getAllByText(/Relay is not running — cannot read the setting/)).toHaveLength(2);
    expect(screen.getByText(/Relay is not running — cannot read this setting/)).toBeInTheDocument();
  });

  it("Receive says the call status is unknown instead of claiming a camera", async () => {
    renderScreen(<Receive />);
    await settle();
    expect(screen.getByText(/Relay is not running — status unknown/)).toBeInTheDocument();
    // The status card claims nothing. (The S50 guide names Relay Camera as
    // a choice, not a status, so it is not what this is about.)
    expect(card("In calls").textContent).not.toMatch(/Relay Camera/);
  });

  it("the app shell still mounts and does not trap the user on first run", async () => {
    const h = renderApp();
    await settle();
    // vdevice_status rejected, so the gate must fail open to the normal shell.
    expect(screen.queryByRole("heading", { name: /Before anything is installed/ })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: /Profiles/ })).toBeInTheDocument();
    expect(screen.getByText("Not running")).toBeInTheDocument();
    h.expectClean();
  });

  it("empty libraries read as empty, not as a crash", async () => {
    renderScreen(<Profiles />);
    await settle();
    expect(screen.getAllByText("Library is empty")).toHaveLength(2);
    expect(screen.getByText(/No profiles yet/)).toBeInTheDocument();
  });
});

describe("live core (scripted fake behind the pipe)", () => {
  beforeEach(() => {
    tauri.useFakeCore(makeFakeCore().handler);
  });

  for (const s of SCREENS) {
    it(`${s.name} renders`, async () => {
      const h = renderScreen(s.node);
      await settle();
      expect(screen.getByRole("heading", { level: 1, name: s.heading })).toBeInTheDocument();
      h.expectClean();
    });
  }

  it("Profiles shows the core's rows and library", async () => {
    renderScreen(<Profiles />);
    await settle();
    expect(screen.getByText("Call of Duty")).toBeInTheDocument();
    expect(screen.getByText("Valorant")).toBeInTheDocument();
    expect(screen.getByText("Elden Ring")).toBeInTheDocument();
    // Once in the profile table, once in the monitor library.
    expect(screen.getAllByText("LG ULTRAGEAR+")).toHaveLength(2);
    expect(screen.getByText("Plugged")).toBeInTheDocument();
    expect(screen.queryByText(/Relay is not running/)).not.toBeInTheDocument();
  });

  it("Settings reads the build info the core reported", async () => {
    renderScreen(<Settings />);
    await settle();
    expect(screen.getByText("0.1.0-test")).toBeInTheDocument();
    expect(screen.getByText("C:\\Users\\test\\AppData\\Local\\Relay")).toBeInTheDocument();
  });

  it("every command the screens issue is one the core implements", async () => {
    const core = makeFakeCore();
    tauri.useFakeCore(core.handler);
    for (const s of SCREENS) {
      const h = renderScreen(s.node);
      await settle();
      h.unmount();
    }
    const seen = tauri.commandNames();
    expect(seen.length).toBeGreaterThan(8);
    // A typo in a command name is otherwise invisible until someone runs the
    // desktop build: `invoke` just rejects and the screen shows its error path.
    const { KNOWN_COMMANDS } = await import("../test/fakeCore");
    expect(seen.filter((c) => !KNOWN_COMMANDS.includes(c))).toEqual([]);
  });
});
