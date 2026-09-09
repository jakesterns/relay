import type { ReactNode } from "react";

export function Card({ title, action, onAction, children }: {
  title?: string; action?: string; onAction?: () => void; children: ReactNode;
}) {
  return (
    <div className="card">
      {title && (
        <h3>{title}{action && <button onClick={onAction}>{action}</button>}</h3>
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

export function Toggle({ on, onChange, label, sub }: {
  on: boolean; onChange?: (v: boolean) => void; label: string; sub?: string;
}) {
  return (
    <div className="tog" onClick={() => onChange?.(!on)}>
      {sub ? <div><b>{label}</b><small>{sub}</small></div> : <span>{label}</span>}
      <div className={"sw" + (on ? " on" : "")} role="switch" aria-checked={on} />
    </div>
  );
}

export function Slider({ label, value, min, max, step = 1, format, onChange, disabled }: {
  label: string; value: number; min: number; max: number; step?: number;
  format?: (v: number) => string; onChange?: (v: number) => void; disabled?: boolean;
}) {
  const pct = ((value - min) / (max - min)) * 100;
  return (
    <div className={"sl" + (disabled ? " dis" : "")}>
      <span>{label}</span>
      <div className="tr" style={{ "--w": `${pct}%` } as React.CSSProperties}>
        {!disabled && (
          <input type="range" min={min} max={max} step={step} value={value}
            onChange={(e) => onChange?.(Number(e.target.value))} />
        )}
      </div>
      <div className="val">{disabled ? "—" : (format ? format(value) : String(value))}</div>
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
        <button key={o.key} className={"chip" + (o.key === value ? " on" : "")} onClick={() => onChange?.(o.key)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Live({ on, text }: { on: boolean; text: string }) {
  return <div className={"live" + (on ? "" : " off")}><i />{text}</div>;
}

export function Pill({ kind, text }: { kind: "on" | "ready" | "off"; text?: string }) {
  return <span className={"pill" + (kind === "off" ? "" : " " + kind)}><i />{text}</span>;
}
