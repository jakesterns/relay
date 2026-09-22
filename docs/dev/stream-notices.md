# How Relay talks about a stream going wrong

Written by S31, to be adopted by S38 (crash restore) rather than reinvented.
Two sessions describing the same four situations in two vocabularies would
give one app two personalities, and the user would have to learn both.

## The four states

There are only four things that can be wrong with a stream, and the user can
act on at most two of them.

| State | What happened | What the user sees | Can they act? |
|---|---|---|---|
| **Coping** | Packets are being lost and repaired in time. | A line in the readout. Never a warning. | No — nothing is wrong. |
| **Degraded** | Packets lost outright, or frames held waiting for a keyframe. | Amber chip in the corner of the picture. | Sometimes (move closer to the router, use a cable). |
| **Ended** | The share stopped — either end, deliberately. | The video area says who it was and that it ended. | Yes: start another. |
| **Dropped** | The share died without being stopped. | Same place as *ended*, different words, plus whatever recovery is happening. | Yes: wait, or retry. |

## Rules

1. **Never warn about recovery.** `rtp_recovered` climbing while `rtp_lost`
   stays at zero is Relay working. S30 measured 2,242 repaired packets with a
   clean picture. An indicator that fires on that teaches the user to ignore
   the indicator that matters.
2. **Nothing flashes.** Warnings need sustained evidence to appear
   (`TRIGGER_WINDOWS`) and more to clear (`CLEAR_WINDOWS`). Clearing is always
   slower than triggering, because loss is bursty and a quiet gap inside a
   burst is not a recovery.
3. **Never blame the user's network unless the evidence says so.** The first
   time this was measured the cause was Relay's own 64 KB receive buffer on a
   LAN with 0.2 ms round trip. "Check your connection" would have sent someone
   to reset a router that was working perfectly.
4. **Never obscure the picture being described.** The chip is a corner, not a
   banner. The user is judging the very thing a banner would cover.
5. **A state that is over leaves nothing behind.** When a share ends, the
   pairing code, the "Paired" status, the health chip and the readout all go.
   A stale code that no longer works is worse than no code, and a frozen last
   frame is how an ordinary share end got reported as a freeze.
6. **Say what happened, not what it means for Relay.** "The share from
   studio-pc ended" — not "receive engine exited".

## Where each one is drawn

- **Readout** (`Stream health` card, Receive screen): fps, bitrate, latency,
  audio delay, repaired, lost. The instrument-strip idiom. Present whenever a
  share is running; absent otherwise.
- **Chip** (`.healthchip`, bottom-left inside the video area): degraded only.
- **Video area text** (`.idlemsg`): ended and dropped.
- **Toasts**: not for stream state. They are for things that already happened
  and need no answer; a stream problem is ongoing and belongs next to the
  stream.

## For S38

Crash restore adds one state, not a new vocabulary: *dropped* becomes
"dropped, and Relay is reconnecting". Reuse the video-area text, and keep rule
5 — if a reconnect fails for good, clear the recovery message rather than
leaving it spinning forever. Rule 2 applies to reconnect attempts as well: do
not announce a retry that has not yet failed.
