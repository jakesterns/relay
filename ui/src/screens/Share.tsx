import { useCallback, useEffect, useRef, useState } from "react";
import { Card, Chips, ChipSet, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { CodecBanner } from "./Receive";
import { useCore } from "../lib/core";
import {
  api, onCoreEvents, presetAudioLabel,
  type DesktopAudio, type DiscoveredReceiver, type ProcessInfo, type SharePresetDef,
  type ShareStats, type SharePreview, type SourceTarget,
} from "../lib/ipc";

/** Instrument-strip readings, fed by the engine's `stats` events. */
interface Strip {
  mbps: number; latencyMs: number; dropped: number; sent: number;
  gpuPct: number; cpuPct: number; audioDb: number; history: number[];
  /** Mic track level, and whether a second audio track is arriving at all. */
  micDb: number; micLive: boolean;
  recording: boolean; recMb: number; recDropped: number;
  replayFill: number; recStoppedDisk: boolean;
}
const idleStrip: Strip = {
  mbps: 0, latencyMs: 0, dropped: 0, sent: 0, gpuPct: 0, cpuPct: 0,
  audioDb: -Infinity, history: Array(18).fill(0),
  micDb: -Infinity, micLive: false,
  recording: false, recMb: 0, recDropped: 0, replayFill: 0, recStoppedDisk: false,
};

export function Share() {
  const { state, mock } = useCore();
  const sharing = state.sharing.kind === "sharing";
  const peer = state.sharing.kind === "sharing" ? state.sharing.peer : null;
  const [presets, setPresets] = useState<SharePresetDef[]>([]);
  const [preset, setPreset] = useState("game");
  const [code, setCode] = useState("");
  const [receivers, setReceivers] = useState<DiscoveredReceiver[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [strip, setStrip] = useState<Strip>(idleStrip);
  const histRef = useRef<number[]>(Array(18).fill(0));
  // Recording + replay state pushed by the engine.
  const [rec, setRec] = useState<{ on: boolean; path: string | null }>({ on: false, path: null });
  const [replayToast, setReplayToast] = useState<string | null>(null);
  // Active capture source; confirmed by `source_changed` events.
  const [source, setSource] = useState<SourceTarget>({ kind: "display", index: 0 });
  const [showRegion, setShowRegion] = useState(false);
  const [region, setRegion] = useState({ x: 0, y: 0, w: 1920, h: 1080 });
  const [showWindows, setShowWindows] = useState(false);
  // Latest capture thumbnail from the engine; cleared when the share stops.
  const [preview, setPreview] = useState<SharePreview | null>(null);
  const [windows, setWindows] = useState<ProcessInfo[]>([]);

  const selectedDef = presets.find((p) => p.id === preset) ?? presets[0];
  const bitrateCeil = Math.max(selectedDef?.bitrate_mbps ?? 60, 1);

  const reloadPresets = useCallback(async (select?: string) => {
    const r = await api.listPresets();
    setPresets(r.presets);
    if (select) setPreset(select);
    else if (!r.presets.some((p) => p.id === preset)) setPreset(r.presets[0]?.id ?? "game");
  }, [preset]);

  useEffect(() => {
    let cancelled = false;
    api.listPresets()
      .then((r) => { if (!cancelled) setPresets(r.presets); })
      .catch(() => {});
    return () => { cancelled = true; };
  }, []);

  // Live stats + recording/replay/source events from the engine while sharing.
  useEffect(() => {
    let unsub = () => {};
    void onCoreEvents({
      sharePreview: (p: SharePreview) => setPreview(p),
      shareStats: (s: ShareStats) => {
        if (s.bitrate_mbps === undefined) return;
        const h = [...histRef.current.slice(1), Math.min(1, (s.bitrate_mbps ?? 0) / bitrateCeil)];
        histRef.current = h;
        setStrip({
          mbps: s.bitrate_mbps ?? 0,
          latencyMs: Math.round(s.capture_to_send_ms ?? 0),
          dropped: s.dropped ?? 0,
          sent: s.frames ?? 0,
          gpuPct: Math.round(s.encode_ms ? (s.encode_ms / (1000 / 60)) * 100 : 0),
          cpuPct: Math.round((s.cpu_percent ?? 0) * 10) / 10,
          audioDb: s.audio_peak ? 20 * Math.log10(Math.max(1e-4, s.audio_peak)) : -Infinity,
          micDb: s.mic_peak ? 20 * Math.log10(Math.max(1e-4, s.mic_peak)) : -Infinity,
          // Packets, not level: a muted mic is still a live track, and the
          // meter should say so rather than vanish.
          micLive: (s.mic_packets ?? 0) > 0,
          history: h,
          recording: s.recording ?? false,
          recMb: s.rec_mb ?? 0,
          recDropped: s.rec_dropped ?? 0,
          replayFill: s.replay_fill ?? 0,
          recStoppedDisk: s.rec_stopped_disk ?? false,
        });
      },
      shareStatus: (st) => { if (st.message) setError(st.message); },
      recordingStatus: (r) => setRec({ on: r.on, path: r.path }),
      replaySaved: (r) => setReplayToast(`Replay saved · ${(r.ms / 1000).toFixed(1)} s · ${r.path}`),
      sourceChanged: (s) => { if (s.target) setSource(s.target); },
    }).then((u) => { unsub = u; });
    return () => unsub();
  }, [bitrateCeil]);

  useEffect(() => {
    if (!sharing) {
      setStrip(idleStrip);
      histRef.current = Array(18).fill(0);
      setRec({ on: false, path: null });
      setSource({ kind: "display", index: 0 });
      setShowRegion(false);
    }
  }, [sharing]);

  useEffect(() => {
    if (!replayToast) return;
    const t = setTimeout(() => setReplayToast(null), 6000);
    return () => clearTimeout(t);
  }, [replayToast]);

  const discover = async () => {
    setBusy(true); setError(null);
    try {
      const list = await api.discoverReceivers();
      setReceivers(list);
      if (list.length && !selected) setSelected(list[0].name);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  const start = async () => {
    setBusy(true); setError(null);
    try { await api.startSharePreset(preset, code.trim(), selected); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  const stop = async () => {
    setBusy(true);
    setPreview(null);
    try { await api.stopShare(); } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  const switchTo = async (target: SourceTarget) => {
    setError(null);
    setSource(target); // optimistic; source_changed confirms
    try { await api.switchSource(target); } catch (e) { setError(String(e)); }
  };

  const toggleRecord = async () => {
    setError(null);
    try { await api.record(!rec.on); } catch (e) { setError(String(e)); }
  };

  const saveReplay = async () => {
    setError(null);
    try { await api.saveReplay(); } catch (e) { setError(String(e)); }
  };

  // "Display 1", "Display 2"… from the connected monitors. Never guess more
  // than one: offering a display that isn't there just fails the switch.
  const displayCount = Math.max(state.hardware.monitors.length, 1);
  const canStart = code.trim().length === 6 && !busy && !!selectedDef;
  const replayOn = (selectedDef?.replay_secs ?? 0) > 0;

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Display 1 <em>— this PC</em></h1>
          <Live on={sharing} text={sharing ? "Sharing" : "Not sharing"} />
        </div>
        <OfflineBanner />
        <CodecBanner need="share" />
        <Chips label="Preset" value={preset} onChange={setPreset}
          options={presets.map((p) => ({ key: p.id, label: p.name }))} />
        {sharing && (
          <div className="chips">
            <span>Source</span>
            {Array.from({ length: displayCount }, (_, i) => (
              <button key={i}
                className={"chip" + (source.kind === "display" && source.index === i ? " on" : "")}
                onClick={() => void switchTo({ kind: "display", index: i })}>
                Display {i + 1}
              </button>
            ))}
            <button className={"chip" + (source.kind === "window" ? " on" : showWindows ? " on" : "")}
              onClick={() => {
                setShowWindows((v) => !v);
                setShowRegion(false);
                void api.listProcesses().then(setWindows).catch(() => {});
              }}>
              Window…
            </button>
            <button className={"chip" + (source.kind === "region" ? " on" : showRegion ? " on" : "")}
              onClick={() => { setShowRegion((v) => !v); setShowWindows(false); }}>
              Region…
            </button>
          </div>
        )}
        {sharing && showWindows && (
          <div className="region">
            <label style={{ flex: 1 }}>
              <span>window</span>
              <select
                value={source.kind === "window" ? String(source.hwnd) : ""}
                onChange={(e) => {
                  const w = windows.find((p) => String(p.hwnd) === e.target.value);
                  if (w) void switchTo({ kind: "window", hwnd: w.hwnd });
                }}>
                <option value="" disabled>{windows.length ? "Pick a window" : "Loading…"}</option>
                {windows.map((p) => (
                  <option key={p.hwnd} value={String(p.hwnd)}>{p.exe} — {p.title}</option>
                ))}
              </select>
            </label>
          </div>
        )}
        {sharing && showRegion && (
          <div className="region">
            {(["x", "y", "w", "h"] as const).map((k) => (
              <label key={k}>
                <span>{k}</span>
                <input inputMode="numeric" value={region[k]}
                  onChange={(e) => setRegion({ ...region, [k]: Number(e.target.value.replace(/\D/g, "")) || 0 })} />
              </label>
            ))}
            <button className="btn q" disabled={region.w === 0 || region.h === 0}
              onClick={() => void switchTo({ kind: "region", display: 0, ...region })}>
              Apply region
            </button>
          </div>
        )}
        <div className="preview">
          {/* A real thumbnail of the capture once one arrives; the drawn
              placeholder until then, so the box is never empty. */}
          {sharing && preview
            ? <img className="shot" src={`data:image/jpeg;base64,${preview.jpeg}`} alt="What is being shared" />
            : <div className={"scene" + (sharing ? "" : " idle")} />}
          {sharing && !preview && <div className="horizon" />}
          <div className="tag"><span>Up to 3840×2160</span><span>60 fps</span><span>HEVC</span></div>
          {sharing
            ? <div className="cap">{preview ? `Live · ${preview.width}×${preview.height} thumbnail` : "Waiting for the first frame…"}</div>
            : <div className="idlemsg">Capture starts when you share. Nothing is running now.</div>}
        </div>
        <InstrumentStrip s={strip} live={sharing} recOn={rec.on} />
        {sharing && (
          <div className="recrow">
            <button className={"btn" + (rec.on ? " danger" : "")} onClick={() => void toggleRecord()}>
              {rec.on ? "Stop recording" : "Record"}
            </button>
            <button className="btn q" onClick={() => void saveReplay()} disabled={!replayOn}
              title={replayOn ? "" : "This preset has no replay buffer"}>
              Save replay (Ctrl+Alt+R)
            </button>
            {rec.on && <div className="ind"><i />REC</div>}
            {rec.on && rec.path && <div className="path" title={rec.path}>{rec.path}</div>}
            {replayToast && <div className="toast">{replayToast}</div>}
          </div>
        )}
      </section>
      <aside className="side">
        <Card title="Send to">
          {receivers.length === 0
            ? <p className="note">No receivers found yet. Open Relay on the other PC's Receive screen, then scan.</p>
            : receivers.map((r) => (
              <label key={r.name} className="dev" style={{ cursor: "pointer" }}>
                <input type="radio" name="rcv" checked={selected === r.name}
                  onChange={() => setSelected(r.name)} />
                <div><b>{r.name}</b><span>{r.addr}</span></div>
              </label>
            ))}
          <button className="btn" onClick={discover} disabled={busy}>Scan for receivers</button>
        </Card>
        <Card title="Pairing code">
          <input className="in" inputMode="numeric" maxLength={6} placeholder="6 digits from the receiver"
            value={code} onChange={(e) => setCode(e.target.value.replace(/\D/g, ""))} />
        </Card>
        <PresetCard def={selectedDef} locked={sharing} onSaved={reloadPresets} />
        <Card>
          <Kv k="Connection" v={sharing ? "Direct, encrypted (DTLS-SRTP)" : "—"} />
          <Kv k="Path" v={sharing ? "LAN · host candidates only" : "—"} mono />
          <Kv k="Peer" v={peer ?? "—"} mono />
        </Card>
        {error && <p className="note" style={{ color: "#d98b6a" }}>{error}</p>}
        {sharing
          ? <button className="btn acc" onClick={stop} disabled={busy}>Stop sharing</button>
          : <button className="btn acc" onClick={start} disabled={!canStart}
              title={canStart ? "" : "Enter the 6-digit code shown on the receiver"}>Start sharing</button>}
        {mock && <p className="note">Preview data — the core service isn't running.</p>}
        <p className="note">Captures the screen the same way Windows does. Never touches games or other apps.</p>
      </aside>
    </>
  );
}

/** The three ids `presets.rs::builtins()` ships, pinned on the Rust side by
 *  `builtins_match_the_plan`. They can be edited like any other preset, but
 *  not deleted: `PresetStore::load` only re-seeds them when presets.json is
 *  missing entirely, so removing one here would be permanent. */
const BUILTIN_PRESETS = ["game", "daw", "desktop"];

/** The selected preset: its settings, and an editor for them.
 *
 *  Read-only until you press Edit, because this card sits next to the Start
 *  button and the common case is checking what is about to be sent, not
 *  changing it. Locked outright while a share is running — the engine read
 *  these values when it started and editing them here would not reach it. */
function PresetCard({ def, locked, onSaved }: {
  def: SharePresetDef | undefined;
  locked: boolean;
  onSaved: (select?: string) => Promise<void>;
}) {
  const [draft, setDraft] = useState<SharePresetDef | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const isNew = !!draft && !!def && draft.id !== def.id;
  const builtin = !!draft && BUILTIN_PRESETS.includes(draft.id);

  const edit = (patch: Partial<SharePresetDef>) =>
    setDraft((d) => (d ? { ...d, ...patch } : d));

  const run = async (action: () => Promise<string | undefined>) => {
    setBusy(true);
    setError(null);
    try {
      const select = await action();
      await onSaved(select);
      setDraft(null);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  if (!def) return <Card title="Preset"><div className="empty">No presets</div></Card>;

  if (!draft) {
    return (
      <Card title={`${def.name} preset`} action={locked ? undefined : "Edit"}
        onAction={locked ? undefined : () => { setDraft({ ...def }); setError(null); }}>
        <Kv k="Bitrate" v={`${def.bitrate_mbps} Mb/s`} mono />
        <Kv k="Frame rate" v={`${def.fps} fps`} mono />
        <Kv k="Size" v={def.size ? `${def.size[0]}×${def.size[1]}` : "Native"} mono />
        <Kv k="Audio" v={presetAudioLabel(def.audio)} />
        <Kv k="Cursor" v={def.cursor ? "Shown" : "Hidden"} />
        <Kv k="Replay buffer" v={def.replay_secs ? `${def.replay_secs} s` : "Off"} mono />
        <Kv k="Container" v={(def.container ?? "mp4").toUpperCase()} mono />
        {locked && <p className="note">Stop sharing to change the preset.</p>}
      </Card>
    );
  }

  return (
    <Card title={isNew ? "New preset" : `Edit ${def.name}`} action="Cancel" onAction={() => setDraft(null)}>
      <div className="form">
        <div className="field">
          <span>Name</span>
          <input value={draft.name} onChange={(e) => edit({ name: e.target.value })} />
        </div>
        <div className="two" style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10 }}>
          <div className="field">
            <span>Bitrate (Mb/s)</span>
            <input className="mono" inputMode="numeric" value={draft.bitrate_mbps}
              onChange={(e) => edit({ bitrate_mbps: Number(e.target.value.replace(/\D/g, "")) || 0 })} />
          </div>
          <div className="field">
            <span>Frame rate</span>
            <input className="mono" inputMode="numeric" value={draft.fps}
              onChange={(e) => edit({ fps: Number(e.target.value.replace(/\D/g, "")) || 0 })} />
          </div>
        </div>
        <div className="field">
          <span>Encode size</span>
          <input className="mono" placeholder="Native — or 2560x1440"
            value={draft.size ? `${draft.size[0]}x${draft.size[1]}` : ""}
            onChange={(e) => {
              // Blank means "capture at the monitor's native size"; the
              // engine only scales when a cap is given.
              const m = /^\s*(\d+)\s*[x×]\s*(\d+)\s*$/.exec(e.target.value);
              edit({ size: m ? [Number(m[1]), Number(m[2])] : undefined });
            }} />
        </div>
        <ChipSet label="Audio"
          values={[
            ...(draft.audio.desktop === "off" ? [] : [draft.audio.desktop]),
            ...(draft.audio.mic ? ["mic" as const] : []),
          ]}
          options={[
            { key: "system", label: "System mix" }, { key: "game", label: "Game only" },
            { key: "mic", label: "Microphone" },
          ]}
          onToggle={(k) => {
            // The two desktop sources exclude each other — you cannot capture
            // the whole endpoint and one process at once — but the microphone
            // is its own track and rides alongside either.
            if (k === "mic") edit({ audio: { ...draft.audio, mic: !draft.audio.mic } });
            else edit({
              audio: {
                ...draft.audio,
                desktop: (draft.audio.desktop === k ? "off" : k) as DesktopAudio,
              },
            });
          }} />
        <p className="note">Pick any combination: the microphone travels as its own track alongside the desktop mix, and the person on the other end hears them together. Nothing selected means a silent share.</p>
        <Toggle on={draft.cursor} onChange={(v) => edit({ cursor: v })}
          label="Show the mouse cursor" sub="Games draw their own, so this is usually off for Game." />
        <Toggle on={draft.record} onChange={(v) => edit({ record: v })}
          label="Start recording with the share"
          sub="Writes the same bitstream to disk; costs no extra encode." />
        <div className="field">
          <span>Replay buffer (seconds, 0 = off)</span>
          <input className="mono" inputMode="numeric" value={draft.replay_secs}
            onChange={(e) => edit({ replay_secs: Number(e.target.value.replace(/\D/g, "")) || 0 })} />
        </div>
        <Chips label="Recording container" value={draft.container ?? "mp4"}
          onChange={(v) => edit({ container: v as SharePresetDef["container"] })}
          options={[{ key: "mp4", label: "MP4" }, { key: "mkv", label: "MKV" }]} />
        <p className="note">Same video and audio either way — the file is written straight from the stream already being sent, so neither costs an extra encode. MKV is the safer choice if Relay or the PC ever stops mid-recording: the part already written stays usable, where an MP4 cut off without a clean stop will not open in a video editor.</p>
        <div className="ab">
          <button className="btn acc" disabled={busy || !draft.name.trim()}
            onClick={() => void run(async () => {
              await api.savePreset({ ...draft, name: draft.name.trim() });
              return draft.id;
            })}>{busy ? "Saving…" : "Save preset"}</button>
          <button className="btn q" disabled={busy}
            onClick={() => {
              // Duplicate-as-new: the fastest way to a custom preset is to
              // start from one that already works.
              const id = `custom-${Date.now().toString(36)}`;
              setDraft({ ...draft, id, name: `${draft.name} copy` });
            }}>Duplicate</button>
        </div>
        {!builtin && !isNew && (
          <button className="btn q" disabled={busy}
            onClick={() => void run(async () => {
              await api.deletePreset(draft.id);
              return undefined;
            })}>Delete this preset</button>
        )}
        {builtin && <p className="note">Built-in presets can be edited but not deleted. Duplicate it to make a version you can remove.</p>}
        {error && <div className="offline"><i />{error}</div>}
      </div>
    </Card>
  );
}

function errText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}

function InstrumentStrip({ s, live, recOn }: { s: Strip; live: boolean; recOn: boolean }) {
  const audioSegs = 12;
  const segsFor = (db: number) =>
    live && isFinite(db) ? Math.round(((db + 40) / 40) * audioSegs) : 0;
  const lit = segsFor(s.audioDb);
  const micLit = segsFor(s.micDb);
  const recording = live && (s.recording || recOn);
  const recWarn = live && (s.recDropped > 0 || s.recStoppedDisk);
  return (
    <div className="meter">
      <div>
        <label>Bitrate</label>
        <div className="v">{live ? s.mbps.toFixed(1) : "—"}<u>Mb/s</u></div>
        <div className="bars">
          {s.history.map((h, i) => (
            <b key={i} className={h === 0 ? "off" : h > 0.9 ? "hi" : ""} style={{ height: `${h === 0 ? 100 : h * 100}%` }} />
          ))}
        </div>
      </div>
      <div>
        <label>Latency (capture→send)</label>
        <div className="v">{live ? s.latencyMs : "—"}<u>ms</u></div>
        <div className="hint">{live ? "Sender pipeline" : "Idle"}</div>
      </div>
      <div>
        <label>Dropped frames</label>
        <div className="v">{live ? s.dropped : "—"}</div>
        <div className="hint">{live ? `of ${s.sent.toLocaleString()} sent` : "Nothing sent"}</div>
      </div>
      <div>
        <label>Load</label>
        <div className="v">{live ? s.gpuPct : "0"}<u>% enc</u></div>
        <div className="hint">{live ? `NVENC · CPU ${s.cpuPct}%` : "Encoder not loaded"}</div>
      </div>
      <div>
        <label>{s.micLive ? "Desktop audio" : "Audio"}</label>
        <div className="v">{live && isFinite(s.audioDb) ? s.audioDb.toFixed(1) : "—"}<u>dB</u></div>
        <div className="seg">{Array.from({ length: audioSegs }, (_, i) => <b key={i} className={i < lit ? "" : "off"} />)}</div>
      </div>
      {s.micLive && (
        <div>
          <label>Mic</label>
          <div className="v">{isFinite(s.micDb) ? s.micDb.toFixed(1) : "—"}<u>dB</u></div>
          <div className="seg">{Array.from({ length: audioSegs }, (_, i) => <b key={i} className={i < micLit ? "" : "off"} />)}</div>
        </div>
      )}
      <div className={recWarn ? "warn" : ""}>
        <label><i className={"recdot" + (recording ? " on" : "")} />Rec</label>
        <div className="v">{recording ? s.recMb.toFixed(0) : "—"}<u>MB</u></div>
        <div className="fill" role="meter" aria-label="Replay buffer" aria-valuenow={Math.round(s.replayFill * 100)}>
          <i style={{ width: `${live ? Math.min(1, Math.max(0, s.replayFill)) * 100 : 0}%` }} />
        </div>
        <div className="hint">
          {s.recStoppedDisk ? "Stopped — disk floor"
            : recWarn ? `${s.recDropped} rec frames dropped`
            : `Replay · ${live ? Math.round(Math.min(1, Math.max(0, s.replayFill)) * 100) : 0}%`}
        </div>
      </div>
    </div>
  );
}
