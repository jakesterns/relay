import { useCallback, useEffect, useRef, useState } from "react";
import { Card, Slider } from "./Controls";
import {
  api, devicePrefKey, type AudioDevice, type AudioDevicePrefs, type AudioDevices,
  type DeviceTrack, type FaderSet, type MixerSide, type MixerTrack,
} from "../lib/ipc";

/** A track the mixer can show: which fader it is, and what to call it. */
export interface MixerRow {
  key: MixerTrack;
  label: string;
  /** Current level in dBFS from the stats line, or null before any packet. */
  peakDb?: number | null;
}

/** A device picker the mixer can show (S40). With `row`, it sits at the end
 *  of that fader's row and only while the row exists; without, it is a row
 *  of its own under the faders. */
export interface MixerDevice {
  track: DeviceTrack;
  label: string;
  row?: MixerTrack;
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

/** The endpoints a track can use: capture for a mic, render for an output. */
function endpointsFor(track: DeviceTrack, all: AudioDevices | null): AudioDevice[] {
  if (!all) return [];
  return track === "mic" ? all.capture : all.render;
}

/** First option's text: what "System default" means right now. */
export function defaultLabel(options: AudioDevice[]): string {
  const d = options.find((o) => o.is_default);
  return d ? `System default (${d.name})` : "System default";
}

/**
 * One compact endpoint select. "System default" is the empty value and the
 * first option, so a user who never touches it follows Windows. A saved
 * device that is not plugged in stays visible as what was picked, rather
 * than silently reading as the default.
 */
function DevicePicker({ label, options, value, onChange, onOpen }: {
  label: string; options: AudioDevice[]; value: string | null;
  onChange: (id: string | null) => void; onOpen: () => void;
}) {
  const missing = !!value && !options.some((o) => o.id === value);
  return (
    <select className="devsel" aria-label={label} value={value ?? ""}
      onFocus={onOpen} onChange={(e) => onChange(e.target.value || null)}>
      <option value="">{defaultLabel(options)}</option>
      {missing && <option value={value}>Saved device (not connected, using default)</option>}
      {options.map((o) => <option key={o.id} value={o.id}>{o.name}</option>)}
    </select>
  );
}

/** Per-track gain and mute, live while a share runs (S37), plus the device
 *  each device-backed track uses (S40).
 *
 *  Rows are only the tracks that exist on this end right now. Local state is
 *  the truth for the sliders; the engine is told within `SEND_MS` of a change
 *  and never restarts. Everything resets to unity when `sessionKey` changes —
 *  a new share starts from a known place, not from wherever the last one's
 *  faders were left. Device picks are the exception: they are saved by the
 *  core and carry over, because they describe the room, not the share. */
export function MixerCard({ side, rows, sessionKey, note, devices = [] }: {
  side: MixerSide; rows: MixerRow[]; sessionKey: string; note?: string; devices?: MixerDevice[];
}) {
  const [state, setState] = useState<Record<MixerTrack, FaderState>>({
    app: unity(), rest: unity(), mic: unity(), call: unity(),
  });
  const pending = useRef<FaderSet>({});
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [endpoints, setEndpoints] = useState<AudioDevices | null>(null);
  const [picks, setPicks] = useState<AudioDevicePrefs>({});
  const wantsDevices = devices.length > 0;

  // A new share: back to unity, on screen and in the engine.
  useEffect(() => {
    setState({ app: unity(), rest: unity(), mic: unity(), call: unity() });
    pending.current = {};
    if (timer.current) { clearTimeout(timer.current); timer.current = null; }
  }, [sessionKey]);

  const refreshEndpoints = useCallback(() => {
    api.listAudioDevices().then(setEndpoints).catch(() => {});
  }, []);

  // The endpoint list and the saved picks, once per share. The list is
  // read again whenever a picker takes focus, so a headset plugged in
  // mid-share shows up without a restart.
  useEffect(() => {
    if (!wantsDevices) return;
    refreshEndpoints();
    api.getUiPrefs().then((p) => setPicks(p.audio_devices ?? {})).catch(() => {});
  }, [sessionKey, wantsDevices, refreshEndpoints]);

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

  const pick = (track: DeviceTrack, id: string | null) => {
    const key = devicePrefKey(side, track);
    if (!key) return;
    setPicks((p) => ({ ...p, [key]: id }));
    void api.setAudioDevice(side, track, id).catch(() => {});
  };

  const picker = (d: MixerDevice) => {
    const key = devicePrefKey(side, d.track);
    if (!key) return null;
    return (
      <DevicePicker label={d.label} options={endpointsFor(d.track, endpoints)}
        value={picks[key] ?? null} onChange={(id) => pick(d.track, id)} onOpen={refreshEndpoints} />
    );
  };

  if (rows.length === 0) return null;
  const present = new Set(rows.map((r) => r.key));
  const own = devices.filter((d) => !d.row);

  return (
    <Card title="Mixer">
      {rows.map((r) => {
        const f = state[r.key];
        const dev = devices.find((d) => d.row === r.key);
        return (
          <div key={r.key} className="fader" data-testid={`fader-${r.key}`}>
            <Slider label={r.label} value={f.db} min={MIN_DB} max={MAX_DB} step={1}
              format={fmtDb} disabled={f.mute}
              onChange={(db) => change(r.key, { ...f, db })} />
            {dev && present.has(r.key) && picker(dev)}
            <button type="button" className={"btn q" + (f.mute ? " on" : "")}
              aria-pressed={f.mute}
              aria-label={f.mute ? `Unmute ${r.label}` : `Mute ${r.label}`}
              onClick={() => change(r.key, { ...f, mute: !f.mute })}>
              {f.mute ? "Muted" : "Mute"}
            </button>
          </div>
        );
      })}
      {own.map((d) => (
        <div key={`dev-${d.track}`} className="devrow" data-testid={`device-${d.track}`}>
          <span>{d.label}</span>
          {picker(d)}
        </div>
      ))}
      {note && <p className="note">{note}</p>}
    </Card>
  );
}
