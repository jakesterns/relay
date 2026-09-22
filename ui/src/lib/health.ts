/** Stream health, derived from the receiver's `stats` line (S31).
 *
 *  The engine reports cumulative counters every 500 ms. What a user needs to
 *  know is not a total but a *state*: is the picture being damaged right now,
 *  and has it stopped. So this turns successive samples into deltas, and
 *  deltas into a state that is deliberately slow to appear and slower to
 *  clear.
 *
 *  Three rules shape everything here, all of them from real runs:
 *
 *  1. **Warn on `rtp_lost` and `frames_withheld`, never on `rtp_recovered`.**
 *     On S30's acceptance runs `rtp_recovered` was 970 and 2,242 while
 *     `rtp_lost` stayed at 0 and the picture was clean. Recovery is Relay
 *     working, not Relay failing; showing it as a warning would train the user
 *     to ignore the one indicator that matters.
 *  2. **A single lost packet must never flash anything.** Loss is bursty, and
 *     an indicator that blinks on one packet is noise the user learns to
 *     ignore. It takes sustained damage to appear.
 *  3. **Clearing is slower than triggering.** Loss arrives in bursts with
 *     quiet gaps inside them; clearing at the first clean sample would
 *     flicker the indicator through a single bad patch.
 */

import type { ShareStats } from "./ipc";

/** What the picture is doing, in the order of how much the user should care. */
export type HealthState =
  /** Nothing wrong, or nothing measured yet. */
  | "ok"
  /** The link is losing packets and Relay is repairing them in time. The
   *  picture is fine. Never warned about; visible only in the readout. */
  | "coping"
  /** Packets are being lost outright, or frames are being held back waiting
   *  for a keyframe. The picture is, or is about to be, visibly wrong. */
  | "degraded";

/** Consecutive 500 ms windows with damage before `degraded` shows. Two windows
 *  is about a second — long enough that a burst has to persist, short enough
 *  that the user is told while it is still happening rather than afterwards. */
export const TRIGGER_WINDOWS = 2;
/** Consecutive clean windows before `degraded` clears. Deliberately longer
 *  than the trigger: loss comes in bursts with quiet gaps inside them, and
 *  clearing on the first clean sample would flicker through one bad patch. */
export const CLEAR_WINDOWS = 6;

/** A sample's worth of change, in the terms the user cares about. */
export interface HealthDelta {
  lost: number;
  recovered: number;
  withheld: number;
  keyframeRequests: number;
}

const ZERO: HealthDelta = { lost: 0, recovered: 0, withheld: 0, keyframeRequests: 0 };

/** Counters we diff between samples. Cumulative and monotonic in the engine. */
type Counters = Pick<
  ShareStats,
  "rtp_lost" | "rtp_recovered" | "frames_withheld" | "keyframe_requests"
>;

function num(v: number | undefined): number {
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

/** Tracks health across samples. One per share; `reset()` between shares.
 *
 *  Deliberately a small state machine rather than a hook: the decision of
 *  *what* the stream is doing is separable from how it is drawn, and this way
 *  it can be tested by feeding it samples with no DOM in sight.
 */
export class HealthTracker {
  private prev: Counters | null = null;
  private badRun = 0;
  private goodRun = 0;
  private state: HealthState = "ok";
  private last: HealthDelta = ZERO;

  /** Forget everything. Call when a share ends, so the next one does not
   *  inherit a stale `degraded` or diff against counters from a dead engine
   *  — a new engine restarts its counters at zero, which would otherwise read
   *  as a huge negative delta. */
  reset(): void {
    this.prev = null;
    this.badRun = 0;
    this.goodRun = 0;
    this.state = "ok";
    this.last = ZERO;
  }

  /** Feed one `stats` line. Returns the state to display now. */
  push(s: ShareStats): HealthState {
    const cur: Counters = {
      rtp_lost: num(s.rtp_lost),
      rtp_recovered: num(s.rtp_recovered),
      frames_withheld: num(s.frames_withheld),
      keyframe_requests: num(s.keyframe_requests),
    };

    // The first sample establishes a baseline and cannot be a delta. Joining
    // a share already in progress would otherwise report every packet lost
    // since it started as though it had just happened.
    if (this.prev === null) {
      this.prev = cur;
      this.last = ZERO;
      return this.state;
    }

    // Counters only ever climb. A drop means the engine restarted, so
    // re-baseline instead of reporting a negative delta as damage.
    const diff = (k: keyof Counters): number => {
      const d = num(cur[k]) - num(this.prev![k]);
      return d < 0 ? 0 : d;
    };
    const delta: HealthDelta = {
      lost: diff("rtp_lost"),
      recovered: diff("rtp_recovered"),
      withheld: diff("frames_withheld"),
      keyframeRequests: diff("keyframe_requests"),
    };
    this.prev = cur;
    this.last = delta;

    // Withheld frames count as damage even without loss: the picture is
    // frozen while Relay waits for a keyframe, which the user sees as a stall
    // whatever the packet counters say.
    const damaged = delta.lost > 0 || delta.withheld > 0;
    if (damaged) {
      this.badRun += 1;
      this.goodRun = 0;
    } else {
      this.goodRun += 1;
      this.badRun = 0;
    }

    if (this.state === "degraded") {
      if (this.goodRun >= CLEAR_WINDOWS) this.state = delta.recovered > 0 ? "coping" : "ok";
    } else if (this.badRun >= TRIGGER_WINDOWS) {
      this.state = "degraded";
    } else {
      // Repair with no loss is not a warning state, so it is free to change
      // as fast as it likes — nothing flashes.
      this.state = delta.recovered > 0 ? "coping" : "ok";
    }
    return this.state;
  }

  /** The most recent window's change, for the readout. */
  delta(): HealthDelta {
    return this.last;
  }

  current(): HealthState {
    return this.state;
  }
}

/** What to tell the user, in plain language.
 *
 *  Never blames the user's network. When this was first measured the cause was
 *  Relay's own 64 KB receive buffer on a LAN with 0.2 ms round trip, and a
 *  message accusing the network would have sent someone to reset a router that
 *  was working perfectly.
 */
export function healthText(state: HealthState, d: HealthDelta): string | null {
  if (state !== "degraded") return null;
  if (d.lost > 0 && d.withheld > 0) {
    return "The picture is breaking up — some video is arriving damaged and cannot be repaired.";
  }
  if (d.withheld > 0) {
    return "The picture is paused while Relay waits for a clean frame from the other PC.";
  }
  return "Some video is not arriving. The picture may smear or freeze briefly.";
}
