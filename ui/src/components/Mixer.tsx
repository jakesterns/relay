import { useEffect, useRef, useState } from "react";
import { Card, Slider } from "./Controls";
import { api, type FaderSet, type MixerSide, type MixerTrack } from "../lib/ipc";

/** A track the mixer can show: which fader it is, and what to call it. */
export interface MixerRow {
  key: MixerTrack;
  label: string;
  /** Current level in dBFS from the stats line, or null before any packet. */
  peakDb?: number | null;
}

/** The fader's travel in dB. The bottom step reads as silence and sends a
 *  gain of 0; the top is +6 dB, the most the engine will apply. */
const MIN_DB = -60;
const MAX_DB = 6;
/** How long a run of slider moves is batched before it goes to the engine.
 *  Short enough to feel live, long enough that a drag is one command a
 *  frame rather than one per pixel. */
const SEND_MS = 50;

export function dbToGain(db: number): number {
  return db <= MIN_DB ? 0 : Math.pow(10, db / 20);
}

export function fmtDb(db: number): string {
  if (db <= MIN_DB) return "−∞ dB";
  if (db === 0) return "0 dB";
  return `${db > 0 ? "+" : "−"}${Math.abs(db)} dB`;
}

interface FaderState { db: number; mute: boolean }
const unity = (): FaderState => ({ db: 0, mute: false });

/** Per-track gain and mute, live while a share runs (S37).
 *
 *  Rows are only the tracks that exist on this end right now. Local state is
 *  the truth for the sliders; the engine is told within `SEND_MS` of a change
 *  and never restarts. Everything resets to unity when `sessionKey` changes —
 *  a new share starts from a known place, not from wherever the last one's
 *  faders were left. */
export function MixerCard({ side, rows, sessionKey, note }: {
  side: MixerSide; rows: MixerRow[]; sessionKey: string; note?: string;
}) {
  const [state, setState] = useState<Record<MixerTrack, FaderState>>({
    app: unity(), rest: unity(), mic: unity(),
  });
  const pending = useRef<FaderSet>({});
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // A new share: back to unity, on screen and in the engine.
  useEffect(() => {
    setState({ app: unity(), rest: unity(), mic: unity() });
    pending.current = {};
    if (timer.current) { clearTimeout(timer.current); timer.current = null; }
  }, [sessionKey]);

  const flush = () => {
    timer.current = null;
    const faders = pending.current;
    pending.current = {};
    if (Object.keys(faders).length === 0) return;
    void api.setMixer(side, faders).catch(() => {});
  };

  const change = (key: MixerTrack, next: FaderState) => {
    setState((s) => ({ ...s, [key]: next }));
    pending.current = { ...pending.current, [key]: { gain: dbToGain(next.db), mute: next.mute } };
    if (!timer.current) timer.current = setTimeout(flush, SEND_MS);
  };

  if (rows.length === 0) return null;

  return (
    <Card title="Mixer">
      {rows.map((r) => {
        const f = state[r.key];
        return (
          <div key={r.key} className="fader" data-testid={`fader-${r.key}`}>
            <Slider label={r.label} value={f.db} min={MIN_DB} max={MAX_DB} step={1}
              format={fmtDb} disabled={f.mute}
              onChange={(db) => change(r.key, { ...f, db })} />
            <button type="button" className={"btn q" + (f.mute ? " on" : "")}
              aria-pressed={f.mute}
              aria-label={f.mute ? `Unmute ${r.label}` : `Mute ${r.label}`}
              onClick={() => change(r.key, { ...f, mute: !f.mute })}>
              {f.mute ? "Muted" : "Mute"}
            </button>
          </div>
        );
      })}
      {note && <p className="note">{note}</p>}
    </Card>
  );
}
