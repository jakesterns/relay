/**
 * One confirmation pattern, everywhere.
 *
 * Relay used to have four: an instant preset delete, an unconfirmed
 * "Restore original state", a native `window.confirm` for the hardware
 * library, and the two-step delete in the profile editor. The two-step one
 * won. These tests hold the line — including the part where no native dialog
 * is ever opened, which matters because this window has custom chrome and a
 * native dialog in it looks like it belongs to another program.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, screen, within } from "@testing-library/react";
import { renderScreen, settle } from "./render";
import { card, inCard } from "./dom";
import { makeFakeCore, type FakeCore } from "./fakeCore";
import * as tauri from "./tauriMock";
import { ConfirmButton, CONFIRM_MS } from "../components/Controls";
import { Profiles } from "../screens/Profiles";
import { Settings } from "../screens/Settings";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

describe("the confirm button", () => {
  const mount = (onConfirm: () => void) =>
    renderScreen(<ConfirmButton label="Delete…" confirm="Confirm delete" onConfirm={onConfirm} />);

  it("takes two presses", async () => {
    const hit = vi.fn();
    const h = mount(hit);
    await h.user.click(screen.getByRole("button", { name: "Delete…" }));
    expect(hit).not.toHaveBeenCalled();
    await h.user.click(screen.getByRole("button", { name: "Confirm delete" }));
    expect(hit).toHaveBeenCalledTimes(1);
    // And it goes back to resting, so a third click cannot fire it again.
    expect(screen.getByRole("button", { name: "Delete…" })).toBeInTheDocument();
  });

  it("works from the keyboard", async () => {
    const hit = vi.fn();
    const h = mount(hit);
    screen.getByRole("button", { name: "Delete…" }).focus();
    await h.user.keyboard("{Enter}");
    expect(hit).not.toHaveBeenCalled();
    await h.user.keyboard("{Enter}");
    expect(hit).toHaveBeenCalledTimes(1);
  });

  it("disarms on Escape", async () => {
    const hit = vi.fn();
    const h = mount(hit);
    const b = screen.getByRole("button", { name: "Delete…" });
    b.focus();
    await h.user.keyboard("{Enter}");
    await h.user.keyboard("{Escape}");
    expect(screen.getByRole("button", { name: "Delete…" })).toBeInTheDocument();
    expect(hit).not.toHaveBeenCalled();
  });

  it("disarms when focus leaves it", async () => {
    const hit = vi.fn();
    const h = mount(hit);
    await h.user.click(screen.getByRole("button", { name: "Delete…" }));
    await act(async () => { screen.getByRole("button", { name: "Confirm delete" }).blur(); });
    expect(screen.getByRole("button", { name: "Delete…" })).toBeInTheDocument();
    expect(hit).not.toHaveBeenCalled();
  });

  it("disarms itself after a few seconds", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const hit = vi.fn();
      const h = mount(hit);
      await h.user.click(screen.getByRole("button", { name: "Delete…" }));
      await vi.advanceTimersByTimeAsync(CONFIRM_MS + 50);
      expect(screen.getByRole("button", { name: "Delete…" })).toBeInTheDocument();
      expect(hit).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("every destructive action", () => {
  it("deletes a profile only on the second press", async () => {
    const h = renderScreen(<Profiles />);
    await settle();
    await h.user.click(document.querySelector(".rowname") as HTMLElement);
    await settle();
    const form = card("Edit profile");
    await h.user.click(within(form).getByRole("button", { name: "Delete…" }));
    expect(tauri.lastCall("delete_profile")).toBeUndefined();
    await h.user.click(within(form).getByRole("button", { name: "Confirm delete" }));
    await settle();
    expect(tauri.lastCall("delete_profile")).toBeDefined();
  });

  it("removes hardware only on the second press", async () => {
    const h = renderScreen(<Profiles />);
    await settle();
    const hw = inCard("Headsets & IEMs");
    await h.user.click(hw.getAllByRole("button", { name: "Remove" })[0]);
    expect(tauri.lastCall("delete_hardware")).toBeUndefined();
    await h.user.click(hw.getByRole("button", { name: "Remove HD 560S?" }));
    await settle();
    expect(tauri.lastCall("delete_hardware")?.args).toEqual({ id: "hd560s" });
  });

  it("restores original state only on the second press, and says it worked", async () => {
    const h = renderScreen(<Settings />);
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Restore original state now" }));
    expect(tauri.lastCall("restore_all")).toBeUndefined();
    await h.user.click(screen.getByRole("button", { name: "Confirm restore" }));
    await settle();
    expect(tauri.lastCall("restore_all")).toBeDefined();
    expect(screen.getByRole("status")).toHaveTextContent(/back to the settings Windows had/);
  });

  it("says so when the restore fails, instead of looking like it worked", async () => {
    core.fail.set("restore_all", "display backup file is missing");
    tauri.useFakeCore(core.handler);
    const h = renderScreen(<Settings />);
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Restore original state now" }));
    await h.user.click(screen.getByRole("button", { name: "Confirm restore" }));
    await settle();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("display backup file is missing");
    await h.user.click(within(alert).getByRole("button", { name: "Dismiss this message" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});

describe("native dialogs", () => {
  // Every source file, as text, straight from Vite. No node:fs — this suite
  // runs in jsdom with no @types/node, and the glob is what the bundler
  // already knows.
  const sources = import.meta.glob("../**/*.{ts,tsx}", {
    query: "?raw", import: "default", eager: true,
  }) as Record<string, string>;

  /** Comments mention these deliberately (this file included), so strip them
   *  before looking for a call. */
  const code = (text: string) =>
    text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");

  it("are gone from the source", () => {
    const offenders = Object.entries(sources)
      .filter(([path]) => !path.includes("/test/") && !path.endsWith(".test.ts") && !path.endsWith(".test.tsx"))
      .filter(([, text]) => /(?:^|[^\w.$])(?:window\.)?(?:confirm|alert|prompt)\s*\(/.test(code(text)))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });

  it("would fail loudly if one came back", () => {
    expect(() => window.confirm("x")).toThrow(/ConfirmButton/);
    expect(() => window.alert("x")).toThrow(/ConfirmButton/);
  });
});
