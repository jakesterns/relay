/**
 * React context holding the live core state. One poll on mount, then pushed
 * events; falls back to polling every few seconds when the core is offline.
 */
import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { api, isTauri, mockHardware, mockProfiles, mockState, onCoreEvents, type CoreState, type HardwareReply, type ProfileSummary } from "./ipc";

export interface Core {
  state: CoreState;
  profiles: ProfileSummary[];
  /** Hardware library + connected view (`list_hardware`). */
  hardware: HardwareReply;
  offline: boolean;
  mock: boolean;
  notice: string | null;
  refresh: () => Promise<void>;
}

const Ctx = createContext<Core | null>(null);

export function CoreProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<CoreState>(mockState);
  const [profiles, setProfiles] = useState<ProfileSummary[]>(isTauri() ? [] : mockProfiles);
  const [hardware, setHardware] = useState<HardwareReply>(isTauri() ? { headsets: [], monitors: [], interfaces: [], connected: { endpoints: [], monitors: [], headset: null } } : mockHardware);
  const [offline, setOffline] = useState(isTauri());
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const [s, p, h] = await Promise.all([api.status(), api.listProfiles(), api.listHardware()]);
      setState(s);
      setProfiles(p);
      setHardware(h);
      setOffline(false);
    } catch {
      setOffline(true);
    }
  };

  useEffect(() => {
    void refresh();
    let unsub = () => {};
    void onCoreEvents({
      state: (s) => {
        setState(s);
        // Pushed states carry the live connected view; keep the pills fresh
        // without a round-trip.
        setHardware((h) => ({ ...h, connected: s.hardware }));
        setOffline(false);
      },
      notice: (t) => setNotice(t),
      offline: () => setOffline(true),
    }).then((u) => { unsub = u; });
    const t = setInterval(() => { if (offline || !isTauri()) void refresh(); }, 4000);
    return () => { unsub(); clearInterval(t); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice(null), 4000);
    return () => clearTimeout(t);
  }, [notice]);

  return (
    <Ctx.Provider value={{ state, profiles, hardware, offline, mock: !isTauri(), notice, refresh }}>
      {children}
    </Ctx.Provider>
  );
}

export function useCore(): Core {
  const c = useContext(Ctx);
  if (!c) throw new Error("useCore outside CoreProvider");
  return c;
}
