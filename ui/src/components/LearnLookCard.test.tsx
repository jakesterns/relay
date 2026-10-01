/**
 * S47: the "Learn this game's look" card. Each control reaches the core with
 * the right command; the privacy line and the owner's tournament notice are
 * always shown; an import reads "Applied (imported)" with learning off.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import type { LearnView, MonitorLearnView } from "../lib/ipc";
import { TOURNAMENT_NOTICE } from "../lib/ipc";
import { LearnLookCard } from "./LearnLookCard";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount(exe: string | null = "game.exe") {
  const user = userEvent.setup();
  render(<LearnLookCard exe={exe} />);
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
  return user;
}

function monitor(over: Partial<MonitorLearnView> = {}): MonitorLearnView {
  return {
    monitor: "GSM5C7C-x", monitor_name: "LG ULTRAGEAR+", panel: "oled", phase: "learning",
    readiness: { frames: 300, frames_needed: 600, scenes: 2, scenes_needed: 3, stable_checkpoints: 1, checkpoints_needed: 3, progress: 0.45, checkpoints: 2, scene_frames: [120, 180, 0, 0, 0] },
    converged: null, applied: null, use_learned: false, hdr_skipped: false, status: "learning",
    adjustments: null, excluded: 12,
    excluded_by: { static_frames: 3, loading: 7, cutscene: 0, outlier: 0, idle: 1, warmup: 1 }, ...over,
  };
}

function seed(v: Partial<LearnView>) {
  core.learn.set("game.exe", {
    exe: "game.exe", enabled: true, status: "learning", sampling: true, monitors: [], imported: null,
    privacy: "Frames are analysed in memory at low resolution while the game has focus. No frames are recorded or saved, and nothing leaves this PC.",
    tournament: TOURNAMENT_NOTICE, ...v,
  });
}

const cmds = () => tauri.calls.map((c) => c.cmd);

describe("the learn-this-game's-look card", () => {
  it("starts off and shows the privacy line and the exact tournament notice", async () => {
    await mount();
    expect(screen.getByTestId("look-status")).toHaveTextContent(/^Off/);
    expect(screen.getByRole("switch", { name: /Learn this game's look/ })).toHaveAttribute("aria-checked", "false");
    expect(screen.getByText(/No frames are recorded or saved, and nothing leaves this PC/)).toBeInTheDocument();
    expect(screen.getByTestId("tournament-notice")).toHaveTextContent(
      "Relay's visual enhancements may not be allowed in some tournaments or professional environments. Check with your tournament host or rules.",
    );
    // A notice only: no confirmation step anywhere for it.
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByRole("button", { name: "Apply" })).toBeDisabled();
  });

  it("turning learning on reaches the core", async () => {
    const user = await mount();
    await user.click(screen.getByRole("switch", { name: /Learn this game's look/ }));
    expect(tauri.calls.find((c) => c.cmd === "learn_display_set")?.args).toEqual({ exe: "game.exe", enabled: true });
    expect(screen.getByTestId("look-status")).toHaveTextContent(/^Learning/);
  });

  it("shows evidence-based progress per monitor", async () => {
    seed({ monitors: [monitor()] });
    await mount();
    const bar = screen.getByRole("progressbar");
    expect(bar).toHaveAttribute("aria-valuenow", "45");
    expect(screen.getByText(/300 \/ 600 gameplay frames · 2 \/ 3 kinds of scene/)).toBeInTheDocument();
    expect(screen.getByTestId("look-skipped")).toHaveTextContent("12 skipped: static/menu 3 · loading 7 · idle 1 · warm-up 1");
    expect(screen.getByTestId("look-scenes")).toHaveTextContent("Scenes (dark → bright): 120 / 180 / 0 / 0 / 0");
    expect(screen.getByText(/checkpoints 1 stable of 2/)).toBeInTheDocument();
  });

  it("Apply is offered once a look has settled, and applies it", async () => {
    const look = { shadow: 0.4, saturation: 0.1, highlight: 0 };
    seed({ status: "ready", monitors: [monitor({ phase: "converged", converged: look, status: "ready" })] });
    const user = await mount();
    expect(screen.getByTestId("look-status")).toHaveTextContent(/^Ready/);
    expect(screen.getByText(/shadows 40% · colour 10% · highlights 0%/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Apply" }));
    expect(cmds()).toContain("learn_display_apply");
    expect(screen.getByTestId("look-status")).toHaveTextContent(/^Applied\./);
  });

  it("Relearn and Reset reach the core; Reset asks first", async () => {
    seed({ monitors: [monitor()] });
    const user = await mount();
    await user.click(screen.getByRole("button", { name: "Relearn" }));
    expect(cmds()).toContain("learn_display_relearn");
    await user.click(screen.getByRole("button", { name: "Reset" }));
    expect(cmds()).not.toContain("learn_display_reset");
    await user.click(screen.getByRole("button", { name: "Forget this game's look" }));
    expect(cmds()).toContain("learn_display_reset");
    expect(screen.getByTestId("look-status")).toHaveTextContent(/^Off/);
  });

  it("an import applies at once, reads Applied (imported), and turns learning off", async () => {
    seed({ enabled: true });
    const user = await mount();
    const file = new File([JSON.stringify({
      format: "relay-game-display", version: 1, game: { exe: "game.exe" },
      look: { shadow: 0.3, saturation: 0.1, highlight: 0 }, note: "tuned on an OLED",
    })], "game.relay-display.json", { type: "application/json" });
    await user.upload(screen.getByTestId("look-import"), file);
    await act(async () => { await Promise.resolve(); });
    expect(cmds()).toContain("learn_display_import");
    expect(screen.getByTestId("look-status")).toHaveTextContent("Applied (imported).");
    const fineTune = screen.getByRole("switch", { name: /Keep learning to fine-tune for my monitor/ });
    expect(fineTune).toHaveAttribute("aria-checked", "false");
    expect(screen.getByText(/"tuned on an OLED"/)).toBeInTheDocument();
    await user.click(fineTune);
    expect(tauri.calls.filter((c) => c.cmd === "learn_display_set").pop()?.args).toEqual({ exe: "game.exe", enabled: true });
  });

  it("a rejected import says why", async () => {
    const user = await mount();
    const file = new File([JSON.stringify({ format: "relay-game-display", version: 1, game: { exe: "other.exe" }, look: { shadow: 0, saturation: 0, highlight: 0 } })], "x.json");
    await user.upload(screen.getByTestId("look-import"), file);
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByRole("alert")).toHaveTextContent(/this file is for other\.exe/);
  });

  it("export sends the note", async () => {
    const user = await mount();
    URL.createObjectURL = () => "blob:x";
    URL.revokeObjectURL = () => {};
    await user.type(screen.getByRole("textbox", { name: "Export note" }), "dark maps");
    await user.click(screen.getByRole("button", { name: "Export" }));
    expect(tauri.calls.find((c) => c.cmd === "learn_display_export")?.args).toMatchObject({ exe: "game.exe", note: "dark maps" });
  });

  it("an HDR monitor says nothing was learned there", async () => {
    seed({ status: "hdr_skipped", monitors: [monitor({ hdr_skipped: true, status: "hdr_skipped" })] });
    await mount();
    expect(screen.getByText(/HDR was on/)).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("without a game it asks for one and calls nothing", async () => {
    await mount(null);
    expect(screen.getByText(/Pick a game/)).toBeInTheDocument();
    expect(cmds()).toHaveLength(0);
  });
});
