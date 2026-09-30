import { useState } from "react";
import { Card, ErrorNote, Pill } from "../components/Controls";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import {
  activeListening, api, listeningKey, sameListening,
  type EndpointInfo, type Headset, type ListeningDevice, type OtherProcessor,
} from "../lib/ipc";

const SPEAKERS: ListeningDevice = { kind: "speakers" };
const SPEAKERS_LABEL = "Speakers / home theater (no correction)";

/** The sentence shown for one other processor on an output. */
export function processingLine(p: OtherProcessor): string {
  if (p.kind === "apo") {
    return `${p.name} is also processing this output. Relay's correction adds to it; ${lowerFirst(p.advice)}.`;
  }
  return `${p.name} is running and may also be processing this output. Relay's correction adds to it; ${p.advice} for accurate correction.`;
}

function lowerFirst(s: string): string {
  return s ? s[0].toLowerCase() + s.slice(1) : s;
}

/**
 * "What are you listening on?" per output (S41). Windows only sees the
 * output (a RODECaster, a DAC, the motherboard jack), not what is plugged
 * into it, so the user lists what each output feeds and marks the one in
 * use. Headphone correction follows the one in use; speakers get none.
 */
export function ListeningCard() {
  const { hardware, refresh } = useCore();
  const [err, setErr] = useState<string | null>(null);
  const eps = hardware.connected.endpoints;
  const lists = hardware.connected.listening ?? [];
  const other = hardware.connected.other_processing ?? [];

  const nameOf = (d: ListeningDevice) =>
    d.kind === "speakers" ? SPEAKERS_LABEL : hardware.headsets.find((h) => h.id === d.id)?.name ?? d.id;

  const run = async (f: () => Promise<void>) => {
    setErr(null);
    try { await f(); await refresh(); }
    catch (e) { setErr(errText(e)); }
  };

  // Default output first; it is the one playing now.
  const ordered = [...eps].sort((a, b) => Number(b.default) - Number(a.default));

  return (
    <Card title="What are you listening on?">
      {ordered.length === 0 ? (
        <div className="empty">No audio outputs found</div>
      ) : ordered.map((ep) => {
        const key = listeningKey(eps, ep);
        const list = lists.find((l) => l.endpoint === key);
        const devices = list?.devices ?? [];
        return (
          <Output key={key} ep={ep} devices={devices} active={activeListening(list)}
            processors={other.find((o) => o.endpoint === key)?.processors ?? []}
            library={hardware.headsets} nameOf={nameOf}
            onSet={(next) => run(() => api.setListeningDevices(key, next))}
            onUse={(d) => run(() => api.setActiveListening(key, d))} />
        );
      })}
      <ErrorNote text={err} onDismiss={() => setErr(null)} />
      <p className="note">
        Processing inside the interface itself — a RODECaster's own EQ, a DAC's filters — comes after
        Relay. Set it flat on that output for accurate correction.
      </p>
      <p className="note">Listing what you listen on changes nothing in Windows.</p>
    </Card>
  );
}

function Output({ ep, devices, active, processors, library, nameOf, onSet, onUse }: {
  ep: EndpointInfo;
  devices: ListeningDevice[];
  active: ListeningDevice | null;
  processors: OtherProcessor[];
  library: Headset[];
  nameOf: (d: ListeningDevice) => string;
  onSet: (next: ListeningDevice[]) => Promise<void>;
  onUse: (d: ListeningDevice) => Promise<void>;
}) {
  const [q, setQ] = useState("");
  const listed = (d: ListeningDevice) => devices.some((x) => sameListening(x, d));
  const needle = q.trim().toLowerCase();
  const matches: ListeningDevice[] = needle
    ? [
      ...library
        .filter((h) => h.name.toLowerCase().includes(needle) || h.id.includes(needle))
        .map((h): ListeningDevice => ({ kind: "headset", id: h.id })),
      ...(SPEAKERS_LABEL.toLowerCase().includes(needle) || "home theater".includes(needle) ? [SPEAKERS] : []),
    ].filter((d) => !listed(d)).slice(0, 6)
    : [];

  return (
    <div className="listen" data-output={ep.name}>
      <div className="hwl">
        <div className="ic" />
        <div><b>{ep.name}</b><span>{ep.default ? "Playing now" : "Output"}</span></div>
        {ep.default && <Pill kind="on" text="Default" />}
      </div>
      {devices.length === 0 ? (
        <p className="note">Nothing listed. Add what is plugged into this output so correction knows what you hear.</p>
      ) : devices.map((d) => {
        const on = sameListening(d, active);
        const label = nameOf(d);
        return (
          <div className="hwl" key={d.kind === "speakers" ? "speakers" : d.id}>
            <div className={"ic" + (d.kind === "headset" ? " r" : "")} />
            <div>
              <b>{label}</b>
              <span>{d.kind === "speakers" ? "No headphone correction" : on ? "Correction follows this" : "Listed"}</span>
            </div>
            {on
              ? <Pill kind="on" text="In use" />
              : <button type="button" className="go" onClick={() => void onUse(d)}
                  aria-label={`Use ${label}`}>Use</button>}
            <button type="button" className="rm" aria-label={`Remove ${label} from ${ep.name}`}
              onClick={() => void onSet(devices.filter((x) => !sameListening(x, d)))}>Remove</button>
          </div>
        );
      })}
      {devices.length > 1 && !active && (
        <p className="note">Pick the one you are using. Until then Relay applies no headphone correction on this output.</p>
      )}
      <div className="field">
        <input type="search" value={q} onChange={(e) => setQ(e.target.value)}
          placeholder="Add from your library…" aria-label={`Add a listening device to ${ep.name}`} />
      </div>
      {matches.length > 0 && (
        <div className="catalog-lite">
          {matches.map((d) => (
            <button type="button" className="btn q" key={d.kind === "speakers" ? "speakers" : d.id}
              onClick={() => { setQ(""); void onSet([...devices, d]); }}>
              Add {nameOf(d)}
            </button>
          ))}
        </div>
      )}
      {needle && matches.length === 0 && (
        <p className="note">Not in your library. Add it under Headsets &amp; IEMs first.</p>
      )}
      {processors.map((p) => (
        <div className="offline" key={p.name + (p.clsid ?? "")}><i />{processingLine(p)}</div>
      ))}
    </div>
  );
}
