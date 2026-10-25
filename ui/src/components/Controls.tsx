import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";

export function Card({ title, action, onAction, children }: {
  title?: string; action?: string; onAction?: () => void; children: ReactNode;
}) {
  return (
    <div className="card">
      {title && (
        <h3>{title}{action && <button type="button" onClick={onAction}>{action}</button>}</h3>
      )}
      {children}
    </div>
  );
}

export function Kv({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="kv"><span>{k}</span><strong className={mono ? "m" : ""}>{mono ? breakable(v) : v}</strong></div>
  );
}

/** A path wraps after a separator ("…\Local\" / "Relay"), not mid-word. <wbr>
 *  adds no character, so the copied text is unchanged. */
function breakable(v: string): ReactNode {
  const parts = v.split(/(?<=[\\/])/);
  return parts.length < 2 ? v : parts.map((p, i) => <Fragment key={i}>{p}{i < parts.length - 1 && <wbr />}</Fragment>);
}

/**
 * A switch you can reach with Tab and flip with Space.
 *
 * The whole row is the control, not just the 30px sled: the label and its
 * explanatory line are what the row is about, so they are inside the button
 * and become its accessible name. The sled itself is decoration, hidden from
 * the accessibility tree so the name does not end in a stray "on".
 */
export function Toggle({ on, onChange, label, sub }: {
  on: boolean; onChange?: (v: boolean) => void; label: string; sub?: string;
}) {
  return (
    <button type="button" className="tog" role="switch" aria-checked={on}
      disabled={!onChange} onClick={() => onChange?.(!on)}>
      {sub ? <span className="txt"><b>{label}</b><small>{sub}</small></span> : <span>{label}</span>}
      <div className={"sw" + (on ? " on" : "")} aria-hidden="true" />
    </button>
  );
}

/**
 * A labelled range with its value readout.
 *
 * A disabled slider still prints its value: a setting that is locked, by the
 * panel or by there being no profile open, is still worth reading. `unset` is
 * the one case that prints no number — a field the profile leaves alone, where
 * `value` is only where the thumb rests and printing it would claim a setting
 * nobody chose.
 */
export function Slider({ label, value, min, max, step = 1, format, parse = parseTyped, typeable = true, onChange, disabled, unset }: {
  label: string; value: number; min: number; max: number; step?: number;
  format?: (v: number) => string; onChange?: (v: number) => void; disabled?: boolean;
  unset?: boolean;
  /** Typed text to a value, when `format` shows a scaled number (e.g. %). */
  parse?: (s: string) => number | null;
  /** False where typing a number makes no sense (a list of named levels). */
  typeable?: boolean;
}) {
  // Owner 2026-10-24: dragging felt stiff. While the pointer is down the
  // thumb follows a local value at once, and the caller hears at most one
  // change per frame (its re-render — graphs, tables — no longer sits between
  // the pointer and the thumb). Late updates from the core cannot pull the
  // thumb back mid-drag. Keys and plain change events still report at once.
  const [live, setLive] = useState<number | null>(null);
  const dragging = useRef(false);
  const pending = useRef<number | null>(null);
  const frame = useRef<number | null>(null);
  const flush = () => {
    if (frame.current !== null) { cancelAnimationFrame(frame.current); frame.current = null; }
    if (pending.current !== null) { const v = pending.current; pending.current = null; onChange?.(v); }
  };
  const end = () => {
    if (!dragging.current) return;
    dragging.current = false;
    flush();
    setLive(null);
  };
  useEffect(() => () => { if (frame.current !== null) cancelAnimationFrame(frame.current); }, []);
  const shown = live ?? value;
  const frac = Math.min(1, Math.max(0, (shown - min) / (max - min)));
  // Owner 2026-10-24: type an exact value. Click the number, type, Enter.
  const [typed, setTyped] = useState<string | null>(null);
  const editable = typeable && !!onChange && !disabled && !unset;
  const commit = () => {
    if (typed === null) return;
    const v = parse(typed);
    setTyped(null);
    if (v === null) return;
    const snapped = Math.round((Math.min(max, Math.max(min, v)) - min) / step) * step + min;
    onChange?.(Number(snapped.toFixed(decimalsOf(step))));
  };
  const text = unset ? "Not set" : (format ? format(shown) : String(shown));
  return (
    <div className={"sl" + (disabled ? " dis" : "")}>
      <span>{label}</span>
      <div className="tr" style={{ "--w": `${frac * 100}%`, "--f": frac } as React.CSSProperties}>
        {!disabled && (
          <input type="range" min={min} max={max} step={step} value={shown} aria-label={label}
            aria-valuetext={format ? format(shown) : undefined}
            onPointerDown={() => {
              dragging.current = true;
              // The pointer may be released outside the track.
              window.addEventListener("pointerup", end, { once: true });
              window.addEventListener("pointercancel", end, { once: true });
            }}
            onPointerUp={end} onPointerCancel={end} onLostPointerCapture={end} onBlur={end}
            onChange={(e) => {
              const v = Number(e.target.value);
              if (!dragging.current) { onChange?.(v); return; }
              setLive(v);
              pending.current = v;
              if (frame.current === null) {
                frame.current = requestAnimationFrame(() => { frame.current = null; flush(); });
              }
            }} />
        )}
      </div>
      {editable && typed !== null ? (
        <input className="val num" type="text" inputMode="decimal" autoFocus value={typed}
          aria-label={`${label}, type a value from ${min} to ${max}`}
          onFocus={(e) => e.currentTarget.select()}
          onChange={(e) => setTyped(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") { e.preventDefault(); commit(); }
            else if (e.key === "Escape") { e.preventDefault(); setTyped(null); }
          }} />
      ) : editable ? (
        <button type="button" className="val edit" title="Click to type an exact value"
          aria-label={`${label}: ${text}. Type an exact value`}
          onClick={() => setTyped(format ? text : String(Number(shown.toFixed(decimalsOf(step)))))}>
          {text}
        </button>
      ) : (
        <div className={"val" + (unset ? " unset" : "")}>{text}</div>
      )}
    </div>
  );
}

/** A typed slider value as a number: accepts "+3", "-2.5", "−4 dB", "150%"
 *  (read as typed). `null` when there is no number in it. */
export function parseTyped(s: string): number | null {
  const m = s.replace(/−/g, "-").replace(",", ".").match(/[-+]?\d*\.?\d+/);
  if (!m) return null;
  const v = Number(m[0]);
  return Number.isFinite(v) ? v : null;
}

function decimalsOf(step: number): number {
  const s = String(step);
  return s.includes(".") ? s.length - s.indexOf(".") - 1 : 0;
}

export function Chips<T extends string>({ label, value, options, onChange }: {
  label: string; value: T; options: { key: T; label: string }[]; onChange?: (v: T) => void;
}) {
  return (
    <div className="chips">
      <span>{label}</span>
      {options.map((o) => (
        <button type="button" key={o.key} className={"chip" + (o.key === value ? " on" : "")}
          aria-pressed={o.key === value} onClick={() => onChange?.(o.key)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

/**
 * Chips as a *set*: any number can be lit at once and each one toggles on its
 * own. The caller owns the rules between them (e.g. two desktop sources that
 * exclude each other), so this component only reports which chip was hit.
 */
export function ChipSet<T extends string>({ label, values, options, onToggle }: {
  label: string; values: readonly T[]; options: { key: T; label: string }[];
  onToggle?: (key: T) => void;
}) {
  return (
    <div className="chips">
      <span>{label}</span>
      {options.map((o) => {
        const on = values.includes(o.key);
        return (
          <button type="button" key={o.key} className={"chip" + (on ? " on" : "")} aria-pressed={on}
            onClick={() => onToggle?.(o.key)}>
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/** "This profile is tuned for HDR", next to its name wherever a profile is
 *  picked, so an HDR tune is never chosen by surprise. */
export function HdrTag() {
  return <span className="tag hdr" title="Tuned for HDR: this profile uses HDR, or its game has a learned HDR look">HDR</span>;
}

export function Live({ on, text }: { on: boolean; text: string }) {
  return <div className={"live" + (on ? "" : " off")}><i />{text}</div>;
}

export function Pill({ kind, text }: { kind: "on" | "ready" | "off"; text?: string }) {
  return <span className={"pill" + (kind === "off" ? "" : " " + kind)}><i />{text}</span>;
}

/** How long an armed confirmation waits before giving up, in ms. */
export const CONFIRM_MS = 4000;

/**
 * The one way Relay asks "are you sure".
 *
 * Two presses of the same button: the first arms it and renames it, the
 * second does the thing. It disarms itself after {@link CONFIRM_MS}, on Escape,
 * and when anything else takes focus — so a stray press cannot leave a live
 * delete button sitting under the cursor.
 *
 * No `window.confirm`. A native dialog in a window with custom chrome looks
 * like it belongs to another program, steals focus from the webview, and on
 * Windows cannot be dismissed by clicking away from it.
 */
export function ConfirmButton({ label, confirm, onConfirm, className = "btn q", confirmClassName = "btn danger", disabled, title }: {
  /** Resting label. */
  label: string;
  /** Label once armed. Say what will happen, not "OK". */
  confirm: string;
  onConfirm: () => void;
  className?: string;
  confirmClassName?: string;
  disabled?: boolean;
  title?: string;
}) {
  const [armed, setArmed] = useState(false);
  const ref = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), CONFIRM_MS);
    return () => clearTimeout(t);
  }, [armed]);

  // Disarm if the button goes away mid-countdown (the row was removed, the
  // form closed), so it cannot come back armed.
  useEffect(() => () => setArmed(false), []);

  return (
    <button
      ref={ref}
      type="button"
      className={armed ? confirmClassName : className}
      title={title}
      disabled={disabled}
      aria-live="polite"
      onKeyDown={(e) => { if (e.key === "Escape" && armed) { e.stopPropagation(); setArmed(false); } }}
      onBlur={() => setArmed(false)}
      onClick={() => {
        if (armed) { setArmed(false); onConfirm(); }
        else setArmed(true);
      }}>
      {armed ? confirm : label}
    </button>
  );
}

/**
 * A failure, next to the control that caused it, that the user can put away.
 *
 * Errors used to be permanent until the next action replaced them, which left
 * the screen claiming something was broken long after it had been fixed.
 */
export function ErrorNote({ text, onDismiss }: { text: string | null; onDismiss?: () => void }) {
  if (!text) return null;
  return (
    <div className="offline err" role="alert">
      <i />
      <span className="msg">{text}</span>
      {onDismiss && (
        <button type="button" className="x" aria-label="Dismiss this message" onClick={onDismiss}>×</button>
      )}
    </div>
  );
}

/** A success line with the same shape as {@link ErrorNote}, so a thing that
 *  worked is as visible as a thing that did not. */
export function DoneNote({ text, onDismiss }: { text: string | null; onDismiss?: () => void }) {
  if (!text) return null;
  return (
    <div className="offline done" role="status">
      <i />
      <span className="msg">{text}</span>
      {onDismiss && (
        <button type="button" className="x" aria-label="Dismiss this message" onClick={onDismiss}>×</button>
      )}
    </div>
  );
}
