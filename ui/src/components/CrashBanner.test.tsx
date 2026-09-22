/**
 * The one line about the last crash (S38): shown once, acknowledged once,
 * and never inside the toasts, which must stay button-free.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { CrashBanner } from "./CrashBanner";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

describe("CrashBanner", () => {
  it("shows nothing when the last run ended cleanly", async () => {
    renderScreen(<CrashBanner />);
    await settle();
    expect(screen.queryByTestId("crash-banner")).not.toBeInTheDocument();
  });

  it("names the crash once and tells the core when it has been read", async () => {
    core.state.last_crash =
      "Relay did not shut down cleanly last time: exit: 3 (1700000000-relay-share-exit3.txt).";
    tauri.useFakeCore(core.handler);
    const h = renderScreen(<CrashBanner />);
    await settle();
    const banner = screen.getByTestId("crash-banner");
    expect(banner).toHaveTextContent(/exit: 3/);
    // Where to look, in words a person can act on.
    expect(banner).toHaveTextContent(/logs\\crash/);

    await h.user.click(screen.getByRole("button", { name: "OK" }));
    await settle();
    expect(tauri.lastCall("ack_crash")).toBeDefined();
    expect(core.state.last_crash).toBeNull();
    expect(screen.queryByTestId("crash-banner")).not.toBeInTheDocument();
  });
});
