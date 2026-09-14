import { useEffect, useRef, useState } from "react";
import { Card, Chips, Kv, Live, Slider, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import {
  api, isTauri,
  type ApoStatus, type Limiter, type Preview, type Profile, type SharePreset, type SharePresetDef,
} from "../lib/ipc";

export type Section = "audio" | "display" | "sharing";

/**
 * Per-game editor. Hosts the Audio / Display / Sharing sections from mocks
 * s2 + s3.
 *
 * Audio and Display share one draft of the subject profile: both sections
 * edit it through `update` and both side panels save it with the same button,
 * so switching between the two tabs never loses unsaved changes.
 */
export function Games({ section, onSection }: { section: Section; onSection: (s: Section) => void }) {
  const { state, profiles } = useCore();
  const active = state.active_profile;
  const subject = active ?? profiles[0] ?? null;
  const title = subject?.name ?? "No game";
  const sectionLabel = section === "audio" ? "Audio" : section === "display" ? "Display" : "Sharing";

  // Draft of the subject profile, shared by the Audio and Display editors.
  const [draft, setDraft] = useState<Profile | null>(null);
  const [dirty, setDirty] = useState(false);
  const subjectId = subject?.id ?? null;
  useEffect(() => {
    let live = true;
    if (subjectId) {
      api.getProfile(subjectId)
        .then((p) => { if (live) { setDraft(p); setDirty(false); } })
        .catch(() => { if (live) setDraft(null); });
    } else {
      setDraft(null);
    }
    return () => { live = false; };
  }, [subjectId]);

  const update = (fn: (p: Profile) => void) => {
    setDraft((d) => {
      if (!d) return d;
      const next = structuredClone(d);
      fn(next);
      return next;
    });
    setDirty(true);
  };
  const save = async () => {
    if (!draft) return;
    await api.saveProfile(draft);
    setDirty(false);
  };

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
        {section === "audio" && <AudioSection draft={draft} update={update} />}
        {section === "display" && <DisplaySection draft={draft} update={update} />}
        {section === "sharing" && <SharingSection draft={draft} update={update} />}
      </section>
      {section === "audio" && <AudioSide draft={draft} update={update} save={save} dirty={dirty} />}
      {section === "display" && <DisplaySide draft={draft} update={update} save={save} dirty={dirty} />}
      {section === "sharing" && <SharingSide draft={draft} save={save} dirty={dirty} />}
    </>
  );
}

/* ---------- Audio ---------- */

/** The five bands the UI exposes, and the `EqBand` each one maps to.
 *
 *  A profile can hold any bands the DSP accepts, but a fixed set of five is
 *  what someone tuning by ear can actually hold in their head. Q is wide
 *  enough that the five overlap into a continuous curve rather than five
 *  separate bumps. */
const UI_BANDS = [
  { label: "Sub · 60 Hz", freq: 60, q: 0.9 },
  { label: "Low · 250 Hz", freq: 250, q: 0.9 },
  { label: "Mid · 1 kHz", freq: 1000, q: 0.9 },
  { label: "Presence · 3 kHz", freq: 3000, q: 0.9 },
  { label: "Air · 10 kHz", freq: 10000, q: 0.9 },
];

/** Gains for the five sliders, read out of whatever the profile stores. */
function gainsOf(draft: Profile | null): number[] {
  return UI_BANDS.map(
    (u) => draft?.audio.bands.find((b) => Math.abs(b.freq_hz - u.freq) < 1)?.gain_db ?? 0,
  );
}

/** Write the five sliders back as `EqBand`s, dropping the flat ones so an
 *  untouched profile stores no bands at all — which is what makes the core
 *  treat it as "no audio processing" and skip the exclusive-mode watcher. */
function setGain(p: Profile, index: number, gain: number) {
  const u = UI_BANDS[index];
  const rest = p.audio.bands.filter((b) => Math.abs(b.freq_hz - u.freq) >= 1);
  p.audio.bands = gain === 0 ? rest : [...rest, { freq_hz: u.freq, gain_db: gain, q: u.q }];
  p.audio.bands.sort((a, b) => a.freq_hz - b.freq_hz);
}

function AudioSection({ draft, update }: {
  draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
}) {
  const gains = gainsOf(draft);
  const off = !draft;
  const dbFmt = (v: number) => `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v).toFixed(1)} dB`;
  return (
    <>
      <ExclusiveBanner />
      <EqGraph bands={gains} />
      <div className="two">
        <Card title="Bands" action="Reset"
          onAction={() => update((p) => { p.audio.bands = []; })}>
          {UI_BANDS.map((u, i) => (
            <Slider key={u.label} label={u.label} value={gains[i]} min={-12} max={12} step={0.5}
              format={dbFmt} disabled={off}
              onChange={(v) => update((p) => setGain(p, i, v))} />
          ))}
        </Card>
        <Card title="Tune with your assistant">
          <p className="p">Planned for a later release, not built yet: compare footsteps against explosions and ambience in live game audio, then adjust the profile for this headset using your own API key.</p>
          <p className="note">Until then, the A/B listening test below renders the same clip with and without your chain so you can judge a change by ear.</p>
        </Card>
      </div>
      <HeadsetCorrectionCard draft={draft} update={update} />
      <AbListeningCard profileId={draft?.id ?? null} />
    </>
  );
}

/** The measured headset curve, and whether this profile uses it.
 *
 *  Worth its own card because the curve is invisible otherwise: it is
 *  imported on the Profiles screen and then silently shapes everything you
 *  hear, so the one place you tune audio should say whether it is on and
 *  which headset it came from. */
function HeadsetCorrectionCard({ draft, update }: {
  draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
}) {
  const { hardware, offline, mock } = useCore();

  // The profile's headset, or whatever is plugged in — the same fallback the
  // core uses when it resolves the curve.
  const headsetId = draft?.headset ?? hardware.connected.headset;
  const headset = hardware.headsets.find((h) => h.id === headsetId);
  const points = headset?.curve?.length ?? 0;
  const on = draft?.audio.headset_correction ?? false;

  const sub = !draft
    ? (offline && !mock ? "Core offline." : "No profile selected.")
    : !headset
      ? "No headset chosen for this profile, and none recognised as plugged in."
      : points === 0
        ? `${headset.name} has no measured curve yet — import one on the Profiles screen.`
        : `${headset.name} · ${points} measured points, fitted to at most 8 filters ahead of your own bands.`;

  return (
    <Card title="Headset correction">
      <Toggle on={on}
        onChange={draft && points > 0 ? (v) => update((p) => { p.audio.headset_correction = v; }) : undefined}
        label="Correct this headset's measured response" sub={sub} />
      {points > 0 && on && (
        <p className="p small">Correction runs first, so the bands above are your taste on top of a
          neutral headset rather than a fight with it.</p>
      )}
    </Card>
  );
}

/** Latency / chain-state / route readout. Latency is the chain's real
 *  figure at 48 kHz: EQ adds none, the limiter 1 ms of look-ahead, HRTF one
 *  128-frame partition (2.7 ms). The route reflects whether the endpoint APO
 *  is actually registered — without it the profile is preview-only. */
function ChainReadout({ chain, hrtf, tamer }: { chain: string; hrtf: boolean; tamer: boolean }) {
  const { offline } = useCore();
  const [apo, setApo] = useState<ApoStatus | null>(null);
  useEffect(() => {
    api.apoStatus().then(setApo).catch(() => setApo(null));
  }, [offline]);

  const ms = (tamer ? 1.0 : 0) + (hrtf ? 2.7 : 0);
  const route = apo === null
    ? "Endpoint · status unknown"
    : apo.installed
      ? `Endpoint APO${apo.running ? "" : " · idle"}`
      : "Preview only · APO not installed";
  return (
    <Card>
      <Kv k="Processing" v={chain === "active" ? `${ms.toFixed(1)} ms` : "0 ms"} mono />
      <Kv k="Chain" v={chain === "bypass" ? "Bypass" : chain === "active" ? "Active" : "Bypassed by game (exclusive)"} />
      <Kv k="Route" v={route} />
    </Card>
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

/** Default limiter when the "explosion tamer" toggle is switched on. Matches
 *  the copy beside it, and the band-split the DSP was tuned against. */
const TAMER: Limiter = { below_hz: 120, threshold_db: -10 };

function AudioSide({ draft, update, save, dirty }: {
  draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
  save: () => Promise<void>;
  dirty: boolean;
}) {
  const { state, hardware } = useCore();
  const [picking, setPicking] = useState(false);
  const chain = state.audio_chain;
  const off = !draft;
  const hrtf = draft?.audio.hrtf ?? false;
  const tamer = !!draft?.audio.limiter;
  const toShare = draft?.audio.apply_to_share ?? false;
  // The profile's headset, falling back to whatever the core resolved from
  // the default endpoint so the card is never blank.
  const headset = hardware.headsets.find((h) => h.id === (draft?.headset ?? hardware.connected.headset));
  return (
    <aside className="side">
      <Card title="Headset" action={off ? undefined : picking ? "Done" : "Change"}
        onAction={off ? undefined : () => setPicking(!picking)}>
        {picking ? (
          <label className="field">
            <select value={draft?.headset ?? ""}
              onChange={(e) => {
                const v = e.target.value;
                update((p) => { if (v) p.headset = v; else delete p.headset; });
                setPicking(false);
              }}>
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
        <Toggle label="Spatial audio" sub="HRTF · Relay Arena" on={hrtf}
          onChange={off ? undefined : (v) => update((p) => { p.audio.hrtf = v; })} />
        <Toggle label="Explosion tamer" sub="Soft limiter under 120 Hz" on={tamer}
          onChange={off ? undefined : (v) => update((p) => {
            if (v) p.audio.limiter = { ...TAMER }; else delete p.audio.limiter;
          })} />
        <Toggle label="Apply to share feed" sub="Call hears what you hear" on={toShare}
          onChange={off ? undefined : (v) => update((p) => { p.audio.apply_to_share = v; })} />
      </Card>
      <ChainReadout chain={chain} hrtf={hrtf} tamer={tamer} />
      <button className="btn acc" disabled={off || !dirty} onClick={() => void save()}>
        {dirty ? "Save to profile" : "Saved"}
      </button>
      <p className="note">Runs inside Windows audio on this headset only. Other apps and your desktop are unaffected.</p>
    </aside>
  );
}

/* ---------- Display ---------- */

const RESPONSE_LEVELS = ["off", "normal", "fast", "faster"];

/** Advertised-VCP check. Unknown capabilities (never probed) allow the
 *  standard codes, matching the core's `plan_writes` behaviour. */
function vcpAvailable(codes: number[] | undefined, code: number): boolean {
  return !codes || codes.includes(code);
}

function DisplaySection({ draft, update }: { draft: Profile | null; update: (fn: (p: Profile) => void) => void }) {
  const { hardware } = useCore();
  const signed = (v: number) => `${v > 0 ? "+" : ""}${v}`;
  const gpu = draft?.display.gpu ?? { vibrance: 50, gamma: 1, contrast: 0, shadow_lift: 0, hue_deg: 0 };
  const mon = draft?.display.monitor ?? {};
  const off = !draft;

  // The profile's monitor (or the main connected one) decides which DDC/CI
  // controls exist. Vendor codes (black eq, response) stay disabled until a
  // verified opcode for the model lands in the quirks table.
  const mainId = hardware.connected.monitors.find((m) => m.primary)?.id ?? null;
  const libMonitor = hardware.monitors.find((m) => m.id === (draft?.monitor ?? mainId));
  const codes = libMonitor?.ddcci;
  const hue = ((gpu.hue_deg + 180) % 360) - 180;
  const responseIx = Math.max(0, RESPONSE_LEVELS.indexOf(mon.response ?? "off"));
  return (
    <>
      <div className="cmp">
        <div className="a"><div className="cap">Monitor default</div><Scene /></div>
        <div className="b"><div className="cap">Game profile</div><Scene /></div>
        <div className="div" />
      </div>
      <div className="two">
        <Card title="GPU color" action="Reset"
          onAction={() => update((p) => { p.display.gpu = { vibrance: 50, gamma: 1, contrast: 0, shadow_lift: 0, hue_deg: 0 }; })}>
          <Slider label="Vibrance" value={gpu.vibrance} min={0} max={100} format={signed} disabled={off}
            onChange={(v) => update((p) => { p.display.gpu.vibrance = v; })} />
          <Slider label="Gamma" value={gpu.gamma} min={0.5} max={1.5} step={0.01} format={(v) => v.toFixed(2)} disabled={off}
            onChange={(v) => update((p) => { p.display.gpu.gamma = v; })} />
          <Slider label="Contrast" value={gpu.contrast} min={-50} max={50} format={signed} disabled={off}
            onChange={(v) => update((p) => { p.display.gpu.contrast = v; })} />
          <Slider label="Shadow lift" value={gpu.shadow_lift} min={0} max={50} format={signed} disabled={off}
            onChange={(v) => update((p) => { p.display.gpu.shadow_lift = v; })} />
          <Slider label="Hue" value={hue} min={-180} max={180} format={(v) => `${v}°`} disabled={off}
            onChange={(v) => update((p) => { p.display.gpu.hue_deg = ((v % 360) + 360) % 360; })} />
        </Card>
        <Card title="Monitor" action="Reset" onAction={() => update((p) => { p.display.monitor = {}; })}>
          <Slider label="Brightness" value={mon.brightness ?? 50} min={0} max={100} disabled={off || !vcpAvailable(codes, 0x10)}
            onChange={(v) => update((p) => { p.display.monitor.brightness = v; })} />
          <Slider label="Contrast" value={mon.contrast ?? 50} min={0} max={100} disabled={off || !vcpAvailable(codes, 0x12)}
            onChange={(v) => update((p) => { p.display.monitor.contrast = v; })} />
          <Slider label="Black equalizer" value={mon.black_equalizer ?? 10} min={0} max={20} disabled
            onChange={(v) => update((p) => { p.display.monitor.black_equalizer = v; })} />
          <Slider label="Response" value={responseIx} min={0} max={3}
            format={(v) => ["Off", "Normal", "Fast", "Faster"][v] ?? ""} disabled
            onChange={(v) => update((p) => { p.display.monitor.response = v === 0 ? undefined : RESPONSE_LEVELS[v]; })} />
          <Slider label="Sharpness" value={mon.sharpness ?? 50} min={0} max={100} disabled={off || !vcpAvailable(codes, 0x87)}
            onChange={(v) => update((p) => { p.display.monitor.sharpness = v; })} />
          <p className="note">Black equalizer and Response live on vendor-private DDC/CI codes that differ per model and are not published. Relay will not guess at one — writing the wrong code changes a setting you did not ask for. They stay off until a verified opcode for your panel is added.</p>
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

/** "NvAPI + gamma ramp + DDC/CI", from the live apply. */
function appliedViaText(state: ReturnType<typeof useCore>["state"]): string {
  if (state.display_state !== "applied") return "—";
  const parts = [
    state.display_via.nvapi && "NvAPI",
    state.display_via.gamma && "gamma ramp",
    state.display_via.ddcci && "DDC/CI",
  ].filter(Boolean) as string[];
  return parts.length ? parts.join(" + ") : "nothing to change";
}

function DisplaySide({ draft, update, save, dirty }: {
  draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
  save: () => Promise<void>;
  dirty: boolean;
}) {
  const { state, hardware } = useCore();
  const [picking, setPicking] = useState(false);
  const mainId = hardware.connected.monitors.find((m) => m.primary)?.id ?? null;
  const monitor = hardware.monitors.find((m) => m.id === (draft?.monitor ?? mainId));
  const plugged = monitor && hardware.connected.monitors.find((c) => c.id === monitor.id);
  const d = draft?.display;
  const unsupported = state.display_via.unsupported ?? [];
  return (
    <aside className="side">
      <Card title="Monitor" action={picking ? "Done" : "Change"} onAction={() => setPicking(!picking)}>
        {picking ? (
          <label className="field">
            <select value={draft?.monitor ?? ""} onChange={(e) => { const v = e.target.value; update((p) => { if (v) p.monitor = v; else delete p.monitor; }); setPicking(false); }}>
              <option value="">Any monitor</option>
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
        <Toggle label="Follow game focus" sub="Apply on launch, restore on exit" on={d?.follow_focus ?? true}
          onChange={(v) => update((p) => { p.display.follow_focus = v; })} />
        <Toggle label="Second monitor" sub="Leave untouched" on={d?.leave_other_monitors ?? true}
          onChange={(v) => update((p) => { p.display.leave_other_monitors = v; })} />
        <Toggle label="Send true colors to share" sub="Call sees the unfiltered feed" on={d?.share_true_colors ?? true}
          onChange={(v) => update((p) => { p.display.share_true_colors = v; })} />
      </Card>
      <Card>
        <Kv k="Applied via" v={appliedViaText(state)} />
        {unsupported.length > 0 && <Kv k="Not on this hardware" v={unsupported.join(", ")} />}
        <Kv k="In-game hooks" v="None" />
        <Kv k="Backup" v={state.display_state === "applied" ? "Saved before change" : "Nothing to back up"} />
      </Card>
      <button className="btn acc" disabled={!draft || !dirty} onClick={() => void save()}>
        {dirty ? "Save to profile" : "Saved"}
      </button>
      <p className="note">Original monitor and GPU settings are stored on disk and restored on exit, crash, or reboot.</p>
    </aside>
  );
}

/* ---------- Sharing (per-game preset) ---------- */

function SharingSection({ draft, update }: {
  draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
}) {
  const preset = draft?.share ?? "off";
  return (
    <Card title="Share preset for this game">
      <p className="p" style={{ marginBottom: 12 }}>Which encoder, audio sources and cursor setting Relay uses when you share while this game has focus.</p>
      <Chips<SharePreset> label="Preset" value={preset}
        onChange={draft ? (v) => update((p) => { p.share = v; }) : undefined}
        options={[{ key: "game", label: "Game" }, { key: "daw", label: "DAW" }, { key: "desktop", label: "Desktop" }, { key: "off", label: "Off" }]} />
      {!draft && <p className="note">No profile selected.</p>}
    </Card>
  );
}

/** The real settings behind the chosen preset, read from `presets.json`
 *  rather than restated here — the Share screen lets you edit those numbers,
 *  and two places quoting different values would be worse than none. */
function SharingSide({ draft, save, dirty }: {
  draft: Profile | null;
  save: () => Promise<void>;
  dirty: boolean;
}) {
  const { offline } = useCore();
  const [presets, setPresets] = useState<SharePresetDef[]>([]);

  useEffect(() => {
    let live = true;
    api.listPresets()
      .then((r) => { if (live) setPresets(r.presets); })
      .catch(() => { if (live) setPresets([]); });
    return () => { live = false; };
  }, [offline]);

  const chosen = draft?.share ?? "off";
  const def = presets.find((p) => p.id === chosen);
  const audio: Record<SharePresetDef["audio"], string> = {
    system: "System mix", game: "Game only", mic: "Microphone", off: "None",
  };

  return (
    <aside className="side">
      <Card title={def ? `${def.name} preset` : "Preset"}>
        {chosen === "off" ? (
          <p className="p">Sharing is off for this game. Relay still shares when you start it manually; this only decides what happens automatically.</p>
        ) : def ? (
          <>
            <Kv k="Encoder" v="HEVC · hardware" />
            <Kv k="Bitrate" v={`${def.bitrate_mbps} Mb/s`} mono />
            <Kv k="Frame rate" v={`${def.fps} fps`} mono />
            <Kv k="Size" v={def.size ? `${def.size[0]}×${def.size[1]}` : "Native"} mono />
            <Kv k="Audio" v={audio[def.audio]} />
            <Kv k="Cursor" v={def.cursor ? "Shown" : "Hidden"} />
            <Kv k="Replay buffer" v={def.replay_secs ? `${def.replay_secs} s` : "Off"} mono />
          </>
        ) : (
          <p className="note">Reading presets…</p>
        )}
      </Card>
      <button className="btn acc" disabled={!draft || !dirty} onClick={() => void save()}>
        {dirty ? "Save to profile" : "Saved"}
      </button>
      <p className="note">Edit these numbers on the Share screen. Everything stays on your local network. Nothing on this PC was changed.</p>
    </aside>
  );
}
