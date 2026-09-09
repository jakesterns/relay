import { Card, Kv, Live, Pill } from "../components/Controls";
import { OfflineBanner } from "../components/Offline";
import { useCore } from "../lib/core";
import { api, fmtMb, type ProfileSummary } from "../lib/ipc";

const shareLabel: Record<ProfileSummary["share"], string> = { game: "Game", daw: "DAW", desktop: "Desktop", off: "Off" };

export function Profiles() {
  const { state, profiles, refresh } = useCore();
  const active = state.active_profile;
  const fp = state.footprint;
  const chain = state.audio_chain;

  const apply = async (id: string) => {
    try { await api.applyProfile(id); await refresh(); } catch { /* surfaced via offline banner */ }
  };

  return (
    <>
      <section className="main">
        <div className="hdr">
          <h1>Profiles</h1>
          <Live on={!!active} text={active ? `${active.name} active` : "Nothing active"} />
        </div>
        <OfflineBanner />
        <div className="stat">
          <div><label>Memory</label><div className="v">{fmtMb(fp.rss_bytes)}<u>MB</u></div><div className="hint">Core service only</div></div>
          <div><label>CPU</label><div className="v">{fp.cpu_percent.toFixed(1)}<u>%</u></div><div className="hint">{active ? "Profile active" : "Waiting for a game"}</div></div>
          <div><label>Audio chain</label><div className="v">{chain === "bypass" ? "Bypass" : chain === "active" ? "Active" : "Exclusive"}</div><div className="hint">{chain === "bypass" ? "Pass-through · 0 ms" : chain === "active" ? "EQ + HRTF" : "Game bypasses the APO"}</div></div>
          <div><label>Display</label><div className="v">{state.display_state === "applied" ? "Applied" : "Default"}</div><div className="hint">{state.display_state === "applied" ? "Backup on disk" : "Windows settings"}</div></div>
        </div>
        <Card title="Game profiles" action="New profile">
          {profiles.length === 0 ? (
            <div className="empty">No profiles yet. Add one and it applies the next time that game has focus.</div>
          ) : (
            <table className="tbl">
              <thead>
                <tr><th>Game</th><th>Headset</th><th>Monitor</th><th>Share</th><th /></tr>
              </thead>
              <tbody>
                {profiles.map((p) => (
                  <tr key={p.id} className={active?.id === p.id ? "sel" : ""} onDoubleClick={() => apply(p.id)}>
                    <td><b>{p.name}</b><span>{p.note || p.exe}</span></td>
                    <td>{p.headset ?? "Any"}</td>
                    <td>{p.monitor ?? "Any"}</td>
                    <td>{shareLabel[p.share]}</td>
                    <td className="r">
                      {active?.id === p.id
                        ? <Pill kind="on" text="Active" />
                        : p.status === "ready" ? <Pill kind="ready" text="Ready" /> : <Pill kind="off" text="Draft" />}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Card>
      </section>
      <aside className="side">
        <Card title="Headsets & IEMs" action="Add">
          <div className="empty">Library is empty</div>
        </Card>
        <Card title="Monitors" action="Add">
          <div className="empty">Library is empty</div>
        </Card>
        <Card>
          <Kv k="Auto-switch" v="By plugged hardware" />
          <Kv k="Foreground" v={state.foreground?.exe || "—"} mono />
        </Card>
        <p className="note">Profiles are matched to whatever headset and monitor are connected, so swapping gear swaps the tuning.</p>
      </aside>
    </>
  );
}
