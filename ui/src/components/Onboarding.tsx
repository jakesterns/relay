/**
 * Getting started: the profile offer, the three steps shown where there is
 * nothing to edit yet, and the tip on how Relay learns.
 *
 * Owner's first live setup (2026-10-22): with no profile, the Audio and
 * Games screens were greyed out and nothing said why or what to do; games
 * were never detected. The core now offers a profile when a full-screen app
 * with none comes forward (`CoreState.suggestion`); these components ask,
 * and only the user's click makes anything.
 */
import { useState } from "react";
import { ErrorNote } from "./Controls";
import { useCore } from "../lib/core";
import { errText } from "../lib/err";
import { api, GOALS, newProfile, TOURNAMENT_NOTICE, type Goal, type Suggestion } from "../lib/ipc";
import { offerName, requestNav, setSelected } from "../lib/onboarding";

/** Make a Ready profile for an offered app and select it. Returns its id. */
async function makeFromOffer(s: Suggestion): Promise<string> {
  const p = newProfile(offerName(s), s.exe);
  p.status = "ready";
  await api.saveProfile(p);
  setSelected(p.id);
  return p.id;
}

type Made = { id: string; exe: string; name: string; goal: Goal | null };

/** "<Game> is open — make a profile for it?" A quiet banner, never a modal.
 *  After the click: the goal question, then one line saying what happened. */
export function ProfileOffer() {
  const { state, refresh } = useCore();
  const offer = state.suggestion ?? null;
  const [made, setMade] = useState<Made | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const run = async (f: () => Promise<void>) => {
    setErr(null);
    setBusy(true);
    try { await f(); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };

  const make = (s: Suggestion) => run(async () => {
    const id = await makeFromOffer(s);
    setMade({ id, exe: s.exe, name: offerName(s), goal: null });
    await refresh();
  });
  const decline = (s: Suggestion) => run(async () => {
    await api.declineSuggestion(s.exe);
    await refresh();
  });
  const choose = (m: Made, goal: Goal) => run(async () => {
    await api.gameEq(m.id, { kind: "set_goal", goal });
    await api.gameEq(m.id, { kind: "set_learning", enabled: true });
    // Owner 2026-10-23: one profile setup learns both the sound and the look.
    await api.learnDisplaySet(m.exe, true);
    setMade({ ...m, goal });
  });

  if (made) {
    const label = GOALS.find((g) => g.key === made.goal)?.label;
    return (
      <div className="warnbanner offer" data-testid="profile-offer" role="status">
        <i />
        <div className="msg">
          {made.goal ? (
            <>
              <b>{made.name}</b> has a profile, learning its sound for <b>{label}</b> and its look for your
              monitor. Play with the game in front for 5–15 minutes; nothing changes until you press Apply
              on the Audio and Display screens. Look learning waits while Windows HDR is on.
              <small className="tn" data-testid="tournament-notice">{TOURNAMENT_NOTICE}</small>
            </>
          ) : (
            <div role="group" aria-label="Choose a goal" className="goals">
              <span><b>Profile made for {made.name}.</b> What do you want from this game? Learning starts once you choose.</span>
              {GOALS.map((g) => (
                <button key={g.key} type="button" className="btn q" disabled={busy}
                  aria-label={g.label} onClick={() => void choose(made, g.key)}>
                  <b>{g.label}</b> <small>{g.line}</small>
                </button>
              ))}
            </div>
          )}
          <ErrorNote text={err} onDismiss={() => setErr(null)} />
        </div>
        {made.goal && (
          <button type="button" className="btn q"
            onClick={() => { setMade(null); requestNav({ to: "audio", anchor: "learn-video" }); }}>
            Use a recording
          </button>
        )}
        <button type="button" className="btn q" onClick={() => setMade(null)}>
          {made.goal ? "Done" : "Not now"}
        </button>
      </div>
    );
  }

  if (!offer) return null;
  return (
    <div className="warnbanner offer" data-testid="profile-offer">
      <i />
      <span className="msg">
        <b>{offerName(offer)}</b> is open — make a profile for it? Relay then tunes its sound and look
        while it is in front.
        <ErrorNote text={err} onDismiss={() => setErr(null)} />
      </span>
      <button type="button" className="btn acc" disabled={busy} onClick={() => void make(offer)}>Make profile</button>
      <button type="button" className="btn q" disabled={busy} onClick={() => void decline(offer)}>Not for this game</button>
    </div>
  );
}

/** Three steps, where a screen has no profile to show. Content only: the
 *  caller puts it in a card. */
export function GettingStarted() {
  const { state, profiles, refresh } = useCore();
  const offer = state.suggestion ?? null;
  const [err, setErr] = useState<string | null>(null);
  const has = profiles.length > 0;

  const makeProfile = async () => {
    setErr(null);
    if (!offer) { requestNav({ to: "new-profile" }); return; }
    try { await makeFromOffer(offer); await refresh(); } catch (e) { setErr(errText(e)); }
  };

  return (
    <div className="start" data-testid="getting-started">
      <ol className="steps">
        <li>
          <b>Open your game.</b>
          <span>{offer
            ? <>{offerName(offer)} is open.</>
            : <>Relay notices a full-screen game and offers to make its profile.</>}</span>
        </li>
        <li>
          <b>Make a profile.</b>
          <span>One per game. It applies only while that game is in front.</span>
          <button type="button" className="btn q" onClick={() => void makeProfile()}>
            {offer ? `Make profile for ${offerName(offer)}` : "New profile"}
          </button>
        </li>
        <li>
          <b>Let Relay learn.</b>
          <span>Play for 5–15 minutes with the game in front, or use a recording of your gameplay.</span>
          <button type="button" className="btn q" disabled={!has}
            title={has ? undefined : "Make a profile first"}
            onClick={() => requestNav({ to: "audio", anchor: "learn-video" })}>
            Use a recording
          </button>
        </li>
      </ol>
      <ErrorNote text={err} onDismiss={() => setErr(null)} />
    </div>
  );
}

/** How learning works, in two sentences: the game's sound (Audio) or its
 *  look (Display). */
export function LearnTip({ what = "sound" }: { what?: "sound" | "look" }) {
  return (
    <p className="note tip" data-testid="learn-tip">
      <b>Two ways to learn.</b> Live: play for 5–15 minutes — Relay {what === "sound" ? "listens" : "watches"} only
      while the game is in front, and nothing changes until you press Apply. From a recording: faster than
      real time, with Relay's own recordings listed first.
    </p>
  );
}
