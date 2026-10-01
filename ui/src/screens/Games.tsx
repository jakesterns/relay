import { useEffect, useId, useRef, useState } from "react";
import { Card, Chips, ConfirmButton, ErrorNote, Kv, Live, Slider, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { clearDraft, getDraft, setDraft, useDraft } from "../lib/drafts";
import { errText } from "../lib/err";
import {
  api, GOALS, isTauri, presetAudioLabel,
  type ApoStatus, type ColorInfo, type EqBand, type GameEqAction, type GameEqExport, type GameEqStatus,
  type Goal, type GpuColor, type Limiter, type Preview, type Profile,
  type SharePreset, type SharePresetDef,
} from "../lib/ipc";
import { buildRamp, cascadeDb } from "../lib/honest";
import { colorSummary } from "./Profiles";
import { LearnLookCard } from "../components/LearnLookCard";

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
  const sectionLabel = section === "audio" ? "Audio" : section === "display" ? "Display" : "Sharing";

  // The profile these editors are working on. An unsaved edit lives outside
  // this component (see lib/drafts) so it survives both things that used to
  // throw it away without a word: leaving the screen, and the focused game
  // changing under you.
  const kept = useDraft();
  const [loaded, setLoaded] = useState<Profile | null>(null);
  const draft = kept?.profile ?? loaded;
  const dirty = kept !== null;
  const subjectId = subject?.id ?? null;

  useEffect(() => {
    // An unsaved edit outranks whatever is in focus: reloading here is exactly
    // the silent discard this guards against.
    if (getDraft()) return;
    let live = true;
    if (subjectId) {
      api.getProfile(subjectId)
        .then((p) => { if (live) setLoaded(p); })
        .catch(() => { if (live) setLoaded(null); });
    } else {
      setLoaded(null);
    }
    return () => { live = false; };
  }, [subjectId, dirty]);

  const update = (fn: (p: Profile) => void) => {
    const base = getDraft()?.profile ?? draft;
    if (!base) return;
    const next = structuredClone(base);
    fn(next);
    setDraft(next);
  };
  const save = async () => {
    const d = getDraft()?.profile ?? draft;
    if (!d) return;
    await api.saveProfile(d);
    setLoaded(d);
    clearDraft();
  };
  const discard = () => clearDraft();
  /** S46: a game-EQ action saved the profile in the core. Bring its game-EQ
   *  fields into whatever this screen holds, so a later Save of an unsaved
   *  slider edit cannot put the old values back. */
  const syncGameEq = (fresh: Profile) => {
    const kept = getDraft()?.profile;
    if (!kept || kept.id !== fresh.id) {
      setLoaded(fresh);
      return;
    }
    const next = structuredClone(kept);
    for (const k of ["learn_game_eq", "game_eq_goal", "game_eq_auto_apply", "game_eq"] as const) {
      if (fresh.audio[k] === undefined) delete next.audio[k];
      else (next.audio as unknown as Record<string, unknown>)[k] = structuredClone(fresh.audio[k]);
    }
    setDraft(next);
  };

  // The edit belongs to a profile that is no longer the one in focus.
  const stray = kept && subjectId !== null && kept.profile.id !== subjectId ? kept.profile : null;

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>{draft?.name ?? subject?.name ?? "No game"} <em>— {sectionLabel}</em></h1>
          <Live on={!!active} text={active ? "Active · in focus" : "Not in focus"} />
        </div>
        <OfflineBanner />
        {stray && (
          <div className="warnbanner">
            <i />
            <span className="msg">
              <b>{stray.name}</b> has unsaved changes. {subject?.name ?? "Another game"} is in focus
              now — your edit is kept until you say otherwise.
            </span>
            <button type="button" className="btn q" onClick={() => void save()}>Save {stray.name}</button>
            <ConfirmButton label="Discard…" confirm="Discard changes" onConfirm={discard} />
          </div>
        )}
        <Chips label="Section" value={section} onChange={onSection}
          options={[{ key: "audio", label: "Audio" }, { key: "display", label: "Display" }, { key: "sharing", label: "Sharing" }]} />
        {section === "audio" && <AudioSection draft={draft} update={update} onGameEq={syncGameEq} />}
        {section === "display" && <DisplaySection draft={draft} update={update} />}
        {section === "sharing" && <SharingSection draft={draft} update={update} />}
      </section>
      {section === "audio" && <AudioSide draft={draft} update={update} save={save} dirty={dirty} />}
      {section === "display" && <DisplaySide draft={draft} update={update} save={save} dirty={dirty} />}
      {section === "sharing" && <SharingSide draft={draft} save={save} dirty={dirty} />}
    </>
  );
}

/** The save button all three side panels share.
 *
 *  A refused save used to reject into nothing: the button went back to
 *  "Save to profile" and the reason never reached the screen. */
function SaveRow({ disabled, dirty, save }: {
  disabled: boolean; dirty: boolean; save: () => Promise<void>;
}) {
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setErr(null);
    setBusy(true);
    try { await save(); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  return (
    <>
      <button className="btn acc" disabled={disabled || !dirty || busy} onClick={() => void run()}>
        {busy ? "Saving…" : dirty ? "Save to profile" : "Saved"}
      </button>
      <ErrorNote text={err} onDismiss={() => setErr(null)} />
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

function AudioSection({ draft, update, onGameEq }: {
  draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
  onGameEq: (fresh: Profile) => void;
}) {
  const { hardware } = useCore();
  const gains = gainsOf(draft);
  const off = !draft;
  const dbFmt = (v: number) => `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v).toFixed(1)} dB`;
  // Same headset resolution as the correction card below and the core.
  const headset = hardware.headsets.find((h) => h.id === (draft?.headset ?? hardware.connected.headset));
  return (
    <>
      <ExclusiveBanner />
      <EqGraph bands={draft?.audio.bands ?? []} correction={headset?.curve ?? null}
        correctionOn={draft?.audio.headset_correction ?? false} />
      <div className="two">
        <Card title="Bands" action="Reset"
          onAction={() => update((p) => { p.audio.bands = []; })}>
          {UI_BANDS.map((u, i) => (
            <Slider key={u.label} label={u.label} value={gains[i]} min={-12} max={12} step={0.5}
              format={dbFmt} disabled={off}
              onChange={(v) => update((p) => setGain(p, i, v))} />
          ))}
        </Card>
        <GameEqCard profileId={draft?.id ?? null} onChanged={onGameEq} />
      </div>
      <HeadsetCorrectionCard draft={draft} update={update} />
      <AbListeningCard profileId={draft?.id ?? null} />
    </>
  );
}

/** How often the card re-reads progress while the learner is listening. */
const GAME_EQ_POLL_MS = 5000;

function gameEqStateText(s: GameEqStatus): string {
  switch (s.state) {
    case "off": return "Off";
    case "learning":
      if (s.needs_goal) return "Waiting for a goal";
      return s.learning_now ? `Learning · ${s.progress}%` : `Learning · ${s.progress}% · resumes when the game is in focus`;
    case "ready": return "Ready · a learned curve is waiting";
    case "applied":
      return s.source === "imported" ? "Applied (imported)"
        : s.source === "tuned" ? "Applied (imported, fine-tuned here)" : "Applied";
    case "needs_relearn": return "Game updated · relearning, the previous curve stays on";
  }
}

/** S46: "Learn this game's sound". Relay listens to the focused game's own
 *  audio, keeps statistics only, and derives a game layer for the goal the
 *  player picks — asked before any learning starts. */
function GameEqCard({ profileId, onChanged }: {
  profileId: string | null;
  onChanged: (fresh: Profile) => void;
}) {
  const [status, setStatus] = useState<GameEqStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const [importing, setImporting] = useState(false);
  const [importText, setImportText] = useState("");
  const [exported, setExported] = useState<GameEqExport | null>(null);

  useEffect(() => {
    setStatus(null);
    setAsking(false);
    setExported(null);
    if (!profileId) return;
    let live = true;
    api.gameEq(profileId, { kind: "status" })
      .then((r) => { if (live) setStatus(r.status); })
      .catch(() => { if (live) setStatus(null); });
    return () => { live = false; };
  }, [profileId]);

  // Progress moves only while the game is in focus and the learner listens.
  const listening = !!status?.learning_now;
  useEffect(() => {
    if (!profileId || !listening) return;
    const t = setInterval(() => {
      api.gameEq(profileId, { kind: "status" }).then((r) => setStatus(r.status)).catch(() => {});
    }, GAME_EQ_POLL_MS);
    return () => clearInterval(t);
  }, [profileId, listening]);

  const act = async (action: GameEqAction): Promise<boolean> => {
    if (!profileId) return false;
    setErr(null);
    setBusy(true);
    try {
      const r = await api.gameEq(profileId, action);
      setStatus(r.status);
      if (r.export) setExported(r.export);
      if (action.kind !== "status" && action.kind !== "export") {
        onChanged(await api.getProfile(profileId));
      }
      return true;
    } catch (e) {
      setErr(errText(e));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const choose = async (goal: Goal) => {
    if (!(await act({ kind: "set_goal", goal }))) return;
    if (!status?.learning_on) await act({ kind: "set_learning", enabled: true });
    setAsking(false);
  };

  const readFile = (f: File | undefined) => {
    if (!f) return;
    void f.text().then(setImportText).catch((e) => setErr(errText(e)));
  };

  const off = !profileId || !status || busy;
  const imported = status?.source === "imported" || status?.source === "tuned";
  const prompt = !!status && (asking || status.needs_goal);
  const goalLabel = GOALS.find((g) => g.key === status?.goal)?.label;

  return (
    <Card title="Learn this game's sound">
      {!profileId ? <p className="p">No profile selected.</p> : !status ? <p className="p">Reading…</p> : (
        <>
          <Toggle on={status.learning_on}
            label={imported ? "Keep learning to fine-tune for my setup" : "Learn this game's sound"}
            sub={imported
              ? "Blends the imported curve towards what Relay hears on this PC, under the same rules."
              : "On by default for games with audio processing. Listens only while the game is in focus."}
            onChange={off ? undefined : (v) => {
              if (v && !status.goal) { setAsking(true); return; }
              void act({ kind: "set_learning", enabled: v });
            }} />
          {prompt ? (
            <div role="group" aria-label="Choose a goal" className="goals">
              <p className="p"><b>What do you want from this game?</b> Learning starts once you choose.</p>
              {GOALS.map((g) => (
                <button key={g.key} type="button" className="btn q" disabled={busy}
                  aria-label={g.label} onClick={() => void choose(g.key)}>
                  <b>{g.label}</b> <small>{g.line}</small>
                </button>
              ))}
              {asking && !status.needs_goal && (
                <button type="button" className="btn q" onClick={() => setAsking(false)}>Not now</button>
              )}
            </div>
          ) : status.goal && (
            <Chips label="Goal" value={status.goal}
              onChange={(g) => { if (!busy) void act({ kind: "set_goal", goal: g }); }}
              options={GOALS.map((g) => ({ key: g.key, label: g.label }))} />
          )}
          <Kv k="State" v={gameEqStateText(status)} />
          {(status.state === "learning" || status.state === "needs_relearn") && !status.needs_goal && (
            <div className="meter" role="progressbar" aria-label="Learning progress"
              aria-valuemin={0} aria-valuemax={100} aria-valuenow={status.progress}>
              <i style={{ width: `${status.progress}%` }} />
            </div>
          )}
          {status.learning_on && !status.needs_goal && (
            <Kv k="Heard" mono
              v={`${status.targets}/${status.min_targets} ${status.goal === "dialogue" ? "voice" : "cues"} · ${status.maskers}/${status.min_maskers} loud · ${status.active_minutes} min`} />
          )}
          {goalLabel && status.goal === "dialogue" && status.distinct_voices > 0 && (
            <Kv k="Voices" v={`${status.distinct_voices} distinct`} />
          )}
          <div className="ab">
            <button className="btn acc" disabled={off || status.state !== "ready"}
              onClick={() => void act({ kind: "apply" })}>Apply</button>
            <ConfirmButton label="Relearn" confirm="Forget and relearn" disabled={off}
              onConfirm={() => void act({ kind: "relearn" })} />
            <ConfirmButton label="Reset" confirm="Remove game EQ" disabled={off}
              onConfirm={() => void act({ kind: "reset" })} />
            <button className="btn q" disabled={off} onClick={() => setImporting(!importing)}>Import…</button>
            <button className="btn q" disabled={off || !status.applied}
              onClick={() => void act({ kind: "export", note: "" })}>Export…</button>
          </div>
          <Toggle on={status.auto_apply} label="Apply new curves automatically"
            sub="Otherwise Relay offers them and waits for Apply."
            onChange={off ? undefined : (v) => void act({ kind: "set_auto_apply", enabled: v })} />
          {importing && (
            <div className="import">
              <label className="field">
                <span>Game EQ file</span>
                <textarea aria-label="Game EQ file" rows={4} value={importText}
                  placeholder="Paste a .json game EQ, or choose a file" onChange={(e) => setImportText(e.target.value)} />
              </label>
              <input type="file" accept=".json,application/json" aria-label="Choose a game EQ file"
                onChange={(e) => readFile(e.target.files?.[0])} />
              <button className="btn acc" disabled={busy || !importText.trim()}
                onClick={() => void act({ kind: "import", text: importText }).then((ok) => {
                  if (ok) { setImporting(false); setImportText(""); }
                })}>Import</button>
            </div>
          )}
          {status.notice && <p className="note" role="status">{status.notice}</p>}
          {exported && (
            <p className="note" data-testid="game-eq-exported">
              Saved to <span className="m">{exported.path}</span>{" "}
              <button type="button" className="btn q" onClick={() => void navigator.clipboard?.writeText(exported.text)}>Copy</button>
            </p>
          )}
          <ErrorNote text={err} onDismiss={() => setErr(null)} />
        </>
      )}
      <p className="note">Listens only to this game's own audio, keeps statistics — never a recording — and nothing leaves this PC. Voice chat in other apps is never heard; in-game player chat is left out.</p>
    </Card>
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
    ? (offline && !mock ? "Relay is not running." : "No profile selected.")
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
      setError(errText(e));
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
      <ErrorNote text={error} onDismiss={() => setError(null)} />
      {!isTauri() && <p className="note" style={{ marginTop: 8 }}>Requires the Relay core (desktop app).</p>}
    </Card>
  );
}

/* EQ graph geometry, on the 900×270 viewBox. Frequency is log from 20 Hz to
 * 20 kHz; level is linear, ±6 dB on the gridlines, clipped at the frame. */
const EQ_X0 = 52, EQ_X1 = 884, EQ_Y0 = 20, EQ_Y1 = 250;
const eqX = (hz: number) => EQ_X0 + (Math.log10(hz / 20) / 3) * (EQ_X1 - EQ_X0);
const eqY = (db: number) => Math.min(EQ_Y1, Math.max(EQ_Y0, 135 - db * (80 / 6)));
const EQ_TICKS: [number, string][] = [[20, "20"], [100, "100"], [500, "500"], [1000, "1k"], [4000, "4k"], [10000, "10k"], [20000, "20k"]];
const FOOTSTEPS: [number, number] = [1800, 4500];
const pct = (x: number) => `${(x / 900) * 100}%`;
const pathOf = (pts: [number, number][]) =>
  pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join(" ");

/** The profile's EQ as the DSP will shape it, and the headset's measured
 *  correction behind it.
 *
 *  Both lines are data. The gold line is the actual response of the band
 *  cascade (the same peaking design the DSP uses), not straight segments
 *  between slider values; the dashed line is the imported correction curve,
 *  point for point, and is absent when the headset has none. What is stored
 *  is AutoEQ's *correction*, not the headset's raw response, so that is what
 *  the legend calls it. */
export function EqGraph({ bands, correction, correctionOn }: {
  bands: EqBand[];
  correction: [number, number][] | null;
  correctionOn: boolean;
}) {
  const samples = Array.from({ length: 121 }, (_, i) => 20 * 1000 ** (i / 120));
  const profile = pathOf(samples.map((hz) => [eqX(hz), eqY(cascadeDb(bands, hz))]));
  const dots = UI_BANDS.map((u) => [eqX(u.freq), eqY(cascadeDb(bands, u.freq))] as const);
  const measured = correction && correction.length > 1
    ? pathOf(correction.filter(([hz]) => hz >= 20 && hz <= 20000).map(([hz, db]) => [eqX(hz), eqY(db)]))
    : null;
  const [fs0, fs1] = FOOTSTEPS.map(eqX);
  return (
    <div className="eq">
      <svg viewBox="0 0 900 270" preserveAspectRatio="none">
        <g stroke="rgba(255,255,255,.06)">
          <line x1={EQ_X0} y1="55" x2={EQ_X1} y2="55" /><line x1={EQ_X0} y1="135" x2={EQ_X1} y2="135" stroke="rgba(255,255,255,.12)" /><line x1={EQ_X0} y1="215" x2={EQ_X1} y2="215" />
          {EQ_TICKS.slice(1, -1).map(([hz]) => <line key={hz} x1={eqX(hz)} y1={EQ_Y0} x2={eqX(hz)} y2={EQ_Y1} />)}
        </g>
        <rect x={fs0} y={EQ_Y0} width={fs1 - fs0} height={EQ_Y1 - EQ_Y0} fill="rgba(201,169,106,.06)" />
        <text x="14" y="59" fill="#5A544C" fontFamily="GeistMono" fontSize="10">+6</text>
        <text x="20" y="139" fill="#5A544C" fontFamily="GeistMono" fontSize="10">0</text>
        <text x="14" y="219" fill="#5A544C" fontFamily="GeistMono" fontSize="10">−6</text>
        {measured && (
          <path data-testid="eq-correction" d={measured} fill="none" stroke="#5A544C" strokeWidth="1.5" strokeDasharray="4 4" />
        )}
        <path data-testid="eq-profile" d={profile} fill="none" stroke="#C9A96A" strokeWidth="2" strokeLinejoin="round" />
        <g fill="#ECE6DC">{dots.map(([x, y], i) => <circle key={i} cx={x} cy={y} r="4" />)}</g>
      </svg>
      <div className="leg">
        <span><i style={{ background: "#C9A96A" }} />Profile</span>
        {measured && (
          <span><i style={{ background: "#5A544C" }} />Headset correction · measured{correctionOn ? "" : " (off)"}</span>
        )}
      </div>
      <div className="band" style={{ left: pct(fs0) }}>Footsteps · 1.8–4.5 kHz</div>
      <div className="lbl">
        {EQ_TICKS.map(([hz, t]) => <span key={hz} style={{ left: pct(eqX(hz)) }}>{t}</span>)}
      </div>
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
      <SaveRow disabled={off} dirty={dirty} save={save} />
      <p className="note">Runs inside Windows audio on this headset only. Other apps and your desktop are unaffected.</p>
    </aside>
  );
}

/* ---------- Display ---------- */

/** Title-case a level name from the core's verified value map. The names come
 *  from `vcp::ResponseQuirk::levels`, not from a list the UI keeps in sync. */
function levelLabel(level: string | undefined): string {
  return level ? level.charAt(0).toUpperCase() + level.slice(1) : "";
}

/** Advertised-VCP check. Unknown capabilities (never probed) allow the
 *  standard codes, matching the core's `plan_writes` behaviour. */
function vcpAvailable(codes: number[] | undefined, code: number): boolean {
  return !codes || codes.includes(code);
}

/** What the panel says it can do, next to the sliders that push it there.
 *
 *  Vibrance is a saturation multiplier applied before the panel's own gamut
 *  mapping, so the same setting that looks right on an sRGB monitor clips
 *  skin tones on a wide-gamut one. The panel's EDID knows which it is; saying
 *  so beats leaving the user to discover it on a dark map. */
function PanelColorNote({ color, vibrance }: { color?: ColorInfo; vibrance: number }) {
  if (!color) {
    return <p className="note">No EDID colour data from this monitor, so these are open-loop adjustments.</p>;
  }
  const wide = (color.coverage?.dci_p3 ?? 0) >= 0.9;
  return (
    <>
      <p className="note">Panel reports: {colorSummary(color)}</p>
      {wide && vibrance > 60 && (
        <p className="note">This is a wide-gamut panel — sRGB content is already pushed past its intended saturation before vibrance is applied. Above about 60, reds and skin tones will clip.</p>
      )}
    </>
  );
}

function DisplaySection({ draft, update }: { draft: Profile | null; update: (fn: (p: Profile) => void) => void }) {
  const { hardware } = useCore();
  const signed = (v: number) => `${v > 0 ? "+" : ""}${v}`;
  const gpu = draft?.display.gpu ?? { vibrance: 50, gamma: 1, contrast: 0, shadow_lift: 0, hue_deg: 0 };
  const mon = draft?.display.monitor ?? {};
  const off = !draft;

  // The profile's monitor (or the main connected one) decides which DDC/CI
  // controls exist. Standard codes come from the panel's advertised list;
  // vendor codes (black eq, response) come only from the core, which enables
  // one only when the quirks table holds verified evidence for this model.
  const mainId = hardware.connected.monitors.find((m) => m.primary)?.id ?? null;
  const monId = draft?.monitor ?? mainId;
  const libMonitor = hardware.monitors.find((m) => m.id === monId);
  const codes = libMonitor?.ddcci;
  const vendor = hardware.vendor_controls?.find((v) => v.monitor === monId);
  const responseLevels = vendor?.response ?? [];
  // Prefer what the panel is reporting right now over whatever the library
  // recorded when it was first added.
  const panelColor = hardware.connected.monitors.find((m) => m.id === monId)?.color
    ?? libMonitor?.color;
  const hue = ((gpu.hue_deg + 180) % 360) - 180;
  const responseIx = Math.max(0, responseLevels.indexOf(mon.response ?? responseLevels[0] ?? ""));
  return (
    <>
      <RampPreview gpu={gpu} />
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
          <PanelColorNote color={panelColor} vibrance={gpu.vibrance} />
        </Card>
        <Card title="Monitor" action="Reset" onAction={() => update((p) => { p.display.monitor = {}; })}>
          {/* Monitor fields are optional: an absent one is a setting Relay
              leaves alone, so it reads "Not set" rather than the resting
              position of the thumb. */}
          <Slider label="Brightness" value={mon.brightness ?? 50} min={0} max={100} disabled={off || !vcpAvailable(codes, 0x10)}
            unset={mon.brightness === undefined}
            onChange={(v) => update((p) => { p.display.monitor.brightness = v; })} />
          <Slider label="Contrast" value={mon.contrast ?? 50} min={0} max={100} disabled={off || !vcpAvailable(codes, 0x12)}
            unset={mon.contrast === undefined}
            onChange={(v) => update((p) => { p.display.monitor.contrast = v; })} />
          <Slider label="Black equalizer" value={mon.black_equalizer ?? 10} min={0} max={20}
            disabled={off || !vendor?.black_equalizer} unset={mon.black_equalizer === undefined}
            onChange={(v) => update((p) => { p.display.monitor.black_equalizer = v; })} />
          {/* With no verified levels there is no index to point at, so the
              readout falls back to the level the profile stored, by name. */}
          <Slider label="Response" value={responseIx} min={0} max={Math.max(0, responseLevels.length - 1)}
            format={(v) => levelLabel(responseLevels.length ? responseLevels[v] : mon.response)}
            disabled={off || responseLevels.length < 2}
            unset={responseLevels.length === 0 && mon.response === undefined}
            onChange={(v) => update((p) => {
              const level = responseLevels[v];
              if (!level || v === 0) delete p.display.monitor.response; else p.display.monitor.response = level;
            })} />
          <Slider label="Sharpness" value={mon.sharpness ?? 50} min={0} max={100} disabled={off || !vcpAvailable(codes, 0x87)}
            unset={mon.sharpness === undefined}
            onChange={(v) => update((p) => { p.display.monitor.sharpness = v; })} />
          {(!vendor?.black_equalizer || responseLevels.length < 2) && (
            <p className="note">Black equalizer and Response sit on vendor-private codes Relay has not verified on this panel, so they stay off. Nothing on your monitor was changed.</p>
          )}
        </Card>
      </div>
      <LearnLookCard exe={draft?.game.exe ?? null} />
    </>
  );
}

/** Before / after of the one colour transform the UI can reproduce exactly.
 *
 *  This used to be two copies of the same drawn scene, told apart only by a
 *  fixed background, so moving a slider changed nothing on it. Now both halves
 *  are the same reference pattern — grey ramp, grey steps, primary ramps — and
 *  the right half goes through `buildRamp`, the port of the gamma ramp Relay
 *  writes to the monitor. Vibrance and hue go through the GPU driver, whose
 *  maths Relay does not have, so the caption says they are not shown rather
 *  than faking them with a CSS filter. */
function RampPreview({ gpu }: { gpu: GpuColor }) {
  const filterId = `ramp-${useId().replace(/:/g, "")}`;
  const table = buildRamp(gpu, 33).map((v) => v.toFixed(4)).join(" ");
  const pattern = (filter?: string) => (
    <svg viewBox="0 0 400 300" preserveAspectRatio="none" aria-hidden="true">
      <defs>
        <linearGradient id={`${filterId}-k`}><stop offset="0" stopColor="#000" /><stop offset="1" stopColor="#fff" /></linearGradient>
        <linearGradient id={`${filterId}-r`}><stop offset="0" stopColor="#000" /><stop offset="1" stopColor="#f00" /></linearGradient>
        <linearGradient id={`${filterId}-g`}><stop offset="0" stopColor="#000" /><stop offset="1" stopColor="#0f0" /></linearGradient>
        <linearGradient id={`${filterId}-b`}><stop offset="0" stopColor="#000" /><stop offset="1" stopColor="#00f" /></linearGradient>
      </defs>
      <g filter={filter}>
        <rect x="0" y="0" width="400" height="300" fill="#000" />
        <rect x="0" y="44" width="400" height="70" fill={`url(#${filterId}-k)`} />
        {Array.from({ length: 11 }, (_, i) => {
          const v = Math.round((i / 10) * 255);
          return <rect key={i} x={(400 / 11) * i} y="118" width={400 / 11 + 0.5} height="60" fill={`rgb(${v},${v},${v})`} />;
        })}
        <rect x="0" y="182" width="400" height="36" fill={`url(#${filterId}-r)`} />
        <rect x="0" y="222" width="400" height="36" fill={`url(#${filterId}-g)`} />
        <rect x="0" y="262" width="400" height="38" fill={`url(#${filterId}-b)`} />
      </g>
    </svg>
  );
  return (
    <>
    <div className="cmp">
      <svg width="0" height="0" style={{ position: "absolute" }} aria-hidden="true">
        <filter id={filterId} colorInterpolationFilters="sRGB">
          <feComponentTransfer data-testid="ramp-table">
            <feFuncR type="table" tableValues={table} />
            <feFuncG type="table" tableValues={table} />
            <feFuncB type="table" tableValues={table} />
          </feComponentTransfer>
        </filter>
      </svg>
      <div className="a"><div className="cap">Without profile</div>{pattern()}</div>
      <div className="b"><div className="cap">Through this profile's gamma ramp</div>{pattern(`url(#${filterId})`)}</div>
      <div className="div" />
    </div>
    <p className="note">Gamma, contrast and shadow lift are drawn through the same ramp Relay writes to the monitor. Vibrance and hue are applied by the graphics driver and are not shown here.</p>
    </>
  );
}

/** "NvAPI + gamma ramp + DDC/CI", from the live apply. AMD machines read
 * "AMD ADL" in the same slot -- whichever vendor drives the target monitor. */
function appliedViaText(state: ReturnType<typeof useCore>["state"]): string {
  if (state.display_state !== "applied") return "—";
  const parts = [
    state.display_via.nvapi && "NvAPI",
    state.display_via.amd && "AMD ADL",
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
      <SaveRow disabled={!draft} dirty={dirty} save={save} />
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

  return (
    <aside className="side">
      <Card title={def ? `${def.name} preset` : "Preset"}>
        {chosen === "off" ? (
          <p className="p">Sharing is off for this game. Relay still shares when you start it manually; this only decides what happens automatically.</p>
        ) : def ? (
          <>
            <Kv k="Encoder" v="HEVC or H.264 · hardware" />
            <Kv k="Bitrate" v={`${def.bitrate_mbps} Mb/s`} mono />
            <Kv k="Frame rate" v={`${def.fps} fps`} mono />
            <Kv k="Size" v={def.size ? `${def.size[0]}×${def.size[1]}` : "Native"} mono />
            <Kv k="Audio" v={presetAudioLabel(def.audio)} />
            <Kv k="Cursor" v={def.cursor ? "Shown" : "Hidden"} />
            <Kv k="Replay buffer" v={def.replay_secs ? `${def.replay_secs} s` : "Off"} mono />
            <Kv k="Container" v={(def.container ?? "mp4").toUpperCase()} mono />
          </>
        ) : (
          <p className="note">Reading presets…</p>
        )}
      </Card>
      <SaveRow disabled={!draft} dirty={dirty} save={save} />
      <p className="note">Edit these numbers on the Share screen. Everything stays on your local network. Nothing on this PC was changed.</p>
    </aside>
  );
}
