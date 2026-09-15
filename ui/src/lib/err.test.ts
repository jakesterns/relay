import { describe, expect, it } from "vitest";
import { errText } from "./err";

describe("errText", () => {
  it("passes a plain string through — the usual shape from the core", () => {
    expect(errText("profiles.json is read-only")).toBe("profiles.json is read-only");
  });

  it("reads the message off an Error", () => {
    expect(errText(new Error("pipe closed"))).toBe("pipe closed");
  });

  it("reads the message off the object Tauri rejects with", () => {
    expect(errText({ message: "core refused" })).toBe("core refused");
    expect(errText({ error: "core refused" })).toBe("core refused");
  });

  it("never renders [object Object], which is what String(e) did", () => {
    expect(errText({ code: 5, detail: "denied" })).not.toContain("[object Object]");
    expect(errText({ code: 5, detail: "denied" })).toContain("denied");
    expect(errText({})).toBe("Unknown error");
  });

  it("survives something circular", () => {
    const loop: Record<string, unknown> = {};
    loop.self = loop;
    expect(errText(loop)).toBe("Unknown error");
  });

  it("still says something for a bare value", () => {
    expect(errText(null)).toBe("null");
    expect(errText(404)).toBe("404");
  });
});
