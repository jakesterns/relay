import { useCore } from "../lib/core";

/**
 * Shown when running inside Tauri but the core service is not reachable.
 *
 * This used to tell people to open a terminal and type a command, which is the
 * one thing an installed desktop app must never do. Now the shell starts the
 * core itself; this banner reports that attempt, and if it failed it says why
 * in terms the reader can act on and offers to try again.
 */
export function OfflineBanner() {
  const { offline, checked, mock, start, startCore } = useCore();
  // Nothing is claimed until the first status round-trip has answered — a
  // healthy launch must not flash "not running" on its way to live state.
  if (!offline || mock || !checked) return null;

  if (start.kind === "starting") {
    return (
      <div className="offline working">
        <i />
        Starting Relay…
      </div>
    );
  }

  if (start.kind === "failed") {
    return (
      <div className="offline">
        <i />
        <span>{start.message}</span>
        <button className="btn tiny" onClick={() => void startCore()}>Try again</button>
      </div>
    );
  }

  // Offline with no attempt in flight: the shell's start finished but the core
  // has since gone away (it was quit, or it crashed). One button, no prose
  // about services.
  return (
    <div className="offline">
      <i />
      <span>Relay is not running.</span>
      <button className="btn tiny" onClick={() => void startCore()}>Start Relay</button>
    </div>
  );
}
