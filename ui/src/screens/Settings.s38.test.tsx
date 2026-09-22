/**
 * The two S38 switches: on by default, each saved without disturbing the
 * others. The second half is the one that matters -- a toggle that sent a
 * partial object would quietly reset every other preference.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Settings } from "./Settings";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

const resilience = () => screen.getByRole("switch", { name: /Bring a share back on its own/ });
const closeNotice = () => screen.getByRole("switch", { name: /Say so in the notification area/ });

describe("resilience and the close notice", () => {
  it("are on by default, in plain words", async () => {
    renderScreen(<Settings />);
    await settle();
    expect(resilience()).toHaveAttribute("aria-checked", "true");
    expect(closeNotice()).toHaveAttribute("aria-checked", "true");
    expect(resilience()).toHaveTextContent(/keeps trying for three minutes/);
  });

  it("turning one off leaves the others exactly as they were", async () => {
    core.prefs = { close_action: "quit_relay", resilience: true, close_notice: true };
    tauri.useFakeCore(core.handler);
    const h = renderScreen(<Settings />);
    await settle();

    await h.user.click(resilience());
    await settle();
    expect(tauri.lastCall("set_ui_prefs")?.args).toEqual({
      prefs: { close_action: "quit_relay", resilience: false, close_notice: true },
    });
    expect(resilience()).toHaveAttribute("aria-checked", "false");
    expect(resilience()).toHaveTextContent(/stays dead until you start it again/);

    await h.user.click(closeNotice());
    await settle();
    expect(core.prefs).toEqual({ close_action: "quit_relay", resilience: false, close_notice: false });
  });

  it("cannot be toggled while the setting is unknown", async () => {
    core.fail.set("get_ui_prefs", "not running");
    renderScreen(<Settings />);
    await settle();
    expect(resilience()).toBeDisabled();
    expect(closeNotice()).toBeDisabled();
  });
});
