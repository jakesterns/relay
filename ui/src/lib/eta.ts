/** S48: an honest "time left" for learning. `null` = the core cannot tell
 *  yet (too little play, or a missing kind of scene), and then we say so
 *  rather than guess. Minutes are rounded up: "about 4 min" never means 4:50. */
export function etaText(secs: number | null | undefined, unknown = "time left not known yet"): string {
  if (secs === null || secs === undefined || !Number.isFinite(secs)) return unknown;
  if (secs <= 0) return "ready";
  if (secs < 60) return "less than a minute left";
  return `about ${Math.ceil(secs / 60)} min left`;
}

/** "12 s of 3 min", for a video's position. */
export function clockText(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  const m = Math.floor(s / 60);
  return m > 0 ? `${m}:${String(s % 60).padStart(2, "0")}` : `${s} s`;
}
