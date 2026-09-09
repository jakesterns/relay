import { useCore } from "../lib/core";

/** Shown when running inside Tauri but the core service is not reachable. */
export function OfflineBanner() {
  const { offline, mock } = useCore();
  if (!offline || mock) return null;
  return (
    <div className="offline">
      <i />
      Core service not running. Start it with <code>relay-core run</code> to see live state.
    </div>
  );
}
