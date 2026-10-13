import { useEffect, useState } from "react";
import { Card, ErrorNote, Kv, Toggle } from "./Controls";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import { api, type NdiLive, type NdiRuntime, type UiPrefs } from "../lib/ipc";

/** Shown while the runtime status is still loading, or from an old core. */
const TRADEMARK = "NDI® is a registered trademark of Vizrt NDI AB.";

/**
 * NDI® output (S51), on Receive and on Share.
 *
 * One saved switch per side, off by default. The running engine follows it
 * live, so turning it on mid-share publishes at once. Release installers
 * bundle NDI's runtime next to Relay (docs/dev/ndi-licensing.md, option B);
 * a build without it falls back to a runtime installed from NDI. When neither
 * is there the card says so and links NDI's own download, and the switch
 * still saves — the next share picks it up once the runtime is there.
 *
 * NDI's licence asks for an ndi.video link and the trademark line near every
 * place NDI is turned on; both sit at the foot of this card.
 */
export function NdiCard({ side, live, sourceName }: {
  side: "receive" | "share";
  /** The engine's `ndi` stats object, while one is running. */
  live: NdiLive | null | undefined;
  /** The name the source will have, for before anything is running. */
  sourceName: string;
}) {
  const { offline, mock } = useCore();
  const key = side === "receive" ? "ndi_receive" : "ndi_share";
  const [prefs, setPrefs] = useState<UiPrefs | null>(null);
  const [rt, setRt] = useState<NdiRuntime | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api.getUiPrefs().then((p) => { if (alive) setPrefs(p); }).catch(() => {});
    api.ndiStatus().then((r) => { if (alive) setRt(r); }).catch(() => { if (alive) setRt(null); });
    return () => { alive = false; };
  }, [offline]);

  const on = !!prefs?.[key];
  const flip = async (v: boolean) => {
    if (!prefs) return;
    setErr(null);
    // The core replaces the whole file with what it is sent, so start from
    // what it holds now: a device pick made in the mixer since this card
    // loaded must not be written back over.
    try { setPrefs(await api.setUiPrefs({ ...(await api.getUiPrefs()), [key]: v })); }
    catch (e) { setErr(errText(e)); }
  };
  const open = (which: "ndi" | "runtime") => {
    api.openNdiLink(which).catch((e) => setErr(errText(e)));
  };

  // The engine's word wins over the file check: it is what actually loaded.
  const missing = live?.runtime_missing || (rt !== null && !rt.present);
  const name = live?.name || sourceName;
  const sub = on
    ? side === "receive"
      ? `Published on this network as "${name}" for OBS, vMix, Studio Monitor and other NDI apps.`
      : `This PC's share is also published as "${name}" on this network.`
    : side === "receive"
      ? "Off. Turn on to use the stream in OBS, vMix or any NDI app on this network, with no capture."
      : "Off. Turn on to publish what you share as an NDI source on this network too.";

  return (
    <Card title="NDI® output">
      <Toggle on={on} onChange={prefs ? (v) => void flip(v) : undefined}
        label={side === "receive" ? "Publish as an NDI source" : "Publish my share as NDI too"} sub={sub} />
      {on && live?.on && (
        <>
          <Kv k="Source" v={live.name} />
          <Kv k="NDI receivers" v={String(live.connections ?? 0)} mono />
          {rt?.present && (
            <Kv k="NDI runtime" v={rt.bundled ? "Included with Relay" : "Installed from NDI"} />
          )}
          {(live.video?.dropped ?? 0) > 0 && (
            <Kv k="Frames skipped" v={String(live.video?.dropped ?? 0)} mono />
          )}
        </>
      )}
      {on && !live?.on && live?.error && !live.runtime_missing && (
        <p className="note" role="status">{live.error}</p>
      )}
      {missing && (
        <p className="note" data-testid="ndi-runtime-missing">
          NDI output needs the NDI runtime, which is missing from this copy of Relay. It is a free install from NDI.{" "}
          <button type="button" className="linkbtn" onClick={() => open("runtime")}>Get the NDI runtime</button>
          {" "}— then start the share again.
        </p>
      )}
      {offline && !mock && <p className="note">Relay is not running — the setting cannot be changed.</p>}
      <ErrorNote text={err} onDismiss={() => setErr(null)} />
      <p className="note">
        Only on this network; nothing on your PC is changed.{" "}
        <button type="button" className="linkbtn" onClick={() => open("ndi")}>ndi.video</button>
        {" · "}{rt?.trademark ?? TRADEMARK}
      </p>
    </Card>
  );
}
