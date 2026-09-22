import { describe, expect, it } from "vitest";
import { ago } from "./ago";

describe("ago", () => {
  const now = 1_800_000_000_000; // ms
  const at = (secsAgo: number) => ago(now / 1000 - secsAgo, now);

  it("is coarse on purpose", () => {
    expect(at(5)).toBe("just now");
    expect(at(59)).toBe("just now");
    expect(at(60)).toBe("1 min ago");
    expect(at(3 * 60)).toBe("3 min ago");
    expect(at(3600)).toBe("1 hour ago");
    expect(at(5 * 3600)).toBe("5 hours ago");
    expect(at(86400)).toBe("yesterday");
    expect(at(3 * 86400)).toBe("3 days ago");
    expect(at(40 * 86400)).toBe("1 month ago");
    expect(at(400 * 86400)).toBe("1 year ago");
  });

  it("never reports the future, and says so for a zero timestamp", () => {
    expect(ago(now / 1000 + 500, now)).toBe("just now");
    expect(ago(0, now)).toBe("never");
  });
});
