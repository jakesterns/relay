import { useEffect, useState } from "react";
import { Card, DoneNote, ErrorNote, Toggle } from "./Controls";
import { errText } from "../lib/err";
import { api, type UiPrefs, type UpdateStatus } from "../lib/ipc";

/**
 * Settings → Updates (S45). The owner's rule: Relay checks on its own, and
 * installs only when the user says so. The card shows what was found, the
 * release notes, and the three choices; the two switches sit under them.
 */
export function UpdatesCard({ prefs, setPref }: {
  prefs: UiPrefs | null;
  setPref: (patch: Partial<UiPrefs>) => void | Promise<void>;
}) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.updateStatus()
      .then((s) => { if (!cancelled) setStatus(s); })
      .catch(() => { if (!cancelled) setStatus(null); });
    return () => { cancelled = true; };
  }, []);

  // While the core is checking or downloading, follow it.
  const busy = status !== null && status.phase !== "idle";
  useEffect(() => {
    if (!busy) return;
    const t = setInterval(() => {
      api.updateStatus().then(setStatus).catch(() => {});
    }, 1500);
    return () => clearInterval(t);
  }, [busy]);

  const run = async (f: () => Promise<UpdateStatus>) => {
    setErr(null);
    try { setStatus(await f()); } catch (e) { setErr(errText(e)); }
  };

  const a = status?.available ?? null;
  const phaseLine = status === null ? null : ({
    idle: null,
    checking: "Checking for updates…",
    downloading: "Downloading and verifying the update…",
    waiting: status.waiting_for ?? "Verified. Installing as soon as nothing is in the way…",
    installing: "Installing. Relay will close and reopen on its own.",
  } as const)[status.phase];

  return (
    <Card title="Updates">
      <p className="p">
        Relay {status?.current ?? ""} — {a
          ? <>Relay {a.version}{a.prerelease ? " (pre-release)" : ""} is available.</>
          : status?.last_check
            ? <>you have the latest version. Last checked {new Date(status.last_check * 1000).toLocaleString()}.</>
            : <>not checked yet.</>}
      </p>
      {a && (
        <div className="update-offer" data-testid="update-offer">
          <p className="p small"><strong>What's new in {a.version}</strong></p>
          <pre className="p small mono" style={{ whiteSpace: "pre-wrap", maxHeight: 220, overflow: "auto" }}>
            {a.notes || "No release notes were published."}
          </pre>
          <div className="row" style={{ display: "flex", gap: 8 }}>
            <button className="btn acc" disabled={busy} onClick={() => void run(api.installUpdate)}>Install now</button>
            <button className="btn q" disabled={busy} onClick={() => void run(api.updateLater)}>Later</button>
            <button className="btn q" disabled={busy} onClick={() => void run(() => api.skipUpdate(a.version))}>
              Skip this version
            </button>
          </div>
          <p className="p small">
            Installing downloads the installer from this project's GitHub releases, checks it against the
            published SHA-256 and its signature, and never runs during a share or while a game profile is applied.
            Your settings and profiles are kept.
          </p>
        </div>
      )}
      {phaseLine && <p className="note" role="status">{phaseLine}</p>}
      <button className="btn" disabled={busy || status === null} onClick={() => void run(api.checkForUpdates)}>
        Check now
      </button>
      {status?.last_result && (status.last_result.ok
        ? <DoneNote text={status.last_result.message} />
        : <ErrorNote text={status.last_result.message} />)}
      <ErrorNote text={status?.last_error ?? null} />
      <ErrorNote text={err} onDismiss={() => setErr(null)} />
      <Toggle
        on={prefs?.auto_check_updates ?? true}
        onChange={prefs === null ? undefined : (v) => void setPref({ auto_check_updates: v })}
        label="Check for updates automatically"
        sub={prefs?.auto_check_updates ?? true
          ? "Once a day, Relay asks GitHub whether a newer release exists. Nothing about you or your PC is sent, and nothing is installed without your say."
          : "Relay never checks on its own. Use Check now."} />
      <Toggle
        on={prefs?.auto_install_updates ?? false}
        onChange={prefs === null ? undefined : (v) => void setPref({ auto_install_updates: v })}
        label="Install updates automatically"
        sub={prefs?.auto_install_updates
          ? "A verified update installs on its own, never during a share or a game."
          : "Off. Relay tells you about an update and waits for you to choose."} />
      <Toggle
        on={prefs?.prerelease_updates ?? false}
        onChange={prefs === null ? undefined : (v) => void setPref({ prerelease_updates: v })}
        label="Include pre-releases"
        sub="Test builds before they are released. Off unless you want them." />
      <p className="p small">Checking for updates changes nothing on your PC.</p>
    </Card>
  );
}
