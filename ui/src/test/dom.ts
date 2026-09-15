/**
 * Queries for the shapes this UI uses that Testing Library has no role for.
 *
 * The design leans on `<div class="kv">`, `<div class="field">` and the
 * instrument strip rather than on labelled form controls, so these helpers
 * pin a value to the label printed next to it instead of to a DOM path that
 * a restyle would break.
 */
import { within } from "@testing-library/react";

function scopeOf(scope?: HTMLElement): HTMLElement {
  return scope ?? document.body;
}

/** The input/select/textarea of a `.field`, `.sl` (slider) or `.chips` row,
 *  found by the label printed beside it. */
export function field(label: string, scope?: HTMLElement): HTMLElement {
  const span = [...scopeOf(scope).querySelectorAll("span")]
    .find((s) => (s.textContent ?? "").trim() === label);
  if (!span?.parentElement) throw new Error(`no field labelled "${label}"`);
  const el = span.parentElement.querySelector("input, select, textarea");
  if (!el) throw new Error(`field "${label}" has no control`);
  return el as HTMLElement;
}

/** A `<Slider>` row, by its label. A disabled slider renders no `<input>` at
 *  all and carries `.dis`, which is the only way to tell the two apart. */
export function slider(label: string, scope?: HTMLElement): {
  el: HTMLElement; input: HTMLInputElement | null; disabled: boolean; value: string;
} {
  const span = [...scopeOf(scope).querySelectorAll(".sl > span")]
    .find((x) => (x.textContent ?? "").trim() === label);
  const el = span?.parentElement;
  if (!el) throw new Error(`no slider labelled "${label}"`);
  return {
    el,
    input: el.querySelector("input[type=range]"),
    disabled: el.classList.contains("dis"),
    value: (el.querySelector(".val")?.textContent ?? "").trim(),
  };
}

/** The value beside a `<Kv k=… v=…>` key. */
export function kv(key: string, scope?: HTMLElement): string {
  const row = [...scopeOf(scope).querySelectorAll(".kv")]
    .find((r) => (r.querySelector("span")?.textContent ?? "").trim() === key);
  if (!row) throw new Error(`no Kv row for "${key}"`);
  return (row.querySelector("strong")?.textContent ?? "").trim();
}

/** One instrument-strip cell: its big value and the hint under it. */
export function readout(label: string): { value: string; hint: string } {
  const el = [...document.querySelectorAll(".meter label")]
    .find((l) => (l.textContent ?? "").includes(label));
  if (!el?.parentElement) throw new Error(`no instrument readout for "${label}"`);
  return {
    value: (el.parentElement.querySelector(".v")?.textContent ?? "").trim(),
    hint: (el.parentElement.querySelector(".hint")?.textContent ?? "").trim(),
  };
}

/** The `.card` whose heading matches — cards are the unit of layout here, and
 *  scoping a query to one is how these tests stay unambiguous. */
export function card(heading: RegExp | string): HTMLElement {
  const match = (t: string) => (typeof heading === "string" ? t.startsWith(heading) : heading.test(t));
  const h = [...document.querySelectorAll<HTMLElement>(".card h3")]
    .find((e) => match((e.textContent ?? "").trim()));
  const el = h?.closest(".card");
  if (!el) throw new Error(`no card headed ${heading}`);
  return el as HTMLElement;
}

/** `within(card(heading))`, which is how most assertions read. */
export function inCard(heading: RegExp | string) {
  return within(card(heading));
}

/** The lines of a mono listing (dry-run keys, uninstall plan steps). Block
 *  `div.mono` only — the same class is used inline in prose to set a path in
 *  the monospace face, and those are not listing lines. */
export function monoLines(scope?: HTMLElement): string[] {
  return [...scopeOf(scope).querySelectorAll("div.mono")]
    .map((e) => (e.textContent ?? "").trim())
    .filter(Boolean);
}
