import { useCallback, useEffect, useRef, useState } from "react";
import { Card, Chips, ChipSet, ConfirmButton, ErrorNote, Kv, Live, Toggle } from "../components/Controls";
import { MixerCard, type MixerRow } from "../components/Mixer";
import { OfflineBanner } from "../components/Offline";
import { CodecBanner, FirewallBanner } from "./Receive";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import { encoderBrand, shareTags } from "../lib/honest";
import { ago } from "../lib/ago";
import {
  api, onCoreEvents, presetAudioLabel,
  type DesktopAudio, type DiscoveredReceiver, type Peer, type ProcessInfo, type ShareCapabilities,
  type SharePresetDef, type ShareStats, type VideoCodec, type SharePreview, type SourceTarget,
} from "../lib/ipc";

/** Instrument-strip readings, fed by the engine's `stats` events. */
interface Strip {
  mbps: number; latencyMs: number; dropped: number; sent: number;
  gpuPct: number; cpuPct: number; audioDb: number; history: number[];
  /** Frames per second the engine reports sending; 0 before the first stats line. */
  fps: number;
  /** Mic track level, and whether a second audio track is arriving at all. */
  micDb: number; micLive: boolean;
  /** The rest-of-PC track (S37), same shape. */
  restDb: number; restLive: boolean;
  recording: boolean; recMb: number; recDropped: number;
  replayFill: number; recStoppedDisk: boolean;
  /** The codec the running share negotiated; null until the engine says. */
  codec: VideoCodec | null;
}
const idleStrip: Strip = {
  mbps: 0, latencyMs: 0, dropped: 0, sent: 0, gpuPct: 0, cpuPct: 0, fps: 0,
  audioDb: -Infinity, history: Array(18).fill(0),
  micDb: -Infinity, micLive: false,
  restDb: -Infinity, restLive: false,
  recording: false, recMb: 0, recDropped: 0, replayFill: 0, recStoppedDisk: false,
  codec: null,
};

/** The mixer rows a preset's audio produces on the sending end (S37): one
 *  per track that will actually be on the wire, in the order they sound. */
export function sendRows(audio: SharePresetDef["audio"] | undefined): MixerRow[] {
  if (!audio) return [];
  const rows: MixerRow[] = [];
  if (audio.desktop === "game") rows.push({ key: "app", label: "Game" });
  else if (audio.desktop === "system") rows.push({ key: "app", label: "System mix" });
  if (audio.desktop === "game" && audio.rest) rows.push({ key: "rest", label: "Everything else" });
  if (audio.mic) rows.push({ key: "mic", label: "Microphone" });
  return rows;
}

export function Share() {
  const { state, mock, offline } = useCore();
  // Reconnecting counts as sharing for everything but the pill: the share
  // is the user's until they stop it, and Stop is the button they need.
  const reconnecting = state.sharing.kind === "reconnecting" ? state.sharing : null;
  const sharing = state.sharing.kind === "sharing" || reconnecting !== null;
  const peer = state.sharing.kind === "off" ? null : state.sharing.peer || null;
  const [presets, setPresets] = useState<SharePresetDef[]>([]);
  const [preset, setPreset] = useState("game");
  const [code, setCode] = useState("");
  const [receivers, setReceivers] = useState<DiscoveredReceiver[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  // Remembered PCs (S35): one click, no code. `selectedPeer` is an id, and
  // choosing one clears `selected` and vice versa -- the two lists are one
  // choice, not two.
  const [peers, setPeers] = useState<Peer[]>([]);
  const [selectedPeer, setSelectedPeer] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Two error channels, because the controls that fail are at opposite ends
  // of the screen: session errors belong beside Start/Stop in the side panel,
  // capture errors beside the source chips and the record row they came from.
  const [error, setError] = useState<string | null>(null);
  const [capErr, setCapErr] = useState<string | null>(null);
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
  // What the capability probe found, so the strip names the encoder this PC
  // actually has rather than assuming NVENC.
  const [caps, setCaps] = useState<ShareCapabilities | null>(null);
  // The preset the running share was started with from this screen. Null
  // while idle, and also when a share is running that this screen did not
  // start (the core does not report which preset that one uses).
  const [running, setRunning] = useState<string | null>(null);

  const selectedDef = presets.find((p) => p.id === preset) ?? presets[0];
  const runningDef = running ? presets.find((p) => p.id === running) : undefined;
  const liveDef = sharing ? runningDef : selectedDef;
  const bitrateCeil = Math.max(liveDef?.bitrate_mbps ?? 60, 1);
  // Encode load is encode time over the frame budget, and the budget is the
  // preset's frame rate, not a fixed 60.
  const frameMs = 1000 / Math.max(liveDef?.fps ?? 60, 1);

  const reloadPresets = useCallback(async (select?: string) => {
    const r = await api.listPresets();
    setPresets(r.presets);
    if (select) setPreset(select);
    else if (!r.presets.some((p) => p.id === preset)) setPreset(r.presets[0]?.id ?? "game");
  }, [preset]);

  useEffect(() => {
    let live = true;
    api.shareCapabilities()
      .then((c) => { if (live) setCaps(c); })
      .catch(() => { if (live) setCaps(null); });
    return () => { live = false; };
  }, [offline]);

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
          fps: s.fps ?? 0,
          gpuPct: Math.round(s.encode_ms ? (s.encode_ms / frameMs) * 100 : 0),
          cpuPct: Math.round((s.cpu_percent ?? 0) * 10) / 10,
          audioDb: s.audio_peak ? 20 * Math.log10(Math.max(1e-4, s.audio_peak)) : -Infinity,
          micDb: s.mic_peak ? 20 * Math.log10(Math.max(1e-4, s.mic_peak)) : -Infinity,
          // Packets, not level: a muted mic is still a live track, and the
          // meter should say so rather than vanish.
          micLive: (s.mic_packets ?? 0) > 0,
          restDb: s.rest_peak ? 20 * Math.log10(Math.max(1e-4, s.rest_peak)) : -Infinity,
          restLive: (s.rest_packets ?? 0) > 0,
          history: h,
          recording: s.recording ?? false,
          recMb: s.rec_mb ?? 0,
          recDropped: s.rec_dropped ?? 0,
          replayFill: s.replay_fill ?? 0,
          recStoppedDisk: s.rec_stopped_disk ?? false,
          codec: s.codec ?? null,
        });
      },
      shareStatus: (st) => { if (st.message) setError(st.message); },
      recordingStatus: (r) => setRec({ on: r.on, path: r.path }),
      replaySaved: (r) => setReplayToast(`Replay saved · ${(r.ms / 1000).toFixed(1)} s · ${r.path}`),
      sourceChanged: (s) => { if (s.target) setSource(s.target); },
    }).then((u) => { unsub = u; });
    return () => unsub();
  }, [bitrateCeil, frameMs]);

  useEffect(() => {
    if (!sharing) {
      setStrip(idleStrip);
      histRef.current = Array(18).fill(0);
      setRec({ on: false, path: null });
      setSource({ kind: "display", index: 0 });
      setShowRegion(false);
      setRunning(null);
    }
  }, [sharing]);

  useEffect(() => {
    if (!replayToast) return;
    const t = setTimeout(() => setReplayToast(null), 6000);
    return () => clearTimeout(t);
  }, [replayToast]);

  // Reloaded whenever a share starts or stops: a code-paired share is what
  // adds a PC to this list, so the next visit should already show it.
  const loadPeers = useCallback(async () => {
    try { setPeers(await api.listPeers()); }
    catch { /* a convenience list; the code path works without it */ }
  }, []);
  useEffect(() => { void loadPeers(); }, [loadPeers, sharing]);

  const discover = async () => {
    setBusy(true); setError(null);
    try {
      const list = await api.discoverReceivers();
      setReceivers(list);
      if (list.length && !selected && !selectedPeer) setSelected(list[0].name);
    } catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };

  const start = async () => {
    setBusy(true); setError(null);
    try {
      // A remembered PC: no code, and the core resolves the id to a name and
      // fingerprint itself. Anything else: the code, as before.
      await api.startSharePreset(
        preset,
        selectedPeer ? "" : code.trim(),
        selectedPeer ? null : selected,
        selectedPeer,
      );
      setRunning(preset);
    }
    catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };

  const stop = async () => {
    setBusy(true);
    setPreview(null);
    try { await api.stopShare(); } catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };

  const switchTo = async (target: SourceTarget) => {
    setCapErr(null);
    setSource(target); // optimistic; source_changed confirms
    try { await api.switchSource(target); } catch (e) { setCapErr(errText(e)); }
  };

  const toggleRecord = async () => {
    setCapErr(null);
    try { await api.record(!rec.on); } catch (e) { setCapErr(errText(e)); }
  };

  const saveReplay = async () => {
    setCapErr(null);
    try { await api.saveReplay(); } catch (e) { setCapErr(errText(e)); }
  };

  // "Display 1", "Display 2"… from the connected monitors. Never guess more
  // than one: offering a display that isn't there just fails the switch.
  const displayCount = Math.max(state.hardware.monitors.length, 1);
  const canStart = (!!selectedPeer || code.trim().length === 6) && !busy && !!selectedDef;
  const chosenPeer = peers.find((p) => p.id === selectedPeer) ?? null;
  // A PC that is both remembered and just scanned is one PC; list it once,
  // under the name that needs no code.
  const strangers = receivers.filter(
    (r) => !peers.some((p) => p.name.toLowerCase() === r.name.toLowerCase()),
  );
  const replayOn = (selectedDef?.replay_secs ?? 0) > 0;

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>{sourceTitle(sharing ? source : { kind: "display", index: 0 })} <em>— this PC</em></h1>
          <Live on={sharing} text={reconnecting
            ? `Reconnecting${reconnecting.attempt > 0 ? ` (${reconnecting.attempt})` : ""}…`
            : sharing ? "Sharing" : "Not sharing"} />
        </div>
        <OfflineBanner />
        <CodecBanner need="share" />
        <FirewallBanner />
        {/* Locked while sharing: the engine read the preset when it started,
            so switching the chip mid-share would change nothing but the labels. */}
        <Chips label="Preset" value={preset} onChange={sharing ? undefined : setPreset}
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
          {/* A real thumbnail of the capture once one arrives; an empty
              frame until then, never a painted stand-in for one. */}
          {sharing && preview
            ? <img className="shot" src={`data:image/jpeg;base64,${preview.jpeg}`} alt="What is being shared" />
            : <div className="scene" />}
          <div className="tag" data-testid="share-tags">
            {(sharing ? shareTags(runningDef, strip.fps, strip.codec) : shareTags(selectedDef)).map((t) => <span key={t}>{t}</span>)}
          </div>
          {sharing
            ? <div className="cap">{preview ? `Live · ${preview.width}×${preview.height} thumbnail` : "Waiting for the first frame…"}</div>
            : <div className="idlemsg">Capture starts when you share. Nothing is running now.</div>}
        </div>
        <InstrumentStrip s={strip} live={sharing} recOn={rec.on} encoder={encoderBrand(caps)} />
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
        <ErrorNote text={capErr} onDismiss={() => setCapErr(null)} />
      </section>
      <aside className="side">
        <Card title="Send to">
          {peers.map((p) => (
            <label key={p.id} className="dev" style={{ cursor: "pointer" }} data-testid="peer-row">
              <input type="radio" name="rcv" checked={selectedPeer === p.id}
                onChange={() => { setSelectedPeer(p.id); setSelected(null); }} />
              <div>
                <b>{p.favourite ? "★ " : ""}{p.name}</b>
                <span>Remembered · {ago(p.last_seen_unix)}</span>
              </div>
              <button type="button" className="btn q" style={{ marginLeft: "auto" }}
                aria-label={p.favourite ? `Unfavourite ${p.name}` : `Favourite ${p.name}`}
                onClick={(e) => {
                  e.preventDefault();
                  void api.setPeerFavourite(p.id, !p.favourite).then(loadPeers).catch(() => {});
                }}>{p.favourite ? "★" : "☆"}</button>
            </label>
          ))}
          {strangers.map((r) => (
            <label key={r.name} className="dev" style={{ cursor: "pointer" }}>
              <input type="radio" name="rcv" checked={selected === r.name}
                onChange={() => { setSelected(r.name); setSelectedPeer(null); }} />
              <div><b>{r.name}</b><span>{r.addr}</span></div>
            </label>
          ))}
          {peers.length === 0 && strangers.length === 0 && (
            <p className="note">No receivers found yet. Open Relay on the other PC's Receive screen, then scan.</p>
          )}
          <button className="btn" onClick={discover} disabled={busy}>Scan for receivers</button>
        </Card>
        <Card title="Pairing code">
          {chosenPeer
            // Remembering removed the code, not the consent: the other PC
            // still has to be on Start receiving, and that is worth saying
            // here because it is the one way this can fail.
            ? <p className="note" data-testid="no-code-needed">
                No code needed — {chosenPeer.name} remembers this PC. It just has to be on
                its Receive screen with Start receiving pressed.
              </p>
            : <input className="in" inputMode="numeric" maxLength={6} placeholder="6 digits from the receiver"
                value={code} onChange={(e) => setCode(e.target.value.replace(/\D/g, ""))} />}
        </Card>
        <PresetCard def={selectedDef} locked={sharing} onSaved={reloadPresets} />
        <Card>
          <Kv k="Connection" v={sharing ? "Direct, encrypted (DTLS-SRTP)" : "—"} />
          <Kv k="Path" v={sharing ? "LAN · host candidates only" : "—"} mono />
          <Kv k="Peer" v={peer ?? "—"} mono />
        </Card>
        <ErrorNote text={error} onDismiss={() => setError(null)} />
        {sharing
          ? <button className="btn acc" onClick={stop} disabled={busy}>Stop sharing</button>
          : <button className="btn acc" onClick={start} disabled={!canStart}
              title={canStart ? "" : "Pick a remembered PC, or enter the 6-digit code shown on the receiver"}>Start sharing</button>}
        {/* S37: one fader per track this share is sending. Rows come from
            the preset the engine read at start, so they match the wire. */}
        {sharing && (
          <MixerCard side="send" rows={sendRows(runningDef?.audio)} sessionKey={`send-${running ?? ""}`}
            note={runningDef?.audio.rest && runningDef.audio.desktop === "game"
              ? "Recordings keep the game and the microphone; everything else is sent live but not written to disk."
              : undefined} />
        )}
        {mock && <p className="note">Preview data — Relay isn't running.</p>}
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
  // Encode size is typed, not picked, and "2560x" is not a valid size — so
  // the text has to live outside the draft. Deriving the field's value from
  // `draft.size` alone makes React reset the box on every keystroke that does
  // not yet parse, which means it can never be typed into at all.
  const [sizeText, setSizeText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const isNew = !!draft && !!def && draft.id !== def.id;
  const builtin = !!draft && BUILTIN_PRESETS.includes(draft.id);

  const edit = (patch: Partial<SharePresetDef>) =>
    setDraft((d) => (d ? { ...d, ...patch } : d));

  const open = (d: SharePresetDef) => {
    setDraft(d);
    setSizeText(d.size ? `${d.size[0]}x${d.size[1]}` : "");
    setError(null);
  };

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
        onAction={locked ? undefined : () => open({ ...def })}>
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
            value={sizeText}
            onChange={(e) => {
              // Blank means "capture at the monitor's native size"; the
              // engine only scales when a cap is given. Anything half-typed
              // reads as blank until it parses.
              setSizeText(e.target.value);
              const m = /^\s*(\d+)\s*[x×]\s*(\d+)\s*$/.exec(e.target.value);
              edit({ size: m ? [Number(m[1]), Number(m[2])] : undefined });
            }} />
        </div>
        <ChipSet label="Audio"
          values={[
            ...(draft.audio.desktop === "off" ? [] : [draft.audio.desktop]),
            ...(draft.audio.desktop === "game" && draft.audio.rest ? ["rest" as const] : []),
            ...(draft.audio.mic ? ["mic" as const] : []),
          ]}
          options={[
            { key: "system", label: "System mix" }, { key: "game", label: "Game only" },
            // Only means anything beside Game: with the system mix there is
            // no "else". Shown always so the choice is discoverable; a click
            // with System selected does nothing rather than silently
            // switching the desktop source.
            { key: "rest", label: "+ everything else" },
            { key: "mic", label: "Microphone" },
          ]}
          onToggle={(k) => {
            // The two desktop sources exclude each other — you cannot capture
            // the whole endpoint and one process at once — but the microphone
            // is its own track and rides alongside either. "Everything else"
            // (S37) is a third track that only exists beside Game.
            if (k === "rest") {
              if (draft.audio.desktop === "game") {
                edit({ audio: { ...draft.audio, rest: !draft.audio.rest } });
              }
              return;
            }
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
              open({ ...draft, id, name: `${draft.name} copy` });
            }}>Duplicate</button>
        </div>
        {!builtin && !isNew && (
          <ConfirmButton label="Delete this preset" confirm="Confirm delete" disabled={busy}
            onConfirm={() => void run(async () => {
              await api.deletePreset(draft.id);
              return undefined;
            })} />
        )}
        {builtin && <p className="note">Built-in presets can be edited but not deleted. Duplicate it to make a version you can remove.</p>}
        <ErrorNote text={error} onDismiss={() => setError(null)} />
      </div>
    </Card>
  );
}

/** The heading names what is being captured, not always "Display 1". */
function sourceTitle(source: SourceTarget): string {
  switch (source.kind) {
    case "display": return `Display ${source.index + 1}`;
    case "window": return "A window";
    case "region": return `Region of Display ${source.display + 1}`;
  }
}

function InstrumentStrip({ s, live, recOn, encoder }: {
  s: Strip; live: boolean; recOn: boolean;
  /** Vendor encoder name from the capability probe; null when unknown or ambiguous. */
  encoder: string | null;
}) {
  const audioSegs = 12;
  const segsFor = (db: number) =>
    live && isFinite(db) ? Math.round(((db + 40) / 40) * audioSegs) : 0;
  const lit = segsFor(s.audioDb);
  const micLit = segsFor(s.micDb);
  const restLit = segsFor(s.restDb);
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
        <div className="hint">{live ? `${encoder ?? "Hardware encoder"} · CPU ${s.cpuPct}%` : "Encoder not loaded"}</div>
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
      {s.restLive && (
        <div>
          <label>Everything else</label>
          <div className="v">{isFinite(s.restDb) ? s.restDb.toFixed(1) : "—"}<u>dB</u></div>
          <div className="seg">{Array.from({ length: audioSegs }, (_, i) => <b key={i} className={i < restLit ? "" : "off"} />)}</div>
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
