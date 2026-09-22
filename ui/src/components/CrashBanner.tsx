import { useEffect, useState } from "react";
import { useCore } from "../lib/core";
import { api } from "../lib/ipc";

/** One line about the last crash, once (S38).
 *
 *  The core sets `state.last_crash` when it starts and finds a crash record
 *  nobody has seen; it stays until acknowledged, across every screen. Not a
 *  toast: toasts report things that need no answer and go away by
 *  themselves, and a crash is worth one deliberate "OK" so it is not missed.
 *  Not modal either -- it explains, it does not obstruct. */
export function CrashBanner() {
  const { state } = useCore();
  const text = state.last_crash ?? null;
  const [hidden, setHidden] = useState(false);

  // A new sentence (a later crash) shows again even after an earlier OK.
  useEffect(() => { setHidden(false); }, [text]);

  if (!text || hidden) return null;

  const ok = () => {
    setHidden(true);
    void api.ackCrash().catch(() => {});
  };

  return (
    <div className="offline crash" role="status" data-testid="crash-banner">
      <i />
      <span className="msg">
        {text} The details are in <code>logs\crash</code> in Relay's data folder.
      </span>
      <button className="btn tiny" onClick={ok}>OK</button>
    </div>
  );
}
