/**
 * React context holding the live core state. One poll on mount, then pushed
 * events; falls back to polling every few seconds when the core is offline.
 *
 * It also owns getting the core *running*. Relay is an installed app, so
 * opening the window has to reach live state on its own: the Tauri shell
 * attempts a start as soon as it is up, this context follows that attempt, and
 * `startCore` re-runs it for the retry button. Nothing here ever asks the user
 * to run a command.
 */
import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { api, isTauri, mockHardware, mockProfiles, mockState, onCoreEvents, type CoreState, type HardwareReply, type ProfileSummary } from "./ipc";

/** How long a `notice` stays on screen before it fades out by itself. */
export const NOTICE_MS = 4000;

/** One backend notice, with an id so repeats of the same text still show. */
export interface Notice {
  id: number;
  text: string;
}

/** Where the attempt to get a core running has got to. */
export type CoreStart =
  /** Nothing tried yet, or a core was already there.  */
  | { kind: "idle" }
  /** A start is in flight; the window shows it is working on it. */
  | { kind: "starting" }
  /** It failed. `message` is written for a person and shown verbatim. */
  | { kind: "failed"; message: string };

export interface Core {
  state: CoreState;
  profiles: ProfileSummary[];
  /** Hardware library + connected view (`list_hardware`). */
  hardware: HardwareReply;
  offline: boolean;
  mock: boolean;
  /** The most recent notice, kept for callers that want just the latest. */
  notice: string | null;
  /** Every notice currently on screen, oldest first. */
  notices: Notice[];
  /** Progress of getting a core running. */
  start: CoreStart;
  /** Try (again) to start the core. Safe to call when one is already up. */
  startCore: () => Promise<void>;
  refresh: () => Promise<void>;
}

const Ctx = createContext<Core | null>(null);

export function CoreProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<CoreState>(mockState);
  const [profiles, setProfiles] = useState<ProfileSummary[]>(isTauri() ? [] : mockProfiles);
  const [hardware, setHardware] = useState<HardwareReply>(isTauri() ? { headsets: [], monitors: [], interfaces: [], connected: { endpoints: [], monitors: [], headset: null } } : mockHardware);
  const [offline, setOffline] = useState(isTauri());
  const [notices, setNotices] = useState<Notice[]>([]);
  const [start, setStart] = useState<CoreStart>({ kind: "idle" });
  // Monotonic, so two identical notices are still two entries.
  const nextId = useRef(1);

  const pushNotice = (text: string) => {
    const id = nextId.current++;
    setNotices((n) => [...n, { id, text }]);
    // Each notice expires on its own clock rather than a shared one, so a
    // second notice cannot cut the first one short.
    setTimeout(() => setNotices((n) => n.filter((x) => x.id !== id)), NOTICE_MS);
  };

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

  const startCore = async () => {
    setStart({ kind: "starting" });
    try {
      await api.startCore();
      setStart({ kind: "idle" });
      await refresh();
    } catch (e) {
      // The message is already a finished sentence from `startup::StartError`.
      const message = String((e as { message?: string })?.message ?? e);
      setStart({ kind: "failed", message });
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
      notice: (t) => pushNotice(t),
      offline: () => setOffline(true),
      // The shell attempts a start of its own the moment it comes up; follow
      // that attempt rather than racing it with a second one.
      starting: () => setStart({ kind: "starting" }),
      started: () => { setStart({ kind: "idle" }); void refresh(); },
      startFailed: (message) => setStart({ kind: "failed", message }),
    }).then((u) => { unsub = u; });
    const t = setInterval(() => { if (offline || !isTauri()) void refresh(); }, 4000);
    return () => { unsub(); clearInterval(t); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const notice = notices.length ? notices[notices.length - 1].text : null;

  return (
    <Ctx.Provider value={{ state, profiles, hardware, offline, mock: !isTauri(), notice, notices, start, startCore, refresh }}>
      {children}
    </Ctx.Provider>
  );
}

export function useCore(): Core {
  const c = useContext(Ctx);
  if (!c) throw new Error("useCore outside CoreProvider");
  return c;
}
