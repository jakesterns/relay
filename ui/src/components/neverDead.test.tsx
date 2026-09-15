/**
 * The S23 promises, as tests.
 *
 * Three things the app got wrong and must not get wrong again:
 *
 *  1. Opening Relay with no core running reached a dead end whose only way out
 *     was a terminal command. It now starts the core itself, and says so.
 *  2. The core has always emitted `notice` events; nothing ever rendered them,
 *     so every hotkey acknowledgement went in the bin.
 *  3. Closing the window leaves Relay running, which is correct and was also
 *     completely unstated.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { act } from "@testing-library/react";
import { renderApp, renderScreen, settle } from "../test/render";
import { makeFakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Profiles } from "../screens/Profiles";
import { Settings } from "../screens/Settings";
import { NOTICE_MS } from "../lib/core";

describe("reaching live state without a terminal", () => {
  beforeEach(() => tauri.useOfflineCore());

  it("offers a button rather than a command, in every offline string", async () => {
    renderScreen(<Profiles />);
    await settle();

    const banner = document.querySelector(".offline");
    expect(banner).not.toBeNull();
    expect(banner!.textContent).toMatch(/Relay is not running/);
    // The old banner rendered the command in a <code>. Nothing may again.
    expect(banner!.querySelector("code")).toBeNull();
    expect(document.body.textContent).not.toMatch(/relay-core|relay-svc/);
    expect(screen.getByRole("button", { name: "Start Relay" })).toBeInTheDocument();
  });

  it("the button asks the shell to start the core", async () => {
    const h = renderScreen(<Profiles />);
    await settle();

    await h.user.click(screen.getByRole("button", { name: "Start Relay" }));
    await settle();
    expect(tauri.commandNames()).toContain("start_core");
  });

  it("says it is starting while the shell's own attempt is in flight", async () => {
    renderScreen(<Profiles />);
    await settle();

    await act(async () => { tauri.emit("core://starting"); });
    expect(screen.getByText(/Starting Relay/)).toBeInTheDocument();
    // Progress, not a fault: the banner drops its error styling.
    expect(document.querySelector(".offline.working")).not.toBeNull();
  });

  it("shows the core's own explanation when it cannot start, and offers a retry", async () => {
    renderScreen(<Profiles />);
    await settle();

    // Exactly the shape `startup::StartError::message` produces.
    const message =
      "Windows would not start Relay's background service: Access is denied. " +
      "This is usually security software blocking it — allow Relay in your " +
      "antivirus or security settings, then try again.";
    await act(async () => { tauri.emit("core://start-failed", message); });

    expect(screen.getByText(message)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("clears the banner once a core answers", async () => {
    const core = makeFakeCore();
    renderScreen(<Profiles />);
    await settle();
    expect(document.querySelector(".offline")).not.toBeNull();

    // The shell reports success and the context re-reads the core.
    tauri.useFakeCore(core.handler);
    await act(async () => { tauri.emit("core://started"); });
    await waitFor(() => expect(document.querySelector(".offline")).toBeNull());
  });
});

describe("notices reach the screen", () => {
  // The whole shell, because that is where `<Toasts />` is mounted — a screen
  // on its own would pass this suite while the real app rendered nothing.
  beforeEach(() => {
    const core = makeFakeCore();
    tauri.useFakeCore(core.handler);
  });

  it("renders a pushed notice and expires it quietly", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      renderApp();
      await settle();

      await act(async () => { tauri.emit("core://notice", "Preview on"); });
      expect(screen.getByText("Preview on")).toBeInTheDocument();

      // It goes on its own — no dismiss control, nothing to click away.
      await act(async () => { vi.advanceTimersByTime(NOTICE_MS + 50); });
      expect(screen.queryByText("Preview on")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("stacks notices, and one does not cut the next one short", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      renderApp();
      await settle();

      await act(async () => { tauri.emit("core://notice", "Relay Camera registered"); });
      // Half of the first notice's life later, a second one arrives.
      await act(async () => { vi.advanceTimersByTime(NOTICE_MS / 2); });
      await act(async () => { tauri.emit("core://notice", "Everything restored"); });

      expect(document.querySelectorAll(".toast")).toHaveLength(2);

      // The first expires; the second still has half its time left.
      await act(async () => { vi.advanceTimersByTime(NOTICE_MS / 2 + 50); });
      expect(screen.queryByText("Relay Camera registered")).not.toBeInTheDocument();
      expect(screen.getByText("Everything restored")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("repeats of the same text are two notices, not one", async () => {
    renderApp();
    await settle();

    await act(async () => { tauri.emit("core://notice", "Preview on"); });
    await act(async () => { tauri.emit("core://notice", "Preview on"); });
    expect(document.querySelectorAll(".toast")).toHaveLength(2);
  });

  it("does not steal focus or block the page", async () => {
    renderApp();
    await settle();
    await act(async () => { tauri.emit("core://notice", "Preview on"); });

    const toasts = document.querySelector(".toasts")!;
    // Announced to screen readers, but never a dialog and never focusable.
    expect(toasts.getAttribute("role")).toBe("status");
    expect(toasts.getAttribute("aria-live")).toBe("polite");
    expect(toasts.querySelector("button")).toBeNull();
  });
});

describe("what keeps running after the window closes", () => {
  let core: ReturnType<typeof makeFakeCore>;
  beforeEach(() => {
    core = makeFakeCore();
    tauri.useFakeCore(core.handler);
  });

  it("states it plainly, and names the tray as the way back", async () => {
    renderScreen(<Settings />);
    await settle();

    const card = [...document.querySelectorAll(".card")]
      .find((c) => (c.textContent ?? "").includes("When you close the window"))!;
    expect(card).toBeDefined();
    expect(card.textContent).toMatch(/Relay keeps working after you close the window/);
    expect(card.textContent).toMatch(/notification area/);
    // Names the menu the core actually puts there.
    expect(card.textContent).toMatch(/Open Relay/);
    expect(card.textContent).toMatch(/Quit Relay/);
  });

  it("defaults to keeping Relay running", async () => {
    renderScreen(<Settings />);
    await settle();
    expect(core.prefs.close_action).toBe("keep_running");
    expect(screen.getByText(/Closing the window leaves Relay running/)).toBeInTheDocument();
  });

  it("the preference is the user's, and it is saved", async () => {
    const h = renderScreen(<Settings />);
    await settle();

    await h.user.click(screen.getByText("Quit Relay completely when I close the window"));
    await settle();

    expect(tauri.lastCall("set_ui_prefs")?.args).toEqual({
      prefs: { close_action: "quit_relay" },
    });
    expect(core.prefs.close_action).toBe("quit_relay");
    // And the explanation switches to what now happens.
    expect(screen.getByText(/Closing the window stops Relay and restores/)).toBeInTheDocument();
  });
});
