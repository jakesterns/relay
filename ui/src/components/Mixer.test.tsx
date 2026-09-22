/**
 * The mixer card (S37): faders that reach the engine as gains, mutes that
 * reach it as silence, nothing sent for a track the user did not touch, and
 * everything back at unity when a new share starts.
 */
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { MixerCard, dbToGain, fmtDb } from "./Mixer";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => { vi.useRealTimers(); });

const rows = [
  { key: "app" as const, label: "Game" },
  { key: "rest" as const, label: "Everything else" },
  { key: "mic" as const, label: "Microphone" },
];

const slider = (label: string) => screen.getByRole("slider", { name: label }) as HTMLInputElement;
const flush = async () => { vi.advanceTimersByTime(60); await settle(); };

describe("dB and gain", () => {
  it("map the way the engine expects: unity at 0 dB, silence at the bottom", () => {
    expect(dbToGain(0)).toBe(1);
    expect(dbToGain(6)).toBeCloseTo(1.995, 2);
    expect(dbToGain(-20)).toBeCloseTo(0.1, 5);
    expect(dbToGain(-60)).toBe(0);
    expect(fmtDb(0)).toBe("0 dB");
    expect(fmtDb(6)).toBe("+6 dB");
    expect(fmtDb(-12)).toBe("−12 dB");
    expect(fmtDb(-60)).toBe("−∞ dB");
  });
});

describe("MixerCard", () => {
  it("shows one row per track, all at unity, and sends nothing until touched", async () => {
    renderScreen(<MixerCard side="send" rows={rows} sessionKey="a" />);
    await settle();
    expect(screen.getAllByRole("slider")).toHaveLength(3);
    expect(slider("Game").value).toBe("0");
    await flush();
    expect(tauri.lastCall("set_mixer")).toBeUndefined();
  });

  it("sends only the fader that moved, as a linear gain", async () => {
    renderScreen(<MixerCard side="send" rows={rows} sessionKey="a" />);
    await settle();
    fireEvent.change(slider("Everything else"), { target: { value: "-20" } });
    await flush();
    expect(tauri.lastCall("set_mixer")?.args).toEqual({
      side: "send",
      faders: { rest: { gain: expect.closeTo(0.1, 5), mute: false } },
    });
  });

  it("batches a drag into one command", async () => {
    renderScreen(<MixerCard side="receive" rows={rows} sessionKey="a" />);
    await settle();
    fireEvent.change(slider("Game"), { target: { value: "-3" } });
    fireEvent.change(slider("Game"), { target: { value: "-6" } });
    fireEvent.change(slider("Game"), { target: { value: "-9" } });
    await flush();
    expect(tauri.callsOf("set_mixer")).toHaveLength(1);
    expect(tauri.lastCall("set_mixer")?.args).toEqual({
      side: "receive",
      faders: { app: { gain: expect.closeTo(dbToGain(-9), 5), mute: false } },
    });
  });

  it("mute keeps the gain and sends mute, and unmute puts it back", async () => {
    const h = renderScreen(<MixerCard side="send" rows={rows} sessionKey="a" />);
    await settle();
    fireEvent.change(slider("Microphone"), { target: { value: "-6" } });
    await flush();
    await h.user.click(screen.getByRole("button", { name: "Mute Microphone" }));
    await flush();
    expect(tauri.lastCall("set_mixer")?.args).toEqual({
      side: "send",
      faders: { mic: { gain: expect.closeTo(dbToGain(-6), 5), mute: true } },
    });
    // A muted fader is locked -- there is nothing to hear while adjusting it.
    expect(screen.queryByRole("slider", { name: "Microphone" })).not.toBeInTheDocument();
    await h.user.click(screen.getByRole("button", { name: "Unmute Microphone" }));
    await flush();
    expect(tauri.lastCall("set_mixer")?.args).toEqual({
      side: "send",
      faders: { mic: { gain: expect.closeTo(dbToGain(-6), 5), mute: false } },
    });
  });

  it("goes back to unity for a new share", async () => {
    // A tiny host that changes the session the way a new share would.
    function Host() {
      const [key, setKey] = useState("a");
      return (
        <>
          <button onClick={() => setKey("b")}>next share</button>
          <MixerCard side="send" rows={rows} sessionKey={key} />
        </>
      );
    }
    const h = renderScreen(<Host />);
    await settle();
    fireEvent.change(slider("Game"), { target: { value: "-12" } });
    await flush();
    expect(slider("Game").value).toBe("-12");
    await h.user.click(screen.getByRole("button", { name: "next share" }));
    await settle();
    expect(slider("Game").value).toBe("0");
  });

  it("renders nothing with no tracks", async () => {
    renderScreen(<MixerCard side="send" rows={[]} sessionKey="a" />);
    await settle();
    expect(screen.queryByText("Mixer")).not.toBeInTheDocument();
  });
});
