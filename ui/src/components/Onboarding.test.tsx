/**
 * Getting started (owner's first live setup, 2026-10-22): the profile offer
 * for a full-screen game with none, the three steps where there is nothing
 * to edit yet, and the tip on how learning works. Nothing is created
 * without a click.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderApp, renderScreen, settle } from "../test/render";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Games } from "../screens/Games";
import { Profiles } from "../screens/Profiles";
import { offerName } from "../lib/onboarding";

let core: FakeCore;
// The fake already has a profile for cod.exe; this is a game with none.
const COD = { exe: "mw4.exe", title: "Call of Duty®" };

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

const offer = () => screen.getByTestId("profile-offer");
const saved = () => [...core.profiles.values()].find((p) => p.game.exe === COD.exe);

describe("the profile offer", () => {
  it("is not shown when the core offers nothing", async () => {
    renderScreen(<Profiles />);
    await settle();
    expect(screen.queryByTestId("profile-offer")).not.toBeInTheDocument();
  });

  it("asks, makes a Ready profile only on the click, then asks for a goal", async () => {
    core.state.suggestion = COD;
    const h = renderScreen(<Profiles />);
    await settle();
    expect(offer()).toHaveTextContent("Call of Duty® is open — make a profile for it?");
    // Asking created nothing.
    expect(tauri.lastCall("save_profile")).toBeUndefined();

    await h.user.click(within(offer()).getByRole("button", { name: "Make profile" }));
    await settle();
    const p = saved();
    expect(p).toMatchObject({ name: "Call of Duty®", status: "ready", game: { exe: "mw4.exe" } });
    // Learning is not switched on until a goal is chosen.
    expect(tauri.calls.filter((c) => c.cmd === "game_eq")).toEqual([]);
    expect(offer()).toHaveTextContent("Profile made for Call of Duty®");

    await h.user.click(within(offer()).getByRole("button", { name: "Awareness" }));
    await settle();
    const actions = tauri.calls.filter((c) => c.cmd === "game_eq").map((c) => c.args.action);
    expect(actions).toEqual([{ kind: "set_goal", goal: "awareness" }, { kind: "set_learning", enabled: true }]);
    expect(offer()).toHaveTextContent(/learning for Awareness.*nothing about your sound changes until you press Apply/);
    await h.user.click(within(offer()).getByRole("button", { name: "Done" }));
    expect(screen.queryByTestId("profile-offer")).not.toBeInTheDocument();
    h.expectClean();
  });

  it("Not for this game declines it for good and makes nothing", async () => {
    core.state.suggestion = COD;
    const h = renderScreen(<Profiles />);
    await settle();
    await h.user.click(within(offer()).getByRole("button", { name: "Not for this game" }));
    await settle();
    expect(tauri.lastCall("decline_suggestion")?.args).toEqual({ exe: "mw4.exe" });
    expect(core.declined).toEqual(["mw4.exe"]);
    expect(tauri.lastCall("save_profile")).toBeUndefined();
    expect(screen.queryByTestId("profile-offer")).not.toBeInTheDocument();
  });

  it("names the profile after the window, or the exe when the title is no name", () => {
    expect(offerName(COD)).toBe("Call of Duty®");
    expect(offerName({ exe: "EldenRing.EXE", title: "  " })).toBe("EldenRing");
  });

  it("a profile made from the offer is the one Games opens on, not the first", async () => {
    core.state.suggestion = COD;
    const h = renderApp();
    await settle();
    await h.user.click(within(offer()).getByRole("button", { name: "Make profile" }));
    await settle();
    await h.user.click(within(document.querySelector(".rail") as HTMLElement).getByRole("button", { name: "Audio" }));
    await settle();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Call of Duty®");
  });

  it("Use a recording goes to the Audio screen's recording card", async () => {
    core.state.suggestion = COD;
    const h = renderApp();
    await settle();
    await h.user.click(within(offer()).getByRole("button", { name: "Make profile" }));
    await settle();
    await h.user.click(within(offer()).getByRole("button", { name: "Awareness" }));
    await settle();
    await h.user.click(within(offer()).getByRole("button", { name: "Use a recording" }));
    await settle();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(/Call of Duty®.*Audio/);
    expect(document.getElementById("learn-video")).not.toBeNull();
  });
});

describe("getting started, where there is nothing to edit", () => {
  beforeEach(() => core.profiles.clear());

  it("Profiles lists the three steps; New profile opens the form", async () => {
    const h = renderApp();
    await settle();
    const steps = screen.getByTestId("getting-started");
    expect(within(steps).getAllByRole("listitem").map((li) => li.querySelector("b")?.textContent))
      .toEqual(["Open your game.", "Make a profile.", "Let Relay learn."]);
    // Nothing to learn for yet.
    expect(within(steps).getByRole("button", { name: "Use a recording" })).toBeDisabled();
    await h.user.click(within(steps).getByRole("button", { name: "New profile" }));
    await settle();
    expect(screen.getByRole("heading", { name: /^New profile/ })).toBeInTheDocument();
  });

  it("from another screen, New profile takes you to Profiles with the form open", async () => {
    const h = renderApp();
    await settle();
    await h.user.click(within(document.querySelector(".rail") as HTMLElement).getByRole("button", { name: "Audio" }));
    await settle();
    const steps = screen.getByTestId("getting-started");
    await h.user.click(within(steps).getByRole("button", { name: "New profile" }));
    await settle();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Profiles");
    expect(screen.getByRole("heading", { name: /^New profile/ })).toBeInTheDocument();
  });

  it("with a game open, step 2 uses the offer", async () => {
    core.state.suggestion = COD;
    const h = renderScreen(<Games section="audio" onSection={() => {}} />);
    await settle();
    const steps = screen.getByTestId("getting-started");
    expect(steps).toHaveTextContent("Call of Duty® is open.");
    await h.user.click(within(steps).getByRole("button", { name: "Make profile for Call of Duty®" }));
    await settle();
    expect(saved()).toMatchObject({ status: "ready" });
  });

  it("is not shown once there is a profile", async () => {
    const one = [...makeFakeCore().profiles.values()][0];
    core.profiles.set(one.id, one);
    renderScreen(<Games section="audio" onSection={() => {}} />);
    await settle();
    expect(screen.queryByTestId("getting-started")).not.toBeInTheDocument();
    renderScreen(<Profiles />);
    await settle();
    expect(screen.queryByTestId("getting-started")).not.toBeInTheDocument();
  });
});

describe("the learning tip", () => {
  it("explains live learning and a recording on Audio and Display", async () => {
    renderScreen(<Games section="audio" onSection={() => {}} />);
    await settle();
    expect(screen.getByTestId("learn-tip")).toHaveTextContent(
      /5–15 minutes — Relay listens only while the game is in front, and nothing changes until you press Apply\. From a recording: faster than real time, with Relay's own recordings listed first\./,
    );
  });

  it("says watches, not listens, for the look", async () => {
    renderScreen(<Games section="display" onSection={() => {}} />);
    await settle();
    expect(screen.getByTestId("learn-tip")).toHaveTextContent("Relay watches only while the game is in front");
  });
});
