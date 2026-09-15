import { useState } from "react";
import { Rail, type Screen } from "./components/Rail";
import { TitleBar } from "./components/TitleBar";
import { CoreProvider, useCore } from "./lib/core";
import { useDraft } from "./lib/drafts";
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
  // null = unknown yet; true = the first-run consent decision is still due.
  const [firstRun, setFirstRun] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.vdeviceStatus()
      .then((s) => { if (!cancelled) setFirstRun(s.consent === null); })
      .catch(() => { if (!cancelled) setFirstRun(false); }); // offline: don't block the app
    return () => { cancelled = true; };
  }, [offline]);

  const sharing = state.sharing.kind === "sharing";
  const profile = state.active_profile;
  const idle = !sharing && !profile;

  const subtitle = sharing && state.sharing.kind === "sharing"
    ? `Sending to ${state.sharing.peer}`
    : profile ? `${profile.name} · profile active`
    : offline && !mock ? "Core offline"
    : "Idle";

  const note = sharing
    ? "Everything stays on your local network. Nothing on this PC was changed."
    : profile
    ? "Applies only while the game has focus. Restored the moment you alt-tab."
    : "Nothing is applied right now. Your desktop, apps, and audio are exactly as Windows set them.";

  const nav = (s: Screen) => {
    if (s === "audio") { setScreen("games"); setSection("audio"); return; }
    if (s === "display") { setScreen("games"); setSection("display"); return; }
    setScreen(s);
  };
  const railKey: Screen = screen === "games" ? (section === "audio" ? "audio" : section === "display" ? "display" : "games") : screen;

  if (firstRun === true) {
    return (
      <div className="app">
        <TitleBar subtitle="First run" idle />
        <div className="body solo">
          <FirstRun onDone={() => setFirstRun(false)} />
        </div>
      </div>
    );
  }

  return (
    <div className="app">
      <TitleBar subtitle={subtitle} idle={idle} />
      <div className="body three">
        <Rail screen={railKey} onNav={nav} state={state} note={note} unsaved={unsaved} />
        {screen === "share" && <Share />}
        {screen === "receive" && <Receive />}
        {screen === "games" && <Games section={section} onSection={setSection} />}
        {screen === "profiles" && <Profiles />}
        {screen === "settings" && <Settings />}
      </div>
    </div>
  );
}
