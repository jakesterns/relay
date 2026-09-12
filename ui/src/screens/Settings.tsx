import { useEffect, useState } from "react";
import { Card, Kv, Live, Toggle } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, type ApoStatus } from "../lib/ipc";

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
          <ApoConsentRow />
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
