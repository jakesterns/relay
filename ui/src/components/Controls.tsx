import { useEffect, useRef, useState, type ReactNode } from "react";

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
    <div className="kv"><span>{k}</span><strong className={mono ? "m" : ""}>{v}</strong></div>
  );
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
export function Slider({ label, value, min, max, step = 1, format, onChange, disabled, unset }: {
  label: string; value: number; min: number; max: number; step?: number;
  format?: (v: number) => string; onChange?: (v: number) => void; disabled?: boolean;
  unset?: boolean;
}) {
  const pct = ((value - min) / (max - min)) * 100;
  return (
    <div className={"sl" + (disabled ? " dis" : "")}>
      <span>{label}</span>
      <div className="tr" style={{ "--w": `${pct}%` } as React.CSSProperties}>
        {!disabled && (
          <input type="range" min={min} max={max} step={step} value={value} aria-label={label}
            onChange={(e) => onChange?.(Number(e.target.value))} />
        )}
      </div>
      <div className={"val" + (unset ? " unset" : "")}>
        {unset ? "Not set" : (format ? format(value) : String(value))}
      </div>
    </div>
  );
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
