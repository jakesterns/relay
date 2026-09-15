/** Global test setup. The three Tauri entry points are redirected at the
 *  in-process fake by `resolve.alias` in vitest.config.ts; this file resets
 *  that fake between tests and fills the DOM APIs jsdom leaves out. */
import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";
import * as tauri from "./tauriMock";

/** What `window.confirm` returns next. The hardware-library Remove button is
 *  the only caller; jsdom would otherwise throw "not implemented". */
export const confirmResult = { value: true };
window.confirm = () => confirmResult.value;

// jsdom has no media stack; the A/B card constructs an Audio element.
Object.defineProperty(window.HTMLMediaElement.prototype, "play", {
  configurable: true,
  value: () => Promise.resolve(),
});
Object.defineProperty(window.HTMLMediaElement.prototype, "pause", {
  configurable: true,
  value: () => {},
});

if (!("randomUUID" in crypto)) {
  Object.defineProperty(crypto, "randomUUID", {
    configurable: true,
    value: () => "00000000-0000-4000-8000-" + String(Date.now()).padStart(12, "0").slice(-12),
  });
}

afterEach(() => {
  cleanup();
  tauri.reset();
  confirmResult.value = true;
});
