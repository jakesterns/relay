import { useEffect, useState } from "react";
import { Card, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, type RecordingSettings } from "../lib/ipc";

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
          <div className="tog"><div><b>Endpoint audio processor (APO)</b><small>Per-game EQ and spatial audio on one headset. Not installed.</small></div><button className="btn q" disabled>Install…</button></div>
          <div className="tog"><div><b>Virtual camera &amp; microphone</b><small>Lets the receiving PC appear as a webcam in calls. Not installed.</small></div><button className="btn q" disabled>Install…</button></div>
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
      </section>
      <aside className="side">
        <Card>
          <Kv k="Core service" v={offline && !mock ? "Offline" : "Running"} />
          <Kv k="Data folder" v="%LOCALAPPDATA%\\Relay" mono />
          <Kv k="Log file" v="…\\Relay\\logs\\core.log" mono />
          <Kv k="Version" v="0.1.0" mono />
        </Card>
        <p className="note">Uninstalling removes every component listed here, the startup entry, and restores the audio chain. Nothing is left behind.</p>
      </aside>
    </>
  );
}
