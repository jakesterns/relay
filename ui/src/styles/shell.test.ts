/**
 * Guards for the stylesheet that jsdom cannot see, because it does no layout.
 * Each one pins a bug the layout sweep found on a real window.
 */
import { describe, expect, it } from "vitest";
import css from "./shell.css?raw";

/** Every rule whose selector list starts with exactly this class. */
const rulesFor = (cls: string) =>
  [...css.matchAll(new RegExp(`^\\.${cls}\\s*\\{([^}]*)\\}`, "gm"))].map((m) => m[1]);

describe("shell.css", () => {
  it("defines the Share instrument strip once, so nothing can squash it to a bar", () => {
    // S46's learning progress bar was also `.meter { height: 3px }`, which
    // collapsed the strip on the Share screen until it was renamed .progbar.
    const meter = rulesFor("meter");
    expect(meter).toHaveLength(1);
    expect(meter[0]).not.toMatch(/height/);
    expect(rulesFor("progbar")).toHaveLength(1);
  });

  it("renders native controls dark", () => {
    // Without it the scrollbars and an unstyled input came out white.
    expect(css).toMatch(/:root\s*\{[^}]*color-scheme:\s*dark/);
  });

  it("styles the plain text box", () => {
    // Share's pairing code is <input class="in">; it had no rule at all.
    expect(rulesFor("in")).toHaveLength(1);
  });
});
