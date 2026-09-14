import { useEffect, useState } from "react";
import { Card, Chips, Kv, Live, Pill } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import {
  api, fmtMb, newProfile,
  type CatalogEntry, type ColorInfo, type HardwareMonitor, type Headset, type HeadsetKind, type ProcessInfo,
  type Profile, type ProfileSummary, type SharePreset, type ProfileStatus,
} from "../lib/ipc";

const shareLabel: Record<ProfileSummary["share"], string> = { game: "Game", daw: "DAW", desktop: "Desktop", off: "Off" };
const kindLabel: Record<HeadsetKind, string> = { headphone: "Headphones", iem: "IEM", speakers: "Speakers" };

/** Which controls Relay can drive on this panel, from its advertised VCP codes. */
function ddcControls(codes: number[]): string {
  const known: [number, string][] = [[0x10, "brightness"], [0x12, "contrast"], [0x87, "sharpness"]];
  const names = known.filter(([c]) => codes.includes(c)).map(([, n]) => n);
  return names.length ? `controls: ${names.join(", ")}` : `DDC/CI ${codes.length} codes`;
}

export function Profiles() {
  const { state, profiles, hardware, refresh } = useCore();
  const active = state.active_profile;
  const fp = state.footprint;
  const chain = state.audio_chain;

  const [editing, setEditing] = useState<Profile | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [adding, setAdding] = useState<"headset" | "monitor" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hwError, setHwError] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);

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

  /** Remove a headset or monitor from the library.
   *
   *  Profiles that named it keep the id and fall back to matching "Any", so
   *  this loses the entry's name and measured curve but never a profile. The
   *  confirm is here because a curve can represent a long import. */
  const removeHw = async (id: string, name: string) => {
    if (!window.confirm(`Remove ${name} from the hardware library?\n\nProfiles that use it stay, but stop matching on it.`)) return;
    setHwError(null);
    try { await api.deleteHardware(id); await refresh(); }
    catch (e) { setHwError(errText(e)); }
  };

  /** Full re-probe, including the slow per-monitor DDC/CI capability query
   *  that fills in which controls each panel actually exposes. */
  const rescan = async () => {
    setScanning(true);
    setHwError(null);
    try { await api.probeHardware(); await refresh(); }
    catch (e) { setHwError(errText(e)); }
    finally { setScanning(false); }
  };

  /** Library names for the table; fall back to the raw id. */
  const headsetName = (id: string | null) => hardware.headsets.find((h) => h.id === id)?.name ?? id ?? "Any";
  const monitorName = (id: string | null) => hardware.monitors.find((m) => m.id === id)?.name ?? id ?? "Any";

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
        {adding === "headset" && (
          <HeadsetDialog onClose={() => setAdding(null)} onSaved={async () => { setAdding(null); await refresh(); }} />
        )}
        {adding === "monitor" && (
          <MonitorDialog onClose={() => setAdding(null)} onSaved={async () => { setAdding(null); await refresh(); }} />
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
                    <td>{headsetName(p.headset)}</td>
                    <td>{monitorName(p.monitor)}</td>
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
        <Card title="Headsets & IEMs" action="Add" onAction={() => setAdding("headset")}>
          {hardware.headsets.length === 0 ? (
            <div className="empty">Library is empty</div>
          ) : hardware.headsets.map((h) => (
            <div className="hwl" key={h.id}>
              <div className="ic r" />
              <div>
                <b>{h.name}</b>
                <span>{kindLabel[h.kind]}{h.curve ? ` · curve (${h.source || "measured"})` : " · no curve"}</span>
              </div>
              {hardware.connected.headset === h.id && <Pill kind="on" text="Plugged" />}
              <button className="rm" title={`Remove ${h.name} from the library`}
                onClick={() => void removeHw(h.id, h.name)}>Remove</button>
            </div>
          ))}
        </Card>
        <Card title="Monitors" action="Add" onAction={() => setAdding("monitor")}>
          {hardware.monitors.length === 0 ? (
            <div className="empty">Library is empty</div>
          ) : hardware.monitors.map((m) => {
            const plugged = hardware.connected.monitors.find((c) => c.id === m.id);
            return (
              <div className="hwl" key={m.id}>
                <div className="ic" />
                <div>
                  <b>{m.name}</b>
                  <span>{m.panel || "Panel unknown"}{m.ddcci ? ` · ${ddcControls(m.ddcci)}` : " · controls not scanned"}</span>
                  {m.color && <span className="mono" style={{ fontSize: 11 }}>{colorSummary(m.color)}</span>}
                </div>
                {plugged && <Pill kind={plugged.primary ? "on" : "ready"} text={plugged.primary ? "Main" : "Second"} />}
                <button className="rm" title={`Remove ${m.name} from the library`}
                  onClick={() => void removeHw(m.id, m.name)}>Remove</button>
              </div>
            );
          })}
          {/* The only thing that asks each monitor which DDC/CI controls it
              actually has. It is a slow query (a capability string per panel),
              so it is a button rather than something the core does on every
              probe. Until it runs, the Display sliders allow everything and
              report what the monitor refused. */}
          <button className="btn q" disabled={scanning} onClick={() => void rescan()}>
            {scanning ? "Scanning…" : "Scan monitor controls"}
          </button>
          {hwError && <div className="offline"><i />{hwError}</div>}
        </Card>
        <Card>
          <Kv k="Auto-switch" v="By plugged hardware" />
          <Kv k="Default audio" v={hardware.connected.endpoints.find((e) => e.default)?.name ?? "—"} />
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

/** One line of what the panel says about itself: gamut, HDR, bit depth.
 *
 *  Coverage is containment of the reference gamut, not an area ratio, so
 *  "97% P3" means the panel really reaches 97% of those colours. Panel
 *  technology is deliberately absent — EDID does not report it, so the
 *  free-text `panel` field beside this is the user's to fill in. */
function colorSummary(c: ColorInfo): string {
  const parts: string[] = [];
  if (c.coverage) {
    const p3 = Math.round(c.coverage.dci_p3 * 100);
    const bt = Math.round(c.coverage.bt2020 * 100);
    parts.push(p3 >= 90 ? `P3 ${p3}%` : `sRGB ${Math.round(c.coverage.srgb * 100)}%`);
    if (bt >= 60) parts.push(`BT.2020 ${bt}%`);
  }
  const hdr = [
    c.hdr.dolby_vision && "Dolby Vision",
    c.hdr.hdr10_plus && "HDR10+",
    c.hdr.hdr10 && "HDR10",
    c.hdr.hlg && "HLG",
  ].filter(Boolean) as string[];
  if (hdr.length) {
    parts.push(c.hdr.max_nits ? `${hdr[0]} · ${Math.round(c.hdr.max_nits)} nits` : hdr[0]);
  }
  if (c.bit_depth) parts.push(`${c.bit_depth}-bit`);
  return parts.join(" · ") || "no colour data";
}

/** Search 8,849 measured models and add one with its curve.
 *
 *  The index ships with Relay, so typing is offline and instant. Picking a
 *  model is the one moment Relay reaches the network: it downloads that
 *  model's measurement, caches it under the data folder, and credits whoever
 *  measured it — the licence requires the credit, and the download-on-demand
 *  is why Relay can use this data at all without redistributing it. */
function CatalogSearch({ endpoint, onAdded }: { endpoint: string; onAdded: () => Promise<void> }) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<CatalogEntry[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);

  useEffect(() => {
    const q = query.trim();
    if (q.length < 2) { setHits([]); return; }
    // Debounced: the index is scanned per keystroke otherwise.
    let live = true;
    setSearching(true);
    const t = setTimeout(() => {
      api.searchCatalog(q)
        .then((r) => { if (live) setHits(r); })
        .catch((e) => { if (live) setError(errText(e)); })
        .finally(() => { if (live) setSearching(false); });
    }, 180);
    return () => { live = false; clearTimeout(t); };
  }, [query]);

  const add = async (e: CatalogEntry) => {
    setBusy(`${e.name}|${e.source}`);
    setError(null);
    try { await api.addHeadsetFromCatalog(e, endpoint || null); await onAdded(); }
    catch (err) { setError(errText(err)); }
    finally { setBusy(null); }
  };

  return (
    <>
      <label className="field">
        <span>Find your headphones</span>
        <input value={query} autoFocus placeholder="HD 560S, Blessing 3, DT 770…"
          onChange={(e) => setQuery(e.target.value)} />
      </label>
      {query.trim().length >= 2 && (
        <div className="catalog">
          {searching && hits.length === 0 && <div className="empty">Searching…</div>}
          {!searching && hits.length === 0 && <div className="empty">No match in the catalogue</div>}
          {hits.map((e) => {
            const key = `${e.name}|${e.source}`;
            return (
              <div className="hwl" key={`${key}|${e.rig}`}>
                <div className="ic r" />
                <div>
                  <b>{e.name}</b>
                  <span>Measured by {e.source}{e.rig ? ` on ${e.rig}` : ""}</span>
                </div>
                <button className="btn q" disabled={busy !== null}
                  onClick={() => void add(e)}>{busy === key ? "Adding…" : "Add"}</button>
              </div>
            );
          })}
        </div>
      )}
      <p className="p small">
        Measurements come from the <b>AutoEQ</b> project and its contributors (oratory1990,
        crinacle and others). Relay ships only the list of names; the curve itself is downloaded
        when you pick a model, cached on this PC, and never redistributed.
      </p>
      {error && <div className="offline"><i />{error}</div>}
    </>
  );
}

/** Turn "Moondrop Blessing 3" into a stable-ish library id. */
function slug(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

/** Add a headset: name it, bind it to an audio endpoint, optionally paste an
 *  AutoEQ result. The endpoint binding is what makes it "plugged". */
function HeadsetDialog({ onClose, onSaved }: { onClose: () => void; onSaved: () => Promise<void> }) {
  const { hardware } = useCore();
  const endpoints = hardware.connected.endpoints;
  const [name, setName] = useState("");
  const [kind, setKind] = useState<HeadsetKind>("headphone");
  const [endpoint, setEndpoint] = useState(endpoints.find((e) => e.default)?.key ?? endpoints[0]?.key ?? "");
  const [curveText, setCurveText] = useState("");
  const [source, setSource] = useState("");
  const [error, setError] = useState<string | null>(null);
  const valid = name.trim().length > 0;

  const save = async () => {
    if (!valid) return;
    const h: Headset = {
      id: slug(name),
      name: name.trim(),
      kind,
      source: source.trim(),
      endpoints: endpoint ? [endpoint] : [],
    };
    try {
      await api.saveHardware({ kind: "headset", value: h });
      if (curveText.trim()) await api.importCurve(h.id, curveText);
      await onSaved();
    } catch (e) { setError(errText(e)); }
  };

  return (
    <Card title="Add headset" action="Cancel" onAction={onClose}>
      <div className="form">
        <CatalogSearch endpoint={endpoint} onAdded={onSaved} />
        <div className="hdr" style={{ margin: "6px 0 0" }}>
          <span className="note">Or describe it yourself</span>
        </div>
        <label className="field">
          <span>Name</span>
          <input value={name} placeholder="HD 560S" autoFocus onChange={(e) => setName(e.target.value)} />
        </label>
        <Chips<HeadsetKind> label="Kind" value={kind} onChange={setKind}
          options={[{ key: "headphone", label: "Headphones" }, { key: "iem", label: "IEM" }, { key: "speakers", label: "Speakers" }]} />
        <label className="field">
          <span>Plugged into</span>
          <select value={endpoint} onChange={(e) => setEndpoint(e.target.value)}>
            <option value="">Not bound yet</option>
            {endpoints.map((e) => (
              <option key={e.key} value={e.key}>{e.name}{e.default ? " (default)" : ""}</option>
            ))}
          </select>
        </label>
        <div className="two">
          <label className="field">
            <span>Curve source</span>
            <input value={source} placeholder="oratory1990" onChange={(e) => setSource(e.target.value)} />
          </label>
        </div>
        <label className="field">
          <span>Measured curve (AutoEQ CSV, optional)</span>
          <textarea rows={4} className="mono" value={curveText} placeholder={"frequency,raw,…\n20.00,-4.11,…"}
            onChange={(e) => setCurveText(e.target.value)} />
        </label>
        <p className="p small">Paste the contents of an AutoEQ result file, or leave this empty and use the search above.</p>
        {error && <div className="offline"><i />{error}</div>}
        <div className="actions">
          <button className="btn acc" disabled={!valid} onClick={() => void save()}>Add to library</button>
          <button className="btn q" onClick={onClose}>Cancel</button>
        </div>
      </div>
    </Card>
  );
}

/** Add a monitor: pick a detected panel (EDID identity prefilled) or type one in. */
function MonitorDialog({ onClose, onSaved }: { onClose: () => void; onSaved: () => Promise<void> }) {
  const { hardware } = useCore();
  const detected = hardware.connected.monitors;
  const [pick, setPick] = useState(detected[0]?.id ?? "");
  const picked = detected.find((m) => m.id === pick);
  const [name, setName] = useState(detected[0]?.name ?? "");
  const [panel, setPanel] = useState("");
  const [error, setError] = useState<string | null>(null);
  const valid = (picked ? true : pick.trim().length > 0) && name.trim().length > 0;

  const save = async () => {
    if (!valid) return;
    const m: HardwareMonitor = { id: pick.trim(), name: name.trim(), panel: panel.trim() };
    try {
      await api.saveHardware({ kind: "monitor", value: m });
      await onSaved();
    } catch (e) { setError(errText(e)); }
  };

  return (
    <Card title="Add monitor" action="Cancel" onAction={onClose}>
      <div className="form">
        <label className="field">
          <span>Detected</span>
          <select value={picked ? pick : ""} onChange={(e) => {
            setPick(e.target.value);
            const d = detected.find((m) => m.id === e.target.value);
            if (d) setName(d.name);
          }}>
            <option value="">Enter manually…</option>
            {detected.map((m) => (
              <option key={m.id} value={m.id}>
                {m.name}{m.native ? ` · ${m.native[0]}×${m.native[1]}` : ""}{m.primary ? " (main)" : ""}
              </option>
            ))}
          </select>
        </label>
        {!picked && (
          <label className="field">
            <span>Monitor id</span>
            <input value={pick} className="mono" placeholder="mon:GSM5C7C:402NTCZ9E219"
              onChange={(e) => setPick(e.target.value)} />
          </label>
        )}
        <div className="two">
          <label className="field">
            <span>Name</span>
            <input value={name} placeholder="LG 27GP850" onChange={(e) => setName(e.target.value)} />
          </label>
          <label className="field">
            <span>Panel</span>
            <input value={panel} placeholder="Nano IPS" onChange={(e) => setPanel(e.target.value)} />
          </label>
        </div>
        <p className="p small">The id comes from the monitor's EDID, so it stays the same on any port or GPU output. DDC/CI controls are filled in the first time a display profile probes this panel.</p>
        {error && <div className="offline"><i />{error}</div>}
        <div className="actions">
          <button className="btn acc" disabled={!valid} onClick={() => void save()}>Add to library</button>
          <button className="btn q" onClick={onClose}>Cancel</button>
        </div>
      </div>
    </Card>
  );
}

/** New / Edit form. Headset and monitor come from the hardware library. */
function ProfileForm({ initial, isNew, error, onSave, onDelete, onCancel }: {
  initial: Profile; isNew: boolean; error: string | null;
  onSave: (p: Profile) => void; onDelete?: (id: string) => void; onCancel: () => void;
}) {
  const { hardware } = useCore();
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
      headset: p.headset || undefined,
      monitor: p.monitor || undefined,
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
            <select value={p.headset ?? ""} onChange={(e) => set("headset", e.target.value || undefined)}>
              <option value="">Any</option>
              {hardware.headsets.map((h) => <option key={h.id} value={h.id}>{h.name}</option>)}
            </select>
          </label>
          <label className="field">
            <span>Monitor</span>
            <select value={p.monitor ?? ""} onChange={(e) => set("monitor", e.target.value || undefined)}>
              <option value="">Any</option>
              {hardware.monitors.map((m) => <option key={m.id} value={m.id}>{m.name}</option>)}
            </select>
          </label>
        </div>
        <Chips<SharePreset> label="Share preset" value={p.share} onChange={(v) => set("share", v)}
          options={[{ key: "game", label: "Game" }, { key: "daw", label: "DAW" }, { key: "desktop", label: "Desktop" }, { key: "off", label: "Off" }]} />
        <Chips<ProfileStatus> label="Status" value={p.status} onChange={(v) => set("status", v)}
          options={[{ key: "draft", label: "Draft" }, { key: "ready", label: "Ready" }]} />
        <p className="p small">Only <b>Ready</b> profiles apply automatically. With several Ready rows for one game, the row matching the plugged headset and monitor wins.</p>
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
