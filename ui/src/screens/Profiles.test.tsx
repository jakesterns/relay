/**
 * Profiles: the headphone catalogue (search + import) and the profile/hardware
 * CRUD around it. The catalogue was a stub until this month — typing did
 * nothing and Add was decorative — so it gets the closest look here.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { screen, within } from "@testing-library/react";
import { renderScreen, settle } from "../test/render";
import { card, field, inCard, kv } from "../test/dom";
import { makeFakeCore, type FakeCore } from "../test/fakeCore";
import * as tauri from "../test/tauriMock";
import { Profiles, scanSummary } from "./Profiles";

let core: FakeCore;

beforeEach(() => {
  core = makeFakeCore();
  tauri.useFakeCore(core.handler);
});

async function mount() {
  const h = renderScreen(<Profiles />);
  await settle();
  return h;
}

async function openHeadsetDialog(h: Awaited<ReturnType<typeof mount>>) {
  await h.user.click(inCard("Headsets & IEMs").getByRole("button", { name: "Add" }));
  return card("Add headset");
}

/** Catalogue hits only. The library list below uses the same row markup and
 *  already holds some of the same model names, so every catalogue assertion
 *  has to be scoped or it matches the library instead. */
function hits() {
  const el = document.querySelector(".catalog");
  if (!el) throw new Error("no catalogue results are showing");
  return within(el as HTMLElement);
}

/** The `.hwl` row carrying this text, in a given card. */
function hwRow(cardHeading: string, text: string): HTMLElement {
  const row = [...card(cardHeading).querySelectorAll<HTMLElement>(".hwl")]
    .find((r) => (r.textContent ?? "").includes(text));
  if (!row) throw new Error(`no hardware row for "${text}"`);
  return row;
}

describe("the headphone catalogue", () => {
  it("does not search on one character — the index scan is per keystroke", async () => {
    const h = await mount();
    await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "h");
    await settle();
    expect(tauri.lastCall("search_catalog")).toBeUndefined();
    expect(screen.queryByText(/No match in the catalogue/)).not.toBeInTheDocument();
  });

  it("searches on what was typed and lists who measured each hit", async () => {
    const h = await mount();
    await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "hd 5");

    expect(await screen.findByText("Sennheiser HD 560S")).toBeInTheDocument();
    expect(tauri.lastCall("search_catalog")?.args).toEqual({ query: "hd 5" });
    expect(hits().getByText("Measured by oratory1990")).toBeInTheDocument();
    // "HD 600" is in the catalogue but not in this result.
    expect(hits().queryByText("Sennheiser HD 600")).not.toBeInTheDocument();
  });

  it("debounces: several keystrokes are one query", async () => {
    const h = await mount();
    await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "blessing");
    await screen.findByText("Measured by crinacle on 711");
    expect(tauri.calls.filter((c) => c.cmd === "search_catalog")).toHaveLength(1);
    expect(tauri.lastCall("search_catalog")?.args).toEqual({ query: "blessing" });
  });

  it("says so when nothing matches, rather than showing an empty box", async () => {
    const h = await mount();
    await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "zzzz");
    expect(await screen.findByText("No match in the catalogue")).toBeInTheDocument();
  });

  it("picking a model fills the form and saves nothing until Add", async () => {
    const h = await mount();
    const dialog = await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "blessing");
    const hit = (await screen.findByText("Measured by crinacle on 711")).closest(".hwl") as HTMLElement;
    await h.user.click(within(hit).getByRole("button", { name: /^Use Moondrop Blessing 3/ }));
    await settle();

    // Still open, nothing downloaded, the rest of the form to fill in.
    expect(tauri.lastCall("add_headset_from_catalog")).toBeUndefined();
    expect(within(dialog).getByTestId("catalog-picked")).toHaveTextContent("Moondrop Blessing 3");
    expect(field("Name", dialog)).toHaveValue("Moondrop Blessing 3");
    expect(within(dialog).getByRole("combobox")).toHaveValue("ep:dac");
    // The catalogue fills the curve: no curve fields to confuse it with.
    expect(within(dialog).queryByText("Curve source")).not.toBeInTheDocument();

    // Rename it and bind it nowhere, then add: the core gets both.
    await h.user.clear(field("Name", dialog));
    await h.user.type(field("Name", dialog), "My Blessing");
    await h.user.selectOptions(within(dialog).getByRole("combobox"), "");
    await h.user.click(within(dialog).getByRole("button", { name: "Add to library" }));
    await settle();
    expect(tauri.lastCall("add_headset_from_catalog")?.args).toEqual({
      entry: { name: "Moondrop Blessing 3", source: "crinacle", rig: "711", path: "crinacle/711%20in-ear/Moondrop%20Blessing%203" },
      endpoint: null, name: "My Blessing", kind: "iem",
    });
    expect(inCard("Headsets & IEMs").getByText("My Blessing")).toBeInTheDocument();
  });

  it("Change goes back to the search without adding anything", async () => {
    const h = await mount();
    const dialog = await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "blessing");
    const hit = (await screen.findByText("Measured by crinacle on 711")).closest(".hwl") as HTMLElement;
    await h.user.click(within(hit).getByRole("button", { name: /^Use / }));
    await h.user.click(within(dialog).getByRole("button", { name: "Change" }));
    expect(screen.getByPlaceholderText(/HD 560S, Blessing 3/)).toBeInTheDocument();
    expect(tauri.lastCall("add_headset_from_catalog")).toBeUndefined();
  });

  it("imports the picked model against the default endpoint and shows it plugged in", async () => {
    const h = await mount();
    const dialog = await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "blessing");
    const hit = (await screen.findByText("Measured by crinacle on 711")).closest(".hwl") as HTMLElement;

    await h.user.click(within(hit).getByRole("button", { name: /^Use / }));
    await h.user.click(within(dialog).getByRole("button", { name: "Add to library" }));
    await settle();

    expect(tauri.lastCall("add_headset_from_catalog")?.args).toEqual({
      entry: { name: "Moondrop Blessing 3", source: "crinacle", rig: "711", path: "crinacle/711%20in-ear/Moondrop%20Blessing%203" },
      endpoint: "ep:dac", name: "Moondrop Blessing 3", kind: "iem",
    });
    // The dialog closes and the library reloads from the core.
    expect(screen.queryByRole("heading", { name: /Add headset/ })).not.toBeInTheDocument();
    const added = core.hardware.headsets.find((x) => x.id === "moondrop-blessing-3-crinacle");
    expect(added?.kind).toBe("iem");
    expect(added?.curve).toHaveLength(3);
    expect(inCard("Headsets & IEMs").getAllByText("Moondrop Blessing 3")).toHaveLength(2);
    h.expectClean();
  });

  it("reports a failed download instead of adding a headset with no curve", async () => {
    core.fail.set("add_headset_from_catalog", "could not reach the measurement host");
    const h = await mount();
    const dialog = await openHeadsetDialog(h);
    await h.user.type(screen.getByPlaceholderText(/HD 560S, Blessing 3/), "hd 600");
    const hit = (await screen.findByText("Sennheiser HD 600")).closest(".hwl") as HTMLElement;
    expect(hit).toBeTruthy();
    await h.user.click(within(hit).getByRole("button", { name: /^Use / }));
    await h.user.click(within(dialog).getByRole("button", { name: "Add to library" }));
    await settle();

    expect(screen.getByText("could not reach the measurement host")).toBeInTheDocument();
    expect(core.hardware.headsets).toHaveLength(2);
    expect(screen.getByRole("heading", { name: /Add headset/ })).toBeInTheDocument();
  });

  it("credits AutoEQ and says the curve is fetched, not bundled", async () => {
    const h = await mount();
    const dialog = await openHeadsetDialog(h);
    expect(within(dialog).getByText(/Measurements come from the/)).toHaveTextContent(
      /Relay ships only the list of names; the curve itself is downloaded when you add the model you picked, cached on this PC, and never redistributed\./,
    );
  });

  it("still takes a hand-typed headset with a pasted AutoEQ curve", async () => {
    const h = await mount();
    const dialog = await openHeadsetDialog(h);
    await h.user.type(field("Name", dialog), "DT 770");
    await h.user.type(field("Curve source", dialog), "oratory1990");
    await h.user.type(field("Measured curve (AutoEQ CSV, optional)", dialog), "20.00,-4.11");
    await h.user.click(within(dialog).getByRole("button", { name: "Add to library" }));
    await settle();

    expect(tauri.lastCall("save_hardware")?.args).toEqual({
      item: { kind: "headset", value: { id: "dt-770", name: "DT 770", kind: "headphone", source: "oratory1990", endpoints: ["ep:dac"] } },
    });
    expect(tauri.lastCall("import_curve")?.args).toEqual({ headset: "dt-770", csv: "20.00,-4.11" });
    h.expectClean();
  });
});

describe("profiles", () => {
  it("creates one, and the core has it afterwards", async () => {
    const h = await mount();
    await h.user.click(inCard("Game profiles").getByRole("button", { name: "New profile" }));
    const form = card("New profile");

    expect(within(form).getByRole("button", { name: "Create profile" })).toBeDisabled();
    await h.user.type(field("Name", form), "Deadlock");
    await h.user.type(field("Executable", form), "deadlock.exe");
    await h.user.click(within(form).getByRole("button", { name: "Ready" }));
    await h.user.click(within(form).getByRole("button", { name: "Create profile" }));
    await settle();

    const saved = [...core.profiles.values()].find((p) => p.name === "Deadlock");
    expect(saved?.game.exe).toBe("deadlock.exe");
    expect(saved?.status).toBe("ready");
    expect(inCard("Game profiles").getByText("Deadlock")).toBeInTheDocument();
    h.expectClean();
  });

  it("offers the running processes as executables", async () => {
    const h = await mount();
    await h.user.click(inCard("Game profiles").getByRole("button", { name: "New profile" }));
    await settle();
    expect(within(card("New profile")).getByRole("option", { name: "cod.exe — Call of Duty" })).toBeInTheDocument();
  });

  it("edits an existing row through the core", async () => {
    const h = await mount();
    await h.user.click(screen.getByText("Valorant"));
    await settle();
    const form = card("Edit profile");
    await h.user.clear(field("Note", form));
    await h.user.type(field("Note", form), "flat EQ");
    await h.user.click(within(form).getByRole("button", { name: "Save changes" }));
    await settle();

    expect(core.profiles.get("2")?.note).toBe("flat EQ");
    expect(screen.queryByRole("heading", { name: /Edit profile/ })).not.toBeInTheDocument();
  });

  it("asks twice before deleting", async () => {
    const h = await mount();
    await h.user.click(screen.getByText("Elden Ring"));
    await settle();
    const form = card("Edit profile");
    await h.user.click(within(form).getByRole("button", { name: "Delete…" }));
    expect(tauri.lastCall("delete_profile")).toBeUndefined();
    await h.user.click(within(form).getByRole("button", { name: "Confirm delete" }));
    await settle();

    expect(core.profiles.has("3")).toBe(false);
    expect(screen.queryByText("Elden Ring")).not.toBeInTheDocument();
    h.expectClean();
  });

  it("applies a profile on double-click and shows it as active", async () => {
    const h = await mount();
    await h.user.dblClick(screen.getByText("Call of Duty"));
    await settle();

    expect(tauri.lastCall("apply_profile")?.args).toEqual({ id: "1" });
    expect(screen.getByText("Call of Duty active")).toBeInTheDocument();
    expect(screen.getByText("Active")).toBeInTheDocument();
    expect(screen.getByText("Applied")).toBeInTheDocument();
  });

  it("reports a core that refused the save and keeps the form open", async () => {
    core.fail.set("save_profile", "profiles.json is read-only");
    const h = await mount();
    await h.user.click(inCard("Game profiles").getByRole("button", { name: "New profile" }));
    const form = card("New profile");
    await h.user.type(field("Name", form), "X");
    await h.user.type(field("Executable", form), "x.exe");
    await h.user.click(within(form).getByRole("button", { name: "Create profile" }));
    await settle();

    expect(screen.getByText("profiles.json is read-only")).toBeInTheDocument();
    expect(card("New profile")).toBeInTheDocument();
  });
});

describe("the hardware library", () => {
  it("asks twice before removing an entry, in Relay's own window", async () => {
    const h = await mount();
    const row = () => within(hwRow("Headsets & IEMs", "HD 560S"));
    await h.user.click(row().getByRole("button", { name: "Remove" }));
    await settle();
    // Armed, not fired. (`window.confirm` throws in this harness, so a native
    // dialog here would fail the test rather than pass it silently.)
    expect(tauri.lastCall("delete_hardware")).toBeUndefined();
    // Short, so it fits the row whatever the model is called: the row
    // already names it.
    expect(row().getByRole("button", { name: "Remove?" })).toBeInTheDocument();

    await h.user.click(row().getByRole("button", { name: "Remove?" }));
    await settle();
    expect(tauri.lastCall("delete_hardware")?.args).toEqual({ id: "hd560s" });
    expect(core.hardware.headsets.map((x) => x.id)).toEqual(["blessing3"]);
    h.expectClean();
  });

  it("lights the icon of what is in use: the plugged headset and the output playing now", async () => {
    core.hardware.connected.headset = "hd560s";
    await mount();
    const on = (row: HTMLElement) => row.querySelector(".ic")!.classList.contains("on");
    expect(on(hwRow("Headsets & IEMs", "HD 560S"))).toBe(true);
    const others = core.hardware.headsets.filter((x) => x.id !== "hd560s");
    for (const x of others) expect(on(hwRow("Headsets & IEMs", x.name))).toBe(false);
    // "What are you listening on?": only the default output is lit.
    const outputs = [...document.querySelectorAll<HTMLElement>("[data-testid=output-state]")];
    expect(outputs.length).toBeGreaterThan(0);
    const lit = outputs.filter((i) => i.classList.contains("on")).map((i) => i.closest(".listen")?.getAttribute("data-output"));
    expect(lit).toEqual([core.hardware.connected.endpoints.find((e) => e.default)!.name]);
  });

  it("edits a saved monitor: name and panel type, id and scan kept", async () => {
    const m = core.hardware.monitors[0];
    m.ddcci = [0x10, 0x12];
    const h = await mount();
    await h.user.click(within(hwRow("Monitors", m.name)).getByRole("button", { name: `Edit ${m.name}` }));
    await settle();
    const dialog = card("Edit monitor");
    expect(dialog).toHaveTextContent(m.id);
    await h.user.clear(field("Panel", dialog));
    await h.user.type(field("Panel", dialog), "Nano IPS");
    await h.user.click(within(dialog).getByRole("button", { name: "Save changes" }));
    await settle();
    const saved = tauri.lastCall("save_hardware")?.args as { item: { kind: string; value: Record<string, unknown> } };
    expect(saved.item.kind).toBe("monitor");
    expect(saved.item.value).toMatchObject({ id: m.id, name: m.name, panel: "Nano IPS", ddcci: [0x10, 0x12] });
    expect(screen.queryByRole("heading", { name: /Edit monitor/ })).not.toBeInTheDocument();
  });

  it("edits a saved headset in place and keeps its measured curve", async () => {
    const before = structuredClone(core.hardware.headsets.find((x) => x.id === "hd560s")!);
    const h = await mount();
    await h.user.click(within(hwRow("Headsets & IEMs", "HD 560S")).getByRole("button", { name: "Edit HD 560S" }));
    await settle();
    const dialog = card("Edit headset");
    expect(field("Name", dialog)).toHaveValue("HD 560S");
    // Editing is not adding: no catalogue search, no duplicate entry.
    expect(within(dialog).queryByPlaceholderText(/HD 560S, Blessing 3/)).not.toBeInTheDocument();

    await h.user.clear(field("Name", dialog));
    await h.user.type(field("Name", dialog), "HD 560S (desk)");
    await h.user.click(within(dialog).getByRole("button", { name: "IEM" }));
    await h.user.click(within(dialog).getByRole("button", { name: "Save changes" }));
    await settle();

    const saved = tauri.lastCall("save_hardware")?.args as { item: { value: Record<string, unknown> } };
    expect(saved.item.value).toMatchObject({ id: "hd560s", name: "HD 560S (desk)", kind: "iem" });
    expect(saved.item.value.curve).toEqual(before.curve);
    expect(tauri.lastCall("import_curve")).toBeUndefined();
    expect(core.hardware.headsets.filter((x) => x.id === "hd560s")).toHaveLength(1);
    expect(screen.queryByRole("heading", { name: /Edit headset/ })).not.toBeInTheDocument();
    expect(inCard("Headsets & IEMs").getByText("HD 560S (desk)")).toBeInTheDocument();
  });

  it("re-probes DDC/CI capabilities on demand, not on every refresh", async () => {
    const h = await mount();
    expect(tauri.lastCall("probe_hardware")).toBeUndefined();
    await h.user.click(screen.getByRole("button", { name: "Scan monitor controls" }));
    await settle();
    expect(tauri.lastCall("probe_hardware")).toBeDefined();
    // The result is said, not just applied.
    expect(screen.getByRole("status")).toHaveTextContent(/:/);
  });

  it("the scan summary names found controls or the lack of a response", () => {
    expect(scanSummary([{ name: "AW2518H" }])).toMatch(/AW2518H: no DDC\/CI response/);
    expect(scanSummary([{ name: "LG", ddc: [0x10, 0x12] }])).toMatch(/LG: controls: brightness, contrast/);
    expect(scanSummary([])).toBe("No monitors found.");
    // AW2518H caps: presets and RGB gain are listed too, not just the first two.
    expect(scanSummary([{ name: "AW2518H", ddc: [0x10, 0x12, 0x14, 0x16, 0x18, 0x1a, 0x60] }]))
      .toMatch(/brightness, contrast, colour preset, red gain, green gain, blue gain, input source/);
  });

  it("names the DDC/CI controls the panel actually advertises", async () => {
    await mount();
    // 0x10 and 0x12 are advertised; 0x87 (sharpness) is not.
    expect(inCard("Monitors").getByText(/controls: brightness, contrast/)).toBeInTheDocument();
  });

  it("reads the default endpoint and foreground out of core state", async () => {
    core.state.foreground = { pid: 7, exe: "cod.exe", title: "Call of Duty", hmonitor: 1 };
    tauri.useFakeCore(core.handler);
    await mount();
    expect(kv("Default audio")).toBe("USB Audio 2.0");
    expect(kv("Foreground")).toBe("cod.exe");
  });
});
