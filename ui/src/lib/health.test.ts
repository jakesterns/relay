import { describe, expect, it } from "vitest";
import { CLEAR_WINDOWS, HealthTracker, TRIGGER_WINDOWS, healthText } from "./health";
import type { ShareStats } from "./ipc";

/** A receiver `stats` line with cumulative counters. */
function stats(o: Partial<ShareStats>): ShareStats {
  return { event: "stats", ...o };
}

/** Cumulative counters, carried across `feed` calls the way the engine does.
 *
 *  Kept as one object on purpose: an earlier version of this helper took a
 *  scalar "start from" and applied it to every counter, which silently bumped
 *  `frames_withheld` at the start of what was meant to be a clean run and made
 *  a passing implementation look broken. The counters have to move
 *  independently, because that is how the engine reports them. */
function counters() {
  return { rtp_lost: 0, rtp_recovered: 0, frames_withheld: 0, keyframe_requests: 0 };
}

/** Feed `n` samples that each add `per` to `c`, mutating `c` as the engine would. */
function feed(
  t: HealthTracker,
  c: ReturnType<typeof counters>,
  n: number,
  per: Partial<ShareStats> = {},
) {
  for (let i = 0; i < n; i++) {
    c.rtp_lost += per.rtp_lost ?? 0;
    c.rtp_recovered += per.rtp_recovered ?? 0;
    c.frames_withheld += per.frames_withheld ?? 0;
    c.keyframe_requests += per.keyframe_requests ?? 0;
    t.push(stats({ ...c }));
  }
}

describe("HealthTracker", () => {
  it("says nothing on the first sample, whatever the counters say", () => {
    const t = new HealthTracker();
    // Joining a share in progress: 5,000 packets were lost before we were
    // watching. None of that happened *now*, so none of it is reportable.
    expect(t.push(stats({ rtp_lost: 5000, frames_withheld: 200 }))).toBe("ok");
    expect(t.delta().lost).toBe(0);
  });

  it("stays quiet through a single lost packet", () => {
    const t = new HealthTracker();
    t.push(stats({ rtp_lost: 0 }));
    // One bad window, then clean. This is rule 2: one packet never flashes.
    expect(t.push(stats({ rtp_lost: 1 }))).toBe("ok");
    expect(t.push(stats({ rtp_lost: 1 }))).toBe("ok");
  });

  it("warns only after sustained damage", () => {
    const t = new HealthTracker();
    t.push(stats({ rtp_lost: 0 }));
    for (let i = 1; i < TRIGGER_WINDOWS; i++) {
      expect(t.push(stats({ rtp_lost: i * 10 }))).toBe("ok");
    }
    expect(t.push(stats({ rtp_lost: TRIGGER_WINDOWS * 10 }))).toBe("degraded");
  });

  it("never warns about repair alone, however much of it there is", () => {
    const t = new HealthTracker();
    const c = counters();
    feed(t, c, 1);
    // S30's acceptance runs: 2,242 repaired, 0 lost, picture clean. If this
    // ever reports "degraded", the indicator is lying on a good stream.
    feed(t, c, 20, { rtp_recovered: 120 });
    expect(t.current()).toBe("coping");
  });

  it("treats withheld frames as damage even with no loss", () => {
    const t = new HealthTracker();
    const c = counters();
    feed(t, c, 1);
    // The picture is frozen waiting for a keyframe. The packet counters look
    // fine; the user is staring at a stalled image.
    feed(t, c, TRIGGER_WINDOWS, { frames_withheld: 3, keyframe_requests: 1 });
    expect(t.current()).toBe("degraded");
  });

  it("clears more slowly than it triggers", () => {
    const t = new HealthTracker();
    const c = counters();
    feed(t, c, 1);
    feed(t, c, TRIGGER_WINDOWS, { rtp_lost: 10 });
    expect(t.current()).toBe("degraded");

    // A quiet gap inside a burst must not clear it.
    feed(t, c, CLEAR_WINDOWS - 1);
    expect(t.current()).toBe("degraded");

    feed(t, c, 1);
    expect(t.current()).toBe("ok");
  });

  it("does not report a restarted engine's counters as damage", () => {
    const t = new HealthTracker();
    t.push(stats({ rtp_lost: 900, rtp_recovered: 900 }));
    // A new engine starts at zero. Naive subtraction gives -900, and treating
    // that as a delta would be nonsense in either direction.
    expect(t.push(stats({ rtp_lost: 0, rtp_recovered: 0 }))).toBe("ok");
    expect(t.delta().lost).toBe(0);
  });

  it("forgets the previous share on reset", () => {
    const t = new HealthTracker();
    const c = counters();
    feed(t, c, 1);
    feed(t, c, TRIGGER_WINDOWS, { rtp_lost: 10 });
    expect(t.current()).toBe("degraded");

    t.reset();
    expect(t.current()).toBe("ok");
    // And the first sample of the next share is a baseline, not a delta.
    expect(t.push(stats({ rtp_lost: 10_000 }))).toBe("ok");
  });

  it("ignores missing and malformed counters rather than throwing", () => {
    const t = new HealthTracker();
    // A sender's stats line has none of these fields at all.
    expect(t.push(stats({ fps: 60, bitrate_mbps: 40 }))).toBe("ok");
    expect(t.push(stats({ rtp_lost: undefined }))).toBe("ok");
    expect(t.push(stats({ rtp_lost: NaN as unknown as number }))).toBe("ok");
  });
});

describe("healthText", () => {
  it("says nothing unless the picture is actually degraded", () => {
    expect(healthText("ok", { lost: 0, recovered: 0, withheld: 0, keyframeRequests: 0 })).toBeNull();
    expect(healthText("coping", { lost: 0, recovered: 999, withheld: 0, keyframeRequests: 0 }))
      .toBeNull();
  });

  it("never blames the user's network", () => {
    // The first time this was measured the cause was Relay's own 64 KB
    // receive buffer on a 0.2 ms LAN. Naming the network would have sent
    // someone to reset a router that was working perfectly.
    const all = [
      healthText("degraded", { lost: 5, recovered: 0, withheld: 0, keyframeRequests: 0 }),
      healthText("degraded", { lost: 0, recovered: 0, withheld: 2, keyframeRequests: 1 }),
      healthText("degraded", { lost: 5, recovered: 0, withheld: 2, keyframeRequests: 1 }),
    ].join(" ").toLowerCase();
    expect(all).not.toMatch(/network|wi-?fi|router|connection is|your internet/);
  });

  it("distinguishes a frozen picture from a breaking one", () => {
    const frozen = healthText("degraded", { lost: 0, recovered: 0, withheld: 4, keyframeRequests: 1 });
    const breaking = healthText("degraded", { lost: 9, recovered: 0, withheld: 0, keyframeRequests: 0 });
    expect(frozen).toMatch(/paused/);
    expect(breaking).not.toMatch(/paused/);
  });
});
