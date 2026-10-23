/**
 * The small amount of state getting started needs across screens.
 *
 * - The profile the per-game editor should show when none is in focus. Games
 *   used to show "the active profile, else the first one", so a profile just
 *   made from an offer could open on another game's settings.
 * - A navigation request, so a button on one screen ("New profile", "Use a
 *   recording") can take you to where that happens. The shell owns screens;
 *   it subscribes and acts.
 *
 * Held outside React like `drafts`, because both outlive the screen that set
 * them.
 */
import { useSyncExternalStore } from "react";
import type { Suggestion } from "./ipc";

/** Where a getting-started button can send you. */
export type NavRequest =
  /** Profiles, with the New profile form open. */
  | { to: "new-profile" }
  /** The per-game editor's Audio section, scrolled to `anchor` if given. */
  | { to: "audio"; anchor?: string };

let selected: string | null = null;
let pending: NavRequest | null = null;
const subs = new Set<() => void>();

function emit() {
  for (const f of [...subs]) f();
}

function subscribe(f: () => void): () => void {
  subs.add(f);
  return () => { subs.delete(f); };
}

/** The profile Games shows when nothing is in focus. */
export function getSelected(): string | null {
  return selected;
}

export function setSelected(id: string | null): void {
  if (selected === id) return;
  selected = id;
  emit();
}

export function useSelected(): string | null {
  return useSyncExternalStore(subscribe, getSelected, getSelected);
}

/** Ask the shell to go somewhere. The latest request wins. */
export function requestNav(r: NavRequest): void {
  pending = r;
  emit();
}

function getPending(): NavRequest | null {
  return pending;
}

/** The request not yet acted on, for whoever handles it. */
export function usePendingNav(): NavRequest | null {
  return useSyncExternalStore(subscribe, getPending, getPending);
}

/** Take the pending request if it is of this kind (one handler acts on it). */
export function takeNav<K extends NavRequest["to"]>(to: K): Extract<NavRequest, { to: K }> | null {
  if (pending?.to !== to) return null;
  const r = pending as Extract<NavRequest, { to: K }>;
  pending = null;
  emit();
  return r;
}

/** Forget everything (tests). */
export function resetOnboarding(): void {
  selected = null;
  pending = null;
  emit();
}

/** What a profile made from an offer is called — the window title when it
 *  reads like a name, otherwise the exe without `.exe`. Same rule as the
 *  core's `suggest::profile_name`. */
export function offerName(s: Suggestion): string {
  const t = s.title.trim();
  if (t && t.length <= 60) return t;
  return s.exe.replace(/\.exe$/i, "");
}
