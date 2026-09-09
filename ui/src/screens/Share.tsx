import { useState } from "react";
import { Card, Chips, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";

type Preset = "game" | "daw" | "desktop";

/** Instrument-strip readings. Fed by relay-capture once it exists; static now. */
interface Strip { mbps: number; latencyMs: number; dropped: number; sent: number; gpuPct: number; cpuPct: number; audioDb: number; history: number[] }
const idleStrip: Strip = { mbps: 0, latencyMs: 0, dropped: 0, sent: 0, gpuPct: 0, cpuPct: 0, audioDb: -Infinity, history: Array(18).fill(0) };

export function Share() {
  const { state } = useCore();
  const sharing = state.sharing.kind === "sharing";
  const peer = state.sharing.kind === "sharing" ? state.sharing.peer : null;
  const [preset, setPreset] = useState<Preset>("game");
  const [sysAudio, setSysAudio] = useState(true);
  const [mic, setMic] = useState(false);
  const [cursor, setCursor] = useState(true);
  const [record, setRecord] = useState(true);
  const strip = idleStrip;

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
          <div className="tag"><span>3840×2160</span><span>60 fps</span><span>HEVC</span></div>
          {sharing
            ? <div className="cap">Preview — press P to hide</div>
            : <div className="idlemsg">Capture starts when you share. Nothing is running now.</div>}
        </div>
        <InstrumentStrip s={strip} live={sharing} />
      </section>
      <aside className="side">
        <Card title="Receiving on">
          <div className="dev">
            <div className="ic" />
            <div><b>{peer ?? "No receiver paired"}</b><span>{peer ? "Paired · appears as “Relay Camera”" : "Pair by code on the other PC"}</span></div>
          </div>
        </Card>
        <Card>
          <Kv k="Connection" v={sharing ? "Direct, encrypted" : "—"} />
          <Kv k="Path" v={sharing ? "LAN" : "—"} mono />
          <Kv k="Uptime" v={sharing ? "00:00" : "—"} mono />
        </Card>
        <Card>
          <Toggle label="System audio" on={sysAudio} onChange={setSysAudio} />
          <Toggle label="Microphone" on={mic} onChange={setMic} />
          <Toggle label="Cursor" on={cursor} onChange={setCursor} />
          <Toggle label="Local recording" on={record} onChange={setRecord} />
        </Card>
        <button className="btn acc" disabled={!peer && !sharing}>{sharing ? "Stop sharing" : "Start sharing"}</button>
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
        <label>Latency</label>
        <div className="v">{live ? s.latencyMs : "—"}<u>ms</u></div>
        <div className="hint">{live ? "Wired · stable" : "Idle"}</div>
      </div>
      <div>
        <label>Dropped frames</label>
        <div className="v">{live ? s.dropped : "—"}</div>
        <div className="hint">{live ? `of ${s.sent.toLocaleString()} sent` : "Nothing sent"}</div>
      </div>
      <div>
        <label>Load</label>
        <div className="v">{live ? s.gpuPct : "0"}<u>% GPU</u></div>
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
