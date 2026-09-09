import { Card, Kv, Live } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api } from "../lib/ipc";

export function Settings() {
  const { state, refresh } = useCore();
  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Settings</h1>
          <Live on={!state.active_profile} text={state.active_profile ? "Profile applied" : "Nothing applied"} />
        </div>
        <OfflineBanner />
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
          <Kv k="Core service" v={state.foreground ? "Running" : "—"} />
          <Kv k="Data folder" v="%LOCALAPPDATA%\\Relay" mono />
          <Kv k="Version" v="0.1.0" mono />
        </Card>
        <p className="note">Uninstalling removes every component listed here and restores the audio chain. Nothing is left behind.</p>
      </aside>
    </>
  );
}
