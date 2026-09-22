import { useEffect, useState } from "react";
import { Card, ConfirmButton, DoneNote, ErrorNote, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import { api, type ApoStatus, type ElevatedOp, type ElevationResult, type RecordingSettings, type UiPrefs, type VdeviceStatus } from "../lib/ipc";

export function Settings() {
  const { state, refresh, offline, mock } = useCore();
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartErr, setAutostartErr] = useState<string | null>(null);
  const [recording, setRecording] = useState<RecordingSettings | null>(null);
  const [recDirty, setRecDirty] = useState(false);
  const [recErr, setRecErr] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);
  const [restored, setRestored] = useState<string | null>(null);
  const [restoreErr, setRestoreErr] = useState<string | null>(null);
  const [prefs, setPrefs] = useState<UiPrefs | null>(null);
  const [prefsErr, setPrefsErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.getAutostart()
      .then((v) => { if (!cancelled) setAutostart(v); })
      .catch(() => { if (!cancelled) setAutostart(null); });
    api.listPresets()
      .then((r) => { if (!cancelled) { setRecording(r.recording); setRecDirty(false); } })
      .catch(() => { if (!cancelled) setRecording(null); });
    api.getUiPrefs()
      .then((p) => { if (!cancelled) setPrefs(p); })
      .catch(() => { if (!cancelled) setPrefs(null); });
    return () => { cancelled = true; };
  }, [offline]);

  const toggleAutostart = async (v: boolean) => {
    setAutostartErr(null);
    try { setAutostart(await api.setAutostart(v)); }
    catch (e) { setAutostartErr(errText(e)); }
  };

  /** The one button that undoes everything Relay applied. It used to have no
   *  catch at all: a core that refused looked exactly like a core that had
   *  done the work. */
  const restoreAll = async () => {
    setRestoring(true);
    setRestoreErr(null);
    setRestored(null);
    try {
      await api.restoreAll();
      await refresh();
      setRestored("Audio and display are back to the settings Windows had before Relay.");
    } catch (e) {
      setRestoreErr(errText(e));
    } finally {
      setRestoring(false);
    }
  };

  // Always the whole object: the core replaces the file with what it is
  // sent, so a partial patch would silently reset every other preference to
  // its default -- exactly the "an update reset my settings" the standing
  // rule forbids, only self-inflicted.
  const setPref = async (patch: Partial<UiPrefs>) => {
    if (!prefs) return;
    setPrefsErr(null);
    try { setPrefs(await api.setUiPrefs({ ...prefs, ...patch })); }
    catch (e) { setPrefsErr(String((e as { message?: string })?.message ?? e)); }
  };
  const setClose = (quit: boolean) => setPref({ close_action: quit ? "quit_relay" : "keep_running" });

  const editRecording = (patch: Partial<RecordingSettings>) => {
    setRecording((r) => (r ? { ...r, ...patch } : r));
    setRecDirty(true);
  };

  const saveRecording = async () => {
    if (!recording) return;
    setRecErr(null);
    // Empty folder = the default %USERPROFILE%\Videos\Relay (dir omitted).
    const settings: RecordingSettings = {
      cap_gb: recording.cap_gb, free_floor_gb: recording.free_floor_gb,
    };
    if (recording.dir?.trim()) settings.dir = recording.dir.trim();
    try { await api.setRecordingSettings(settings); setRecDirty(false); }
    catch (e) { setRecErr(errText(e)); }
  };

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Settings</h1>
          <Live on={!state.active_profile} text={state.active_profile ? "Profile applied" : "Nothing applied"} />
        </div>
        <OfflineBanner />
        <Card title="Startup">
          <Toggle
            on={autostart === true}
            onChange={autostart === null ? undefined : (v) => void toggleAutostart(v)}
            label="Start Relay at login"
            sub={autostart === null
              ? (offline && !mock ? "Relay is not running — cannot read the setting." : "Reading…")
              : "Adds one value under HKCU\\...\\CurrentVersion\\Run. Nothing else on your PC is changed; turning this off removes it."} />
          <ErrorNote text={autostartErr} onDismiss={() => setAutostartErr(null)} />
        </Card>
        <Card title="When you close the window">
          {/* The honest line the background story needs. Relay is useful
              precisely while its window is shut, so what stays running has to
              be stated, not discovered. */}
          <p className="p">
            Relay keeps working after you close the window — that is how profiles
            stay applied while you play, with the window costing nothing. What
            keeps running is a small background part of Relay, about 7 MB of
            memory and no measurable CPU when idle. Its icon is in the
            notification area by the clock — Windows hides new icons at first,
            so click the arrow there to see it, and drag it out to keep it in
            view. Right-click it for Open Relay, Restore everything, and Quit
            Relay.
          </p>
          <Toggle
            on={prefs?.close_action === "quit_relay"}
            onChange={prefs === null ? undefined : (v) => void setClose(v)}
            label="Quit Relay completely when I close the window"
            sub={prefs === null
              ? (offline && !mock ? "Relay is not running — cannot read this setting." : "Reading…")
              : prefs.close_action === "quit_relay"
                ? "Closing the window stops Relay and restores your audio and display first. Profiles will not apply again until you open Relay."
                : "Closing the window leaves Relay running so your profiles keep working. Quit Relay from the notification area to stop it."} />
          <Toggle
            on={prefs?.close_notice ?? true}
            onChange={prefs === null ? undefined : (v) => void setPref({ close_notice: v })}
            label="Say so in the notification area when I close the window"
            sub={prefs === null
              ? "Reading…"
              : prefs.close_notice
                ? "A short message by the clock each time the window closes and Relay keeps running."
                : "Nothing is shown when the window closes. The icon by the clock is still there."} />
          <Toggle
            on={prefs?.resilience ?? true}
            onChange={prefs === null ? undefined : (v) => void setPref({ resilience: v })}
            label="Bring a share back on its own if it drops"
            sub={prefs === null
              ? "Reading…"
              : prefs.resilience
                ? "If a share you started dies — a crash, a dropped link, a reboot — Relay reconnects, keeps trying for three minutes, and tells you what it is doing. Stop sharing ends it as usual."
                : "A share that dies stays dead until you start it again. Relay still records what happened."} />
          {prefsErr && <div className="offline"><i />{prefsErr}</div>}
        </Card>
        <Card title="What Relay installs">
          <p className="p">Relay works at the OS and hardware layer only. It never injects into games, reads their memory, or changes your default devices. Two optional components need your explicit consent:</p>
          <ApoConsentRow />
          <VdeviceConsentRow />
        </Card>
        <Card title="Recording">
          {recording === null
            ? <p className="note">{offline && !mock ? "Relay is not running — cannot read the settings." : "Reading…"}</p>
            : (
              <div className="form">
                <div className="field">
                  <span>Folder</span>
                  <input className="mono" placeholder="%USERPROFILE%\Videos\Relay"
                    value={recording.dir ?? ""}
                    onChange={(e) => editRecording({ dir: e.target.value })} />
                </div>
                <div className="two" style={{ display: "grid", gridTemplateColumns: "1fr 1fr" }}>
                  <div className="field">
                    <span>Disk cap (GB)</span>
                    <input className="mono" inputMode="numeric" value={recording.cap_gb}
                      onChange={(e) => editRecording({ cap_gb: Number(e.target.value.replace(/\D/g, "")) || 0 })} />
                  </div>
                  <div className="field">
                    <span>Keep free (GB)</span>
                    <input className="mono" inputMode="numeric" value={recording.free_floor_gb}
                      onChange={(e) => editRecording({ free_floor_gb: Number(e.target.value.replace(/\D/g, "")) || 0 })} />
                  </div>
                </div>
                <p className="p small">Recordings stay on this PC. Oldest files are deleted past the cap; recording stops before your disk fills.</p>
                <button className="btn" onClick={() => void saveRecording()} disabled={!recDirty}>
                  {recDirty ? "Save recording settings" : "Saved"}
                </button>
                <ErrorNote text={recErr} onDismiss={() => setRecErr(null)} />
              </div>
            )}
        </Card>
        <Card title="Hotkeys">
          <Kv k="Toggle share" v="Ctrl + Alt + S" mono />
          <Kv k="Toggle game profile" v="Ctrl + Alt + G" mono />
          <Kv k="Toggle preview" v="Ctrl + Alt + P" mono />
          <Kv k="Save replay clip" v="Ctrl + Alt + R" mono />
        </Card>
        <Card title="Restore">
          <p className="p" style={{ marginBottom: 10 }}>Put every audio and display setting back to what Windows had before Relay touched it. Safe to press at any time.</p>
          <ConfirmButton className="btn" confirmClassName="btn danger"
            label="Restore original state now" confirm="Confirm restore"
            disabled={restoring}
            onConfirm={() => void restoreAll()} />
          <DoneNote text={restored} onDismiss={() => setRestored(null)} />
          <ErrorNote text={restoreErr} onDismiss={() => setRestoreErr(null)} />
        </Card>
        <UninstallCard />
      </section>
      <aside className="side">
        <Card>
          <Kv k="Relay" v={offline && !mock ? "Offline" : "Running"} />
          <Kv k="Data folder" v={state.build?.data_dir ?? "—"} mono />
          <Kv k="Log file" v={state.build?.log_file ?? "—"} mono />
          <Kv k="Version" v={state.build?.version ?? "—"} mono />
        </Card>
        <p className="note">Uninstalling removes every component listed here, the startup entry, and restores the audio chain. Nothing is left behind.</p>
        <p className="note">Relay is installed for your user account only — it writes nothing to Program Files and installs no drivers unless you opt in above.</p>
      </aside>
    </>
  );
}

/** Uninstall. The listing is not written here — it comes from the core's
 *  uninstall planner, the same code `relay-core uninstall` executes, so what
 *  the user reads is what actually happens. The button hands over to the one
 *  Windows uninstaller rather than removing anything itself. */
function UninstallCard() {
  const { offline, mock } = useCore();
  const [keepData, setKeepData] = useState(true);
  const [lines, setLines] = useState<string[] | null>(null);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    api.uninstallPlan(keepData)
      .then((l) => { if (!cancelled) setLines(l); })
      .catch(() => { if (!cancelled) setLines(null); });
    return () => { cancelled = true; };
  }, [open, keepData, offline]);

  return (
    <Card title="Uninstall Relay">
      <p className="p">Removing Relay puts your audio and display settings back first, then takes out every component it registered. Nothing is left behind.</p>
      {!open ? (
        <button className="btn q" onClick={() => setOpen(true)}>Show what will be removed…</button>
      ) : (
        <div className="consent">
          <Toggle on={keepData} onChange={setKeepData}
            label="Keep my profiles and hardware library"
            sub="Your tuning work stays in %LOCALAPPDATA%\Relay so a reinstall picks it up. Turn this off to delete it too." />
          {lines === null
            ? <p className="note">{offline && !mock ? "Relay is not running — cannot read the plan." : "Reading…"}</p>
            : lines.map((l, i) => (
              l === ""
                ? <div key={`gap-${i}`} style={{ height: 8 }} />
                : l.startsWith("[")
                  ? <div className="mono" key={l} style={{ fontSize: 12 }}>{l}</div>
                  : <p className="p small" key={l}>{l}</p>
            ))}
          <div className="ab">
            <button className="btn" disabled={lines === null}
              onClick={() => void api.launchUninstaller().catch((e) => setError(errText(e)))}>
              Uninstall Relay
            </button>
            <button className="btn q" onClick={() => setOpen(false)}>Cancel</button>
          </div>
          <ErrorNote text={error} onDismiss={() => setError(null)} />
        </div>
      )}
    </Card>
  );
}

/** Shared plumbing for the two opt-in cards.
 *
 *  Both components need the same three-beat sequence, and it is the sequence
 *  the elevation promise is made of: read the plan (read-only, no prompt) →
 *  show it → only then ask Windows for permission. `declined` comes back as a
 *  normal answer with the one sentence that matters, never as an error. */
function useElevation(op: ElevatedOp, onDone: () => void) {
  const [plan, setPlan] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadPlan = () => {
    setPlan(null);
    setNote(null);
    setError(null);
    api.elevationPlan(op).then(setPlan).catch((e) => setError(errText(e)));
  };

  const run = async (): Promise<ElevationResult | null> => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.runElevated(op);
      setNote(r.lines);
      return r;
    } catch (e) {
      setError(errText(e));
      return null;
    } finally {
      setBusy(false);
      onDone();
    }
  };

  return {
    plan, busy, note, error, loadPlan, run,
    clearError: () => setError(null),
    reset: () => { setPlan(null); setNote(null); setError(null); },
  };
}

/** The plan listing, rendered the same way the uninstall card renders its
 *  own — because for the removal ops it is literally the same lines. */
function PlanLines({ lines }: { lines: string[] | null }) {
  if (lines === null) return <p className="note">Reading what would change…</p>;
  return (
    <>
      {lines.map((l, i) =>
        l === ""
          ? <div key={`gap-${i}`} style={{ height: 8 }} />
          : l.startsWith("[") || l.startsWith("HKLM") || l.startsWith("file:") || l.startsWith("backup:")
            ? <div className="mono" key={`${l}-${i}`} style={{ fontSize: 12 }}>{l}</div>
            : <p className="p small" key={`${l}-${i}`}>{l}</p>)}
    </>
  );
}

/** The endpoint-APO opt-in: live status, the exact list of registry values an
 *  install would write, and a remove path that restores the prior state from
 *  the on-disk backup. Both directions run in `relay-elevate.exe` behind one
 *  UAC prompt — the core itself never holds an administrator token. */
function ApoConsentRow() {
  const { offline, mock } = useCore();
  const [status, setStatus] = useState<ApoStatus | null>(null);
  const [mode, setMode] = useState<"idle" | "install" | "remove">("idle");

  const refreshStatus = () => {
    api.apoStatus().then(setStatus).catch(() => setStatus(null));
  };
  useEffect(refreshStatus, [offline]);

  const installed = status?.installed === true;
  const sub = status === null
    ? (offline && !mock ? "Relay is not running — status unknown." : "Per-game EQ and spatial audio on one headset.")
    : installed
      ? `Installed on your headset endpoint${status.running ? " · active" : ""}. Only that endpoint carries it.`
      : "Per-game EQ and spatial audio on one headset. Not installed.";

  return (
    <>
      <div className="tog">
        <div><b>Endpoint audio processor (APO)</b><small>{sub}</small></div>
        <button className="btn q" disabled={status === null}
          onClick={() => setMode(mode === "idle" ? (installed ? "remove" : "install") : "idle")}>
          {installed ? "Remove…" : "Install…"}
        </button>
      </div>
      {mode !== "idle" && (
        <ElevatedPanel
          op={mode === "install" ? "install_apo" : "uninstall_apo"}
          onDone={() => { refreshStatus(); }}
          onClose={() => setMode("idle")}
          verb={mode === "install" ? "Install" : "Remove"}
          blurb={mode === "install" ? (
            <>
              <p className="p"><b>What this installs:</b> one audio-effect DLL registered on your headset's
                render endpoint only. Your endpoint's complete prior state is saved to
                <span className="mono"> %LOCALAPPDATA%\Relay\apo-backup</span> before anything is written.
                These are the exact values that change:</p>
            </>
          ) : (
            <p className="p"><b>What this removes:</b> the saved state is written back byte-for-byte and the
              registration is deleted. No other endpoint, app or global setting is touched.</p>
          )} />
      )}
    </>
  );
}

/** The virtual camera & microphone opt-in. The camera's COM class has to live
 *  in HKLM: the Frame Server runs as LOCAL SERVICE and never loads a per-user
 *  registration (measured — docs/dev/vcam-live.md), so this is the one write
 *  that genuinely needs the prompt. */
function VdeviceConsentRow() {
  const { offline, mock } = useCore();
  const [status, setStatus] = useState<VdeviceStatus | null>(null);
  const [mode, setMode] = useState<"idle" | "install" | "remove">("idle");

  const refreshStatus = () => {
    api.vdeviceStatus().then(setStatus).catch(() => setStatus(null));
  };
  useEffect(refreshStatus, [offline]);

  const registered = status?.camera_registered === true;
  const micNote = status && status.mic_targets.length > 0
    ? `Mic route: ${status.mic_targets[0].name}.`
    : "Mic: waiting on the signed driver; install VB-Cable for the interim route.";
  const sub = status === null
    ? (offline && !mock ? "Relay is not running — status unknown." : "Show incoming shares as a webcam in calls.")
    : !status.camera_supported
      ? `Needs Windows 11 22H2+ (this PC: build ${status.windows_build ?? "?"}).${status.obs_virtualcam ? " OBS VirtualCam detected as a fallback." : ""}`
      : registered
        ? `"Relay Camera" registered — it appears in calls while receiving. ${micNote}`
        : `Not installed. ${micNote}`;

  return (
    <>
      <div className="tog">
        <div><b>Virtual camera &amp; microphone</b><small>{sub}</small></div>
        <button className="btn q"
          disabled={status === null || (status?.camera_supported === false && !registered)}
          onClick={() => setMode(mode === "idle" ? (registered ? "remove" : "install") : "idle")}>
          {registered ? "Remove…" : "Install…"}
        </button>
      </div>
      {mode !== "idle" && (
        <ElevatedPanel
          op={mode === "install" ? "install_camera" : "uninstall_camera"}
          verb={mode === "install" ? "Install" : "Remove"}
          onClose={() => setMode("idle")}
          onDone={refreshStatus}
          // Consent is recorded before the prompt, never after: the helper
          // refuses to register the camera without it, and recording it is a
          // per-user write that needs no elevation of its own.
          before={mode === "install"
            ? async () => { await api.setVdeviceConsent(status?.consent?.apo ?? false, true, true); }
            : undefined}
          after={mode === "remove"
            ? async () => { await api.setVdeviceConsent(status?.consent?.apo ?? false, false, false); }
            : undefined}
          blurb={mode === "install" ? (
            <p className="p"><b>What this installs</b> — one COM class so the Windows camera service
              can load Relay's media source (the DLL stays where it is):</p>
          ) : (
            <p className="p"><b>What this removes</b> — exactly the keys recorded in
              <span className="mono"> %LOCALAPPDATA%\Relay\installed.json</span>, and nothing else:</p>
          )} />
      )}
    </>
  );
}

/** The consent panel both cards share: blurb, the real plan, then one button
 *  that raises the Windows prompt. Nothing here can change the machine — the
 *  plan is read-only and the button is the only thing that elevates. */
function ElevatedPanel({ op, verb, blurb, onClose, onDone, before, after }: {
  op: ElevatedOp;
  verb: string;
  blurb: React.ReactNode;
  onClose: () => void;
  onDone: () => void;
  before?: () => Promise<void>;
  after?: () => Promise<void>;
}) {
  const { plan, busy, note, error, loadPlan, run, clearError } = useElevation(op, onDone);
  const [prepError, setPrepError] = useState<string | null>(null);
  useEffect(loadPlan, [op]);

  const go = async () => {
    setPrepError(null);
    try {
      if (before) await before();
      const r = await run();
      // Only mirror the result into per-user state if the machine actually
      // changed. A declined prompt or a failed helper must leave
      // installed.json exactly as it was: withdrawing consent for a camera
      // that is still registered leaves a registration nothing consented to,
      // which is the one state this file is meant to make impossible.
      if (after && r && r.ok && !r.declined) await after();
    } catch (e) {
      setPrepError(errText(e));
    }
  };

  return (
    <div className="consent">
      {blurb}
      <PlanLines lines={plan} />
      {note === null ? (
        <div className="ab">
          <button className="btn acc" disabled={busy || plan === null} onClick={() => void go()}>
            {busy ? "Waiting for Windows…" : `${verb} now`}
          </button>
          <button className="btn q" disabled={busy} onClick={onClose}>Cancel</button>
        </div>
      ) : (
        <>
          {note.map((l, i) => <p className="p small" key={`${l}-${i}`}>{l}</p>)}
          <div className="ab"><button className="btn q" onClick={onClose}>Close</button></div>
        </>
      )}
      <ErrorNote text={error ?? prepError} onDismiss={() => { clearError(); setPrepError(null); }} />
    </div>
  );
}
