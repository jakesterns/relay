import { useEffect, useRef, useState } from "react";
import { Card, ConfirmButton, DoneNote, ErrorNote, Kv, Toggle } from "./Controls";
import { errText } from "../lib/err";
import { api, LOOK_PRIVACY, TOURNAMENT_NOTICE, type LearnView, type LookStatus, type MonitorLearnView } from "../lib/ipc";

/**
 * Display → "Learn this game's look" (S47).
 *
 * Relay watches the game's own frames (1 per second, 480×270, in memory) and
 * works out how much shadow recovery and saturation help it wants. The look
 * belongs to the game; each monitor gets it fitted to what that panel can do.
 * No presets: a new game or a game update is learned the same way.
 */
export function LearnLookCard({ exe }: { exe: string | null }) {
  const [view, setView] = useState<LearnView | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!exe) { setView(null); return; }
    let cancelled = false;
    const load = () => api.learnDisplayStatus(exe)
      .then((v) => { if (!cancelled) setView(v); })
      .catch(() => { if (!cancelled) setView(null); });
    void load();
    // Follow progress while learning; nothing to poll otherwise.
    const t = setInterval(() => { void load(); }, 5000);
    return () => { cancelled = true; clearInterval(t); };
  }, [exe]);

  const run = async (f: () => Promise<LearnView>, ok?: string) => {
    setErr(null); setDone(null);
    try { setView(await f()); if (ok) setDone(ok); } catch (e) { setErr(errText(e)); }
  };

  const doExport = async () => {
    if (!exe) return;
    setErr(null); setDone(null);
    try {
      const json = await api.learnDisplayExport(exe, note);
      const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = `${exe.replace(/\.exe$/i, "")}.relay-display.json`;
      a.click();
      URL.revokeObjectURL(url);
      setDone("Exported. The file holds this game's look only — no monitor or PC details.");
    } catch (e) { setErr(errText(e)); }
  };

  const doImport = async (file: File | undefined) => {
    if (!exe || !file) return;
    const text = await file.text();
    await run(() => api.learnDisplayImport(exe, text), "Imported look applied. Learning is off for this game.");
    if (fileRef.current) fileRef.current.value = "";
  };

  /** Write the panel type into the hardware library (the user confirming
   *  or correcting a guess), then refresh the view. */
  const setPanel = async (monitor: string, panel: string) => {
    if (!exe) return;
    setErr(null); setDone(null);
    try {
      const hw = await api.listHardware();
      const m = hw.monitors.find((x) => x.id === monitor);
      if (!m) throw new Error("this monitor is not in the hardware library yet");
      await api.saveHardware({ kind: "monitor", value: { ...m, panel } });
      setView(await api.learnDisplayStatus(exe));
      setDone(`Panel type set to ${panel}.`);
    } catch (e) { setErr(errText(e)); }
  };

  if (!exe) {
    return (
      <Card title="Learn this game's look">
        <p className="p small">Pick a game to let Relay learn its look.</p>
      </Card>
    );
  }

  const imported = view?.imported ?? null;
  const enabled = view?.enabled ?? false;
  const anyReady = view?.monitors.some((m) => m.converged !== null) ?? false;
  return (
    <Card title="Learn this game's look">
      <p className="p" data-testid="look-status">{statusLine(view?.status ?? "off", view?.sampling ?? false)}</p>
      <Toggle
        on={enabled}
        onChange={view === null ? undefined : (v) => void run(() => api.learnDisplaySet(exe, v))}
        label={imported ? "Keep learning to fine-tune for my monitor" : "Learn this game's look"}
        sub={imported
          ? "Starts from the imported look and changes it only if this monitor clearly needs something different."
          : "While the game has focus, Relay studies its frames and suggests gentle shadow and colour corrections for each monitor."} />
      {view?.monitors.map((m) => (
        <MonitorRow key={m.monitor} m={m} onPanel={(p) => void setPanel(m.monitor, p)} />
      ))}
      {imported && (
        <Kv k="Imported" v={imported.note ? `"${imported.note}"` : "No note"} />
      )}
      <div className="row" style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button className="btn acc" disabled={!anyReady} onClick={() => void run(() => api.learnDisplayApply(exe), "Applied. It takes effect the next time the game has focus.")}>Apply</button>
        <button className="btn" disabled={view === null} onClick={() => void run(() => api.learnDisplayRelearn(exe), "Learning again from scratch. What is applied stays until the new look settles.")}>Relearn</button>
        <ConfirmButton className="btn q" label="Reset" confirm={"Forget this game's look"}
          onConfirm={() => void run(() => api.learnDisplayReset(exe), "Forgotten. The profile is back to exactly what you set.")} />
      </div>
      <div className="row" style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
        <label className="field" style={{ flex: 1, minWidth: 160 }}>
          <input type="text" placeholder="Note for the file (optional)" maxLength={500}
            value={note} onChange={(e) => setNote(e.target.value)} aria-label="Export note" />
        </label>
        <button className="btn" onClick={() => void doExport()}>Export</button>
        <button className="btn" onClick={() => fileRef.current?.click()}>Import</button>
        <input ref={fileRef} type="file" accept=".json,application/json" hidden
          data-testid="look-import" onChange={(e) => void doImport(e.target.files?.[0])} />
      </div>
      <DoneNote text={done} onDismiss={() => setDone(null)} />
      <ErrorNote text={err} onDismiss={() => setErr(null)} />
      <p className="p small">{view?.privacy ?? LOOK_PRIVACY}</p>
      <p className="p small" data-testid="tournament-notice">{view?.tournament ?? TOURNAMENT_NOTICE}</p>
      <p className="p small">Learning changes nothing on your PC. An applied look is restored on exit, crash, or reboot like every other display setting.</p>
    </Card>
  );
}

function statusLine(s: LookStatus, sampling: boolean): string {
  switch (s) {
    case "off": return "Off. Relay is not looking at this game.";
    case "learning": return sampling ? "Learning now, from this game's own frames." : "Learning. Play the game and progress continues.";
    case "ready": return "Ready. The look has settled — press Apply to use it.";
    case "applied": return "Applied. Relay keeps watching for game updates and only changes it if the look clearly moves.";
    case "applied_imported": return "Applied (imported).";
    case "hdr_skipped": return "This monitor was in HDR mode, so nothing was learned there.";
  }
}

function skippedText(e: NonNullable<MonitorLearnView["excluded_by"]>): string {
  const parts: [string, number][] = [
    ["static/menu", e.static_frames], ["loading", e.loading], ["cutscene", e.cutscene],
    ["outlier", e.outlier], ["idle", e.idle], ["warm-up", e.warmup],
  ];
  return parts.filter(([, n]) => n > 0).map(([k, n]) => `${k} ${n}`).join(" · ");
}

function pct(v: number): string { return `${Math.round(v * 100)}%`; }

const PANELS = ["OLED", "IPS", "VA", "TN"] as const;

function MonitorRow({ m, onPanel }: { m: MonitorLearnView; onPanel: (panel: string) => void }) {
  const r = m.readiness;
  const lit = Math.round(r.progress * 10);
  const a = m.adjustments;
  return (
    <div className="look-monitor" data-testid="look-monitor">
      <Kv k={m.monitor_name || "Monitor"}
        v={m.panel === "unknown" ? "Panel type unknown"
          : m.panel_guessed ? `${m.panel.toUpperCase()} (guessed from the model)` : m.panel.toUpperCase()} />
      {(m.panel === "unknown" || m.panel_guessed) && (
        <label className="field">
          <span>Panel type</span>
          <select aria-label={`Panel type of ${m.monitor_name || "this monitor"}`}
            value={m.panel === "unknown" ? "" : m.panel.toUpperCase()}
            onChange={(e) => { if (e.target.value) onPanel(e.target.value); }}>
            <option value="">Not sure</option>
            {PANELS.map((p) => <option key={p} value={p}>{p}</option>)}
          </select>
        </label>
      )}
      {m.hdr_skipped ? (
        <p className="p small">HDR was on. Relay only learns from SDR; turn HDR off for this game to learn its look.</p>
      ) : (
        <>
          <div className="bars" role="progressbar" aria-label={`Learning progress on ${m.monitor_name || "this monitor"}`}
            aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(r.progress * 100)}>
            {Array.from({ length: 10 }, (_, i) => <b key={i} className={i < lit ? "hi" : "off"} style={{ height: "100%" }} />)}
          </div>
          <p className="p small mono">
            {r.frames} / {r.frames_needed} gameplay frames · {r.scenes} / {r.scenes_needed} kinds of scene
            {` · checkpoints ${r.stable_checkpoints} stable of ${r.checkpoints ?? 0}`}
          </p>
          {r.delta && (
            <p className="p small mono" data-testid="look-delta">
              Checkpoint spread: gamma {r.delta.gamma.toFixed(3)} · lift {r.delta.shadow_lift} · vibrance {r.delta.vibrance}
            </p>
          )}
          {r.scene_frames && (
            <p className="p small mono" data-testid="look-scenes">
              Scenes (dark → bright): {r.scene_frames.join(" / ")}
            </p>
          )}
          {m.excluded > 0 && (
            <p className="p small mono" data-testid="look-skipped">
              {m.excluded} skipped{m.excluded_by ? `: ${skippedText(m.excluded_by)}` : ""}
            </p>
          )}
        </>
      )}
      {m.converged && (
        <Kv k="Learned" v={`shadows ${pct(m.converged.shadow)} · colour ${pct(m.converged.saturation)} · highlights ${pct(m.converged.highlight)}`} mono />
      )}
      {a && (
        <Kv k="On this panel" v={`gamma ${a.gamma.toFixed(2)} · lift ${a.shadow_lift} · vibrance ${a.vibrance}${a.black_equalizer !== undefined ? ` · black eq ${a.black_equalizer}` : ""}`} mono />
      )}
      {a?.notes.map((n) => <p key={n} className="p small">{n}</p>)}
    </div>
  );
}
