import { useEffect, useState } from "react";
import { Card } from "./Controls";
import { errText } from "../lib/err";
import {
  api, fmtMode, presetName, sameMode,
  type DisplayMode, type DisplayOptions, type NvColorFormat, type NvDynamicRange,
  type PanelKind, type Profile, type Recommendation,
} from "../lib/ipc";

/** What the screen says while a mode change happens, both ways. */
export const MODE_BLANK_NOTE =
  "Changing the mode blanks the screen for about a second when the game comes to the front, and again when it leaves.";

const FORMATS: { key: NvColorFormat; label: string }[] = [
  { key: "rgb", label: "RGB" }, { key: "ycbcr444", label: "YCbCr 4:4:4" }, { key: "ycbcr422", label: "YCbCr 4:2:2" },
];
const RANGES: { key: NvDynamicRange; label: string }[] = [
  { key: "full", label: "Full" }, { key: "limited", label: "Limited" }, { key: "auto", label: "Auto" },
];
const DEPTHS = [6, 8, 10, 12];
const PANEL_NAMES: Record<PanelKind, string> = {
  oled: "OLED", ips: "IPS", va: "VA", tn: "TN", unknown: "an unknown panel",
};

const modeKey = (m: DisplayMode) => `${m.width}x${m.height}@${m.refresh_hz}`;

/** A recommendation's value in the words its control uses. */
export function recText(r: Recommendation): string {
  const v = r.value;
  switch (r.field) {
    case "mode": return fmtMode(v as DisplayMode);
    case "hdr": return v ? "On" : "Off";
    case "nvidia_color.format": return FORMATS.find((f) => f.key === v)?.label ?? String(v);
    case "nvidia_color.range": return RANGES.find((f) => f.key === v)?.label ?? String(v);
    case "nvidia_color.bpc": return `${v}-bit`;
    case "color_preset": return presetName(v as number);
    case "shadow_lift": return `+${v}`;
    case "vibrance": return String(v);
    default: return String(v);
  }
}

/** Write one recommendation into the draft profile. Never called except by
 *  the user's own "Use" click. */
export function applyRec(p: Profile, r: Recommendation): void {
  const d = p.display;
  const nv = () => (d.nvidia_color ??= {});
  switch (r.field) {
    case "mode": d.mode = r.value as DisplayMode; break;
    case "hdr": d.hdr = r.value as boolean; break;
    case "nvidia_color.format": nv().format = r.value as NvColorFormat; break;
    case "nvidia_color.range": nv().range = r.value as NvDynamicRange; break;
    case "nvidia_color.bpc": nv().bpc = r.value as number; break;
    case "color_preset": d.monitor.color_preset = r.value as number; break;
    case "shadow_lift": d.gpu.shadow_lift = r.value as number; break;
    case "vibrance": d.gpu.vibrance = r.value as number; break;
  }
}

/** Is the profile already set to what the recommendation says? */
function matches(p: Profile | null, r: Recommendation): boolean {
  if (!p) return false;
  const d = p.display;
  switch (r.field) {
    case "mode": return sameMode(d.mode, r.value as DisplayMode);
    case "hdr": return d.hdr === r.value;
    case "nvidia_color.format": return d.nvidia_color?.format === r.value;
    case "nvidia_color.range": return d.nvidia_color?.range === r.value;
    case "nvidia_color.bpc": return d.nvidia_color?.bpc === r.value;
    case "color_preset": return d.monitor.color_preset === r.value;
    case "shadow_lift": return d.gpu.shadow_lift === r.value;
    case "vibrance": return d.gpu.vibrance === r.value;
    default: return false;
  }
}

function Rec({ field, recs, draft, update }: {
  field: string; recs: Recommendation[]; draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
}) {
  const r = recs.find((x) => x.field === field);
  if (!r) return null;
  const done = matches(draft, r);
  return (
    <p className="note rec" data-testid={`rec-${field}`}>
      Recommended: <b>{recText(r)}</b> — {r.reason}{" "}
      {done ? <span className="m">In use</span> : (
        <button type="button" className="btn q sm" disabled={!draft}
          aria-label={`Use the recommended ${field.replace("nvidia_color.", "")}: ${recText(r)}`}
          onClick={() => update((p) => applyRec(p, r))}>Use</button>
      )}
    </p>
  );
}

/**
 * S52: the Windows and GPU output for this game — display mode, HDR, NVIDIA
 * output colour — and the monitor's own colour preset, each beside its
 * recommendation. What this monitor offers is read from the core when the
 * card opens; nothing changes on screen until the profile is applied (focus),
 * and it is all put back when the game leaves the front.
 */
export function DisplayOutputCard({ draft, update, monitor }: {
  draft: Profile | null;
  update: (fn: (p: Profile) => void) => void;
  /** The profile's monitor, `null` for the main one. */
  monitor: string | null;
}) {
  const [opts, setOpts] = useState<DisplayOptions | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setErr(null);
    api.displayOptions(monitor)
      .then((o) => { if (live) setOpts(o); })
      .catch((e) => { if (live) { setOpts(null); setErr(errText(e)); } });
    return () => { live = false; };
  }, [monitor]);

  const off = !draft;
  const d = draft?.display;
  const recs = opts?.recommendations ?? [];
  const set = (fn: (p: Profile) => void) => update(fn);

  if (!opts) {
    return (
      <Card title="Output & HDR">
        <p className="note">{err ? `Display options are not available: ${err}` : "Reading what this monitor offers…"}</p>
      </Card>
    );
  }
  const nvNow = opts.nvidia_color;
  return (
    <Card title="Output & HDR" action="Reset" onAction={() => set((p) => {
      delete p.display.mode; delete p.display.hdr; delete p.display.nvidia_color; delete p.display.monitor.color_preset;
    })}>
      <div className="dout">
      <p className="note" data-testid="panel-advice">
        Advice for {opts.name}: {PANEL_NAMES[opts.panel_kind]}
        {opts.panel_from_user ? "" : opts.panel_kind === "unknown"
          ? " — set its panel type under Profiles › Monitors for panel-specific advice"
          : " (guessed from the model; set it under Profiles › Monitors if that's wrong)"}.
      </p>

      <label className="field">
        <span>Display mode{opts.current_mode ? ` · now ${fmtMode(opts.current_mode)}` : ""}</span>
        <select aria-label="Display mode" disabled={off || opts.modes.length === 0}
          value={d?.mode ? modeKey(d.mode) : ""}
          onChange={(e) => {
            const m = opts.modes.find((x) => modeKey(x) === e.target.value);
            set((p) => { if (m) p.display.mode = m; else delete p.display.mode; });
          }}>
          <option value="">Leave as it is</option>
          {opts.modes.map((m) => <option key={modeKey(m)} value={modeKey(m)}>{fmtMode(m)}</option>)}
        </select>
      </label>
      {d?.mode && <p className="note" data-testid="mode-blank">{MODE_BLANK_NOTE}</p>}
      <Rec field="mode" recs={recs} draft={draft} update={update} />

      <label className="field">
        <span>HDR{opts.hdr ? ` · now ${opts.hdr.enabled ? "on" : "off"}` : ""}</span>
        <select aria-label="HDR" disabled={off || !opts.hdr_supported}
          value={d?.hdr === undefined ? "" : d.hdr ? "on" : "off"}
          onChange={(e) => {
            const v = e.target.value;
            set((p) => { if (v === "") delete p.display.hdr; else p.display.hdr = v === "on"; });
          }}>
          <option value="">Leave as it is</option>
          <option value="on">On</option>
          <option value="off">Off</option>
        </select>
      </label>
      {!opts.hdr_supported && <p className="note">Windows does not offer HDR on this monitor.</p>}
      <Rec field="hdr" recs={recs} draft={draft} update={update} />

      {opts.vendor === "nvidia" && nvNow ? (
        <>
          <div className="three">
            <label className="field">
              <span>NVIDIA colour format</span>
              <select aria-label="NVIDIA colour format" disabled={off} value={d?.nvidia_color?.format ?? ""}
                onChange={(e) => set((p) => {
                  const v = e.target.value as NvColorFormat | "";
                  const nv = (p.display.nvidia_color ??= {});
                  if (v) nv.format = v; else delete nv.format;
                })}>
                <option value="">Leave ({FORMATS.find((f) => f.key === nvNow.format)?.label ?? "driver"})</option>
                {FORMATS.map((f) => <option key={f.key} value={f.key}>{f.label}</option>)}
              </select>
            </label>
            <label className="field">
              <span>Dynamic range</span>
              <select aria-label="NVIDIA dynamic range" disabled={off} value={d?.nvidia_color?.range ?? ""}
                onChange={(e) => set((p) => {
                  const v = e.target.value as NvDynamicRange | "";
                  const nv = (p.display.nvidia_color ??= {});
                  if (v) nv.range = v; else delete nv.range;
                })}>
                <option value="">Leave ({RANGES.find((f) => f.key === nvNow.range)?.label ?? "driver"})</option>
                {RANGES.map((f) => <option key={f.key} value={f.key}>{f.label}</option>)}
              </select>
            </label>
          <label className="field">
            <span>Output colour depth</span>
            <select aria-label="NVIDIA colour depth" disabled={off} value={d?.nvidia_color?.bpc ?? ""}
              onChange={(e) => set((p) => {
                const v = Number(e.target.value);
                const nv = (p.display.nvidia_color ??= {});
                if (v) nv.bpc = v; else delete nv.bpc;
              })}>
              <option value="">Leave ({nvNow.bpc ? `${nvNow.bpc}-bit` : "driver"})</option>
              {DEPTHS.map((b) => <option key={b} value={b}>{b}-bit</option>)}
            </select>
          </label>
          </div>
          <Rec field="nvidia_color.format" recs={recs} draft={draft} update={update} />
          <Rec field="nvidia_color.range" recs={recs} draft={draft} update={update} />
          <Rec field="nvidia_color.bpc" recs={recs} draft={draft} update={update} />
        </>
      ) : (
        <p className="note" data-testid="nv-unavailable">
          {opts.vendor === "nvidia"
            ? "This NVIDIA driver does not expose output colour, so format, range and depth stay as they are."
            : "Output colour format, range and depth are NVIDIA controls; this monitor is not on an NVIDIA GPU."}
        </p>
      )}

      {opts.color_presets.length > 0 && (
        <>
          <label className="field">
            <span>Monitor colour preset</span>
            <select aria-label="Monitor colour preset" disabled={off} value={d?.monitor.color_preset ?? ""}
              onChange={(e) => set((p) => {
                const v = e.target.value;
                if (v === "") delete p.display.monitor.color_preset; else p.display.monitor.color_preset = Number(v);
              })}>
              <option value="">Leave as it is</option>
              {opts.color_presets.map((v) => <option key={v} value={v}>{presetName(v)}</option>)}
            </select>
          </label>
          <Rec field="color_preset" recs={recs} draft={draft} update={update} />
        </>
      )}

      {(recs.some((r) => r.field === "shadow_lift") || recs.some((r) => r.field === "vibrance")) && (
        <div className="recs" aria-label="Colour advice for this panel">
          <Rec field="shadow_lift" recs={recs} draft={draft} update={update} />
          <Rec field="vibrance" recs={recs} draft={draft} update={update} />
        </div>
      )}
      <p className="note">Recommendations are only suggestions: nothing changes until you press Use and save, and everything here is put back when the game leaves the front.</p>
      </div>
    </Card>
  );
}
