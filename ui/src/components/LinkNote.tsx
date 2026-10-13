/** S49: which network the share runs over, and what Relay is doing about it.
 *
 *  Shown only when either PC is on Wi-Fi. On a wired share nothing here
 *  renders and the screens look exactly as they did before S49.
 *
 *  The wording is a recommendation and a description, never a verdict: the
 *  link is short of room, Relay is adjusting. It never says the user did
 *  anything wrong, and it never uses alarm styling — it is a readout in the
 *  instrument-strip idiom, like the rest of the strip. */

import type { AdaptInfo, LinkInfo, ShareStats } from "../lib/ipc";

export interface LinkView {
  /** Either end is on Wi-Fi. */
  wifi: boolean;
  /** For the strip: this PC's link and, as a hint, the other PC's. */
  label: string;
  hint: string;
  /** The calm note: what helps, and what Relay is doing now. */
  note: string;
}

/** What Relay is doing, in words, from the sender's `adapt` object. */
export function adaptLine(a: AdaptInfo | undefined): string | null {
  if (!a) return null;
  if (a.note) return a.note;
  return null;
}

/** Pure: a `stats` line in, what to show out (`null` on wired or unknown). */
export function linkView(s: Pick<ShareStats, "link" | "peer_link" | "adapt"> | null | undefined): LinkView | null {
  const here: LinkInfo | undefined = s?.link;
  const there: LinkInfo | undefined = s?.peer_link;
  const wifiHere = !!here?.wifi;
  const wifiThere = !!there?.wifi;
  if (!wifiHere && !wifiThere) return null;
  const which = wifiHere && wifiThere
    ? "Both PCs are on Wi-Fi"
    : wifiHere ? "This PC is on Wi-Fi" : "The other PC is on Wi-Fi";
  const doing = adaptLine(s?.adapt);
  const note = `${which}. For steady 4K60, Ethernet or Wi-Fi 6E holds up best. `
    + (doing ? `${doing}.` : "Relay adjusts quality to keep the picture smooth.");
  return {
    wifi: true,
    label: here?.label ?? "Unknown link",
    hint: there ? `Other PC: ${there.label}` : "Other PC: not reported",
    note,
  };
}

/** The strip cell. Renders nothing on a wired share. */
export function LinkCell({ s, live }: { s: Pick<ShareStats, "link" | "peer_link" | "adapt"> | null; live: boolean }) {
  const v = live ? linkView(s) : null;
  if (!v) return null;
  return (
    <div data-testid="link-cell">
      <label>Link</label>
      <div className="v link">{v.label}</div>
      <div className="hint">{v.hint}</div>
    </div>
  );
}

/** The note under the strip. Renders nothing on a wired share. */
export function LinkNote({ s, live }: { s: Pick<ShareStats, "link" | "peer_link" | "adapt"> | null; live: boolean }) {
  const v = live ? linkView(s) : null;
  if (!v) return null;
  return <p className="note" data-testid="link-note" role="status">{v.note}</p>;
}
