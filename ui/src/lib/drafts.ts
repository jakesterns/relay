/**
 * The one unsaved profile edit, held outside React's tree.
 *
 * The Games screen is unmounted the moment you touch the rail, and the
 * profile it was editing is refetched whenever the focused game changes.
 * Both used to throw away whatever had been typed without saying so. Keeping
 * the draft here instead means navigating away, or alt-tabbing into another
 * game, leaves the edit exactly where it was — and the rail can say so,
 * because anything can subscribe.
 *
 * One draft is enough: Games edits one profile at a time, and a second one
 * would only raise the question of which is on screen.
 */
import { useSyncExternalStore } from "react";
import type { Profile } from "./ipc";

export interface Draft {
  /** The profile being edited. Its `id` is the subject it belongs to. */
  profile: Profile;
}

let draft: Draft | null = null;
const subs = new Set<() => void>();

function emit() {
  for (const f of [...subs]) f();
}

function subscribe(f: () => void): () => void {
  subs.add(f);
  return () => { subs.delete(f); };
}

export function getDraft(): Draft | null {
  return draft;
}

/** Record (or replace) the unsaved edit. */
export function setDraft(profile: Profile): void {
  draft = { profile };
  emit();
}

/** Throw the unsaved edit away — only ever from an explicit user action. */
export function clearDraft(): void {
  if (draft === null) return;
  draft = null;
  emit();
}

/** Subscribe a component to the draft. */
export function useDraft(): Draft | null {
  return useSyncExternalStore(subscribe, getDraft, getDraft);
}
