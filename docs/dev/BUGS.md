# Open bugs

Found by running Relay on two real machines (2026-09-16/17): a Windows 11 dev
box and a Windows 10 PC that had never seen the code. Almost everything here
was invisible on the dev box.

Rules for this file: a bug stays open until someone has *seen* it fixed, not
until the change compiles. Where a symptom was misattributed, the correction
stays in the record — a wrong theory that looked right is worth remembering.

## Open

### B1 — Frame-rate numbers are not trustworthy
`video_up` reports the requested `fps=30`, but every `stats` line then reads
58–62. Either the counter measures capture rate rather than encode rate, or the
fps cap is not applied to the encoder at all. Seen on both two-PC runs.

**S27 (branch `feat/h264-fallback`): cause found and fixed, verified on loopback
only.** The counter was honest; nothing dropped frames, so the encoder ran at
display refresh while told 30 fps. Reproduced on the pre-S27 binary on loopback
(asked 30, encoded 24–58). `pace::FramePacer` now drops frames before
conversion; loopback reads 30.0 on every tick. Still to see on the second PC.

Matters because the 4K60 acceptance criterion is measured in exactly these
units — we cannot currently prove or disprove it. Fix the measurement before
trusting any fps number already recorded in the plans.

### B2 — ~~`relay-share` leaves no trace when it dies~~ FIXED 2026-09-17
It writes no log file. When the core spawns it, stdout is a pipe the core reads
for NDJSON, and anything that is not a recognised event shape is discarded;
stderr goes nowhere. On the Windows 10 PC there was no log, no WER entry and no
event-log record after the receiver died — the only reason we learned the cause
was running it by hand in a console.

The component doing the hardest work is the one that leaves nothing behind when
it fails on a user's machine.

### B3 — A fatal receiver error is reported as a timeout
The receiver's render thread failed 0.2 s after the first video frame and the
process exited cleanly. The sender only said "peer connection lost" 3.5 s
later, when ICE timed out. The receiver knew exactly what was wrong and did not
tell the peer.

It should close the peer connection with a reason, so the sender can report
"receiver: no HEVC decoder" immediately. Handed to S27, which is already in
`sender.rs`/`receiver.rs` for codec negotiation.

**S27: fix in the branch, not yet seen working.** The render thread's error now
reaches the sender as `SigMsg::Abort { reason }` before the receiver closes, and
the sender prints `{"event":"error","where":"receiver",...}`. A headless loopback
cannot fail the render path, so the first real test is the second PC.

### B4 — Firewall rule policy misses a disabled Private profile
S22 scopes Relay's rule to private + domain, never public — right for a
LAN-only app. But on a machine where the **Private profile is disabled** and
Public is enabled (Jake's dev box), the only profile actually enforcing is the
one we deliberately never touch. Windows confirmed this by offering *only* a
Public checkbox in its own prompt.

Today it happens to work because a disabled profile filters nothing, and
`relay-core firewall status` says so honestly. But the rule we add does not
cover the profile in force, and we should decide what that means rather than
rely on the coincidence.

### B5 — ~~`relay-core status` prints the foreground window title~~ FIXED 2026-09-17
The CLI status line echoes whatever window is in front — during testing that
was a browser tab title. Anyone pasting `status` output into a bug report or a
log leaks it. `relay-pc2` redacted it by hand, which is the only reason it did
not end up in this repo.

### B6 — The HEVC banner becomes wrong the moment S27 lands
The Receive banner currently says this PC "cannot show the shared screen". Once
H.264 negotiation exists, a receiver without HEVC will work fine, and a red
error claiming otherwise is worse than silence. It needs to degrade to an
informational note — HEVC would give better quality per bit where available —
rather than an error. Belongs with S27.

**S27: fixed in the branch.** Red only when neither H.264 nor HEVC decodes;
otherwise a plain note that shares use H.264. The paid-codec link is gone.
Covered by UI tests; still to see on the Windows 10 PC.

### B8 — A windowed `relay-share recv` never exits  |  MITIGATED 2026-09-17, bounded 2026-09-18, not cured
After the sender stops it logs "connection closed" and keeps running. Found by
S27 during loopback work and reproduced against a pre-S27 binary, so it
predates the codec work.

Hidden in normal use because the core kills the child, which is exactly why it
survived this long. Anyone running the binary by hand — as we did for the
Windows 10 diagnosis — leaves a process holding an open render window.

*S29, 2026-09-18:* it bit again on the second PC, in normal use: after the
sender stopped, the receiver's render thread ended on the 3 s idle timeout
but the process stayed resident, so the core never reported the end and the
app kept a stale pairing code and a black window (run 3). `bef8617` adds a
3 s deadline to the receiver's teardown, after which it prints `stopped`
and exits; run 4 showed the deadline firing every time, i.e. the teardown
still hangs (`pc.close()` / the AU sender outliving it) and every share end
now costs 3 s. The cure is still owed.

### B9 — Relay will happily capture its own render window  |  FIXED 2026-09-17, VERIFIED on hardware 2026-09-18
2026-09-17, on the dev box: loopback runs left a receiver window on the display
the sender was capturing, so the capture contained the window showing the
capture. Jake's description was "an infinite loop of whatever is on my screen,
like smearing a painting repeatedly", and with B8 keeping the window alive it
did not stop on its own. His machine was unusable until the processes were
killed.

Not just a test-harness problem: a user receiving on the same PC they share
from hits it, and so does anyone trying Relay against itself to see what it
does. Nothing in the product prevents it.

The fix is to exclude our own window from capture rather than to rely on nobody
doing this — `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` on the
receiver's render window, which makes it invisible to WGC and Desktop
Duplication alike. Worth checking the preview thumbnail path for the same
exposure.

*Verified 2026-09-18 (S29 run 2):* while the dev box shared its desktop to
the second PC, a pattern stub was drawing into the receiver window class on
the dev box. Jake saw the Relay window in the stream but not the surface
inside it; everything else on the desktop came through. The exclusion holds
on a real capture, in the embedded (owned popup) state, at 60 fps.

### B7 — Unverified: did a receiver window ever appear?
On the Windows 10 PC the render thread died 0.2 s after the first frame. Nobody
established whether a window appeared first and vanished, or never appeared at
all. A window that flashes and disappears reads as a crash to a user even when
the failure is handled. Needs eyes on the screen.

### B10 — The picture freezes a few seconds into a share
Seen on the second PC on the first share that actually displayed: video
appeared, then froze on one frame within seconds and stayed there for the rest
of the 30 s run. Audio behaviour at the same moment is unknown (see B11).

`share.log` had no line at all between "decoder up" and the disconnect 33 s
later — no warning, no error, no stall — so there was no way to tell whether
access units stopped arriving, stopped decoding, or stopped reaching the
screen. Instrumented rather than fixed: `video_presented` is now counted
separately from `video_aus` and both are logged every 500 ms, with a warning
when either stops moving. The next run should say which.

The sender saw none of it: 892 frames, zero dropped, for the full 30 s.

### B11 — A long continuous beep from the receiver's speakers
Started when the share started, described as loud and constant. The sender was
sending real audio (2,999 packets, peak 0.091), so this is the receiver's
playback path rather than the source — a stale buffer repeating, or an
underrun turning into a tone. Unknown whether it began at connect or at the
moment the picture froze, which would tie it to B10.

Worse than it sounds: it is the first thing a user hears from Relay, through
whatever their speakers are set to.

### B12 — Installers are not byte-reproducible
Two builds of the same commit from different worktrees ship different files:
`relay-preview.exe` gets cargo-cached when its source has not changed, and
`autoeq-index.tsv` picks up per-worktree line endings (8,849 records either
way — Rust's `.lines()` strips `\r`). Harmless today, but it must be fixed
before anything is signed: a signature over a build nobody can reproduce is
worth very little.

*S29, 2026-09-17:* a third instance. `pnpm tauri build` rewrote
`target
elease
elay-ui.exe` 12 ms *after* writing the NSIS installer, so
the exe inside the installer (`4A2E2BA4...910E`) differs from the one left on
disk (`2E0B8748...45DB`). Hash the installer, or extract from it; never the
loose `relay-ui.exe`.

### B13 — The received stream played in a window of its own  |  S29, two-PC pass run 1 done, close-popout fix in r6
Jake, after the first real two-PC test: the picture belongs *inside* the
Relay window, in the Receive screen's video area, with a pop-out like
Discord's. The old Receive screen painted an empty frame captioned "Playing
in a separate window", and the window itself was a top-level `relay-share`
window with its own message pump on the decode thread, so dragging it stalled
the picture (B10's cousin: a stall that `presented` does not show because
nothing is presented while the modal loop runs).

**S29 (branch `feat/inapp-stream`), design:** the engine's window becomes a
frameless popup *owned by* the app window and the shell keeps it over the
video area; pop-out is the same window with a frame and no owner; closing
the popped-out window puts it back. Decode/present moved off the window
thread. Not a `WS_CHILD`, because the B9 capture exclusion only holds on
top-level windows of the owning process — the engine reasserts it after every
mode change and reports the verified value. Full write-up:
`docs/dev/inapp-stream.md`.

**Local pass, 2026-09-17, dev box, pattern stub (`RELAY_RECEIVE_STUB=1`),
commit `256661b`:** embedded placement exact to the pixel;
owner/styles as designed; pop-out, close-to-re-embed, two shell resizes,
navigate-away (hidden) and back (shown, page state restored), stop (window
gone, "The share from host-stub ended."); `excluded_from_capture=true` after
every transition; present counter climbing throughout. What the stub cannot
show: a real modal drag of the app window, real decode and audio, Windows 10.

**Two-PC pass, run 1, 2026-09-18 00:04-00:08 UTC, r5 = `256661b`, sender
this dev box (main `3036081`), receiver the Windows 10 PC via relay-pc2:**
- Embedded: picture clear and inside Relay, no second window; "little to no
  latency" (Jake). `receiver window up mode="embedded" excluded=true`.
- Drag/resize for ~10 s: no freeze; no "receiver stalled" line anywhere in
  the run; `presented` climbed 30 per 500 ms throughout (60 fps).
- Pop out and "Bring back into Relay": work.
- **Closing the popped-out window (X/Esc) did not reliably re-embed.** Four
  popout->embedded transitions took 7.2 s, 1.6 s, 46.8 s and 9.3 s; Jake
  clicked several times. The failed attempts left no log line. The
  re-embed was a four-hop chain (engine `host_close` -> core -> shell ->
  core `HostReceive` -> engine), and the engine could not take the
  foreground when popping out (`SetForegroundWindow` refused to a process
  without the last input), so Esc went to the app. Fix in r6: the engine
  re-embeds itself on close using the owner it remembers, the shell hands
  the popped-out window the foreground, and every close/Esc and host command
  is logged.
- `aus == presented` all run (12604/12603): the double count is gone.
- Audio: none playing, none heard, no beeping (B11 not reproduced).
- Not reached: Settings navigation, minimise/restore (run 2).
- Ended by Jake's Stop receiving at 00:08:31 (his clock); clean exit.

**Run 2, 00:11-00:16 UTC, still r5:** X-close re-embedded fine (1.1 s,
1.6 s); Esc came back as a *black* video area for ~11 s before the picture
returned (10.9 s, 11.5 s), with `presented` climbing throughout — frames were
presented to a window the app had shown before the engine restyled it, or
that DWM had not yet re-composed. Settings-and-back and minimise/restore
(embedded and popped out) both fine. Two new findings: a few-second
"smeared paint" corruption mid-stream while frames kept arriving and
presenting (B15), and the sender's capture showing the Relay window but not
the pattern stub inside it, which is B9 working — see B9.

**Run 3, 00:21-00:26 UTC, r6 = `44e6be7`:** every close re-embedded on
the engine side in 6.6-11 ms (X and Esc alike, all logged), yet the video
area stayed *pure black* — no text, not even the popped-out caption — on
every attempt, and tab-switching did not recover it. `presented` kept
climbing throughout. When the sender stopped, the engine logged the 3 s
idle end correctly but `relay-share.exe` stayed resident, so the core never
reported the end: no "share ended" text, and the *old pairing code stayed
on screen* until Jake stopped and started receiving again. Video verdict
otherwise: "super clear, little to no latency" 1440p->1080p. r8 (`bef8617`)
answers both: the render thread rebuilds its swapchain after every mode
change, the teardown gets a 3 s deadline after which the engine exits, and
the shell writes `logs/ui.log` so the app's side of a black area is on
record for the first time.

**Run 4, 2026-09-18 22:35-22:39 UTC, r8 = `bef8617`:** the app side is
proven right by `ui.log` — every close produced "stream window event
mode=embedded" then "apply: placed ... ok=true visible=true" at the correct
rect — and the engine side was the fault: `CreateSwapChainForHwnd` failed
with `E_ACCESSDENIED` on all seven mode changes because the *old* swapchain
still held the window when the new one was created. Pop-in stayed black
(X, Esc, Settings-and-back); minimise/restore fine (no rebuild involved);
the Relay icon on the popped-out title bar confirmed. **End of share by
sender stop now works**: "The share from jake ended.", Idle, code cleared,
`relay-share.exe` gone — but only because the 3 s teardown deadline fired
("teardown did not finish within 3 s; exiting now"), so B8's hang is still
there underneath and costs every share end 3 s. Loss for the run at 60 fps
/ 40 Mb/s: 70 gaps, 525 packets, two of 195 and 254 (B15). r9 (`600eeeb`)
releases the old chain and flushes the context before creating the new one;
on the stub every rebuild now succeeds in 2-10 ms.

**Audio run, 00:29-00:31 UTC, r6:** Windows text-to-speech on the dev box,
21 lines over 91 s. Clear on the second PC, no stutter, gap, dropped word or
pitch change; 100 packets/s steady, `aus == presented`, zero stalls. Jake
hears it about 1 s behind the dev box's own speakers: B16.

### B16 — Audio arrives about a second late, and nothing measures it
relay-pc2, audio run: intelligible and steady, but ~1 s behind the sender's
own speakers while the video reads as near-instant, so A/V sync is off by
about that much. The receiver logs no audio latency at all. Candidates: a
fixed jitter/depacketiser target, the WASAPI render buffer, Opus frame
accumulation before playback starts. Needs: audio arrival-to-render time
and render-buffer depth in the stats line, then a lip-sync check with
something on screen that flashes when a sound plays. Own session.

### B15 — Mid-stream smear: a damaged access unit is decoded and nobody asks for a keyframe
relay-pc2, run 2: "every now and then the stream shows a weird smeared-paint
colour screen, almost as if it broke for a few seconds", while `aus ==
presented` and no stall was logged. The receiver estimates packet loss over
1 s windows and sends it to the sender as bitrate feedback, but on an RTP
sequence gap it neither drops the partial access unit nor asks for a
keyframe, so the decoder predicts from a hole until the sender's next
periodic IDR. Fix needs both halves: discard-until-keyframe on the
receiver (the keyframe gate already exists) and a keyframe request over
the signalling channel that the sender answers with `keyframe_wanted`. A
sender-side protocol change, so its own session — the sender on the dev box
would otherwise break on an unknown message. `ac7b4c1` adds `rtp_gaps` /
`rtp_lost` to the stats and a log line per gap so the next smear can be
matched to one.

*Measured, 2026-09-18 (runs 5 and 6, same desktop, same wired LAN):*

| run | rate | length | gaps | lost packets | per minute |
|---|---|---|---|---|---|
| 5 | 60 fps / 40 Mb/s | 4.5 min | 209 | 1420 | ~46 gaps, ~315 lost |
| 6 | 30 fps / 20 Mb/s | 3.0 min | 38 | 246 | ~13 gaps, ~82 lost |

Rate-dependent, ~3.7x less at half the rate, and bursty: run 6 sat at 23
gaps / 24 lost for its first 2m10s, then one burst in the last 50 s added
222 packets. The sender dropped nothing (5272 frames, 0 dropped). So this
looks like something saturating at 40 Mb/s — the receiver's UDP socket
buffer is the first suspect (log `SO_RCVBUF` and overruns; raising it may
be the whole fix) — rather than random LAN loss. Next split: 60 fps at
20 Mb/s, to separate frame rate from bitrate. The sender's periodic IDR is
every ~10 s (18 in 3 min), which is how long a smear lasts today.

### B14 — Receiver latency goes negative: the clock offset is measured once
relay-pc2, run 1: `capture_to_present_ms` started at +2.9 ms, crossed zero
at 00:05:06 and reached -8.0 ms by 00:08:30; 312 of 538 samples negative.
The two PCs' wall clocks also disagreed by 2.6 s at connect and 6.9 s four
and a half minutes later, so one of them drifts about 1 s/min against the
other. The sender/receiver offset (`clocks synced offset_ms=-2421`) is
estimated once at connect and never again, so the "latency" readout is that
drift. Pre-dates S29 (M4 measurement code). Fix: re-estimate periodically,
or measure glass-to-glass against a monotonic clock. Also worth checking
why a Windows 10 PC on the LAN drifts a second a minute (NTP off?).


## Fixed, verified on the second PC

- **`relay-share.exe` could not start on Windows 10 at all.** A static import of
  `MFCreateVirtualCamera` (Windows 11 22H2+ only) made the loader refuse the
  process before `main()`. Neither sending nor receiving worked, regardless of
  the camera being opt-in and declined. Delay-loaded, plus an export probe
  rather than a build-number guess. Verified: `relay-share probe` now runs.
- **The codec error sent users to a package they cannot install.** It named the
  OEM "from Device Manufacturer" package — Install greyed out without OEM
  entitlement — and told them to search the Store, which surfaces paid and
  third-party apps. Now names the installable package and its product id.
- **Console output was mojibake.** Em dashes in the receiver banner and the
  decoder error rendered as `a-tilde-EUR-dash` on a console using the OEM code
  page. Console-facing strings are ASCII now.
- **The codec banner rendered as three columns**, link broken one word per
  line: `.offline` is `display:flex`, so each text node and the `<a>` became a
  flex item. Both banners use the existing `.msg` wrapper now.
- **`Kv` rows ran label into value** — "CameraNeeds Windows 11 22H2+…" —
  because `space-between` leaves no gap once the value wraps.
- **Receive failures could only surface as a return to idle.** The core's pump
  discarded `ShareEvent::Exited` and sent `message: None`, so an engine dying
  at startup reported nothing. It now carries the reason.

  *Correction:* this was filed after a "Start receiving does nothing" report
  that turned out to be a missed click. The code path was genuinely wrong and
  the fix stands, but the symptom that prompted it was misattributed.

## Proven working, two machines, real LAN

Recorded because these had never run across two PCs before and were deferred
from M4: mDNS discovery (picked the real LAN address, not VMware/Tailscale/WSL),
six-digit pairing, the SDP MAC, DTLS-SRTP, clock sync (RTT 0.25 ms), wired-link
detection, and both media pipelines. 252 frames, zero dropped, capture→send
~2 ms, encode ~4.9 ms.
