/**
 * React context holding the live core state. One poll on mount, then pushed
 * events; falls back to polling every few seconds when the core is offline.
 */
import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { api, isTauri, mockProfiles, mockState, onCoreEvents, type CoreState, type ProfileSummary } from "./ipc";

export interface Core {
  state: CoreState;
  profiles: ProfileSummary[];
  offline: boolean;
  mock: boolean;
  notice: string | null;
  refresh: () => Promise<void>;
}

const Ctx = createContext<Core | null>(null);

export function CoreProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<CoreState>(mockState);
  const [profiles, setProfiles] = useState<ProfileSummary[]>(isTauri() ? [] : mockProfiles);
  const [offline, setOffline] = useState(isTauri());
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const [s, p] = await Promise.all([api.status(), api.listProfiles()]);
      setState(s);
      setProfiles(p);
      setOffline(false);
    } catch {
      setOffline(true);
    }
  };

  useEffect(() => {
    void refresh();
    let unsub = () => {};
    void onCoreEvents({
      state: (s) => { setState(s); setOffline(false); },
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
    <Ctx.Provider value={{ state, profiles, offline, mock: !isTauri(), notice, refresh }}>
      {children}
    </Ctx.Provider>
  );
}

export function useCore(): Core {
  const c = useContext(Ctx);
  if (!c) throw new Error("useCore outside CoreProvider");
  return c;
}
