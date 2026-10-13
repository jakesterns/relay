/**
 * S48: "Learn faster: use a recording". Relay's recordings are listed first,
 * a pick (from the list or the native dialog) starts the job, the progress
 * bar follows it, Cancel says nothing was kept, a finished job says what was
 * learned, and the card always says it is local files only — no downloading.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { LearnFromVideoCard, VIDEO_POLL_MS } from "./LearnFromVideoCard";
import { clockText, etaText } from "../lib/eta";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function flush() {
  await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); });
}

async function mount(id: string | null = "1") {
  const user = userEvent.setup();
  render(<LearnFromVideoCard profileId={id} />);
  await flush();
  return user;
}

describe("learn from a video file", () => {
  it("explains what is analysed and that only local files are used", async () => {
    await mount();
    expect(screen.getByTestId("video-what")).toHaveTextContent(/one frame every half second/);
    expect(screen.getByTestId("video-what")).toHaveTextContent(/commentary over the game are left out/);
    expect(screen.getByTestId("video-local-only")).toHaveTextContent(/Local files only/);
    expect(screen.getByTestId("video-local-only")).toHaveTextContent(/does not download videos from YouTube/);
    expect(screen.getByText(/never copied, uploaded or changed/)).toBeInTheDocument();
  });

  it("lists Relay's recordings first and starts learning from a pick", async () => {
    const user = await mount();
    await user.click(screen.getByRole("button", { name: "Learn from a video file…" }));
    await flush();
    const group = screen.getByRole("group", { name: "Choose a video" });
    const buttons = within(group).getAllByRole("button", { name: /^Learn from / });
    expect(buttons[0]).toHaveAccessibleName("Learn from Relay 2026-10-01 21-04.mp4");
    expect(buttons[0]).toHaveTextContent(/Relay recording/);
    expect(buttons[1]).toHaveAccessibleName("Learn from match.mkv");
    await user.click(buttons[1]);
    await flush();
    const call = tauri.lastCall("learn_from_file")!;
    expect(call.args).toEqual({ id: "1", path: "C:\\Users\\test\\Videos\\match.mkv" });
    expect(screen.getByRole("progressbar", { name: "Video learning progress" })).toHaveAttribute("aria-valuenow", "0");
    expect(screen.queryByRole("group", { name: "Choose a video" })).toBeNull();
  });

  it("uses the native dialog for any other file", async () => {
    const user = await mount();
    await user.click(screen.getByRole("button", { name: "Learn from a video file…" }));
    await flush();
    await user.click(screen.getByRole("button", { name: "Choose another file…" }));
    await flush();
    expect(tauri.callsOf("pick_video_file")).toHaveLength(1);
    expect(tauri.lastCall("learn_from_file")!.args).toEqual({ id: "1", path: "C:\\Users\\test\\Desktop\\clip.webm" });
  });

  it("does nothing when the dialog is closed", async () => {
    core.pickedVideo = null;
    const user = await mount();
    await user.click(screen.getByRole("button", { name: "Learn from a video file…" }));
    await flush();
    await user.click(screen.getByRole("button", { name: "Choose another file…" }));
    await flush();
    expect(tauri.callsOf("learn_from_file")).toHaveLength(0);
  });

  it("follows progress and speed while it runs, then says what was learned", async () => {
    core.fileJobs.set("1", {
      profile: "1", exe: "game.exe", file_name: "match.mkv", state: "running", progress: 0.25,
      position_secs: 150, duration_secs: 600, speed: 14.6, audio_secs: 0, look_frames: 0, notes: [],
      message: null, privacy: "p", local_only: "Local files only.",
    });
    await mount();
    expect(screen.getByRole("progressbar", { name: "Video learning progress" })).toHaveAttribute("aria-valuenow", "25");
    expect(screen.getByText("2:30 of 10:00 · 25% · 15× real time")).toBeInTheDocument();
    // The helper finishes; the next poll shows the result.
    Object.assign(core.fileJobs.get("1")!, {
      state: "done", progress: 1, audio_secs: 600, look_frames: 1200, look_gameplay_frames: 950,
      notes: ["The video was decoded on the CPU."],
    });
    await act(async () => { await new Promise((r) => setTimeout(r, VIDEO_POLL_MS + 50)); });
    await flush();
    expect(screen.getByTestId("video-done")).toHaveTextContent(/10:00 of sound and 1200 frames \(950 of gameplay\)/);
    expect(screen.getByTestId("video-done")).toHaveTextContent(/Learning stays on so your play refines it/);
    expect(screen.getByText("The video was decoded on the CPU.")).toBeInTheDocument();
    expect(screen.queryByRole("progressbar", { name: "Video learning progress" })).toBeNull();
  });

  it("cancels and says nothing was kept", async () => {
    core.fileJobs.set("1", {
      profile: "1", exe: "game.exe", file_name: "match.mkv", state: "running", progress: 0.1,
      position_secs: 60, duration_secs: 600, speed: 12, audio_secs: 0, look_frames: 0, notes: [],
      message: null, privacy: "p", local_only: "l",
    });
    const user = await mount();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await flush();
    expect(tauri.callsOf("learn_file_cancel")).toHaveLength(1);
    expect(screen.getByTestId("video-cancelled")).toHaveTextContent("Cancelled. Nothing from the file was kept.");
    // And a new file can be chosen again.
    expect(screen.getByRole("button", { name: "Learn from a video file…" })).toBeEnabled();
  });

  it("shows the core's refusal as an error", async () => {
    core.fail.set("learn_from_file", "Relay can learn from .mp4, .mkv, .mov and .webm videos");
    const user = await mount();
    await user.click(screen.getByRole("button", { name: "Learn from a video file…" }));
    await flush();
    await user.click(screen.getByRole("button", { name: "Learn from match.mkv" }));
    await flush();
    expect(screen.getByText(/Relay can learn from .mp4, .mkv, .mov and .webm videos/)).toBeInTheDocument();
  });

  it("needs a profile", async () => {
    await mount(null);
    expect(screen.getByText("No profile selected.")).toBeInTheDocument();
    expect(tauri.callsOf("learn_file_status")).toHaveLength(0);
  });
});

describe("eta text", () => {
  it("is honest about what it knows", () => {
    expect(etaText(null)).toBe("time left not known yet");
    expect(etaText(undefined, "unknown")).toBe("unknown");
    expect(etaText(0)).toBe("ready");
    expect(etaText(30)).toBe("less than a minute left");
    expect(etaText(60)).toBe("about 1 min left");
    expect(etaText(181)).toBe("about 4 min left");
    expect(clockText(42)).toBe("42 s");
    expect(clockText(150)).toBe("2:30");
  });
});
