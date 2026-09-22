/**
 * The seam every test drives the UI through.
 *
 * `src/lib/ipc.ts` reaches the desktop in exactly three places — dynamic
 * imports of `@tauri-apps/api/{core,event,window}`. `setup.ts` points all
 * three at this module, so a test can play any of the three situations the
 * app really meets:
 *
 *   - **mock**    `__TAURI_INTERNALS__` absent; `ipc.ts` serves its own
 *                 in-memory data (what `pnpm dev` in a browser shows).
 *   - **offline** running inside Tauri, but no core behind the pipe: every
 *                 `invoke` rejects.
 *   - **core**    a scripted fake core answers commands and can push events.
 *
 * Nothing here touches the real desktop: `invoke` never leaves the process
 * and no synthetic input is sent to the OS.
 */

export interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

export type InvokeHandler = (cmd: string, args: Record<string, unknown>) => unknown;

/** Every command the UI issued, in order. Assert against this. */
export const calls: Call[] = [];

let handler: InvokeHandler | null = null;
const listeners = new Map<string, Set<(payload: unknown) => void>>();

export const windowActions: string[] = [];

/** Commands the fake core was asked for but does not implement. */
export const unhandled: string[] = [];

function markTauri(on: boolean) {
  const w = window as unknown as Record<string, unknown>;
  if (on) w.__TAURI_INTERNALS__ = { invoke: () => undefined };
  else delete w.__TAURI_INTERNALS__;
}

/** Browser mode: `ipc.ts` answers from its own mock data. */
export function useMockData() {
  markTauri(false);
  handler = null;
}

/** Inside Tauri, core not reachable. Every command rejects. */
export function useOfflineCore(message = "core service is not running") {
  markTauri(true);
  handler = () => {
    throw new Error(message);
  };
}

/** Inside Tauri with a scripted core behind the pipe. */
export function useFakeCore(h: InvokeHandler) {
  markTauri(true);
  handler = h;
}

export function reset() {
  calls.length = 0;
  unhandled.length = 0;
  windowActions.length = 0;
  listeners.clear();
  handler = null;
  markTauri(false);
}

export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  calls.push({ cmd, args });
  if (!handler) throw new Error(`no core behind the pipe (${cmd})`);
  return (await handler(cmd, args)) as T;
}

export function convertFileSrc(path: string): string {
  return `asset://localhost/${encodeURIComponent(path)}`;
}

export async function listen(
  event: string,
  cb: (e: { payload: unknown }) => void,
): Promise<() => void> {
  const wrapped = (payload: unknown) => cb({ payload });
  const set = listeners.get(event) ?? new Set();
  set.add(wrapped);
  listeners.set(event, set);
  return () => set.delete(wrapped);
}

/** Push a core event to whoever subscribed. Wrap the call in `act()`. */
export function emit(event: string, payload?: unknown) {
  listeners.get(event)?.forEach((f) => f(payload));
}

export function listenerCount(event: string): number {
  return listeners.get(event)?.size ?? 0;
}

export function getCurrentWindow() {
  return {
    minimize: async () => void windowActions.push("minimize"),
    toggleMaximize: async () => void windowActions.push("toggleMaximize"),
    close: async () => void windowActions.push("close"),
  };
}

/** Every command issued, deduplicated, in first-seen order. */
export function commandNames(): string[] {
  return [...new Set(calls.map((c) => c.cmd))];
}

/** Every call of one command, in order. */
export function callsOf(cmd: string): Call[] {
  return calls.filter((c) => c.cmd === cmd);
}

export function lastCall(cmd: string): Call | undefined {
  return [...calls].reverse().find((c) => c.cmd === cmd);
}
