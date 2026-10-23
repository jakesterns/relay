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
  it("is not offered until a layer has one", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned" };
    const { eq } = await mount();
    expect(eq().queryByRole("group", { name: "Loud bursts and footsteps" })).toBeNull();
  });

  it("says what it does and turns off and on", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: stage() };
    const { h, eq } = await mount();
    const g = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    expect(g.getByText(/guns and explosions down by up to 9 dB, footsteps up by up to 6 dB/)).toBeInTheDocument();
    const sw = g.getByRole("switch", { name: /Tame loud bursts, lift quiet footsteps/ });
    expect(sw).toHaveAttribute("aria-checked", "true");
    await h.user.click(sw);
    await settle();
    expect(tunes()).toEqual([{ kind: "set_dynamics", enabled: false }]);
    expect(layer().dynamics_tuning?.enabled).toBe(false);
    const g2 = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    expect(g2.getByText(/Off: the learned curve still applies/)).toBeInTheDocument();
    // Strengths are locked while it is off.
    expect(g2.queryByRole("slider", { name: "Gunshots & explosions" })).toBeNull();
  });

  it("sends one change per drag, scaled and capped as the core caps it", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: stage() };
    const { eq } = await mount();
    const g = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    const guns = g.getByRole("slider", { name: "Gunshots & explosions" });
    for (const v of ["1.1", "1.3", "1.5"]) fireEvent.change(guns, { target: { value: v } });
    expect(g.getByText("150%")).toBeInTheDocument();
    await waitFor(() => expect(tunes()).toEqual([{ kind: "set_dynamics", tame: 1.5 }]));
    await settle();
    expect(layer().dynamics_tuning?.tame).toBe(1.5);
    // 9 dB x 1.5 = 13.5, capped at 12.
    await waitFor(() => expect(eq().getByText(/down by up to 12 dB/)).toBeInTheDocument());

    const steps = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }))
      .getByRole("slider", { name: "Footsteps" });
    fireEvent.change(steps, { target: { value: "0.5" } });
    await waitFor(() => expect(tunes().at(-1)).toEqual({ kind: "set_dynamics", lift: 0.5 }));
    await settle();
    expect(layer().dynamics_tuning?.lift).toBe(0.5);
    await waitFor(() => expect(eq().getByText(/footsteps up by up to 3 dB/)).toBeInTheDocument());
  });

  it("hides the footstep strength when the stage only tames", async () => {
    core.profiles.get("1")!.audio.game_eq = { curve, source: "learned", dynamics: stage(false) };
    const { eq } = await mount();
    const g = within(eq().getByRole("group", { name: "Loud bursts and footsteps" }));
    expect(g.getByRole("slider", { name: "Gunshots & explosions" })).toBeInTheDocument();
    expect(g.queryByRole("slider", { name: "Footsteps" })).toBeNull();
    expect(g.getByText(/down by up to 9 dB\./)).toBeInTheDocument();
  });
});
