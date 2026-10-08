/**
 * S46: "Learn this game's sound". The goal is asked before any learning
 * starts; changing it later re-derives at once (no relearn); Apply / Relearn /
 * Reset / Import / Export reach the core; an import applies as-is with
 * learning off; and the card always says what it listens to and what it keeps.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import type { GameEqAction } from "../lib/ipc";
import { Games } from "./Games";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Games section="audio" onSection={() => {}} />);
  await settle();
  return { h, eq: () => within(card("Learn this game's sound")) };
}

const actions = (): GameEqAction["kind"][] =>
  tauri.callsOf("game_eq").map((c) => (c.args as { action: GameEqAction }).action.kind);
const profile1 = () => core.profiles.get("1")!;
const curve: [number, number][] = [[20, -4], [100, -4], [1000, 0], [3150, 3], [16000, 0]];

describe("the goal prompt", () => {
  it("asks for the goal before learning starts, then starts it", async () => {
    const { h, eq } = await mount();
    const learn = eq().getByRole("switch", { name: /Learn this game's sound/ });
    expect(learn).toHaveAttribute("aria-checked", "false");
    await h.user.click(learn);
    await settle();
    // Nothing reached the core yet: the question comes first.
    expect(actions().filter((a) => a !== "status")).toEqual([]);
    const prompt = screen.getByRole("group", { name: "Choose a goal" });
    for (const g of ["Awareness", "Dialogue", "Immersion"]) {
      expect(within(prompt).getByRole("button", { name: g })).toBeInTheDocument();
    }
    expect(prompt).toHaveTextContent(/Keep voices clear over effects and music/);
    await h.user.click(within(prompt).getByRole("button", { name: "Dialogue" }));
    await settle();
    expect(actions().filter((a) => a !== "status")).toEqual(["set_goal", "set_learning"]);
    expect(profile1().audio.game_eq_goal).toBe("dialogue");
    expect(profile1().audio.learn_game_eq).toBe(true);
    expect(screen.queryByRole("group", { name: "Choose a goal" })).toBeNull();
    expect(eq().getByRole("button", { name: "Dialogue" })).toHaveAttribute("aria-pressed", "true");
  });

  it("is shown straight away when learning is on by default and no goal is set", async () => {
    profile1().audio.bands = [{ freq_hz: 3000, gain_db: 2, q: 0.9 }];
    const { eq } = await mount();
    expect(eq().getByRole("switch", { name: /Learn this game's sound/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("group", { name: "Choose a goal" })).toBeInTheDocument();
    expect(eq().getByText("Waiting for a goal")).toBeInTheDocument();
  });

  it("changing the goal later re-derives at once, without relearning", async () => {
    const p = profile1();
    p.audio.game_eq_goal = "awareness";
    p.audio.learn_game_eq = true;
    p.audio.game_eq = { curve, source: "learned" };
    core.gameEq.set("1", { progress: 100, candidate: curve, needsRelearn: false, learningNow: false });
    const { h, eq } = await mount();
    expect(eq().getByText("Applied")).toBeInTheDocument();
    await h.user.click(eq().getByRole("button", { name: "Immersion" }));
    await settle();
    expect(actions().filter((a) => a !== "status")).toEqual(["set_goal"]);
    expect(profile1().audio.game_eq_goal).toBe("immersion");
    // Same aggregates, gentler curve, applied straight away.
    expect(profile1().audio.game_eq?.curve[3]).toEqual([3150, 1.5]);
    expect(eq().getByText("Applied")).toBeInTheDocument();
    expect(eq().getByRole("button", { name: "Immersion" })).toHaveAttribute("aria-pressed", "true");
  });
});

describe("learning, applying and files", () => {
  it("shows progress while learning and Apply once a curve is ready", async () => {
    const p = profile1();
    p.audio.game_eq_goal = "awareness";
    p.audio.learn_game_eq = true;
    core.gameEq.set("1", { progress: 42, candidate: null, needsRelearn: false, learningNow: true });
    const { h, eq } = await mount();
    expect(eq().getByRole("progressbar", { name: "Learning progress" })).toHaveAttribute("aria-valuenow", "42");
    expect(eq().getByText(/Learning · 42%/)).toBeInTheDocument();
    // S48: the ETA from the core's event rate, rounded up.
    expect(eq().getByText("Learning · 42% · about 4 min left")).toBeInTheDocument();
    expect(eq().getByRole("button", { name: "Apply" })).toBeDisabled();

    core.gameEq.set("1", { progress: 100, candidate: curve, needsRelearn: false, learningNow: false });
    h.unmount();
    const again = await mount();
    expect(again.eq().getByText(/Ready/)).toBeInTheDocument();
    await again.h.user.click(again.eq().getByRole("button", { name: "Apply" }));
    await settle();
    expect(profile1().audio.game_eq).toEqual({ curve, source: "learned" });
    expect(again.eq().getByText("Applied")).toBeInTheDocument();
  });

  it("an import applies at once, as imported, with learning off", async () => {
    const { h, eq } = await mount();
    await h.user.click(eq().getByRole("button", { name: "Import…" }));
    const text = JSON.stringify({ format: "relay-game-eq", schema: 1, game: { exe: "cod.exe" }, curve });
    await h.user.click(eq().getByRole("textbox", { name: "Game EQ file" }));
    await h.user.paste(text);
    await h.user.click(eq().getByRole("button", { name: "Import" }));
    await settle();
    expect(profile1().audio.game_eq?.source).toBe("imported");
    expect(eq().getByText("Applied (imported)")).toBeInTheDocument();
    const keep = eq().getByRole("switch", { name: /Keep learning to fine-tune for my setup/ });
    expect(keep).toHaveAttribute("aria-checked", "false");
  });

  it("says learning is paused after an import", async () => {
    const { h, eq } = await mount();
    await h.user.click(eq().getByRole("button", { name: "Import…" }));
    await h.user.click(eq().getByRole("textbox", { name: "Game EQ file" }));
    await h.user.paste(JSON.stringify({ format: "relay-game-eq", schema: 1, game: { exe: "cod.exe" }, curve }));
    await h.user.click(eq().getByRole("button", { name: "Import" }));
    await settle();
    expect(eq().getByTestId("game-eq-paused"))
      .toHaveTextContent("Learning paused because you imported this EQ. Turn on to fine-tune.");
  });

  it("exports the offered curve before it is applied", async () => {
    const p = profile1();
    p.audio.game_eq_goal = "awareness";
    p.audio.learn_game_eq = true;
    core.gameEq.set("1", { progress: 100, candidate: curve, needsRelearn: false, learningNow: false });
    const { h, eq } = await mount();
    const btn = eq().getByRole("button", { name: "Export…" });
    expect(btn).toBeEnabled();
    await h.user.click(btn);
    await settle();
    expect(eq().getByTestId("game-eq-exported")).toBeInTheDocument();
    expect(profile1().audio.game_eq).toBeUndefined();
  });

  it("says a learned curve is not audible without the audio effect", async () => {
    profile1().audio.game_eq = { curve, source: "learned" };
    core.state.audio_chain = "notinstalled";
    const { eq } = await mount();
    expect(eq().getByTestId("game-eq-inaudible")).toHaveTextContent(/not audible/);
  });

  it("says what the last step waits on and that commentary is left out", async () => {
    const p = profile1();
    p.audio.game_eq_goal = "awareness";
    p.audio.learn_game_eq = true;
    core.gameEq.set("1", { progress: 90, candidate: null, needsRelearn: false, learningNow: true });
    const real = core.handler;
    tauri.useFakeCore((cmd, args) => {
      const r = real(cmd, args);
      if (cmd === "game_eq") {
        const st = (r as { status: Record<string, unknown> }).status;
        st.convergence = { agreeing: 1, needed: 3, max_delta_db: 0.9 };
        st.excluded = { overlay_voice: 3080, player_chat: 0, cutscene_or_idle: 0, silence: 0, clipped: 0, volume_change: 0 };
      }
      return r;
    });
    const { eq } = await mount();
    expect(eq().getByTestId("game-eq-settling")).toHaveTextContent(/1\/3 checks agree, largest change 0\.9 dB/);
    expect(eq().getByTestId("game-eq-overlay")).toBeInTheDocument();
  });

  it("says an installed but unloaded effect is not audible yet", async () => {
    profile1().audio.game_eq = { curve, source: "learned" };
    core.state.audio_chain = "notloaded";
    const { eq } = await mount();
    expect(eq().getByTestId("game-eq-inaudible")).toHaveTextContent(/installed on this output but\s+Windows has not loaded it/);
  });

  it("a file for another game is refused in plain words", async () => {
    const { h, eq } = await mount();
    await h.user.click(eq().getByRole("button", { name: "Import…" }));
    await h.user.click(eq().getByRole("textbox", { name: "Game EQ file" }));
    await h.user.paste(JSON.stringify({ format: "relay-game-eq", schema: 1, game: { exe: "other.exe" }, curve }));
    await h.user.click(eq().getByRole("button", { name: "Import" }));
    await settle();
    expect(eq().getByText(/this file is for other\.exe/)).toBeInTheDocument();
    expect(profile1().audio.game_eq).toBeUndefined();
  });

  it("Export writes the applied layer and says where", async () => {
    profile1().audio.game_eq = { curve, source: "learned" };
    const { h, eq } = await mount();
    await h.user.click(eq().getByRole("button", { name: "Export…" }));
    await settle();
    expect(eq().getByTestId("game-eq-exported")).toHaveTextContent(/exports\\cod-game-eq\.json/);
  });

  it("Reset asks first, then removes the game layer", async () => {
    profile1().audio.game_eq = { curve, source: "learned" };
    const { h, eq } = await mount();
    await h.user.click(eq().getByRole("button", { name: "Reset" }));
    expect(actions()).not.toContain("reset");
    await h.user.click(eq().getByRole("button", { name: "Remove game EQ" }));
    await settle();
    expect(actions()).toContain("reset");
    expect(profile1().audio.game_eq).toBeUndefined();
  });

  it("says when the time left is not known yet (S48)", async () => {
    const p = profile1();
    p.audio.game_eq_goal = "awareness";
    p.audio.learn_game_eq = true;
    core.gameEqEta = null;
    core.gameEq.set("1", { progress: 3, candidate: null, needsRelearn: false, learningNow: true });
    const { eq } = await mount();
    expect(eq().getByText("Learning · 3% · time left not known yet")).toBeInTheDocument();
  });

  it("offers learning from a recording beside it (S48)", async () => {
    await mount();
    const v = within(card("Learn faster: use a recording"));
    expect(v.getByRole("button", { name: "Learn from a video file…" })).toBeInTheDocument();
    expect(v.getByText(/Local files only/)).toBeInTheDocument();
  });

  it("says what it listens to and that nothing leaves the PC", async () => {
    const { eq } = await mount();
    expect(eq().getByText(/never a recording/)).toBeInTheDocument();
    expect(eq().getByText(/nothing leaves this PC/)).toBeInTheDocument();
  });
});
