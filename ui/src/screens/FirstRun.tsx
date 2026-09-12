import { useEffect, useState } from "react";
import { Card, Toggle } from "../components/Controls";
import { api } from "../lib/ipc";

/** First-run consent: the two optional components, exactly what each one
 *  installs (dry-run listing from the core), and how to remove them. Records
 *  the decision only — nothing installs from this screen; installs happen in
 *  Settings with their own explicit step. */
export function FirstRun({ onDone }: { onDone: () => void }) {
  const [apo, setApo] = useState(false);
  const [camMic, setCamMic] = useState(false);
  const [autostart, setAutostart] = useState(false);
  const [dryRun, setDryRun] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.vdeviceDryRun().then(setDryRun).catch(() => setDryRun([]));
  }, []);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.setVdeviceConsent(apo, camMic, camMic);
      // Asked here rather than in the installer: the installer's job is to
      // copy files, and a checkbox buried in a setup wizard is a poor place
      // to ask for the one registry value Relay ever writes on its own.
      if (autostart) await api.setAutostart(true);
      onDone();
    } catch (e) {
      setError(String((e as { message?: string })?.message ?? e));
      setBusy(false);
    }
  };

  return (
    <section className="main" style={{ maxWidth: 720, margin: "0 auto" }}>
      <div className="hdr"><h1>Before anything is installed</h1></div>
      <p className="p">Nothing on your PC has been changed. Relay's screen share and profiles work
        without either component below — they exist for per-game audio processing and for showing
        up as a webcam in calls. Both are off until you say otherwise, and you can change your
        answer any time in Settings.</p>

      <Card title="Endpoint audio processor (APO)">
        <Toggle on={apo} onChange={setApo} label="Allow the audio processor"
          sub="Per-game EQ and spatial audio on your headset endpoint." />
        <div className="consent">
          <p className="p"><b>What it installs:</b> one audio-effect DLL registered on your
            headset's render endpoint only — three values in that endpoint's FX chain and one COM
            class under HKLM. The endpoint's complete prior state is saved to
            <span className="mono"> %LOCALAPPDATA%\Relay\apo-backup</span> before anything is
            written; removal restores it byte-for-byte.</p>
        </div>
      </Card>

      <Card title="Virtual camera &amp; microphone">
        <Toggle on={camMic} onChange={setCamMic} label="Allow the virtual camera & microphone"
          sub={'Discord, Zoom and Meet can pick "Relay Camera" while this PC receives a share.'} />
        <div className="consent">
          <p className="p"><b>What the camera installs</b> (every key, exactly):</p>
          {dryRun.map((l) => <div className="mono" key={l} style={{ fontSize: 12 }}>{l}</div>)}
          <p className="p" style={{ marginTop: 8 }}><b>Microphone:</b> the signed Relay driver is
            not included yet (it ships once driver signing completes). Until then Relay can route
            call audio through an already-installed VB-Cable or VoiceMeeter device — that installs
            nothing.</p>
          <p className="p"><b>How to remove:</b> Settings → "What Relay installs", or the
            uninstaller. Everything registered is recorded in
            <span className="mono"> %LOCALAPPDATA%\Relay\installed.json</span> and removal deletes
            exactly that list.</p>
        </div>
      </Card>

      <Card title="Start at login">
        <Toggle on={autostart} onChange={setAutostart} label="Start Relay when I sign in"
          sub="Adds one value under HKCU\…\CurrentVersion\Run and nothing else. Without it, profiles and hotkeys only work while Relay is open." />
      </Card>

      <div className="ab" style={{ marginTop: 12 }}>
        <button className="btn acc" disabled={busy} onClick={() => void save()}>
          {busy ? "Saving…" : "Continue"}
        </button>
      </div>
      {error && <div className="offline"><i />{error}</div>}
      <p className="note" style={{ marginTop: 10 }}>Saying yes here only records your consent —
        each component still shows an explicit install step (administrator required) before it
        touches the machine.</p>
    </section>
  );
}
