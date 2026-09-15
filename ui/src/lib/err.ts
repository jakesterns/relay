/**
 * One way to turn a rejection into a sentence.
 *
 * Tauri rejects with whatever the command returned, which for this core is a
 * string most of the time and an object the rest of the time. `String(e)` on
 * the latter renders `[object Object]`, which is how a real failure ends up
 * looking like a UI bug. Every screen imports this rather than writing its own.
 */
export function errText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object") {
    const rec = e as Record<string, unknown>;
    for (const k of ["message", "error", "reason"] as const) {
      const v = rec[k];
      if (typeof v === "string" && v.trim()) return v;
    }
    try {
      const json = JSON.stringify(e);
      if (json && json !== "{}") return json;
    } catch {
      /* circular: fall through to the generic line */
    }
    return "Unknown error";
  }
  return String(e);
}
