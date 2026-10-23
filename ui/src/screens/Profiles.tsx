import { useEffect, useState } from "react";
import { Card, Chips, ConfirmButton, DoneNote, ErrorNote, Kv, Live, Pill } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { ListeningCard } from "./Listening";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import {
  api, catalogKind, fmtMb, newProfile,
  type CatalogEntry, type ColorInfo, type HardwareMonitor, type Headset, type HeadsetKind, type ProcessInfo,
  type Profile, type ProfileSummary, type SharePreset, type ProfileStatus,
} from "../lib/ipc";

const shareLabel: Record<ProfileSummary["share"], string> = { game: "Game", daw: "DAW", desktop: "Desktop", off: "Off" };
const kindLabel: Record<HeadsetKind, string> = { headphone: "Headphones", iem: "IEM", speakers: "Speakers" };

/** Which controls Relay can drive on this panel, from its advertised VCP codes. */
/** What a scan found, one clause per monitor. */
export function scanSummary(monitors: { name: string; ddc?: number[] }[]): string {
  if (monitors.length === 0) return "No monitors found.";
  return monitors.map((m) => m.ddc && m.ddc.length > 0
    ? `${m.name || "Monitor"}: ${ddcControls(m.ddc)}`
    : `${m.name || "Monitor"}: no DDC/CI response (turn on DDC/CI in the monitor's menu)`).join(" · ");
}

function ddcControls(codes: number[]): string {
  // Every MCCS control Relay recognises, in the order a person reads them.
  const known: [number, string][] = [
    [0x10, "brightness"], [0x12, "contrast"], [0x14, "colour preset"],
    [0x16, "red gain"], [0x18, "green gain"], [0x1a, "blue gain"],
    [0x6c, "red black level"], [0x6e, "green black level"], [0x70, "blue black level"],
    [0x87, "sharpness"], [0x60, "input source"], [0x62, "volume"], [0x8d, "mute"],
    [0xdc, "display mode"], [0xd6, "power mode"],
  ];
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
  const [editingHs, setEditingHs] = useState<Headset | null>(null);
  const [editingMon, setEditingMon] = useState<HardwareMonitor | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hwError, setHwError] = useState<string | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanNote, setScanNote] = useState<string | null>(null);

  const apply = async (id: string) => {
    setListError(null);
    try { await api.applyProfile(id); await refresh(); }
    catch (e) { setListError(errText(e)); }
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
   *  this loses the entry's name and measured curve but never a profile. It
   *  still asks, because a curve can represent a long import — through the
   *  same two-step button every other destructive action uses.
   *
   *  It used to ask with window.confirm, which in a window with custom chrome
   *  arrives looking like another program's dialog. */
  const removeHw = async (id: string) => {
    setHwError(null);
    try { await api.deleteHardware(id); await refresh(); }
    catch (e) { setHwError(errText(e)); }
  };

  /** Full re-probe, including the slow per-monitor DDC/CI capability query
   *  that fills in which controls each panel actually exposes. */
  const rescan = async () => {
    setScanning(true);
    setHwError(null);
    setScanNote(null);
    try {
      const report = await api.probeHardware();
      await refresh();
      setScanNote(scanSummary(report.monitors));
    }
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
          <div><label>Memory</label><div className="v">{fmtMb(fp.rss_bytes)}<u>MB</u></div><div className="hint">Background part of Relay</div></div>
          <div><label>CPU</label><div className="v">{fp.cpu_percent.toFixed(1)}<u>%</u></div><div className="hint">{active ? "Profile active" : "Waiting for a game"}</div></div>
          <div><label>Audio chain</label><div className="v">{chain === "bypass" ? "Bypass" : chain === "active" ? "Active" : chain === "notinstalled" || chain === "notloaded" ? "Not audible" : "Exclusive"}</div><div className="hint">{chain === "bypass" ? "Pass-through · 0 ms" : chain === "active" ? "EQ + HRTF" : chain === "notinstalled" ? "Audio effects not installed" : chain === "notloaded" ? "Installed, not loaded by Windows" : "Game bypasses the APO"}</div></div>
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
            onDismissError={() => setError(null)}
          />
        )}
        {(adding === "headset" || editingHs) && (
          <HeadsetDialog key={editingHs?.id ?? "new"} editing={editingHs ?? undefined}
            onClose={() => { setAdding(null); setEditingHs(null); }}
            onSaved={async () => { setAdding(null); setEditingHs(null); await refresh(); }} />
        )}
        {(adding === "monitor" || editingMon) && (
          <MonitorDialog key={editingMon?.id ?? "new"} editing={editingMon ?? undefined}
            onClose={() => { setAdding(null); setEditingMon(null); }}
            onSaved={async () => { setAdding(null); setEditingMon(null); await refresh(); }} />
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
                      onDoubleClick={() => void apply(p.id)}>
                    {/* The name is the control: a real button, so Tab reaches
                        the row and Enter opens it. The rest of the row stays
                        clickable for the mouse. */}
                    <td onClick={() => void startEdit(p.id)}>
                      <button type="button" className="rowname" onClick={() => void startEdit(p.id)}>
                        <b>{p.name}</b><span>{p.note || p.exe}</span>
                      </button>
                    </td>
                    <td onClick={() => void startEdit(p.id)}>{headsetName(p.headset)}</td>
                    <td onClick={() => void startEdit(p.id)}>{monitorName(p.monitor)}</td>
                    <td onClick={() => void startEdit(p.id)}>{shareLabel[p.share]}</td>
                    <td className="r">
                      {active?.id === p.id
                        ? <Pill kind="on" text="Active" />
                        : p.status === "ready" ? <Pill kind="ready" text="Ready" /> : <Pill kind="off" text="Draft" />}
                      {active?.id !== p.id && (
                        <button type="button" className="go" aria-label={`Apply ${p.name} now`}
                          title={`Apply ${p.name} now`}
                          onClick={(e) => { e.stopPropagation(); void apply(p.id); }}>Apply</button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <ErrorNote text={listError} onDismiss={() => setListError(null)} />
        </Card>
      </section>
      <aside className="side">
        <Card title="Headsets & IEMs" action="Add" onAction={() => { setEditingHs(null); setAdding("headset"); }}>
          {hardware.headsets.length === 0 ? (
            <div className="empty">Library is empty</div>
          ) : hardware.headsets.map((h) => (
            <div className="hwl" key={h.id}>
              <div className={hardware.connected.headset === h.id ? "ic r on" : "ic r"}
                data-testid="headset-state" aria-hidden />
              {/* The side panel is narrow: the name gets the full width and
                  the pill and buttons sit on their own line beneath it. */}
              <div className="hwt">
                <b>{h.name}</b>
                <span>{kindLabel[h.kind]}{h.curve ? ` · curve (${h.source || "measured"})` : " · no curve"}</span>
                <div className="hwa">
                  {hardware.connected.headset === h.id && <Pill kind="on" text="Plugged" />}
                  <button type="button" className="ed" aria-label={`Edit ${h.name}`}
                    onClick={() => { setAdding(null); setEditingHs(h); }}>Edit</button>
                  <ConfirmButton className="rm" confirmClassName="rm btn danger"
                    label="Remove" confirm="Remove?"
                    title={`Remove ${h.name} from the library. Profiles that use it stay, but stop matching on it.`}
                    onConfirm={() => void removeHw(h.id)} />
                </div>
              </div>
            </div>
          ))}
        </Card>
        <ListeningCard />
        <Card title="Monitors" action="Add" onAction={() => { setEditingMon(null); setAdding("monitor"); }}>
          {hardware.monitors.length === 0 ? (
            <div className="empty">Library is empty</div>
          ) : hardware.monitors.map((m) => {
            const plugged = hardware.connected.monitors.find((c) => c.id === m.id);
            return (
              <div className="hwl" key={m.id}>
                <div className={plugged ? "ic on" : "ic"} data-testid="monitor-state" aria-hidden />
                <div className="hwt">
                  <b>{m.name}</b>
                  <span>{m.panel || "Panel unknown"}{m.ddcci ? ` · ${ddcControls(m.ddcci)}` : " · controls not scanned"}</span>
                  {m.color && <span className="mono" style={{ fontSize: 11 }}>{colorSummary(m.color)}</span>}
                  <div className="hwa">
                    {plugged && <Pill kind={plugged.primary ? "on" : "ready"} text={plugged.primary ? "Main" : "Second"} />}
                    <button type="button" className="ed" aria-label={`Edit ${m.name}`}
                      onClick={() => { setAdding(null); setEditingMon(m); }}>Edit</button>
                    <ConfirmButton className="rm" confirmClassName="rm btn danger"
                      label="Remove" confirm="Remove?"
                      title={`Remove ${m.name} from the library. Profiles that use it stay, but stop matching on it.`}
                      onConfirm={() => void removeHw(m.id)} />
                  </div>
                </div>
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
          <DoneNote text={scanNote} onDismiss={() => setScanNote(null)} />
          <ErrorNote text={hwError} onDismiss={() => setHwError(null)} />
        </Card>
        <Card>
          <Kv k="Auto-switch" v="By plugged hardware" />
          <Kv k="Default audio" v={hardware.connected.endpoints.find((e) => e.default)?.name ?? "—"} />
          <Kv k="Foreground" v={state.foreground?.exe || "—"} mono />
        </Card>
        <p className="note">Click a row — or tab to it and press Enter — to edit it. Apply, or a double-click, puts it on now. Profiles are matched to whatever headset and monitor are connected, so swapping gear swaps the tuning.</p>
      </aside>
    </>
  );
}

/** One line of what the panel says about itself: gamut, HDR, bit depth.
 *
 *  Coverage is containment of the reference gamut, not an area ratio, so
 *  "97% P3" means the panel really reaches 97% of those colours. Panel
 *  technology is deliberately absent — EDID does not report it, so the
 *  free-text `panel` field beside this is the user's to fill in. */
export function colorSummary(c: ColorInfo): string {
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

/** Search 8,849 measured models and pick one.
 *
 *  The index ships with Relay, so typing is offline and instant. Picking only
 *  fills the dialog; "Add to library" is the one moment Relay reaches the
 *  network: it downloads that model's measurement, caches it under the data
 *  folder, and credits whoever measured it — the licence requires the
 *  credit, and the download-on-demand is why Relay can use this data at all
 *  without redistributing it. */
function CatalogSearch({ onPick }: { onPick: (e: CatalogEntry) => void }) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<CatalogEntry[]>([]);
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
                <button className="btn q" aria-label={`Use ${e.name} measured by ${e.source}`}
                  onClick={() => onPick(e)}>Use</button>
              </div>
            );
          })}
        </div>
      )}
      <p className="p small">
        Measurements come from the <b>AutoEQ</b> project and its contributors (oratory1990,
        crinacle and others). Relay ships only the list of names; the curve itself is downloaded
        when you add the model you picked, cached on this PC, and never redistributed.
      </p>
      <ErrorNote text={error} onDismiss={() => setError(null)} />
    </>
  );
}

/** Turn "Moondrop Blessing 3" into a stable-ish library id. */
function slug(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

/** Add or edit a headset. Adding: pick a measured model from the catalogue
 *  (which fills the form) or describe it yourself, then set what the
 *  catalogue cannot know — where it is plugged in — and press Add. Nothing is
 *  saved or downloaded until then. Editing: rename, change the kind or the
 *  output, or paste a new curve; the stored curve is kept otherwise. The
 *  endpoint binding is what makes it "plugged". */
function HeadsetDialog({ editing, onClose, onSaved }: {
  editing?: Headset;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const { hardware } = useCore();
  const endpoints = hardware.connected.endpoints;
  const [picked, setPicked] = useState<CatalogEntry | null>(null);
  const [name, setName] = useState(editing?.name ?? "");
  const [kind, setKind] = useState<HeadsetKind>(editing?.kind ?? "headphone");
  const [endpoint, setEndpoint] = useState(editing
    ? editing.endpoints[0] ?? ""
    : endpoints.find((e) => e.default)?.key ?? endpoints[0]?.key ?? "");
  const [curveText, setCurveText] = useState("");
  const [source, setSource] = useState(editing?.source ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const valid = name.trim().length > 0 && !busy;

  const pick = (e: CatalogEntry) => {
    setPicked(e);
    setName(e.name);
    setKind(catalogKind(e));
    setSource(e.source);
    setCurveText("");
    setError(null);
  };

  const save = async () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    try {
      if (picked) {
        await api.addHeadsetFromCatalog(picked, endpoint || null, name.trim(), kind);
      } else {
        const h: Headset = {
          id: editing?.id ?? slug(name),
          name: name.trim(),
          kind,
          source: source.trim(),
          endpoints: endpoint ? [endpoint] : [],
          ...(editing?.curve ? { curve: editing.curve } : {}),
        };
        await api.saveHardware({ kind: "headset", value: h });
        if (curveText.trim()) await api.importCurve(h.id, curveText);
      }
      await onSaved();
    } catch (e) { setError(errText(e)); }
    finally { setBusy(false); }
  };

  return (
    <Card title={editing ? "Edit headset" : "Add headset"} action="Cancel" onAction={onClose}>
      <div className="form">
        {!editing && (picked ? (
          <div className="hwl picked" data-testid="catalog-picked">
            <div className="ic r" />
            <div className="hwt">
              <b>{picked.name}</b>
              <span>Measured by {picked.source}{picked.rig ? ` on ${picked.rig}` : ""} · downloaded when you add it</span>
            </div>
            <button type="button" className="btn q" onClick={() => setPicked(null)}>Change</button>
          </div>
        ) : (
          <>
            <CatalogSearch onPick={pick} />
            <div className="hdr" style={{ margin: "6px 0 0" }}>
              <span className="note">Or describe it yourself</span>
            </div>
          </>
        ))}
        <label className="field">
          <span>Name</span>
          <input value={name} placeholder="HD 560S" autoFocus={!!editing} onChange={(e) => setName(e.target.value)} />
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
        {!picked && (
          <>
            <div className="two">
              <label className="field">
                <span>Curve source</span>
                <input value={source} placeholder="oratory1990" onChange={(e) => setSource(e.target.value)} />
              </label>
            </div>
            <label className="field">
              <span>{editing ? "Replace measured curve (AutoEQ CSV, optional)" : "Measured curve (AutoEQ CSV, optional)"}</span>
              <textarea rows={4} className="mono" value={curveText} placeholder={"frequency,raw,…\n20.00,-4.11,…"}
                onChange={(e) => setCurveText(e.target.value)} />
            </label>
            <p className="p small">
              <b>Curve source</b> is who measured it. <b>Measured curve</b> is that measurement: the
              frequency response as an AutoEQ CSV, as squig.link and similar sites export it.
              {editing
                ? editing.curve ? " Leave it empty to keep the current curve." : " This headset has no curve yet."
                : " Leave it empty and use the search above to get both filled in for you."}
            </p>
          </>
        )}
        <ErrorNote text={error} onDismiss={() => setError(null)} />
        <div className="actions">
          <button className="btn acc" disabled={!valid} onClick={() => void save()}>
            {busy ? (picked ? "Downloading…" : "Saving…") : editing ? "Save changes" : "Add to library"}
          </button>
          <button className="btn q" onClick={onClose}>Cancel</button>
        </div>
      </div>
    </Card>
  );
}

/** Panel technologies offered as suggestions; the field stays free text
 *  (EDID does not report the technology, so this is the user's to say). */
export const PANEL_TYPES = [
  "IPS", "Fast IPS", "Nano IPS", "VA", "TN", "WOLED", "QD-OLED",
  "Mini-LED (IPS)", "Mini-LED (VA)",
];

/** Add a monitor: pick a detected panel (EDID identity prefilled) or type
 *  one in. Edit: rename it or set its panel type; the id and what a scan
 *  measured (DDC/CI codes, EDID colour) are kept. */
function MonitorDialog({ editing, onClose, onSaved }: {
  editing?: HardwareMonitor;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const { hardware } = useCore();
  const detected = hardware.connected.monitors;
  const [pick, setPick] = useState(editing?.id ?? detected[0]?.id ?? "");
  const picked = detected.find((m) => m.id === pick);
  const [name, setName] = useState(editing?.name ?? detected[0]?.name ?? "");
  const [panel, setPanel] = useState(editing?.panel ?? "");
  const [error, setError] = useState<string | null>(null);
  const valid = (picked || editing ? true : pick.trim().length > 0) && name.trim().length > 0;

  const save = async () => {
    if (!valid) return;
    const m: HardwareMonitor = { ...editing, id: pick.trim(), name: name.trim(), panel: panel.trim() };
    try {
      await api.saveHardware({ kind: "monitor", value: m });
      await onSaved();
    } catch (e) { setError(errText(e)); }
  };

  return (
    <Card title={editing ? "Edit monitor" : "Add monitor"} action="Cancel" onAction={onClose}>
      <div className="form">
        {editing ? (
          <Kv k="Monitor id" v={editing.id} mono />
        ) : (
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
        )}
        {!picked && !editing && (
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
            <input value={panel} placeholder="Nano IPS" list="relay-panel-types" onChange={(e) => setPanel(e.target.value)} />
            <datalist id="relay-panel-types">
              {PANEL_TYPES.map((t) => <option key={t} value={t} />)}
            </datalist>
          </label>
        </div>
        <p className="p small">The id comes from the monitor's EDID, so it stays the same on any port or GPU output. Windows does not report the panel type, so set it here; DDC/CI controls are filled in by Scan monitor controls{editing ? " and are kept when you save" : ""}.</p>
        <ErrorNote text={error} onDismiss={() => setError(null)} />
        <div className="actions">
          <button className="btn acc" disabled={!valid} onClick={() => void save()}>{editing ? "Save changes" : "Add to library"}</button>
          <button className="btn q" onClick={onClose}>Cancel</button>
        </div>
      </div>
    </Card>
  );
}

/** New / Edit form. Headset and monitor come from the hardware library. */
function ProfileForm({ initial, isNew, error, onSave, onDelete, onCancel, onDismissError }: {
  initial: Profile; isNew: boolean; error: string | null;
  onSave: (p: Profile) => void; onDelete?: (id: string) => void; onCancel: () => void;
  onDismissError: () => void;
}) {
  const { hardware } = useCore();
  const [p, setP] = useState<Profile>(initial);
  const [procs, setProcs] = useState<ProcessInfo[]>([]);

  const loadProcs = async () => {
    try { setProcs(await api.listProcesses()); } catch { setProcs([]); }
  };
  useEffect(() => { void loadProcs(); }, []);

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
        <ErrorNote text={error} onDismiss={onDismissError} />
        <div className="actions">
          <button className="btn acc" disabled={!valid} onClick={submit}>{isNew ? "Create profile" : "Save changes"}</button>
          <button className="btn q" onClick={onCancel}>Cancel</button>
          {onDelete && (
            <ConfirmButton label="Delete…" confirm="Confirm delete" onConfirm={() => onDelete(p.id)} />
          )}
        </div>
      </div>
    </Card>
  );
}
