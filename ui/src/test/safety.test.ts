/**
 * The standing rule for this suite, made executable.
 *
 * A previous attempt at UI testing drove a real Tauri window with WebDriver.
 * It moved the actual cursor, and its clicks landed in whatever the user had
 * in front of them. Nothing here may do that, and these tests are what stops
 * it being reintroduced quietly.
 */
import { describe, expect, it } from "vitest";
import pkg from "../../package.json";
import * as core from "@tauri-apps/api/core";
import * as event from "@tauri-apps/api/event";
import * as win from "@tauri-apps/api/window";
import * as fake from "./tauriMock";

/** Anything that can synthesise OS-level input or drive a real browser. */
const FORBIDDEN = [
  "webdriverio", "tauri-driver", "@wdio/cli", "selenium-webdriver",
  "playwright", "@playwright/test", "puppeteer", "puppeteer-core",
  "robotjs", "@nut-tree/nut-js", "nut-js", "cypress",
];

describe("the suite cannot reach the desktop", () => {
  it("resolves every Tauri entry point to the in-process fake", () => {
    // If the alias in vitest.config.ts is ever dropped, `invoke` goes back to
    // the real IPC bridge and these identities stop holding.
    expect(core.invoke).toBe(fake.invoke);
    expect(event.listen).toBe(fake.listen);
    expect(win.getCurrentWindow).toBe(fake.getCurrentWindow);
  });

  it("depends on nothing that can move the mouse or drive a real window", () => {
    const declared = Object.keys({ ...pkg.dependencies, ...pkg.devDependencies });
    expect(declared.filter((d) => FORBIDDEN.includes(d))).toEqual([]);
  });

  it("runs in jsdom, where there is no host window to steal focus from", () => {
    expect(typeof window).toBe("object");
    expect(navigator.userAgent).toMatch(/jsdom/i);
  });

  it("leaves no Tauri marker on the window between tests", () => {
    // `reset()` in setup.ts runs after every test; a leaked marker would make
    // the next file's mock-data tests silently talk to a dead bridge instead.
    expect("__TAURI_INTERNALS__" in window).toBe(false);
  });
});
