import type { CoreState } from "../lib/ipc";

export type Screen = "share" | "games" | "audio" | "display" | "profiles" | "settings";

const items: { key: Screen; label: string; round?: boolean }[] = [
  { key: "share", label: "Share" },
  { key: "games", label: "Games", round: true },
  { key: "audio", label: "Audio" },
  { key: "display", label: "Display" },
  { key: "profiles", label: "Profiles" },
  { key: "settings", label: "Settings" },
];

export function Rail({ screen, onNav, state, note }: {
  screen: Screen; onNav: (s: Screen) => void; state: CoreState; note: string;
}) {
  const sharing = state.sharing.kind === "sharing";
  const profile = state.active_profile;
  return (
    <nav className="rail">
      {items.map((it) => (
        <a key={it.key} className={screen === it.key ? "on" : ""} onClick={() => onNav(it.key)}>
          <i className={it.round ? "round" : ""} />{it.label}
        </a>
      ))}
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
