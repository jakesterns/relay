import { useCore } from "../lib/core";

/**
 * Backend `notice` events, on screen.
 *
 * The core has always emitted these — a hotkey that could not do what it was
 * asked, an elevated install step, a restore from the notification-area icon —
 * and until now every one of them was dropped on the floor. Pressing a hotkey
 * and getting no acknowledgement at all is indistinguishable from the app
 * being broken.
 *
 * They stack, they never take focus, and they expire on their own after a few
 * seconds (`NOTICE_MS` in lib/core). Nothing here is dismissible, because
 * nothing here is important enough to make somebody click it away.
 */
export function Toasts() {
  const { notices } = useCore();
  if (!notices.length) return null;
  return (
    <div className="toasts" role="status" aria-live="polite">
      {notices.map((n) => (
        <div className="toast" key={n.id}>{n.text}</div>
      ))}
    </div>
  );
}
