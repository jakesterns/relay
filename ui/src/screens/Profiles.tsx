import { useEffect, useState } from "react";
import { Card, Chips, Kv, Live, Pill } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, fmtMb, newProfile, type ProcessInfo, type Profile, type ProfileSummary, type SharePreset, type ProfileStatus } from "../lib/ipc";

const shareLabel: Record<ProfileSummary["share"], string> = { game: "Game", daw: "DAW", desktop: "Desktop", off: "Off" };

export function Profiles() {
  const { state, profiles, refresh } = useCore();
  const active = state.active_profile;
  const fp = state.footprint;
  const chain = state.audio_chain;

  const [editing, setEditing] = useState<Profile | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const apply = async (id: string) => {
    try { await api.applyProfile(id); await refresh(); } catch { /* surfaced via offline banner */ }
  };

  const startNew = () => { setEditing(newProfile()); setIsNew(true); setError(null); };
  const startEdit = async (id: string) => {
    try { setEditing(await api.getProfile(id)); setIsNew(false); setError(null); }
    catch (e) { setError(errText(e)); }
  };
  const close = () => { setEditing(null); setError(null); };

  const save = async (p: Profile) => {
    try { await api.saveProfile(p); await refresh(); close(); }
    catch (e) { setError(errText(e)); }
  };
  const remove = async (id: string) => {
    try { await api.deleteProfile(id); await refresh(); close(); }
    catch (e) { setError(errText(e)); }
  };

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Profiles</h1>
          <Live on={!!active} text={active ? `${active.name} active` : "Nothing active"} />
        </div>
        <OfflineBanner />
        <div className="stat">
          <div><label>Memory</label><div className="v">{fmtMb(fp.rss_bytes)}<u>MB</u></div><div className="hint">Core service only</div></div>
          <div><label>CPU</label><div className="v">{fp.cpu_percent.toFixed(1)}<u>%</u></div><div className="hint">{active ? "Profile active" : "Waiting for a game"}</div></div>
          <div><label>Audio chain</label><div className="v">{chain === "bypass" ? "Bypass" : chain === "active" ? "Active" : "Exclusive"}</div><div className="hint">{chain === "bypass" ? "Pass-through · 0 ms" : chain === "active" ? "EQ + HRTF" : "Game bypasses the APO"}</div></div>
          <div><label>Display</label><div className="v">{state.display_state === "applied" ? "Applied" : "Default"}</div><div className="hint">{state.display_state === "applied" ? "Backup on disk" : "Windows settings"}</div></div>
        </div>
        {editing && (
          <ProfileForm
            key={editing.id}
            initial={editing}
            isNew={isNew}
            error={error}
            onSave={save}
            onDelete={isNew ? undefined : remove}
            onCancel={close}
          />
        )}
        <Card title="Game profiles" action="New profile" onAction={startNew}>
          {profiles.length === 0 ? (
            <div className="empty">No profiles yet. Add one and it applies the next time that game has focus.</div>
          ) : (
            <table className="tbl">
              <thead>
                <tr><th>Game</th><th>Headset</th><th>Monitor</th><th>Share</th><th /></tr>
              </thead>
              <tbody>
                {profiles.map((p) => (
                  <tr key={p.id} className={active?.id === p.id ? "sel" : ""}
                      onClick={() => void startEdit(p.id)} onDoubleClick={() => void apply(p.id)}>
                    <td><b>{p.name}</b><span>{p.note || p.exe}</span></td>
                    <td>{p.headset ?? "Any"}</td>
                    <td>{p.monitor ?? "Any"}</td>
                    <td>{shareLabel[p.share]}</td>
                    <td className="r">
                      {active?.id === p.id
                        ? <Pill kind="on" text="Active" />
                        : p.status === "ready" ? <Pill kind="ready" text="Ready" /> : <Pill kind="off" text="Draft" />}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Card>
      </section>
      <aside className="side">
        <Card title="Headsets & IEMs" action="Add">
          <div className="empty">Library is empty</div>
        </Card>
        <Card title="Monitors" action="Add">
          <div className="empty">Library is empty</div>
        </Card>
        <Card>
          <Kv k="Auto-switch" v="By plugged hardware" />
          <Kv k="Foreground" v={state.foreground?.exe || "—"} mono />
        </Card>
        <p className="note">Click a row to edit it, double-click to apply it now. Profiles are matched to whatever headset and monitor are connected, so swapping gear swaps the tuning.</p>
      </aside>
    </>
  );
}

function errText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}

/** New / Edit form. Headset and monitor are free text until M1 lands the hardware library. */
function ProfileForm({ initial, isNew, error, onSave, onDelete, onCancel }: {
  initial: Profile; isNew: boolean; error: string | null;
  onSave: (p: Profile) => void; onDelete?: (id: string) => void; onCancel: () => void;
}) {
  const [p, setP] = useState<Profile>(initial);
  const [procs, setProcs] = useState<ProcessInfo[]>([]);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const loadProcs = async () => {
    try { setProcs(await api.listProcesses()); } catch { setProcs([]); }
  };
  useEffect(() => { void loadProcs(); }, []);
  useEffect(() => {
    if (!confirmDelete) return;
    const t = setTimeout(() => setConfirmDelete(false), 4000);
    return () => clearTimeout(t);
  }, [confirmDelete]);

  const set = <K extends keyof Profile>(k: K, v: Profile[K]) => setP({ ...p, [k]: v });
  const valid = p.name.trim().length > 0 && p.game.exe.trim().length > 0;

  const submit = () => {
    if (!valid) return;
    const clean: Profile = {
      ...p,
      name: p.name.trim(),
      game: { ...p.game, exe: p.game.exe.trim() },
      headset: p.headset?.trim() ? p.headset.trim() : undefined,
      monitor: p.monitor?.trim() ? p.monitor.trim() : undefined,
    };
    onSave(clean);
  };

  return (
    <Card title={isNew ? "New profile" : "Edit profile"} action="Cancel" onAction={onCancel}>
      <div className="form">
        <label className="field">
          <span>Name</span>
          <input value={p.name} placeholder="Call of Duty" autoFocus
            onChange={(e) => set("name", e.target.value)} />
        </label>
        <label className="field">
          <span>Executable</span>
          <div className="row">
            <input value={p.game.exe} placeholder="cod.exe" list="relay-procs" className="mono"
              onChange={(e) => set("game", { ...p.game, exe: e.target.value })} />
            <select value="" onChange={(e) => { if (e.target.value) set("game", { ...p.game, exe: e.target.value }); }}>
              <option value="">Running…</option>
              {procs.map((pr) => <option key={pr.pid} value={pr.exe}>{pr.exe} — {pr.title}</option>)}
            </select>
            <button className="btn q" onClick={() => void loadProcs()} title="Refresh running processes">↻</button>
          </div>
          <datalist id="relay-procs">
            {procs.map((pr) => <option key={pr.pid} value={pr.exe}>{pr.title}</option>)}
          </datalist>
        </label>
        <label className="field">
          <span>Note</span>
          <input value={p.note} placeholder="Footsteps · dark-map colors"
            onChange={(e) => set("note", e.target.value)} />
        </label>
        <div className="two">
          <label className="field">
            <span>Headset</span>
            <input value={p.headset ?? ""} placeholder="Any"
              onChange={(e) => set("headset", e.target.value || undefined)} />
          </label>
          <label className="field">
            <span>Monitor</span>
            <input value={p.monitor ?? ""} placeholder="Any"
              onChange={(e) => set("monitor", e.target.value || undefined)} />
          </label>
        </div>
        <Chips<SharePreset> label="Share preset" value={p.share} onChange={(v) => set("share", v)}
          options={[{ key: "game", label: "Game" }, { key: "daw", label: "DAW" }, { key: "desktop", label: "Desktop" }, { key: "off", label: "Off" }]} />
        <Chips<ProfileStatus> label="Status" value={p.status} onChange={(v) => set("status", v)}
          options={[{ key: "draft", label: "Draft" }, { key: "ready", label: "Ready" }]} />
        <p className="p small">Only <b>Ready</b> profiles apply automatically. Headset and monitor are matched by name until the hardware library lands.</p>
        {error && <div className="offline"><i />{error}</div>}
        <div className="actions">
          <button className="btn acc" disabled={!valid} onClick={submit}>{isNew ? "Create profile" : "Save changes"}</button>
          <button className="btn q" onClick={onCancel}>Cancel</button>
          {onDelete && (
            confirmDelete
              ? <button className="btn danger" onClick={() => onDelete(p.id)}>Confirm delete</button>
              : <button className="btn q" onClick={() => setConfirmDelete(true)}>Delete…</button>
          )}
        </div>
      </div>
    </Card>
  );
}
