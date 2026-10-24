/**
 * S49: the learned dynamic stage on "Learn this game's sound". Shown only
 * when the applied layer has one; the switch and the two strengths reach the
 * core as `set_dynamics`; a slider drag is one change, not one per step; and
 * the card says what the stage does in dB.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, waitFor, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import type { DynamicsParams, GameEqAction } from "../lib/ipc";
import { Games } from "./Games";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

const curve: [number, number][] = [[20, -4], [100, -4], [1000, 0], [3150, 3], [16000, 0]];
const stage = (withLift = true): DynamicsParams => ({
  bands: [
    { mode: "tame", freq_hz: 1000, q: 0.6, threshold_db: -44, ratio: 6, range_db: 9 },
    ...(withLift
      ? [{ mode: "lift" as const, freq_hz: 4000, q: 1, threshold_db: -50, ratio: 2.5, range_db: 6, floor_db: -58 }]
      : []),
  ],
  duck_lift: true,
});

async function mount() {
  const h = renderScreen(<Games section="audio" onSection={() => {}} />);
  await settle();
  return { h, eq: () => within(card("Learn this game's sound")) };
}

const tunes = () =>
  tauri.callsOf("game_eq")
    .map((c) => (c.args as { action: GameEqAction }).action)
    .filter((a): a is Extract<GameEqAction, { kind: "set_dynamics" }> => a.kind === "set_dynamics");
const layer = () => core.profiles.get("1")!.audio.game_eq!;

describe("gunshots, explosions and footsteps (owner 2026-10-24)", () => {
  const group = (eq: () => ReturnType<typeof within>) => within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
  const boom: DynamicsParams = {
    bands: [{ mode: "tame", freq_hz: 63, q: 0.7, threshold_db: -54, ratio: 6, range_db: 10 }],
    duck_lift: true,
  };

  it("is not offered until there is a game layer", async () => {
    const { eq } = await mount();
    expect(eq().queryByRole("group", { name: "Loud bursts and footsteps" })).toBeNull();
  });

  it("starts each part where the learned stage put it, and says what it does", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: stage() };
    const { eq } = await mount();
    const g = group(eq);
    // Learned gun band at 1 kHz cutting 9 dB; footstep lift 6 dB; no explosion band.
    expect(within(g.getByTestId("dyn-gun")).getByText("−9 dB")).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-gun")).getByText(/down by up to 9\.0 dB.*As learned for this game/)).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-steps")).getByText("+6 dB")).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-boom")).getByText("0 dB")).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-boom")).getByText(/exactly as the game mixes them/)).toBeInTheDocument();
  });

  it("takes gunshots to near silent, one change per drag", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: boom };
    const { eq } = await mount();
    const s = group(eq).getByRole("slider", { name: "Gunshots" });
    for (const v of ["-6", "-15", "-24"]) fireEvent.change(s, { target: { value: v } });
    expect(group(eq).getByText("−24 dB")).toBeInTheDocument();
    expect(within(group(eq).getByTestId("dyn-gun")).getByText("Near silent.")).toBeInTheDocument();
    await waitFor(() => expect(tunes()).toEqual([{ kind: "set_dynamics", gun_db: -24 }]));
    await settle();
    expect(layer().dynamics_tuning?.gun_db).toBe(-24);
    const gun = within(group(eq).getByTestId("dyn-gun"));
    await waitFor(() => expect(gun.getByText(/centred on 3\.0 kHz.*down by up to 24\.0 dB.*centred on 500 Hz.*down by up to 24\.0 dB/)).toBeInTheDocument());
    expect(gun.getByText(/safe default/)).toBeInTheDocument();
    // Explosions keep their learned 10 dB.
    expect(within(group(eq).getByTestId("dyn-boom")).getByText(/centred on 63 Hz.*down by up to 10\.0 dB/)).toBeInTheDocument();
  });

  it("makes a part louder too", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: boom };
    const { eq } = await mount();
    fireEvent.change(group(eq).getByRole("slider", { name: "Explosions" }), { target: { value: "6" } });
    await waitFor(() => expect(tunes()).toEqual([{ kind: "set_dynamics", boom_db: 6 }]));
    await settle();
    await waitFor(() => expect(within(group(eq).getByTestId("dyn-boom")).getByText(/Raises .*centred on 63 Hz.* by up to 6\.0 dB/)).toBeInTheDocument());
    fireEvent.change(group(eq).getByRole("slider", { name: "Footsteps" }), { target: { value: "8" } });
    await waitFor(() => expect(tunes().at(-1)).toEqual({ kind: "set_dynamics", steps_db: 8 }));
  });

  it("works on a layer with nothing dynamic learned, and turns off and on", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned" };
    const { h, eq } = await mount();
    fireEvent.change(group(eq).getByRole("slider", { name: "Footsteps" }), { target: { value: "-10" } });
    await waitFor(() => expect(tunes()).toEqual([{ kind: "set_dynamics", steps_db: -10 }]));
    await settle();
    await waitFor(() => expect(within(group(eq).getByTestId("dyn-steps")).getByText(/down by up to 10\.0 dB/)).toBeInTheDocument());
    await h.user.click(group(eq).getByRole("switch", { name: /Gunshots, explosions and footsteps/ }));
    await settle();
    expect(tunes().at(-1)).toEqual({ kind: "set_dynamics", enabled: false });
    await waitFor(() => expect(group(eq).queryByRole("slider", { name: "Gunshots" })).toBeNull());
    expect(group(eq).getByText(/levels are kept for when you turn it back on/)).toBeInTheDocument();
  });
});
