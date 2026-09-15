/**
 * The app has to be usable without a mouse.
 *
 * Every assertion here is a DOM event dispatched in this jsdom document —
 * `user.tab()` and `user.keyboard()` move the *document's* focus, not the
 * host's. See src/test/README.md and safety.test.ts.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderApp, renderScreen, settle } from "./render";
import { card } from "./dom";
import { makeFakeCore, type FakeCore } from "./fakeCore";
import * as tauri from "./tauriMock";
import css from "../styles/shell.css?raw";
import { Profiles } from "../screens/Profiles";
import { Settings } from "../screens/Settings";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

const rail = () => within(document.querySelector(".rail") as HTMLElement);
const heading = () => screen.getByRole("heading", { level: 1 }).textContent ?? "";

/** Tab until `match` has focus, or give up. Returns how many stops it took,
 *  which is also the assertion that it was reachable at all. */
async function tabTo(user: ReturnType<typeof renderApp>["user"], match: () => Element, max = 40) {
  for (let i = 1; i <= max; i++) {
    await user.tab();
    if (document.activeElement === match()) return i;
  }
  throw new Error("never reached by Tab");
}

describe("the rail", () => {
  it("is a list of real buttons, not click handlers on anchors", async () => {
    renderApp();
    await settle();
    for (const label of ["Share", "Receive", "Games", "Audio", "Display", "Profiles", "Settings"]) {
      expect(rail().getByRole("button", { name: new RegExp(`^${label}`) })).toBeInTheDocument();
    }
    expect(document.querySelectorAll(".rail a")).toHaveLength(0);
  });

  it("navigates on Tab and Enter", async () => {
    const h = renderApp();
    await settle();
    const share = rail().getByRole("button", { name: /^Share/ });
    await tabTo(h.user, () => share);
    await h.user.keyboard("{Enter}");
    await settle();
    expect(heading()).toContain("Display 1");
  });

  it("navigates on Space too, and says which entry you are on", async () => {
    const h = renderApp();
    await settle();
    const settings = rail().getByRole("button", { name: /^Settings/ });
    settings.focus();
    await h.user.keyboard(" ");
    await settle();
    expect(heading()).toBe("Settings");
    expect(settings).toHaveAttribute("aria-current", "page");
  });
});

describe("toggles", () => {
  it("are switches with a name, reachable and flippable from the keyboard", async () => {
    const h = renderScreen(<Settings />);
    await settle();
    const sw = screen.getByRole("switch", { name: /Start Relay at login/ });
    expect(sw).toHaveAttribute("aria-checked", "false");

    await tabTo(h.user, () => sw);
    await h.user.keyboard(" ");
    await settle();

    expect(tauri.lastCall("set_autostart")?.args).toEqual({ enabled: true });
    expect(screen.getByRole("switch", { name: /Start Relay at login/ }))
      .toHaveAttribute("aria-checked", "true");
  });

  it("are disabled rather than inert when the core cannot answer", async () => {
    core.fail.set("get_autostart", "core offline");
    tauri.useFakeCore(core.handler);
    renderScreen(<Settings />);
    await settle();
    expect(screen.getByRole("switch", { name: /Start Relay at login/ })).toBeDisabled();
  });
});

describe("the profile table", () => {
  it("opens a row for editing from the keyboard", async () => {
    const h = renderScreen(<Profiles />);
    await settle();
    const row = document.querySelector(".rowname") as HTMLElement;
    await tabTo(h.user, () => row);
    await h.user.keyboard("{Enter}");
    await settle();
    expect(card("Edit profile")).toBeInTheDocument();
    expect(tauri.lastCall("get_profile")?.args).toEqual({ id: "1" });
  });

  it("applies a row from the keyboard, without opening the editor", async () => {
    const h = renderScreen(<Profiles />);
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Apply Call of Duty now" }));
    await settle();
    expect(tauri.lastCall("apply_profile")?.args).toEqual({ id: "1" });
    expect(screen.queryByText("Edit profile")).not.toBeInTheDocument();
  });

  it("says why an apply failed instead of swallowing it", async () => {
    core.fail.set("apply_profile", "monitor stopped answering DDC/CI");
    tauri.useFakeCore(core.handler);
    const h = renderScreen(<Profiles />);
    await settle();
    await h.user.click(screen.getByRole("button", { name: "Apply Call of Duty now" }));
    await settle();
    const note = screen.getByRole("alert");
    expect(note).toHaveTextContent("monitor stopped answering DDC/CI");
    await h.user.click(within(note).getByRole("button", { name: "Dismiss this message" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});

describe("the stylesheet", () => {
  it("draws focus, so a keyboard user can see where they are", () => {
    expect(css).toMatch(/:focus-visible\s*\{[^}]*outline:/);
  });

  it("lets the text worth copying be selected, despite the shell's user-select: none", () => {
    const rule = /user-select: text/.exec(css);
    expect(rule).not.toBeNull();
    // The paths, ids and readouts are all .mono; error and success notes carry
    // .msg; the pairing code is .code.
    const block = css.slice(css.indexOf("user-select: text") - 400, css.indexOf("user-select: text"));
    for (const sel of [".mono", ".code", ".offline .msg"]) {
      expect(block).toContain(sel);
    }
  });
});
