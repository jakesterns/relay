import { useEffect, useState } from "react";
import { Card, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, type ApoStatus, type RecordingSettings, type VdeviceStatus } from "../lib/ipc";

export function Settings() {
  const { state, refresh, offline, mock } = useCore();
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartErr, setAutostartErr] = useState<string | null>(null);
  const [recording, setRecording] = useState<RecordingSettings | null>(null);
  const [recDirty, setRecDirty] = useState(false);
  const [recErr, setRecErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.getAutostart()
      .then((v) => { if (!cancelled) setAutostart(v); })
      .catch(() => { if (!cancelled) setAutostart(null); });
    api.listPresets()
      .then((r) => { if (!cancelled) { setRecording(r.recording); setRecDirty(false); } })
      .catch(() => { if (!cancelled) setRecording(null); });
    return () => { cancelled = true; };
  }, [offline]);

  const toggleAutostart = async (v: boolean) => {
    setAutostartErr(null);
    try { setAutostart(await api.setAutostart(v)); }
    catch (e) { setAutostartErr(String((e as { message?: string })?.message ?? e)); }
  };

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
    catch (e) { setRecErr(String((e as { message?: string })?.message ?? e)); }
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
              ? (offline && !mock ? "Core offline — cannot read the setting." : "Reading…")
              : "Adds one value under HKCU\\...\\CurrentVersion\\Run. Nothing else on your PC is changed; turning this off removes it."} />
          {autostartErr && <div className="offline"><i />{autostartErr}</div>}
        </Card>
        <Card title="What Relay installs">
          <p className="p">Relay works at the OS and hardware layer only. It never injects into games, reads their memory, or changes your default devices. Two optional components need your explicit consent:</p>
          <ApoConsentRow />
          <VdeviceConsentRow />
        </Card>
        <Card title="Recording">
          {recording === null
            ? <p className="note">{offline && !mock ? "Core offline — cannot read the settings." : "Reading…"}</p>
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
                {recErr && <div className="offline"><i />{recErr}</div>}
              </div>
            )}
        </Card>
        <Card title="Hotkeys">
          <Kv k="Toggle share" v="Ctrl + Alt + S" mono />
          <Kv k="Toggle game profile" v="Ctrl + Alt + G" mono />
          <Kv k="Toggle preview" v="Ctrl + Alt + P" mono />
        </Card>
        <Card title="Restore">
          <p className="p" style={{ marginBottom: 10 }}>Put every audio and display setting back to what Windows had before Relay touched it. Safe to press at any time.</p>
          <button className="btn" onClick={() => api.restoreAll().then(refresh)}>Restore original state now</button>
        </Card>
        <UninstallCard />
      </section>
      <aside className="side">
        <Card>
          <Kv k="Core service" v={offline && !mock ? "Offline" : "Running"} />
          <Kv k="Data folder" v="%LOCALAPPDATA%\\Relay" mono />
          <Kv k="Log file" v="…\\Relay\\logs\\core.log" mono />
          <Kv k="Version" v="0.1.0" mono />
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
            ? <p className="note">{offline && !mock ? "Core offline — cannot read the plan." : "Reading…"}</p>
            : lines.map((l, i) => (
              l === ""
                ? <div key={`gap-${i}`} style={{ height: 8 }} />
                : l.startsWith("[")
                  ? <div className="mono" key={l} style={{ fontSize: 12 }}>{l}</div>
                  : <p className="p small" key={l}>{l}</p>
            ))}
          <div className="ab">
            <button className="btn" disabled={lines === null}
              onClick={() => void api.launchUninstaller().catch((e) => setError(String((e as { message?: string })?.message ?? e)))}>
              Uninstall Relay
            </button>
            <button className="btn q" onClick={() => setOpen(false)}>Cancel</button>
          </div>
          {error && <div className="offline"><i />{error}</div>}
        </div>
      )}
    </Card>
  );
}

/** The endpoint-APO opt-in: live status, explicit consent with a plain list
 *  of what gets written, and a remove path that restores the exact prior
 *  state from the on-disk backup. */
function ApoConsentRow() {
  const { offline, mock } = useCore();
  const [status, setStatus] = useState<ApoStatus | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshStatus = () => {
    api.apoStatus().then(setStatus).catch(() => setStatus(null));
  };
  useEffect(refreshStatus, [offline]);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try { await action(); setConfirming(false); }
    catch (e) { setError(String((e as { message?: string })?.message ?? e)); }
    finally { setBusy(false); refreshStatus(); }
  };

  const installed = status?.installed === true;
  const sub = status === null
    ? (offline && !mock ? "Core offline — status unknown." : "Per-game EQ and spatial audio on one headset.")
    : installed
      ? `Installed on your headset endpoint${status.running ? " · active" : ""}. Only that endpoint carries it.`
      : "Per-game EQ and spatial audio on one headset. Not installed.";

  return (
    <>
      <div className="tog">
        <div><b>Endpoint audio processor (APO)</b><small>{sub}</small></div>
        {installed ? (
          <button className="btn q" disabled={busy || status === null}
            onClick={() => void run(() => api.uninstallApo())}>Remove</button>
        ) : (
          <button className="btn q" disabled={busy || status === null}
            onClick={() => setConfirming(!confirming)}>Install…</button>
        )}
      </div>
      {confirming && !installed && (
        <div className="consent">
          <p className="p"><b>What this installs:</b> one audio-effect DLL registered on your headset's
            render endpoint only — three values in that endpoint's FX chain and one COM class under
            HKLM. Your endpoint's complete prior state is saved to
            <span className="mono"> %LOCALAPPDATA%\Relay\apo-backup</span> before anything is written.</p>
          <p className="p"><b>How to remove:</b> this same card (or the uninstaller) restores the saved
            state byte-for-byte and deletes the registration. No other endpoint, app or global setting
            is ever touched.</p>
          <div className="ab">
            <button className="btn acc" disabled={busy} onClick={() => void run(() => api.installApo())}>
              {busy ? "Installing…" : "Install now"}
            </button>
            <button className="btn q" disabled={busy} onClick={() => setConfirming(false)}>Cancel</button>
          </div>
        </div>
      )}
      {error && <div className="offline"><i />{error}</div>}
    </>
  );
}

/** The virtual camera & microphone opt-in: registration status, the exact
 *  dry-run listing before install, and a remove path that empties
 *  installed.json. The install itself needs an elevated core (the button
 *  explains when it isn't). */
function VdeviceConsentRow() {
  const { offline, mock } = useCore();
  const [status, setStatus] = useState<VdeviceStatus | null>(null);
  const [dryRun, setDryRun] = useState<string[]>([]);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshStatus = () => {
    api.vdeviceStatus().then(setStatus).catch(() => setStatus(null));
  };
  useEffect(refreshStatus, [offline]);
  useEffect(() => {
    if (confirming && dryRun.length === 0) {
      api.vdeviceDryRun().then(setDryRun).catch(() => {});
    }
  }, [confirming, dryRun.length]);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try { await action(); setConfirming(false); }
    catch (e) { setError(String((e as { message?: string })?.message ?? e)); }
    finally { setBusy(false); refreshStatus(); }
  };

  const registered = status?.camera_registered === true;
  const micNote = status && status.mic_targets.length > 0
    ? `Mic route: ${status.mic_targets[0].name}.`
    : "Mic: waiting on the signed driver; install VB-Cable for the interim route.";
  const sub = status === null
    ? (offline && !mock ? "Core offline — status unknown." : "Show incoming shares as a webcam in calls.")
    : !status.camera_supported
      ? `Needs Windows 11 22H2+ (this PC: build ${status.windows_build ?? "?"}).${status.obs_virtualcam ? " OBS VirtualCam detected as a fallback." : ""}`
      : registered
        ? `"Relay Camera" registered — it appears in calls while receiving. ${micNote}`
        : `Not installed. ${micNote}`;

  return (
    <>
      <div className="tog">
        <div><b>Virtual camera &amp; microphone</b><small>{sub}</small></div>
        {registered ? (
          <button className="btn q" disabled={busy || status === null}
            onClick={() => void run(async () => {
              await api.uninstallVcam();
              await api.setVdeviceConsent(status?.consent?.apo ?? false, false, false);
            })}>Remove</button>
        ) : (
          <button className="btn q" disabled={busy || status === null || status?.camera_supported === false}
            onClick={() => setConfirming(!confirming)}>Install…</button>
        )}
      </div>
      {confirming && !registered && (
        <div className="consent">
          <p className="p"><b>What this installs</b> — one COM class so the Windows camera service
            can load Relay's media source (the DLL stays where it is):</p>
          {dryRun.map((l) => <div className="mono" key={l} style={{ fontSize: 12 }}>{l}</div>)}
          <p className="p" style={{ marginTop: 8 }}><b>How to remove:</b> this same card or the
            uninstaller deletes exactly those keys; every registration is recorded in
            <span className="mono"> %LOCALAPPDATA%\Relay\installed.json</span>.
            {status?.elevated === false && " Installing needs the core running as administrator."}</p>
          <div className="ab">
            <button className="btn acc" disabled={busy}
              onClick={() => void run(async () => {
                await api.setVdeviceConsent(status?.consent?.apo ?? false, true, true);
                await api.installVcam();
              })}>
              {busy ? "Installing…" : "Install now"}
            </button>
            <button className="btn q" disabled={busy} onClick={() => setConfirming(false)}>Cancel</button>
          </div>
        </div>
      )}
      {error && <div className="offline"><i />{error}</div>}
    </>
  );
}
