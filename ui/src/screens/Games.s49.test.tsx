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

describe("the dynamic stage", () => {
  it("is not offered until there is a game layer", async () => {
    const { eq } = await mount();
    expect(eq().queryByRole("group", { name: "Loud bursts and footsteps" })).toBeNull();
  });

  it("says exactly what each part turns up or down, and turns off and on", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: stage() };
    const { h, eq } = await mount();
    const g = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    expect(within(g.getByTestId("dyn-gun")).getByText(/Turns .* down by up to 9\.0 dB, only when that range gets 6 dB or more above the game's average loudness \(ratio 6:1/)).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-gun")).getByText(/Learned from this game/)).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-steps")).getByText(/Raises .*centred on 4\.0 kHz.* by up to 6\.0 dB while it is quiet/)).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-boom")).getByText(/Not learned for this game/)).toBeInTheDocument();
    const sw = g.getByRole("switch", { name: /Tame loud bursts, lift quiet footsteps/ });
    expect(sw).toHaveAttribute("aria-checked", "true");
    await h.user.click(sw);
    await settle();
    expect(tunes()).toEqual([{ kind: "set_dynamics", enabled: false }]);
    expect(layer().dynamics_tuning?.enabled).toBe(false);
    const g2 = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    expect(g2.getByText(/Off: the learned EQ curve still applies/)).toBeInTheDocument();
    // Strengths are locked while it is off.
    expect(g2.queryByRole("slider", { name: "Gunshots" })).toBeNull();
  });

  it("sends one change per drag, scaled and capped as the core caps it", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: stage() };
    const { eq } = await mount();
    const g = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    const guns = g.getByRole("slider", { name: "Gunshots" });
    for (const v of ["1.1", "1.3", "1.5"]) fireEvent.change(guns, { target: { value: v } });
    expect(g.getByText("150%")).toBeInTheDocument();
    await waitFor(() => expect(tunes()).toEqual([{ kind: "set_dynamics", gun: 1.5 }]));
    await settle();
    expect(layer().dynamics_tuning?.gun).toBe(1.5);
    // 9 dB x 1.5 = 13.5, capped at 12.
    await waitFor(() => expect(eq().getByText(/down by up to 12\.0 dB/)).toBeInTheDocument());

    const steps = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }))
      .getByRole("slider", { name: "Footsteps" });
    fireEvent.change(steps, { target: { value: "0.5" } });
    await waitFor(() => expect(tunes().at(-1)).toEqual({ kind: "set_dynamics", lift: 0.5 }));
    await settle();
    expect(layer().dynamics_tuning?.lift).toBe(0.5);
    await waitFor(() => expect(eq().getByText(/by up to 3\.0 dB while it is quiet/)).toBeInTheDocument());
  });

  it("adds a gunfire band when the game learned none (MW4 on the main PC)", async () => {
    const boom: DynamicsParams = {
      bands: [{ mode: "tame", freq_hz: 63, q: 0.7, threshold_db: -54, ratio: 6, range_db: 10 }],
      duck_lift: true,
    };
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: boom };
    const { eq } = await mount();
    const g = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    // Nothing on guns yet, and it says so.
    expect(within(g.getByTestId("dyn-gun")).getByText("0%")).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-gun")).getByText(/gunshots are not touched/)).toBeInTheDocument();
    expect(within(g.getByTestId("dyn-boom")).getByText(/centred on 63 Hz.*down by up to 10\.0 dB/)).toBeInTheDocument();
    // No footstep band was learned: its slider is locked.
    expect(within(g.getByTestId("dyn-steps")).queryByRole("slider")).toBeNull();

    fireEvent.change(g.getByRole("slider", { name: "Gunshots" }), { target: { value: "1" } });
    await waitFor(() => expect(tunes()).toEqual([{ kind: "set_dynamics", gun: 1 }]));
    await settle();
    const gun = within(within(eq().getByRole("group", { name: "Loud bursts and footsteps" })).getByTestId("dyn-gun"));
    await waitFor(() => expect(gun.getByText(/centred on 3\.0 kHz.*down by up to 6\.0 dB, only when that range gets 0 dB or more above/)).toBeInTheDocument());
    expect(gun.getByText(/Added by you/)).toBeInTheDocument();
  });

  it("works on a layer with no learned stage at all", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned" };
    const { eq } = await mount();
    const g = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    fireEvent.change(g.getByRole("slider", { name: "Gunshots" }), { target: { value: "2" } });
    await waitFor(() => expect(tunes()).toEqual([{ kind: "set_dynamics", gun: 2 }]));
    await settle();
    await waitFor(() => expect(eq().getByText(/down by up to 12\.0 dB/)).toBeInTheDocument());
  });
});
