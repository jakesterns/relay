import { isTauri } from "../lib/ipc";

async function win(action: "min" | "max" | "close") {
  if (!isTauri()) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const w = getCurrentWindow();
  if (action === "min") await w.minimize();
  else if (action === "max") await w.toggleMaximize();
  else await w.close();
}

export function TitleBar({ subtitle, idle }: { subtitle: string; idle: boolean }) {
  return (
    <div className="title" data-tauri-drag-region>
      <div className="brand" data-tauri-drag-region>
        <i className={idle ? "idle" : ""} />
        Relay <span>{subtitle}</span>
      </div>
      <div className="win">
        <button aria-label="Minimize" onClick={() => win("min")}>
          <svg viewBox="0 0 10 10"><path d="M1 7h8" /></svg>
        </button>
        <button aria-label="Maximize" onClick={() => win("max")}>
          <svg viewBox="0 0 10 10"><rect x="1" y="1" width="8" height="8" rx="1" /></svg>
        </button>
        <button aria-label="Close" className="x" onClick={() => win("close")}>
          <svg viewBox="0 0 10 10"><path d="M1 1l8 8M9 1l-8 8" /></svg>
        </button>
      </div>
    </div>
  );
}
