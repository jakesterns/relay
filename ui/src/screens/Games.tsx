import { useEffect, useRef, useState } from "react";
import { Card, Chips, Kv, Live, Slider, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, isTauri, type Preview } from "../lib/ipc";

export type Section = "audio" | "display" | "sharing";

/**
 * Per-game editor. Hosts the Audio / Display / Sharing sections from mocks
 * s2 + s3. The Audio and Display rail items land here with the section preset.
 * Values are local state until profile editing is wired to `save_profile`.
 */
export function Games({ section, onSection }: { section: Section; onSection: (s: Section) => void }) {
  const { state, profiles } = useCore();
  const active = state.active_profile;
  const subject = active ?? profiles[0] ?? null;
  const title = subject?.name ?? "No game";
  const sectionLabel = section === "audio" ? "Audio" : section === "display" ? "Display" : "Sharing";

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>{title} <em>— {sectionLabel}</em></h1>
          <Live on={!!active} text={active ? "Active · in focus" : "Not in focus"} />
        </div>
        <OfflineBanner />
        <Chips label="Section" value={section} onChange={onSection}
          options={[{ key: "audio", label: "Audio" }, { key: "display", label: "Display" }, { key: "sharing", label: "Sharing" }]} />
        {section === "audio" && <AudioSection profileId={subject?.id ?? null} />}
        {section === "display" && <DisplaySection />}
        {section === "sharing" && <SharingSection />}
      </section>
      {section === "audio" && <AudioSide />}
      {section === "display" && <DisplaySide />}
      {section === "sharing" && <SharingSide />}
    </>
  );
}

/* ---------- Audio ---------- */

const defaultBands = [
  { label: "Sub · 60 Hz", gain: -5.0 },
  { label: "Low · 250 Hz", gain: -1.0 },
  { label: "Mid · 1 kHz", gain: 0.5 },
  { label: "Presence · 3 kHz", gain: 4.5 },
  { label: "Air · 10 kHz", gain: -1.5 },
];

function AudioSection({ profileId }: { profileId: string | null }) {
  const [bands, setBands] = useState(defaultBands);
  const dbFmt = (v: number) => `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v).toFixed(1)} dB`;
  return (
    <>
      <ExclusiveBanner />
      <EqGraph bands={bands.map((b) => b.gain)} />
      <div className="two">
        <Card title="Bands" action="Reset" onAction={() => setBands(defaultBands)}>
          {bands.map((b, i) => (
            <Slider key={b.label} label={b.label} value={b.gain} min={-12} max={12} step={0.5} format={dbFmt}
              onChange={(v) => setBands(bands.map((x, j) => (j === i ? { ...x, gain: v } : x)))} />
          ))}
        </Card>
        <Card title="Tune with your assistant">
          <p className="p">Compares footsteps against explosions and ambience in live game audio, then adjusts the profile for this headset. Uses your own API key.</p>
          <div className="ab">
            <button className="btn acc" disabled>Run listening test</button>
            <button className="btn q" disabled>Not run yet</button>
          </div>
        </Card>
      </div>
      <AbListeningCard profileId={profileId} />
    </>
  );
}

/** Shown while the foreground game holds the endpoint in WASAPI-exclusive
 *  mode — Windows routes its audio around the APO, so the profile is silent. */
function ExclusiveBanner() {
  const { state } = useCore();
  if (state.audio_chain !== "exclusivebypassed") return null;
  return (
    <div className="warnbanner">
      <i />
      This game opens the headset exclusively, so Relay's EQ is bypassed. If the game has an
      "exclusive mode" / "WASAPI exclusive" audio option, switch it to shared to use this profile.
    </div>
  );
}

/** Offline A/B listening test: render the saved profile's chain over a demo
 *  clip in the core, then flip between original and processed playback. */
function AbListeningCard({ profileId }: { profileId: string | null }) {
  const [preview, setPreview] = useState<Preview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [playing, setPlaying] = useState<"original" | "processed" | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);

  useEffect(() => () => audioRef.current?.pause(), []);

  const render = async () => {
    if (!profileId) return;
    setBusy(true);
    setError(null);
    try {
      setPreview(await api.renderPreview(profileId));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const play = async (which: "original" | "processed") => {
    if (!preview) return;
    audioRef.current?.pause();
    if (playing === which) {
      setPlaying(null);
      return;
    }
    const { convertFileSrc } = await import("@tauri-apps/api/core");
    const el = new Audio(`${convertFileSrc(preview[which])}?t=${Date.now()}`);
    el.onended = () => setPlaying(null);
    audioRef.current = el;
    setPlaying(which);
    void el.play();
  };

  return (
    <Card title="A/B listening test">
      <p className="p">
        Hear the saved profile before the audio driver ships: the core renders a demo clip
        (footsteps, an explosion for the tamer, a reference beep) through this profile's chain.
        Nothing plays through the game or your endpoint settings.
      </p>
      <div className="ab">
        <button className="btn" disabled={!profileId || busy || !isTauri()} onClick={render}>
          {busy ? "Rendering…" : preview ? "Re-render" : "Render A/B"}
        </button>
        <button className="btn q" disabled={!preview} onClick={() => play("original")}>
          {playing === "original" ? "■ Original" : "▶ Original"}
        </button>
        <button className="btn acc" disabled={!preview} onClick={() => play("processed")}>
          {playing === "processed" ? "■ Processed" : "▶ Processed"}
        </button>
      </div>
      {preview && (
        <p className="note" style={{ marginTop: 8 }}>
          {preview.sample_rate / 1000} kHz · {preview.hrtf_applied ? "EQ + limiter + HRTF" : "EQ + limiter (no HRTF at this rate)"}
        </p>
      )}
      {error && <p className="note" style={{ marginTop: 8 }}>{error}</p>}
      {!isTauri() && <p className="note" style={{ marginTop: 8 }}>Requires the Relay core (desktop app).</p>}
    </Card>
  );
}

/** Static-geometry EQ graph from s2; the curve bends with the five band gains. */
function EqGraph({ bands }: { bands: number[] }) {
  // x positions of the five bands on the 900-wide viewBox; y = 135 - gain * (80/6)
  const xs = [120, 330, 555, 700, 830];
  const pts = bands.map((g, i) => [xs[i], 135 - g * (80 / 6)] as const);
  const path = ["M52 150", ...pts.map(([x, y]) => `L${x} ${y}`), "L884 150"].join(" ");
  return (
    <div className="eq">
      <svg viewBox="0 0 900 270" preserveAspectRatio="none">
        <g stroke="rgba(255,255,255,.06)">
          <line x1="52" y1="55" x2="884" y2="55" /><line x1="52" y1="135" x2="884" y2="135" stroke="rgba(255,255,255,.12)" /><line x1="52" y1="215" x2="884" y2="215" />
          <line x1="190" y1="20" x2="190" y2="250" /><line x1="330" y1="20" x2="330" y2="250" /><line x1="470" y1="20" x2="470" y2="250" /><line x1="610" y1="20" x2="610" y2="250" /><line x1="750" y1="20" x2="750" y2="250" />
        </g>
        <rect x="470" y="20" width="170" height="230" fill="rgba(201,169,106,.06)" />
        <text x="14" y="59" fill="#5A544C" fontFamily="GeistMono" fontSize="10">+6</text>
        <text x="20" y="139" fill="#5A544C" fontFamily="GeistMono" fontSize="10">0</text>
        <text x="14" y="219" fill="#5A544C" fontFamily="GeistMono" fontSize="10">−6</text>
        <path d="M52 150 C150 148 220 132 330 138 S470 160 560 145 S700 120 780 128 S860 150 884 150" fill="none" stroke="#5A544C" strokeWidth="1.5" strokeDasharray="4 4" />
        <path d={path} fill="none" stroke="#C9A96A" strokeWidth="2" strokeLinejoin="round" style={{ transition: "d .4s var(--ease)" }} />
        <g fill="#ECE6DC">{pts.map(([x, y], i) => <circle key={i} cx={x} cy={y} r="4" />)}</g>
      </svg>
      <div className="leg"><span><i style={{ background: "#C9A96A" }} />Profile</span><span><i style={{ background: "#5A544C" }} />Headset raw response</span></div>
      <div className="band">Footsteps · 1.8–4.5 kHz</div>
      <div className="lbl"><span>20</span><span>100</span><span>500</span><span>1k</span><span>4k</span><span>10k</span><span>20k</span></div>
    </div>
  );
}

function AudioSide() {
  const { state, hardware } = useCore();
  const [hrtf, setHrtf] = useState(true);
  const [tamer, setTamer] = useState(true);
  const [toShare, setToShare] = useState(false);
  const [picking, setPicking] = useState(false);
  // Local pick until the section is wired to save_profile; defaults to the
  // headset the core resolved from the default endpoint.
  const [pick, setPick] = useState<string | null>(null);
  const chain = state.audio_chain;
  const headset = hardware.headsets.find((h) => h.id === (pick ?? hardware.connected.headset));
  return (
    <aside className="side">
      <Card title="Headset" action={picking ? "Done" : "Change"} onAction={() => setPicking(!picking)}>
        {picking ? (
          <label className="field">
            <select value={headset?.id ?? ""} onChange={(e) => { setPick(e.target.value || null); setPicking(false); }}>
              <option value="">From plugged hardware</option>
              {hardware.headsets.map((h) => <option key={h.id} value={h.id}>{h.name}</option>)}
            </select>
          </label>
        ) : headset ? (
          <div className="hw"><div className="ic" /><div><b>{headset.name}</b><span>{headset.curve ? `Measured curve · ${headset.source || "imported"}` : "No measured curve yet"}{hardware.connected.headset === headset.id ? " · plugged" : ""}</span></div></div>
        ) : (
          <div className="hw"><div className="ic" /><div><b>No headset selected</b><span>Add one on the Profiles screen</span></div></div>
        )}
      </Card>
      <Card>
        <Toggle label="Spatial audio" sub="HRTF · Relay Arena" on={hrtf} onChange={setHrtf} />
        <Toggle label="Explosion tamer" sub="Soft limiter under 120 Hz" on={tamer} onChange={setTamer} />
        <Toggle label="Apply to share feed" sub="Call hears what you hear" on={toShare} onChange={setToShare} />
      </Card>
      <Card>
        <Kv k="Processing" v={chain === "active" ? "0.4 ms" : "0 ms"} mono />
        <Kv k="Chain" v={chain === "bypass" ? "Bypass" : chain === "active" ? "Active" : "Bypassed by game (exclusive)"} />
        <Kv k="Route" v="Endpoint · no virtual device" />
      </Card>
      <button className="btn acc" disabled>Save to profile</button>
      <p className="note">Runs inside Windows audio on this headset only. Other apps and your desktop are unaffected.</p>
    </aside>
  );
}

/* ---------- Display ---------- */

function DisplaySection() {
  const [gpu, setGpu] = useState({ vibrance: 68, gamma: 0.86, contrast: 5, shadow: 12, hue: 0 });
  const [mon, setMon] = useState({ brightness: 80, contrast: 70, blackEq: 14, response: 2 });
  const signed = (v: number) => `${v > 0 ? "+" : ""}${v}`;
  return (
    <>
      <div className="cmp">
        <div className="a"><div className="cap">Monitor default</div><Scene /></div>
        <div className="b"><div className="cap">Game profile</div><Scene /></div>
        <div className="div" />
      </div>
      <div className="two">
        <Card title="GPU color" action="Reset" onAction={() => setGpu({ vibrance: 50, gamma: 1, contrast: 0, shadow: 0, hue: 0 })}>
          <Slider label="Vibrance" value={gpu.vibrance} min={0} max={100} format={signed} onChange={(v) => setGpu({ ...gpu, vibrance: v })} />
          <Slider label="Gamma" value={gpu.gamma} min={0.5} max={1.5} step={0.01} format={(v) => v.toFixed(2)} onChange={(v) => setGpu({ ...gpu, gamma: v })} />
          <Slider label="Contrast" value={gpu.contrast} min={-50} max={50} format={signed} onChange={(v) => setGpu({ ...gpu, contrast: v })} />
          <Slider label="Shadow lift" value={gpu.shadow} min={0} max={50} format={signed} onChange={(v) => setGpu({ ...gpu, shadow: v })} />
          <Slider label="Hue" value={gpu.hue} min={-180} max={180} format={(v) => `${v}°`} onChange={(v) => setGpu({ ...gpu, hue: v })} />
        </Card>
        <Card title="Monitor" action="Reset" onAction={() => setMon({ brightness: 50, contrast: 50, blackEq: 10, response: 1 })}>
          <Slider label="Brightness" value={mon.brightness} min={0} max={100} onChange={(v) => setMon({ ...mon, brightness: v })} />
          <Slider label="Contrast" value={mon.contrast} min={0} max={100} onChange={(v) => setMon({ ...mon, contrast: v })} />
          <Slider label="Black equalizer" value={mon.blackEq} min={0} max={20} onChange={(v) => setMon({ ...mon, blackEq: v })} />
          <Slider label="Response" value={mon.response} min={0} max={3} format={(v) => ["Off", "Normal", "Fast", "Faster"][v] ?? ""} onChange={(v) => setMon({ ...mon, response: v })} />
          <Slider label="Sharpness" value={50} min={0} max={100} disabled />
        </Card>
      </div>
    </>
  );
}

function Scene() {
  return (
    <>
      <div className="frame" style={{ left: 60, top: 70, width: 120, height: 90 }} />
      <div className="frame" style={{ left: 250, top: 60, width: 90, height: 110 }} />
      <div className="fig" style={{ left: 120, top: 150 }} />
      <div className="fig" style={{ left: 300, top: 200 }} />
    </>
  );
}

function DisplaySide() {
  const { state, hardware } = useCore();
  const [follow, setFollow] = useState(true);
  const [second, setSecond] = useState(true);
  const [trueColors, setTrueColors] = useState(true);
  const [picking, setPicking] = useState(false);
  const [pick, setPick] = useState<string | null>(null);
  const mainId = hardware.connected.monitors.find((m) => m.primary)?.id ?? null;
  const monitor = hardware.monitors.find((m) => m.id === (pick ?? mainId));
  const plugged = monitor && hardware.connected.monitors.find((c) => c.id === monitor.id);
  return (
    <aside className="side">
      <Card title="Monitor" action={picking ? "Done" : "Change"} onAction={() => setPicking(!picking)}>
        {picking ? (
          <label className="field">
            <select value={monitor?.id ?? ""} onChange={(e) => { setPick(e.target.value || null); setPicking(false); }}>
              <option value="">Main monitor</option>
              {hardware.monitors.map((m) => <option key={m.id} value={m.id}>{m.name}</option>)}
            </select>
          </label>
        ) : monitor ? (
          <div className="hw"><div className="ic sq" /><div><b>{monitor.name}</b><span>{monitor.panel || "Panel unknown"}{plugged ? (plugged.primary ? " · main" : " · second") : ""}{monitor.ddcci ? ` · DDC/CI` : ""}</span></div></div>
        ) : (
          <div className="hw"><div className="ic sq" /><div><b>No monitor selected</b><span>Add one on the Profiles screen</span></div></div>
        )}
      </Card>
      <Card>
        <Toggle label="Follow game focus" sub="Apply on launch, restore on exit" on={follow} onChange={setFollow} />
        <Toggle label="Second monitor" sub="Leave untouched" on={second} onChange={setSecond} />
        <Toggle label="Send true colors to share" sub="Call sees the unfiltered feed" on={trueColors} onChange={setTrueColors} />
      </Card>
      <Card>
        <Kv k="Applied via" v="NvAPI + DDC/CI" />
        <Kv k="In-game hooks" v="None" />
        <Kv k="Backup" v={state.display_state === "applied" ? "Saved before change" : "Nothing to back up"} />
      </Card>
      <button className="btn acc" disabled>Save to profile</button>
      <p className="note">Original monitor and GPU settings are stored on disk and restored on exit, crash, or reboot.</p>
    </aside>
  );
}

/* ---------- Sharing (per-game preset) ---------- */

function SharingSection() {
  const [preset, setPreset] = useState<"game" | "daw" | "desktop" | "off">("game");
  return (
    <Card title="Share preset for this game">
      <p className="p" style={{ marginBottom: 12 }}>Which encoder, audio sources and cursor setting Relay uses when you share while this game has focus.</p>
      <Chips label="Preset" value={preset} onChange={setPreset}
        options={[{ key: "game", label: "Game" }, { key: "daw", label: "DAW" }, { key: "desktop", label: "Desktop" }, { key: "off", label: "Off" }]} />
    </Card>
  );
}

function SharingSide() {
  return (
    <aside className="side">
      <Card>
        <Kv k="Encoder" v="HEVC · NVENC" />
        <Kv k="Target" v="4K60 · 40–80 Mb/s" mono />
        <Kv k="Audio" v="System + profile" />
      </Card>
      <p className="note">Everything stays on your local network. Nothing on this PC was changed.</p>
    </aside>
  );
}
