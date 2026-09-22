/** "3 min ago", for a unix-seconds timestamp.
 *
 *  Coarse on purpose. A remembered PC's "last connected" is there to tell
 *  apart the one used yesterday from the one used in March, not to be a
 *  clock; a readout that ticks every second draws the eye to nothing. */
export function ago(unixSecs: number, nowMs: number = Date.now()): string {
  if (!unixSecs) return "never";
  const s = Math.max(0, Math.floor(nowMs / 1000) - unixSecs);
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return h === 1 ? "1 hour ago" : `${h} hours ago`;
  const d = Math.floor(h / 24);
  if (d < 30) return d === 1 ? "yesterday" : `${d} days ago`;
  const mo = Math.floor(d / 30);
  if (mo < 12) return mo === 1 ? "1 month ago" : `${mo} months ago`;
  const y = Math.floor(d / 365);
  return y <= 1 ? "1 year ago" : `${y} years ago`;
}
