import { useEffect, useState } from "react";
import { Card, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api } from "../lib/ipc";

export function Settings() {
  const { state, refresh, offline, mock } = useCore();
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartErr, setAutostartErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.getAutostart()
      .then((v) => { if (!cancelled) setAutostart(v); })
      .catch(() => { if (!cancelled) setAutostart(null); });
    return () => { cancelled = true; };
  }, [offline]);

  const toggleAutostart = async (v: boolean) => {
    setAutostartErr(null);
    try { setAutostart(await api.setAutostart(v)); }
    catch (e) { setAutostartErr(String((e as { message?: string })?.message ?? e)); }
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
