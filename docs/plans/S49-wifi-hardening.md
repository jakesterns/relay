# S49 — Wi-Fi hardening

**Branch** `feat/s49-wifi-hardening` · cut from `main` at `d3a4bc8`

CLAUDE.md's key risk 5: Wi-Fi at 4K60 is unreliable, so surface a wired/6E
recommendation and degrade gracefully. Before S49 Relay detected a Wi-Fi route
on the sender (`netcheck::link_kind_for`), printed a `link` line nothing read,
and otherwise behaved as if every link were the wired LAN it was tuned on.

No test PC has Wi-Fi. Everything here was built against, and proven on, a
**Wi-Fi link model** run under real peer connections on one PC. What that can
and cannot prove is in "Limits", and the plan for a real-Wi-Fi pass is at the
end.

## What was built

### 1. Link classification (`transport/netcheck.rs`)
- Each end classifies the adapter that owns its route to the other PC:
  `GetAdaptersAddresses` `IfType` 71 = Wi-Fi, 6 = Ethernet, plus the adapter's
  link speed (for Wi-Fi, the PHY rate the radio negotiated).
- Wi-Fi band and generation from `wlanapi`, read-only and **without location
  consent**: only `wlan_intf_opcode_channel_number` and
  `wlan_intf_opcode_realtime_connection_quality` (centre frequency, PHY type).
  The current-connection opcode and the BSS list carry the SSID and, since
  Windows 11 24H2, raise a location prompt; they are never called.
  `wlanapi.dll` is loaded on demand, so a PC without it still starts.
- 6 GHz reuses channel numbers 1–233, so a channel alone is only trusted where
  it is unambiguous; otherwise the band is left unsaid ("Wi-Fi (Wi-Fi 6,
  1.2 Gb/s)") rather than guessed.
- Pure parser, fixture-tested (`transport/fixtures/adapters-*.json`: a laptop
  on Wi-Fi, a wired desktop, a docked laptop with both up).
- **Exchanged in signalling**: `Offer.link` / `Answer.link` (optional fields; a
  pre-S49 peer ignores them). Their presence is also the capability flag for
  the new messages, so an older peer is never sent one. `SigMsg` gained
  `#[serde(other)] Unknown`: before S49 an unknown message type was a parse
  error that ended the receiver's share.
- Shown as "Wi-Fi (5 GHz, 866 Mb/s)" in each end's `stats` line (`link`,
  `peer_link`).

### 2. The Wi-Fi link model (`transport/netsim.rs`, `netio::SimSocket`)
Pure and seeded; applied to arriving RTP/RTCP (never STUN/DTLS) on both ends'
media sockets by `RELAY_TEST_NET=<profile>[,overrides]`, or per peer connection
in tests (`build_pc_with`). Per packet, in the order a real link applies them:

| Effect | Model |
|---|---|
| Capacity | a bottleneck rate that drops to `low` for `low_for` every `period`; the AP queue tail-drops past `queue` ms |
| Holds ("spikes") | Poisson, 5–80 ms, everything behind waits (802.11 delivers in order) |
| Stalls | 100–300 ms with nothing delivered, every 6–35 s (channel scans) |
| Jitter | uniform per packet, FIFO |
| Radio loss | Gilbert-Elliott: bursts, not independent loss |

Profiles: `wired` (no-op), `wifi-good` (5 GHz same room), `wifi-busy` (busy
home: ~1 % bursty loss, 2 spikes/s to 80 ms, stalls every 10–20 s, 120→30 Mb/s
for 8 s every 30 s), `wifi-bad` (2.4 GHz through walls: 40→12 Mb/s, ~3 % loss),
`capacity-drop` (200→20 Mb/s, nothing else). The model holds 1 ms timer
resolution while it runs, so its millisecond delays are not rounded to
15.6 ms Windows ticks.

`RELAY_TEST_SOURCE=noise[:WxH]` is a synthetic capture source: a scrolling
block-noise field with a band of new detail sweeping down it, so the encoder
works every frame and follows its target, and nothing on the owner's screen is
captured.

### 3. Hardening

**Delay + loss bitrate control** (`control::RateControl`). The S30 loss
controller is kept, untouched, fed the same 1 s windows — so on a wired link,
where the queue is always empty, the output is identical (unit test
`with_no_queue_it_is_the_loss_controller_exactly`). The delay half reads a new
receiver report every 250 ms (`SigMsg::Feedback`):
- the **standing queue**: the smallest queueing delay any frame saw in the
  window (CoDel's signal), measured from each frame's *first* packet — read
  from its SEI capture stamp on arrival, so a frame whose tail is lost still
  counts;
- whether the queue rose or fell inside the window (first vs last frame);
- the rate the link actually carried (every delivered packet, not only whole
  frames — under loss most frames are incomplete).

Back off quickly: two reports over 15 ms, or one over 40 ms that is not
draining, cut to 85 % of what arrived — and deeper when a queue is standing,
so it drains in ~0.5 s (`DRAIN_MS`). No second cut while the queue drains.
Recover slowly: hold 3 s, then +5 %/s, stopping at 95 % of the rate that built
the queue for 30 s.

**Resolution / fps ladder** (`ladder.rs`): 2160p60 → 1440p60 → 1080p60 →
1080p30, never above what was asked for. Down when the target has sat below
the rung's floor (0.045 bpp HEVC, 0.065 H.264) for 3 s (0.5 s when under half of it: the encoder cannot honour the target at that size at all) *and* the controller
has backed off — a user who chose 4K60 at 20 Mb/s on a wired LAN is never
stepped down. Up after 15 s with 40 % headroom; a step up undone within 60 s
doubles the wait (to 4 min). The sender rebuilds the encoder at the new size
(32–55 ms measured) and the recorder rolls to a new file; the receiver rebuilds
its decoder on the keyframe and scales back to the stream's original size, so
its window, swapchain and **Relay Camera never see the size change** (a call
app reading the camera keeps its format). Only with an S49 receiver, and not
while this PC's own Relay Camera is on.

**Playout buffer** (`playout.rs`): holds each frame by the p90 of the last 2 s
of arrival jitter, bounded at 50 ms, off below 4 ms. On a wired link it stays
off (0 ms added). It does not try to hide a 300 ms stall.

**Keyframe pacing** (`pacing.rs`): while constrained, a frame 3× the recent
average (a keyframe) is spread at 2× the target over at most 3 frame
intervals; ordinary frames are never delayed. Keyframe requests are coalesced
(one per 400 ms) and a forced keyframe waits out a draining queue (up to
300 ms). Wired: unchanged, every request answered at once, nothing paced.

**Retransmission tuning — justified by the model:**
- `NACKS_PER_PACKET` 4 → 12: four asks span 40 ms, Wi-Fi repairs often take
  longer; the reorder hold now adapts 40–150 ms (`reorder::HoldTuner`, grows
  when retransmissions arrive after a give-up, shrinks when they stop).
  Wired: the first answer lands in < 1 ms, so the extra asks are never sent.
- A retransmission budget while constrained (`feedback::RetransmitBudget`:
  15 % of the target, ≥ 1 Mb/s) — B20.
- FEC: not added. After the fixes below, `wifi-good` lost 1 packet in a minute
  at 4K60; the residual loss is in capacity collapses and stalls, which FEC
  overhead would make worse, not better.

**Bugs the model found** (all in `docs/dev/BUGS.md`):
- **B19** — the reorder buffer held burst-loss holes end to end (30 holes =
  1.2 s frozen). Now each hole is held from when it was seen.
- **B20** — retransmissions crowded out the video when capacity fell
  (85 Mb/s arriving against a 2.5 Mb/s target).
- **B21** — a post-stall burst overflowed webrtc-rs's 256-packet track queue
  (400 packets dropped on a link that had lost none). A dedicated drain thread
  now empties it.
- The first pacer (a token bucket over every packet) throttled the sender
  below the encoder's output and the controller chased a queue it had made;
  and a report window ending mid-way through a stall's release looked like a
  severe queue. Both fixed before they shipped.

### 4. UX
When either PC is on Wi-Fi: a **Link** cell on the Share strip and Link /
Other PC rows in the Receive health card ("Wi-Fi (5 GHz, 866 Mb/s)"), and one
calm line — "This PC is on Wi-Fi. For steady 4K60, Ethernet or Wi-Fi 6E holds
up best. Lowered to 1440p60 to stay smooth." It names the link and what Relay
is doing, never the user. On a wired share nothing renders (component tests
assert both). The receiver learns what the sender is doing from
`SigMsg::Adapt`.

## Measured

All runs 2026-10-07 on the main PC (Windows 11, RTX 3090, NVENC HEVC),
loopback, release build, `scripts/wifi-sim-check.sh` (synthetic source,
headless receiver). "Stalls" are gaps over 100 ms between frames as the
playout buffer would show them; "shown" latency is capture to that moment.
Smear is zero by construction in every row: a frame that depends on a damaged
one is never decoded (B15's withholding), so a bad link shows as a pause, not
corrupted video.

### Wired regression (A/B against `main`)
`base` is `origin/main` at `d3a4bc8` with only the test source added. Same
settings as the S32 two-PC matrix.

| Encode | Target | Sent (S49 / main) | Latency p50 / p99, S49 | main | Lost | Stalls | Target moved | Playout added |
|---|---|---|---|---|---|---|---|---|
| 1920x1080 @ 60 | 25 | 24.3 / 24.3 | 4.1 / 5.1 ms | 4.1 / 5.0 | 0 | 0 | no | 0 ms |
| 2560x1440 @ 60 | 40 | 38.9 / 38.9 | 6.5 / 8.5 ms | 6.5 / 8.6 | 0 | 0 | no | 0 ms |
| 3840x2160 @ 60 | 60 | 58.8 / 58.8 | 12.3 / 16.2 ms | 12.4 / 16.2 | 0 | 0 | no | 0 ms |
| 3840x2160 @ 30 | 40 | 39.8 / 39.8 | 12.8 / 24.8 ms | 12.9 / 25.1 | 0 | 0 | no | 0 ms |

Identical within noise: no rung change, no bitrate move, no pacing, playout
buffer off, retransmission budget unlimited.

### Wi-Fi model, after the fixes
| Profile | Asked | Rungs | Target min → end | Stalls / min | Longest gap | Shown p50 / p99 | Lost (unrepaired) / repaired holes |
|---|---|---|---|---|---|---|---|
| wifi-good | 4K60 60 | — | 60 → 60 | 3 | 171 ms | 30 / 61 ms | 4 / 215 |
| wifi-good | 1440p60 40 | — | 40 → 40 | 3 | 170 ms | 23 / 55 ms | 2 / 189 |
| capacity-drop (200→20) | 4K60 60 | 1440p60, 1080p60 | 6.8 → 12.7 | 3 | 1081 ms | 28 / 89 ms | 2965 / 0 |
| wifi-busy | 4K60 60 | 1440p60 | 11.7 → 14.1 | 15 | 1129 ms | 65 / 199 ms | 3178 / 1014 |
| wifi-busy | 1440p60 40 | — | 11.3 → 11.3 | 12 | 754 ms | 62 / 123 ms | 954 / 958 |
| wifi-bad | 1440p60 40 | 1080p60, 1080p30 | 2.5 → 3.0 | 36 | 1173 ms | 67 / 276 ms | 4380 / 722 |
| wifi-bad | 1080p60 25 | 1080p30 | 2.5 → 5.2 | 37 | 1452 ms | 65 / 199 ms | 1674 / 649 |

Encoder rebuild for a rung change: 32–55 ms. CI runs two kinds of test.
`transport::loopback_tests` puts real peer connections on real sockets and
checks only plumbing (frames and reports flow, wired loses nothing): an
earlier version asserted timings there and failed on a loaded PC.
`transport::sim_tests` drives the link model, receiver measurement, controller,
ladder, pacing and keyframe gate on one simulated clock, deterministically:
wired never adapts; a 200→15 Mb/s drop is cut within 0.5 s (to 11 Mb/s), one
~1 s freeze, a step to 1080p60, and the target climbs back after. (It has no
retransmission, so its frame counts on lossy profiles are not meaningful.)

### What the fixes bought (same profiles, before → after)
| Run | Stalls | Longest gap | Unrepaired loss |
|---|---|---|---|
| wifi-good 4K60 (B21 drain thread) | 11 → 3 | 1079 → 171 ms | 1441 → 4 |
| capacity-drop 1440p60 (B19, B20, drain-aware cut, keyframe hold) | 43 → 1 | 2182 → 719 ms | 2765 → 911 |
| capacity-drop 4K60 (deep-starvation ladder step) | 6 → 3 | 3474 → 1081 ms | 6886 → 2965 |

Reading it honestly: on a good link the share is smooth at full 4K60 with
nothing visible beyond the scan stall the model inserts. A sudden 10× capacity
collapse costs about one second of frozen picture, then the share settles at
what the link carries. The busy and bad profiles are hostile by design (80 ms
holds twice a second, 300 ms stalls, bursty loss at 1-3 %); Relay keeps them
moving, bounded and smear-free, but at a stall every 4-5 s and 60-70 ms of
latency they are not smooth, and the note telling the user that Ethernet or
6E holds up best is the right answer there.

## Limits

- **A model, not a radio.** The profiles are plausible, not recorded. They
  prove the mechanisms (detect, back off, recover, step, smooth, repair) and
  the bugs above, not how a particular access point behaves.
- Loopback on one PC: both ends share a CPU, GPU and clock. The receiver is
  headless; smoothness is judged on the playout timeline it computes, not on a
  screen. The windowed path's decoder rebuild and scale-back are covered by
  `tests/rung_change.rs` on the real GPU pieces, but nobody has watched a rung
  change on screen yet.
- The synthetic source is harder than most desktops and easier than some
  games; the NVENC encoder overshoots low targets on it at 1440p+ (6 Mb/s asked,
  ~10 sent), which is part of why the ladder steps down.

## Real-Wi-Fi test plan (owed: needs a Wi-Fi adapter on either PC)

1. **Classification.** On a laptop: `relay-share send` to the desktop; the
   `link` line and the strip must read the band and rate Windows shows in
   Settings → Wi-Fi → Properties. Repeat on 2.4, 5 and (if available) 6 GHz.
   Confirm no location prompt appears on Windows 11 24H2+.
2. **Good link, same room, 5 GHz.** 4K60 at 60 Mb/s for 10 min. Pass: no
   rung change, target ≥ 50 Mb/s, ≤ 1 stall/min, playout ≤ 30 ms. Record the
   receiver's `summary` and the sender's `bitrate target changed` lines.
3. **Distance / walls.** Walk the laptop away until the rate in Settings
   falls below ~200 Mb/s. Expect a cut within 1 s, a step to 1440p60 within
   ~5 s, and the note on both screens; walk back and expect the step up
   within ~1 min.
4. **Contention.** Start a large download on another device on the same AP
   mid-share. Expect cuts, no freeze longer than ~1 s, recovery when it ends.
5. **Scan stalls.** Leave Windows' Wi-Fi list open (it scans). Expect isolated
   ~100-300 ms stalls, no smear, no rung change.
6. **Wired regression on the same build**: the S32 matrix rows, numbers
   within noise of `docs/dev/resolution-matrix.md`.
7. Real-link numbers replace the model's in this file; the profiles in
   `netsim.rs` are re-fitted to what was measured.

## Commands

```
cargo test -p relay-capture --lib transport::        # unit + in-process loopback
cargo test -p relay-capture --lib loopback -- --ignored --nocapture   # busy soak
cargo test -p relay-capture --test rung_change       # GPU: encode/decode/scale at two sizes
RELAY_SHARE_BIN=target/release/relay-share.exe scripts/wifi-sim-check.sh wifi-busy 90 2560x1440 60 40
BASE_BIN=<main build> RELAY_SHARE_BIN=... scripts/wifi-sim-matrix.sh 60
```
