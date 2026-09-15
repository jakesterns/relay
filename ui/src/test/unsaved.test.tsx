/**
 * An unsaved per-game edit must survive the two things that used to discard
 * it without a word: leaving the screen, and the focused game changing
 * underneath you.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, screen, within } from "@testing-library/react";
import { push, renderApp, settle } from "./render";
import { card, slider } from "./dom";
import { makeFakeCore, type FakeCore } from "./fakeCore";
import * as tauri from "./tauriMock";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

const rail = () => within(document.querySelector(".rail") as HTMLElement);
const heading = () => screen.getByRole("heading", { level: 1 }).textContent ?? "";

async function mount() {
  const h = renderApp();
  await settle();
  await h.user.click(rail().getByRole("button", { name: /^Audio/ }));
  await settle();
  return h;
}

/** Nudge one EQ band, which is the cheapest way to make the draft dirty. */
function edit(db = 4) {
  const s = slider("Sub · 60 Hz", card("Bands"));
  if (!s.input) throw new Error("the Sub slider is disabled");
  fireEvent.change(s.input, { target: { value: String(db) } });
}

const saveButton = () => screen.getByRole("button", { name: /^Save to profile$|^Saved$/ });

describe("an unsaved edit", () => {
  it("is still there after leaving the screen and coming back", async () => {
    const h = await mount();
    edit(4);
    expect(slider("Sub · 60 Hz", card("Bands")).value).toBe("+4.0 dB");
    expect(saveButton()).toBeEnabled();

    await h.user.click(rail().getByRole("button", { name: /^Profiles/ }));
    await settle();
    expect(heading()).toBe("Profiles");

    await h.user.click(rail().getByRole("button", { name: /^Audio/ }));
    await settle();
    expect(slider("Sub · 60 Hz", card("Bands")).value).toBe("+4.0 dB");
    expect(saveButton()).toBeEnabled();
    // Nothing reached the core: it is unsaved, not quietly saved.
    expect(tauri.lastCall("save_profile")).toBeUndefined();
  });

  it("is advertised in the rail, so it cannot be forgotten from another screen", async () => {
    const h = await mount();
    expect(rail().queryByLabelText("unsaved changes")).not.toBeInTheDocument();
    edit(3);
    expect(rail().getAllByLabelText("unsaved changes").length).toBeGreaterThan(0);

    await h.user.click(saveButton());
    await settle();
    expect(rail().queryByLabelText("unsaved changes")).not.toBeInTheDocument();
  });

  it("survives the focused game changing, and says which profile it belongs to", async () => {
    await mount();
    expect(heading()).toContain("Call of Duty");
    edit(6);

    // Alt-tab into another game: the core pushes a new active profile.
    core.state.active_profile = {
      id: "2", name: "Valorant", note: "", exe: "valorant.exe",
      headset: "hd560s", monitor: null, share: "game", status: "ready",
    };
    await push(() => tauri.emit("core://state", structuredClone(core.state)));
    await settle();

    // Still editing Call of Duty, with the change intact, and told why.
    expect(heading()).toContain("Call of Duty");
    expect(slider("Sub · 60 Hz", card("Bands")).value).toBe("+6.0 dB");
    const banner = document.querySelector(".warnbanner") as HTMLElement;
    expect(banner).toHaveTextContent(/Call of Duty.*unsaved changes/);
    expect(banner).toHaveTextContent(/Valorant is in focus/);
  });

  it("can be saved from that banner, after which the focused game takes over", async () => {
    const h = await mount();
    edit(6);
    core.state.active_profile = {
      id: "2", name: "Valorant", note: "", exe: "valorant.exe",
      headset: "hd560s", monitor: null, share: "game", status: "ready",
    };
    await push(() => tauri.emit("core://state", structuredClone(core.state)));
    await settle();

    await h.user.click(screen.getByRole("button", { name: "Save Call of Duty" }));
    await settle();

    expect(core.profiles.get("1")?.audio.bands).toEqual([{ freq_hz: 60, gain_db: 6, q: 0.9 }]);
    expect(heading()).toContain("Valorant");
    expect(document.querySelector(".warnbanner")).toBeNull();
  });

  it("is only discarded by asking twice", async () => {
    const h = await mount();
    edit(6);
    core.state.active_profile = {
      id: "2", name: "Valorant", note: "", exe: "valorant.exe",
      headset: "hd560s", monitor: null, share: "game", status: "ready",
    };
    await push(() => tauri.emit("core://state", structuredClone(core.state)));
    await settle();

    await h.user.click(screen.getByRole("button", { name: "Discard…" }));
    await settle();
    // One press only arms it; the edit is still on screen.
    expect(slider("Sub · 60 Hz", card("Bands")).value).toBe("+6.0 dB");

    await h.user.click(screen.getByRole("button", { name: "Discard changes" }));
    await settle();

    expect(heading()).toContain("Valorant");
    expect(core.profiles.get("1")?.audio.bands).toEqual([]);
    expect(document.querySelector(".warnbanner")).toBeNull();
  });

  it("reports a refused save rather than dropping it", async () => {
    core.fail.set("save_profile", "profiles.json is read-only");
    tauri.useFakeCore(core.handler);
    const h = await mount();
    edit(2);
    await h.user.click(saveButton());
    await settle();

    expect(screen.getByRole("alert")).toHaveTextContent("profiles.json is read-only");
    // And the edit is still there to try again with.
    expect(slider("Sub · 60 Hz", card("Bands")).value).toBe("+2.0 dB");
  });
});
