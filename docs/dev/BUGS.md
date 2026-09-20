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

*S33, 2026-09-18, `ebc0c72`: cause found, fixed, measured on one PC; the
windowed two-PC confirmation is recorded under "S33 two-PC pass" below.* Two
separate things were hiding behind the deadline:

1. **What hung.** Not `pc.close()` and not the AU sender: both finish in
   well under a millisecond (`peer connection closed ms=0.03`). `send` and
   `recv` read commands through `tokio::io::stdin()`, which parks a
   blocking-pool thread in `ReadFile`; `Runtime::drop` waits for that thread,
   and the read only returns when the core writes a line or closes the pipe.
   So a share that ended any way *other* than a stdin command — the sender
   stopped, the connection dropped — finished its teardown and then sat in
   the runtime's destructor until the deadline killed it. A user "Stop
   receiving" never showed it because that line is what the read was waiting
   for, which is exactly the split run 3/run 4 saw. `tests/stdin_shutdown.rs`
   plays the core (stdin held open, never written): old drop **never exits**
   (killed at 4 s), `run_async`'s `shutdown_timeout` exits in **0.7 s**
   including process start and 200 ms of simulated work.
2. **Why the receiver was 3-4 s late to begin with.** The sender closed
   without a word, so the receiver learned the share was over from the 3 s AU
   idle timeout (windowed) or ICE's ~4.2 s disconnect (headless). The sender
   now sends `Bye` on the signalling socket before it winds its pipelines
   down, and the receiver ends on `Bye` or on the socket closing.
   `scripts/teardown-check.sh`, headless loopback, time from the sender's
   `stop` to the receiver process exiting:

   | build | runs | sender stop->exit | receiver stop->exit |
   |---|---|---|---|
   | before (`11df2ee` + timing only) | 3 x 6 s | 80-139 ms | 4257-4332 ms |
   | `ebc0c72` | 3 x 6 s | 73-78 ms | 103-109 ms |
   | `ebc0c72` | 2 x 90 s | 79-88 ms | 110-118 ms |

   (The receiver figures are quantised by the script's 100 ms poll.)

The 3 s deadline thread stays as a backstop; the log line it prints is now
a bug report, not the normal path.

**A false alarm worth remembering.** During these runs the loopback sender
or receiver died silently five times in shares longer than ~90 s: no log
line, no `stopped`, exit code 1, once reported by bash as a segfault, at
unrelated moments (46 s, 95-104 s, during teardown), and on main's
pre-S33 binary as readily as on this branch. It looked like a crash in the
engine. The same debug build copied to a different file name ran 150 s and
exited cleanly (sender 73 ms, receiver 107 ms). Exit code 1 with nothing
logged is what `taskkill /F` leaves, and S30 was running its own loopback
tests on this PC at the time, so the likeliest cause is another session
killing `relay-share.exe` by image name. One renamed run is a strong hint,
not proof. Practical rule for parallel sessions: never kill Relay processes
by name; kill the PIDs you started.

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
`target\release\relay-ui.exe` 12 ms *after* writing the NSIS installer, so
the exe inside the installer (`4A2E2BA4...910E`) differs from the one left on
disk (`2E0B8748...45DB`). Hash the installer, or extract from it; never the
loose `relay-ui.exe`.

*S33, 2026-09-18, `ebc0c72`: the Rust binaries are now reproducible, and the
rest is written down.* `scripts/repro-check.ps1` builds one commit from two
folders and compares hashes: plain `cargo build --release`, **0 of 4**
binaries identical (`relay-core`, `relay-svc`, `relay-elevate`,
`relay-share`); with the flags `stage-bundle.ps1` now sets
(`--remap-path-prefix` for repo, cargo home and rustup home, plus `/Brepro`),
**4 of 4** identical. The real cause of the `relay-preview.exe` finding was
the linker timestamp and embedded paths, not caching: two links of identical
source never matched. The catalogue is staged with LF endings whatever the
tree holds (the main checkout still carries a CRLF copy that git does not
report). Not yet shown reproducible, and said so: `relay-ui.exe`, the two
DLLs and `relay-preview.exe` (same flags, not in the measured set), and the
NSIS installer itself (stored mtimes, solid LZMA). `docs/dev/reproducible-builds.md`
has each input, the evidence, and a checklist for reasoning about a mismatch.

### B13 — The received stream played in a window of its own  |  FIXED, verified on the second PC 2026-09-18 (r10 = `5cb414b`)
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

**Run 5, 22:47-22:52 UTC, r9 = `600eeeb`:** the swapchain now rebuilt
cleanly after all four mode changes ("swapchain recreated after a hosting
change", no failures) and the picture was *still* black after X, Esc and
Settings-and-back, while minimise/restore brought it back. The common
factor of every black path, and of none of the working ones: the engine
hid the window on re-embed and the app showed it again 2-3 ms later; r5's
slow round trip (hidden for ~1 s) and minimise/restore (hidden for as long
as the app is minimised) never went black. DWM composed the window black
after a hide-then-show inside one frame. Affinity was cleared as a cause:
`excluded=true` is `GetWindowDisplayAffinity`'s answer after every change.
End of share clean again. Loss at 60/40: 209 gaps, 1420 packets (B15).

**Run 7, 23:01-23:04 UTC, r10 = `5cb414b`, the fix:** the engine no
longer hides its window on re-embed; it is restyled in place and the app
moves it. **Picture back on every path**: pop-out + X, pop-out + Esc,
Settings-and-back, minimise/restore; four mode changes each followed by a
swapchain rebuild within 2-5 ms; end of share clean. One loss burst of 235
packets across the RTP sequence wrap smeared the lower half for 10-15 s and
outlived a periodic keyframe (B15); 34 gaps / 272 packets for the run.

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

*S33, 2026-09-18, `ebc0c72`: measured first, then fixed.* Where the second
went, stage by stage:

| stage | how measured | result |
|---|---|---|
| sender capture -> Opus packet | `relay-share bench-audio 5` (desktop, 48 kHz, 10 ms frames) | p50 0.11 ms, p99 0.22 ms (+ up to one 10 ms loopback period) |
| network | clock-sync RTT | 0.08-0.25 ms |
| receiver: packets waiting to decode | `PlaybackStats.channel_packets` | 0 |
| receiver: decoded PCM queue | `queue_ms` | 0 ms |
| receiver: **WASAPI render buffer** | `render_ms` (`GetCurrentPadding`) | **1000 ms** of a 1000 ms buffer |

`playback.rs` asked WASAPI for a one-second buffer and on every wake filled
*all* free space, with silence when it had nothing else. The first wake
queued a second of silence and every real sample stood behind it for the rest
of the share. Nothing else in the path holds more than a period.

Fix: a per-track jitter queue primed to 40 ms that re-primes after running
dry, a 20 ms cap on what sits in the render buffer, and one-frame-per-480
skip/repeat slewing on the queue's one-second low-water mark so two sound
cards a few dozen ppm apart neither drain it nor let it grow (simulated for an
hour at +/-200 ppm: no underrun, no hard drop). The stream is opened as
48 kHz stereo float with `AUTOCONVERTPCM`, so an endpoint at another rate is
converted by the audio engine rather than played off-pitch — the old code
wrote 48 kHz samples into whatever the mix format was.
`tests/playback_depth.rs` (plays silence on the default endpoint, run by
hand), same binary, `RELAY_AUDIO_LEGACY=1` for the old path:

| path | arrival -> endpoint, mean | render buffer | queue | underruns |
|---|---|---|---|---|
| legacy | 1010 ms | 1000 ms | 0 ms | 0 |
| S33 | 40-60 ms | 20 ms | 20 ms | 0 |

The receiver's `stats` line and `share.log` now carry `audio.buffered_ms`
with its parts, underruns and slew counts. Against video: the picture is
~3 ms capture-to-present plus display, so audio now trails it by roughly
40-60 ms — inside the ~125 ms at which late audio becomes noticeable
(ITU-R BT.1359), where 1 s was not. Two-PC confirmation below.

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
every ~10 s (18 in 3 min), which is how long a smear lasts today. Jake saw
**no smear at all in run 6**, the first corruption-free run, even with 246
packets lost: single-packet gaps are covered by the 10 s cadence, and the
visible damage comes from the bursts.

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


*S33, 2026-09-18, `ebc0c72`: fixed, measured on one PC with manufactured
drift; two-PC confirmation below.* The sender now pings every 2 s on the
signalling socket it already owns, filters the samples (`ClockFilter`: ignore
a sample whose round trip is more than twice the recent best, smooth the
rest, take a >20 ms step at once) and pushes each estimate to the receiver.
Latency stamps on both ends come from the wall clock read once and then
advanced by the monotonic clock, so an NTP step mid-share cannot move the
readout either. An older peer on either end simply keeps the connect-time
offset. `scripts/clock-drift-check.sh 90 -500 [once]` — headless loopback,
receiver clock skewed by -500 ppm (`RELAY_CLOCK_SKEW_PPM`, test hook), newest
frame's capture-to-arrival latency:

| sender | first 10 % | last 10 % | drift | negative samples |
|---|---|---|---|---|
| offset measured once (`RELAY_CLOCK_SYNC_ONCE=1`) | 2.66 ms | -39.00 ms | -29.2 ms/min | 152 of 171 |
| `ebc0c72`, resync every 2 s | 1.584 ms | 1.579 ms | -0.004 ms/min | 1 of 171 (-0.002 ms) |

-500 ppm is ten times a real crystal, chosen so 90 s shows the slope; it
also moves the clock 1 ms between two pings, which is the sawtooth that
produced the single -0.002 ms sample. At a realistic -50 ppm over 120 s:
measured once, 2.18 -> -0.50 ms with 57 of 228 samples negative; with resync,
5.15 -> 4.87 ms, minimum 3.67 ms, **0 of 195 negative** (the runs' absolute
levels differ because the desktop being captured differed).

The 1 s/min wall-clock disagreement in the original report does not match
the 2-3 ms/min the latency actually drifted (that would be ~50 ppm, an
ordinary crystal); the wall-clock figures were read by eye from two screens
and are probably the odd ones out. Worth one look at the Windows 10 PC's
time service all the same.


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
