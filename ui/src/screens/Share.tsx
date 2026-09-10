import { useEffect, useRef, useState } from "react";
import { Card, Chips, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, onCoreEvents, type DiscoveredReceiver, type ShareStats } from "../lib/ipc";

type Preset = "game" | "daw" | "desktop";

/** Instrument-strip readings, fed by the engine's `stats` events. */
interface Strip {
  mbps: number; latencyMs: number; dropped: number; sent: number;
  gpuPct: number; cpuPct: number; audioDb: number; history: number[];
}
const idleStrip: Strip = {
  mbps: 0, latencyMs: 0, dropped: 0, sent: 0, gpuPct: 0, cpuPct: 0,
  audioDb: -Infinity, history: Array(18).fill(0),
};

export function Share() {
  const { state, mock } = useCore();
  const sharing = state.sharing.kind === "sharing";
  const peer = state.sharing.kind === "sharing" ? state.sharing.peer : null;
  const [preset, setPreset] = useState<Preset>("game");
  const [sysAudio, setSysAudio] = useState(true);
  const [mic, setMic] = useState(false);
  const [cursor, setCursor] = useState(true);
  const [bitrate, setBitrate] = useState(60);
  const [code, setCode] = useState("");
  const [receivers, setReceivers] = useState<DiscoveredReceiver[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [strip, setStrip] = useState<Strip>(idleStrip);
  const histRef = useRef<number[]>(Array(18).fill(0));

  // Live stats from the engine while sharing.
  useEffect(() => {
    let unsub = () => {};
    void onCoreEvents({
      shareStats: (s: ShareStats) => {
        if (s.bitrate_mbps === undefined) return;
        const ceil = Math.max(bitrate, 1);
        const h = [...histRef.current.slice(1), Math.min(1, (s.bitrate_mbps ?? 0) / ceil)];
        histRef.current = h;
        setStrip({
          mbps: s.bitrate_mbps ?? 0,
          latencyMs: Math.round(s.capture_to_send_ms ?? 0),
          dropped: s.dropped ?? 0,
          sent: s.frames ?? 0,
          gpuPct: Math.round(s.encode_ms ? (s.encode_ms / (1000 / 60)) * 100 : 0),
          cpuPct: Math.round((s.cpu_percent ?? 0) * 10) / 10,
          audioDb: s.audio_peak ? 20 * Math.log10(Math.max(1e-4, s.audio_peak)) : -Infinity,
          history: h,
        });
      },
      shareStatus: (st) => { if (st.message) setError(st.message); },
    }).then((u) => { unsub = u; });
    return () => unsub();
  }, [bitrate]);

  useEffect(() => {
    if (!sharing) { setStrip(idleStrip); histRef.current = Array(18).fill(0); }
  }, [sharing]);

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
    try {
      await api.startShare({
        peer: selected, code: code.trim(), bitrate_mbps: bitrate, fps: 60,
        audio: sysAudio, cursor,
      });
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  const stop = async () => {
    setBusy(true);
    try { await api.stopShare(); } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  const canStart = code.trim().length === 6 && !busy;

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Display 1 <em>— this PC</em></h1>
          <Live on={sharing} text={sharing ? "Sharing" : "Not sharing"} />
        </div>
        <OfflineBanner />
        <Chips label="Preset" value={preset} onChange={setPreset}
          options={[{ key: "game", label: "Game" }, { key: "daw", label: "DAW" }, { key: "desktop", label: "Desktop" }]} />
        <div className="preview">
          <div className={"scene" + (sharing ? "" : " idle")} />
          {sharing && <div className="horizon" />}
          <div className="tag"><span>Up to 3840×2160</span><span>60 fps</span><span>HEVC</span></div>
          {sharing
            ? <div className="cap">Preview — press P to hide</div>
            : <div className="idlemsg">Capture starts when you share. Nothing is running now.</div>}
        </div>
        <InstrumentStrip s={strip} live={sharing} />
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
          <Kv k="Bitrate" v={`${bitrate} Mb/s`} />
          <input type="range" min={20} max={80} step={5} value={bitrate}
            onChange={(e) => setBitrate(Number(e.target.value))} />
        </Card>
        <Card>
          <Toggle label="System audio" on={sysAudio} onChange={setSysAudio} />
          <Toggle label="Microphone" on={mic} onChange={setMic} />
          <Toggle label="Cursor" on={cursor} onChange={setCursor} />
        </Card>
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

function InstrumentStrip({ s, live }: { s: Strip; live: boolean }) {
  const audioSegs = 12;
  const lit = live && isFinite(s.audioDb) ? Math.round(((s.audioDb + 40) / 40) * audioSegs) : 0;
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
        <label>Audio</label>
        <div className="v">{live && isFinite(s.audioDb) ? s.audioDb.toFixed(1) : "—"}<u>dB</u></div>
        <div className="seg">{Array.from({ length: audioSegs }, (_, i) => <b key={i} className={i < lit ? "" : "off"} />)}</div>
      </div>
    </div>
  );
}
