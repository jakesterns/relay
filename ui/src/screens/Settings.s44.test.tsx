/**
 * S44: the audio-effects switch (Windows' protected-audiodg value).
 *
 * Off by default, explained in plain words, confirmed with the exact change
 * listed before Windows is asked, and the APO install stays unavailable
 * until it is on. The "what was changed" line must be true either way.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { AUDIO_EFFECTS_EXPLAINER, Settings } from "./Settings";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Settings />);
  await settle();
  return h;
}

const section = () => card("What Relay installs").querySelector<HTMLElement>(".audio-effects")!;

describe("the audio-effects switch", () => {
  it("is off by default and says nothing was changed", async () => {
    await mount();
    const s = section();
    expect(s.textContent).toContain("Off — Windows protection is on");
    expect(within(s).getByRole("button", { name: "Turn audio effects on" })).toBeEnabled();
    expect(within(s).getByTestId("audio-effects-changed").textContent)
      .toBe("Nothing on your PC was changed by this setting.");
    // Status read only; nothing elevated.
    expect(tauri.lastCall("audio_effects_status")).toBeDefined();
    expect(tauri.lastCall("run_elevated")).toBeUndefined();
  });

  it("explains the trade-off in plain words", async () => {
    await mount();
    expect(AUDIO_EFFECTS_EXPLAINER).toBe(
      "Windows only loads audio effects signed by Microsoft. Relay's per-game EQ isn't, so Windows needs one audio protection turned off for the whole PC. This is what Equalizer APO does. Turning this off again, or uninstalling Relay, puts it back.",
    );
    expect(section().textContent).toContain(AUDIO_EFFECTS_EXPLAINER);
  });

  it("keeps APO installs disabled, with the reason, until it is on", async () => {
    await mount();
    const btn = screen.getByRole("button", { name: "Install on Headphones (USB DAC)" });
    expect(btn).toBeDisabled();
    expect(card("What Relay installs").textContent)
      .toContain("Install is unavailable until audio effects are turned on above");
  });

  it("lists exactly what changes before asking Windows, and asks nothing yet", async () => {
    const h = await mount();
    await h.user.click(within(section()).getByRole("button", { name: "Turn audio effects on" }));
    await settle();
    expect(tauri.lastCall("elevation_plan")?.args).toEqual({
      op: { set_audio_effects_allowed: { on: true, restart_audio: false } },
    });
    const text = section().textContent ?? "";
    expect(text).toContain("DisableProtectedAudioDG: absent → 1");
    expect(text).toContain("takes effect after");
    expect(tauri.lastCall("run_elevated")).toBeUndefined();

    // Ticking the restart box re-plans and names the sound cut.
    await h.user.click(within(section()).getByRole("checkbox", { name: "Restart Windows audio now" }));
    await settle();
    expect(tauri.lastCall("elevation_plan")?.args).toEqual({
      op: { set_audio_effects_allowed: { on: true, restart_audio: true } },
    });
    expect(section().textContent).toContain("2–3 seconds");
  });

  it("turns on through the prompt, then says what WAS changed and unlocks installs", async () => {
    const h = await mount();
    await h.user.click(within(section()).getByRole("button", { name: "Turn audio effects on" }));
    await settle();
    await h.user.click(within(section()).getByRole("button", { name: "Turn on now" }));
    await settle();
    expect(tauri.lastCall("run_elevated")?.args).toEqual({
      op: { set_audio_effects_allowed: { on: true, restart_audio: false } },
    });
    const changed = within(section()).getByTestId("audio-effects-changed").textContent ?? "";
    expect(changed).toContain("Changed on this PC");
    expect(changed).toContain("DisableProtectedAudioDG = 1");
    expect(changed).toContain("Before Relay it was not set");
    expect(screen.getByRole("button", { name: "Install on Headphones (USB DAC)" })).toBeEnabled();
  });

  it("changes nothing when the prompt is declined", async () => {
    core.elevation.decline = true;
    const h = await mount();
    await h.user.click(within(section()).getByRole("button", { name: "Turn audio effects on" }));
    await settle();
    await h.user.click(within(section()).getByRole("button", { name: "Turn on now" }));
    await settle();
    expect(section().textContent).toContain("Nothing on this PC was changed");
    expect(within(section()).getByTestId("audio-effects-changed").textContent)
      .toBe("Nothing on your PC was changed by this setting.");
  });

  it("names another app's setting and offers no toggle for it", async () => {
    core.audioEffects = { value: 1, allowed: true, changed_by_relay: false, set_elsewhere: true, unknown: false };
    await mount();
    const s = section();
    expect(s.textContent).toContain("another app (such as Equalizer APO) already turned this protection off");
    expect(within(s).getByRole("button", { name: "Turn audio effects off" })).toBeDisabled();
    expect(within(s).getByTestId("audio-effects-changed").textContent)
      .toBe("Nothing on your PC was changed by this setting.");
    expect(screen.getByRole("button", { name: "Install on Headphones (USB DAC)" })).toBeEnabled();
  });

  it("turning it off puts back the recorded state", async () => {
    core.audioEffects = { value: 1, allowed: true, changed_by_relay: true, prior: 0, set_elsewhere: false, unknown: false };
    const h = await mount();
    expect(within(section()).getByTestId("audio-effects-changed").textContent).toContain("Before Relay it was 0");
    await h.user.click(within(section()).getByRole("button", { name: "Turn audio effects off" }));
    await settle();
    expect(section().textContent).toContain("goes back to exactly what it was before");
    await h.user.click(within(section()).getByRole("button", { name: "Turn off now" }));
    await settle();
    expect(tauri.lastCall("run_elevated")?.args).toEqual({
      op: { set_audio_effects_allowed: { on: false, restart_audio: false } },
    });
  });
});
