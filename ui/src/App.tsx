import { useState } from "react";
import { CrashBanner } from "./components/CrashBanner";
import { Rail, type Screen } from "./components/Rail";
import { TitleBar } from "./components/TitleBar";
import { Toasts } from "./components/Toasts";
import { CoreProvider, useCore } from "./lib/core";
import { useDraft } from "./lib/drafts";
import { takeNav, usePendingNav } from "./lib/onboarding";
import { api } from "./lib/ipc";
import { useEffect } from "react";
import { FirstRun } from "./screens/FirstRun";
import { Games, type Section } from "./screens/Games";
import { Profiles } from "./screens/Profiles";
import { Receive } from "./screens/Receive";
import { Settings } from "./screens/Settings";
import { Share } from "./screens/Share";

export default function App() {
  return (
    <CoreProvider>
      <Shell />
    </CoreProvider>
  );
}

function Shell() {
  const { state, offline, mock } = useCore();
  // An unsaved per-game edit outlives the screen that made it, so the rail is
  // where it has to be visible from.
  const unsaved = useDraft()?.profile.name ?? null;
  const [screen, setScreen] = useState<Screen>("profiles");
  const [section, setSection] = useState<Section>("audio");
  const [fromGames, setFromGames] = useState(false);
  // null = unknown yet; true = the first-run consent decision is still due.
  const [firstRun, setFirstRun] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.vdeviceStatus()
      .then((s) => { if (!cancelled) setFirstRun(s.consent === null); })
      .catch(() => { if (!cancelled) setFirstRun(false); }); // offline: don't block the app
    return () => { cancelled = true; };
  }, [offline]);

  // A getting-started button asked to go somewhere. New profile is taken by
  // the Profiles screen itself (it opens the form); the Audio request is
  // taken here, then the card it names is scrolled into view.
  const navReq = usePendingNav();
  useEffect(() => {
    if (navReq?.to === "new-profile") setScreen("profiles");
    if (navReq?.to === "audio") {
      const r = takeNav("audio");
      setScreen("games");
      setSection("audio");
      setFromGames(false);
      const anchor = r?.anchor;
      if (anchor) {
        setTimeout(() => document.getElementById(anchor)?.scrollIntoView?.({ behavior: "smooth", block: "start" }), 0);
      }
    }
  }, [navReq]);

  const sharing = state.sharing.kind !== "off";
  const profile = state.active_profile;
  const idle = !sharing && !profile;

  const subtitle = state.sharing.kind === "sharing"
    ? `Sending to ${state.sharing.peer}`
    : state.sharing.kind === "reconnecting"
    ? `Reconnecting to ${state.sharing.peer}…`
    : profile ? `${profile.name} · profile active`
    : offline && !mock ? "Not running"
    : "Idle";

  const note = sharing
    ? "Everything stays on your local network. Nothing on this PC was changed."
    : profile
    ? "Applies only while the game has focus. Restored the moment you alt-tab."
    : "Nothing is applied right now. Your desktop, apps, and audio are exactly as Windows set them.";

  const nav = (s: Screen) => {
    setFromGames(s === "games");
    if (s === "audio") { setScreen("games"); setSection("audio"); return; }
    if (s === "display") { setScreen("games"); setSection("display"); return; }
    setScreen(s);
  };
  // Games, Audio and Display all open the per-game editor. The rail marks the
  // entry that was clicked: Games stays lit on whichever section it shows
  // (it used to light Audio, so Games looked like it did nothing).
  const railKey: Screen = screen !== "games" ? screen
    : fromGames ? "games"
    : section === "audio" ? "audio" : section === "display" ? "display" : "games";

  if (firstRun === true) {
    return (
      <div className="app">
        <TitleBar subtitle="First run" idle />
        <div className="body solo">
          <FirstRun onDone={() => setFirstRun(false)} />
        </div>
        <Toasts />
      </div>
    );
  }

  return (
    <div className="app">
      <TitleBar subtitle={subtitle} idle={idle} />
      <CrashBanner />
      <div className="body three">
        <Rail screen={railKey} onNav={nav} state={state} note={note} unsaved={unsaved} />
        {screen === "share" && <Share />}
        {screen === "receive" && <Receive />}
        {screen === "games" && <Games section={section} onSection={setSection} />}
        {screen === "profiles" && <Profiles />}
        {screen === "settings" && <Settings />}
      </div>
      <Toasts />
    </div>
  );
}
