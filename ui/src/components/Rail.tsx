import type { CoreState } from "../lib/ipc";

export type Screen = "share" | "receive" | "games" | "audio" | "display" | "profiles" | "settings";

const items: { key: Screen; label: string; round?: boolean }[] = [
  { key: "share", label: "Share" },
  { key: "receive", label: "Receive" },
  { key: "games", label: "Games", round: true },
  { key: "audio", label: "Audio" },
  { key: "display", label: "Display" },
  { key: "profiles", label: "Profiles" },
  { key: "settings", label: "Settings" },
];

/** The three entries that lead to the per-game editor, and so to an edit that
 *  might be unsaved. */
const GAME_SCREENS: Screen[] = ["games", "audio", "display"];

export function Rail({ screen, onNav, state, note, unsaved }: {
  screen: Screen; onNav: (s: Screen) => void; state: CoreState; note: string;
  /** Name of the profile with unsaved edits, if there is one. */
  unsaved?: string | null;
}) {
  const sharing = state.sharing.kind === "sharing";
  const profile = state.active_profile;
  return (
    <nav className="rail" aria-label="Screens">
      {items.map((it) => {
        const mark = unsaved && GAME_SCREENS.includes(it.key);
        return (
          <button type="button" key={it.key} className={screen === it.key ? "on" : ""}
            aria-current={screen === it.key ? "page" : undefined}
            onClick={() => onNav(it.key)}>
            <i className={it.round ? "round" : ""} />{it.label}
            {mark && <em title={`${unsaved} has unsaved changes`} aria-label="unsaved changes">•</em>}
          </button>
        );
      })}
      <div className="grp">Now</div>
      <div className={"st" + (sharing ? " on" : "")}>
        <i />Sharing<small>{sharing ? "on" : "off"}</small>
      </div>
      <div className={"st" + (profile ? " on" : "")}>
        <i />Game profile<small>{profile ? shortName(profile.name) : "none"}</small>
      </div>
      <div className="sp" />
      <small>{note}</small>
    </nav>
  );
}

function shortName(name: string): string {
  const words = name.split(/\s+/);
  return words.length > 1 ? words.map((w) => w[0]?.toUpperCase() ?? "").join("").slice(0, 4) : name.slice(0, 8);
}
