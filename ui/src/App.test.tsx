/**
 * The shell: the first-run gate, the rail, and the line at the bottom of the
 * rail that promises nothing on this PC was changed.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { push, renderApp, settle } from "./test/render";
import { makeFakeCore, type FakeCore } from "./test/fakeCore";
import * as tauri from "./test/tauriMock";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderApp();
  await settle();
  return h;
}

const rail = () => within(document.querySelector(".rail") as HTMLElement);
const subtitle = () => (document.querySelector(".brand span")?.textContent ?? "").trim();
const railNote = () => (document.querySelector(".rail > small")?.textContent ?? "").trim();
const heading = () => screen.getByRole("heading", { level: 1 }).textContent ?? "";
const pushState = () => push(() => tauri.emit("core://state", structuredClone(core.state)));

describe("the first-run gate", () => {
  it("holds the app until the consent question is answered, then lets it through", async () => {
    core.vdevice.consent = null;
    tauri.useFakeCore(core.handler);
    const h = await mount();

    expect(heading()).toContain("Before anything is installed");
    expect(subtitle()).toBe("First run");
    expect(document.querySelector(".rail")).toBeNull();

    await h.user.click(screen.getByRole("button", { name: "Continue" }));
    await settle();

    expect(heading()).toContain("Profiles");
    expect(core.vdevice.consent).toMatchObject({ apo: false, camera: false, microphone: false });
    h.expectClean();
  });

  it("is skipped once a decision exists on disk", async () => {
    const h = await mount();
    expect(heading()).toContain("Profiles");
    expect(screen.queryByRole("button", { name: "Continue" })).not.toBeInTheDocument();
    h.expectClean();
  });
});

describe("the rail", () => {
  it("reaches every screen", async () => {
    const h = await mount();
    const go = async (label: string) => {
      await h.user.click(rail().getByText(label));
      await settle();
    };

    await go("Share");
    expect(heading()).toContain("Display 1");
    await go("Receive");
    expect(heading()).toContain("Receive");
    await go("Audio");
    expect(heading()).toContain("— Audio");
    await go("Display");
    expect(heading()).toContain("— Display");
    // Games keeps whichever section you were last on.
    await go("Games");
    expect(heading()).toContain("— Display");
    await h.user.click(screen.getByRole("button", { name: "Sharing" }));
    await settle();
    expect(heading()).toContain("— Sharing");
    expect(rail().getByText("Games").closest("button")).toHaveAttribute("aria-current", "page");
    await go("Settings");
    expect(heading()).toBe("Settings");
    await go("Profiles");
    expect(heading()).toBe("Profiles");
    h.expectClean();
  });

  it("marks the entry the user is on, and Audio/Display separately from Games", async () => {
    const h = await mount();
    await h.user.click(rail().getByText("Display"));
    await settle();
    expect(rail().getByText("Display").closest("button")).toHaveAttribute("aria-current", "page");
    expect(rail().getByText("Audio").closest("button")).not.toHaveAttribute("aria-current");
  });

  it("shows what is running now", async () => {
    const h = await mount();
    expect(rail().getByText("Sharing").closest(".st")).not.toHaveClass("on");
    expect(within(rail().getByText("Game profile").closest(".st") as HTMLElement).getByText("none")).toBeInTheDocument();

    core.state.sharing = { kind: "sharing", peer: "living-room-pc" };
    core.state.active_profile = { id: "1", name: "Call of Duty", note: "", exe: "cod.exe", headset: null, monitor: null, share: "game", status: "ready" };
    await pushState();

    expect(rail().getByText("Sharing").closest(".st")).toHaveClass("on");
    // The rail is narrow, so a multi-word game name is initialled.
    expect(rail().getByText("COD")).toBeInTheDocument();
    h.expectClean();
  });
});

describe("what the shell claims about this PC", () => {
  it("says nothing is applied while nothing is", async () => {
    await mount();
    expect(subtitle()).toBe("Idle");
    expect(railNote()).toBe(
      "Nothing is applied right now. Your desktop, apps, and audio are exactly as Windows set them.",
    );
  });

  it("says a profile only lasts while the game has focus", async () => {
    core.state.active_profile = { id: "1", name: "Call of Duty", note: "", exe: "cod.exe", headset: null, monitor: null, share: "game", status: "ready" };
    await mount();
    await pushState();
    expect(subtitle()).toBe("Call of Duty · profile active");
    expect(railNote()).toBe("Applies only while the game has focus. Restored the moment you alt-tab.");
  });

  it("says a share stays on the LAN", async () => {
    core.state.sharing = { kind: "sharing", peer: "living-room-pc" };
    await mount();
    await pushState();
    expect(subtitle()).toBe("Sending to living-room-pc");
    expect(railNote()).toBe("Everything stays on your local network. Nothing on this PC was changed.");
  });

  it("says the core is offline rather than reporting stale state as live", async () => {
    tauri.useOfflineCore();
    await mount();
    expect(subtitle()).toBe("Not running");
  });
});

describe("the title bar", () => {
  it("drives the Tauri window rather than the document", async () => {
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Minimize" }));
    await h.user.click(screen.getByRole("button", { name: "Maximize" }));
    await h.user.click(screen.getByRole("button", { name: "Close" }));
    await settle();
    expect(tauri.windowActions).toEqual(["minimize", "toggleMaximize", "close"]);
    h.expectClean();
  });

  it("does nothing outside Tauri, where there is no window to drive", async () => {
    tauri.useMockData();
    const h = await mount();
    await h.user.click(screen.getByRole("button", { name: "Close" }));
    await settle();
    expect(tauri.windowActions).toEqual([]);
  });
});
