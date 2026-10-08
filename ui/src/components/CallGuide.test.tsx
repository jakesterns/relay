/**
 * S50 "share and go": the one-card guide to getting the received stream into
 * Discord, Zoom, Teams, Meet or OBS, and the window title it tells people to
 * pick, which must match what the engine actually names the window.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { push, renderScreen, settle } from "../test/render";
import { kv } from "../test/dom";
import { makeFakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { CallGuide, streamWindowTitle } from "./CallGuide";
import { Receive } from "../screens/Receive";

beforeEach(() => {
  tauri.useFakeCore(makeFakeCore().handler);
});

describe("the stream window's title", () => {
  // The same cases as `placement::tests::titles_name_the_sender` in Rust.
  it("matches the engine's title rule", () => {
    expect(streamWindowTitle("JAKE")).toBe("Relay — from JAKE");
    expect(streamWindowTitle("  den-pc  ")).toBe("Relay — from den-pc");
    expect(streamWindowTitle(null)).toBe("Relay — receiving");
    expect(streamWindowTitle("   ")).toBe("Relay — receiving");
    expect(streamWindowTitle("a\u0007b\nc")).toBe("Relay — from abc");
    expect(streamWindowTitle("x".repeat(80))).toBe(`Relay — from ${"x".repeat(48)}…`);
    expect(streamWindowTitle("é".repeat(60)).endsWith("é…")).toBe(true);
  });
});

describe("the call guide", () => {
  it("names the window to pick, one line per app, and the camera alternative", async () => {
    const h = renderScreen(<CallGuide sender="JAKE" />);
    await settle();
    expect(screen.getByText("Use with Discord / Zoom / Teams / Meet / OBS")).toBeInTheDocument();
    expect(screen.getByTestId("guide-window").textContent).toContain("“Relay — from JAKE”");
    expect(screen.getByTestId("guide-window").textContent).toMatch(/full picture, plus the sound/);
    expect(kv("Discord")).toMatch(/Go Live/);
    expect(kv("Zoom")).toMatch(/Share sound/);
    expect(kv("Teams")).toMatch(/Include sound/);
    expect(kv("Meet")).toMatch(/Picture only/);
    expect(kv("OBS")).toMatch(/Window Capture/);
    expect(screen.getByTestId("guide-camera").textContent).toMatch(/Relay Camera.*picture only, no sound/);
    h.expectClean();
  });

  it("is on the Receive screen and follows the paired sender", async () => {
    const h = renderScreen(<Receive />);
    await settle();
    expect(screen.getByTestId("guide-window").textContent).toContain("“Relay — receiving”");
    await push(() => tauri.emit("core://receive-status", { receiving: true, sender: "JAKE" }));
    expect(screen.getByTestId("guide-window").textContent).toContain("“Relay — from JAKE”");
    h.expectClean();
  });
});
