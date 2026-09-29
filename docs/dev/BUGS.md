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
killing `relay-share.exe` by image name. *Confirmed 2026-09-20 by the S30
session:* its loopback cleanup ran `taskkill /F /IM relay-share.exe` about a
dozen times between 2026-09-18 23:27 UTC and 2026-09-21 00:12 UTC. The
teardown timings above all come from runs that ended with `stopped` and exit
code 0. The B14 drift runs were cut short by a kill (the -50 ppm resync run
has 195 samples instead of ~235), which shortens them but does not touch the
samples taken before it; worth one clean re-run all the same. Rule for
parallel sessions:
never kill Relay processes by name; kill the PIDs you started.

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

### B10 — The picture freezes a few seconds into a share  |  FIXED 2026-09-22 (r12+), cause known
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

*Resolved, 2026-09-22 (the S29/S30 work, r12 onward).* Two things were
stacked. The "freeze" was the last frame of a share that had already ended:
the sender's AU channel was never closed on its way out, so the receiver's
render loop sat on the final picture with nothing saying the share was over.
`render.rs` now ends the receive when no access unit has arrived for
`AU_IDLE_TIMEOUT` (3 s) — generous, so a bad LAN second never ends a live
share — and the sender says `Bye` before it winds down (B8), so the normal
end is immediate. The `aus == 2 × presented` oddity was a double increment
of `video_aus`, removed. The S29 runs 1–7 (B13 below) ran 30 s–5 min with
`presented` climbing throughout and no freeze; what remains of "mid-stream
smear while frames keep presenting" is B15, its own entry.

### B11 — A long continuous beep from the receiver's speakers  |  RESOLVED 2026-09-22: it was the test signal
Started when the share started, described as loud and constant. The sender was
sending real audio (2,999 packets, peak 0.091), so this is the receiver's
playback path rather than the source — a stale buffer repeating, or an
underrun turning into a tone. Unknown whether it began at connect or at the
moment the picture froze, which would tie it to B10.

Worse than it sounds: it is the first thing a user hears from Relay, through
whatever their speakers are set to.

*Resolved, 2026-09-22.* The "beep" was Relay's own bench signal: the M4
measurement scripts play a generated 440 Hz tone through the sender's
default endpoint (`docs/plans/M4-share.md`, "A generated 440 Hz tone
plays…"), and that run had it on. The receiver reproduced it faithfully —
loud, constant, on both PCs. Not a playback fault: the S29 audio run (r6,
Windows text-to-speech, 91 s) was clear with no stutter or tone, and every
run since has been speech or music. Standing rule from this, kept in the
test scripts: **a listening test uses speech or music, never a tone.**

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

*S30, 2026-09-18 to 09-20 — the limiter named. Four two-PC runs, one wrong
turn, recorded as it happened.* All at 1440p H.264, 60 fps / 40 Mb/s requested,
full-motion page (`scripts/motion-test.html`), wired LAN, RTT 0.25 ms.

| run | build | `SO_RCVBUF` | actual rate | result |
|---|---|---|---|---|
| A | `db4936c` (counters only) | 65,536 (OS default) | 38 Mb/s | 370 gaps / 1409 "lost" by the old accounting, 17 track-queue drops, smearing |
| B | `db4936c` | 4 MB | 2.4 Mb/s | **void** — the motion page was covered and not animating |
| C | `4f49de0` (first fix build) | 65,536 | 45 Mb/s | 110 gaps / 746 lost, 14 repaired, 593 frames withheld, 9 shown, 1,627 SRTP "duplicated", share ended itself at 14 s |
| D | `4f49de0` | 4 MB | 38–42 Mb/s | **0 gaps, 0 lost, 597 repaired, 0 withheld, 10,351 frames, latency p50 1.6 / p95 4.9 / max 31 ms** |
| F | r11's `relay-share.exe`, standalone | 65,536 (`RELAY_UDP_RCVBUF=0`) | 39.9 Mb/s, true 60 fps | **0 gaps, 0 lost, 2,242 repaired, 0 withheld, 0 keyframe requests, 10,793 of 10,793 frames, 4 SRTP duplicated (run C: 1,627), latency p95 3.6 / max 15.5 ms** |
| E | r11 = `53642d2`, **installed app, in-app video**, 4 MB (the shipped default, no env var) | 39 Mb/s mean, 47 max | **0 gaps, 0 lost, 970 repaired in 107 s, 0 withheld, 0 keyframe requests, 6,062 of 6,062 frames presented, latency p50 1.3 / p95 3.5 / max 12 ms, not one WARN or ERROR in the log** |

**The limiter was the receiver's UDP socket buffer.** Windows defaults
`SO_RCVBUF` to 65,536 bytes; the sender does not pace, so a 1440p keyframe
leaves as one burst of 446–589 packets (526–697 KB) in 3.6–5.4 ms, and even an
ordinary 83 KB frame is bigger than the buffer. Run D's queue ran at a median
of 48,640 bytes with peaks to 599,394 — the old buffer sat at the median and
was blown through by every keyframe. C and D differ in that one setting.

*The wrong turn.* After run A this file said the buffer was **not** the
limiter: pc2's socket had received 711,228 datagrams for 708,602 sent, UDP
Receive Errors had not moved, and seconds with gaps did not line up with
seconds where the queue was full. All three arguments were unsound. The
receive count includes NACK retransmissions (the surplus is about what
re-sending 1,409 packets adds). Windows does not count socket-overflow drops
anywhere — `netio::tests` overflows a socket and watches UDP InErrors stay
put, and NIC discards were +0 across run C, which lost 746. And the per-second
gap log was dominated by reordering, not loss. Run C, by failing badly at
64 KB, and run D, by not failing at 4 MB, settled it. The only overflow
evidence Windows offers is `queue_peak_bytes == rcvbuf` in the `media socket`
log line.

**Why loss then smeared for 10–15 s: recovery never worked, for three reasons,
all library defaults.** `nack`, `nack pli` and `transport-cc` were negotiated
all along — `register_default_interceptors` appends them to codecs registered
before it runs, so `rtcp_feedback: vec![]` never reached the SDP empty (the
plan's first premise was wrong). But:

1. *SRTP's replay window defaults to 64 packets* — 16 ms at 4,000 packets/s.
   Every retransmission is older than that when it arrives and is rejected as
   a replay before anything above SRTP sees it. Run C: 1,627 rejections over
   685 packets in 6.6 s, 14 holes repaired.
2. *The interceptor chain's terminal drops all received RTCP* (its source
   says so, and says to add an interceptor if the application needs any). The
   sender has polled its track for PLI since M4 and never received one.
   Run C: 21 sent, 0 seen.
3. *There was no reorder buffer*, so even an accepted retransmission was
   counted as a gap when first missed and then appended to the wrong access
   unit. Packets also arrive slightly out of order routinely — 597 holes
   filled in run D with nothing lost.

Separately, webrtc-rs hands packets to a track through a 256-slot queue with
`try_send`; our loop awaited the 8-deep decoder channel, so a decoder stall
dropped the tail of a keyframe (17 in run A). None of this had ever been
visible: webrtc-rs logs through the `log` crate and nothing was listening.

**Fix** (`53642d2`): `SO_RCVBUF` 4 MB / `SO_SNDBUF` 2 MB through a runtime
wrapper at webrtc-rs's one socket seam (`transport/netio.rs`); SRTP replay
window 4096; reorder buffer with a 40 ms hold (`reorder.rs`); NACK every
10 ms × 4 with 2048-packet history; PLI/FIR forwarded to the track
(`feedback.rs`); on an unrepairable hole or a decoder overrun, frames are
withheld and a keyframe requested every 500 ms — for at most 1 s, after
which frames are shown again and the asking continues (run C's dead picture
is what an unbounded version does); a track loop that never awaits the
decoder; damped bitrate control on unrepaired loss; webrtc-rs warnings in
`share.log`. `rtp_gaps` / `rtp_lost` now count only what was given up on;
`rtp_recovered`, `keyframe_requests`, `frames_withheld` are new.

`RELAY_TEST_LOSS` injects loss at the socket wrapper (corrupts chosen video
datagrams so SRTP rejects them), which makes every recovery path testable on
one PC. Headless loopback on `53642d2`: `every=50` → 110 lost, 110 repaired,
0 replay rejections, 0 keyframe requests; `every=200,retx` (no keyframe can
ever complete) → 42 requests sent, 42 received, the limit fires once, 1,163
frames shown, 58 withheld.

*Run E, 2026-09-21 00:16–00:17 UTC, is the acceptance run.* Source was
full-screen game footage on the dev PC (the motion page would not stay in
front; a first attempt was aborted at 58 s and 2 Mb/s and is not a result).
Jake, watching pc2: "No lag, stuttering, audio, or smearing noticed while in
fullscreen streaming video game gameplay in 1440p 60fps from the main pc."
He stopped it by hand at 107 s; the end was clean. The content changes at
~33 fps, so 6,062 frames is the source, not loss. The socket queue again ran
at a median of 50,859 bytes and peaked at 133,828 — this content would have
overrun the old buffer continuously. NIC discards +0, UDP errors +0.

*Run F, 2026-09-21 00:22–00:25 UTC, is the proof that recovery works on a
real link* and not only under `RELAY_TEST_LOSS`: the same 64 KB buffer that
gave run C a dead picture, same Warzone footage as E, full 185 s. The socket
queue was pinned at exactly 65,536 for 43 of 185 seconds (>= 60,000 for 68)
and nothing was lost: NACK repaired ~12 holes a second, 3.7x run E's rate.
Jake: "No freezing, lag, audio, or smearing noticed." (C used the motion page
and F game footage, at the same rate; the buffer and build are what differ.)
So the two fixes are independent: 4 MB removes the need for recovery, and
recovery survives without the 4 MB. Both ship.

*Not exercised on two PCs:* the keyframe-request path and the 1 s withholding
limit. Neither E nor F ever gave up on a packet, so 0 requests were sent and
0 received — consistent, but the only proof those work is the loopback
injection above (42 sent / 42 received). A Wi-Fi or deliberately lossy run
is where they will first be seen for real. Latency went negative in F (191
of 363 samples): B14, unchanged. The sender still
does not pace; with a 4 MB receive buffer that is tolerable on a LAN and is
the first thing to revisit for Wi-Fi.

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

### B17 — Remembered peers could never be recognised: the fingerprint changed every share  |  FIXED 2026-09-23 (r18), two-PC confirmation owed

**Found** writing the DPAPI wrap for `identity.pem`, by a test that compared
fingerprints across two loads of the same key file instead of file bytes.
S35 stored the ECDSA *key* and rebuilt the DTLS certificate from it on
every run, but `RTCCertificate::from_key_pair` mints a fresh self-signed
certificate each call (random serial, random subject) and the fingerprint
is a hash of the certificate. So r12–r17 presented a new fingerprint on
every share; `peers.json` recorded them faithfully and nothing could ever
match. The two-PC symptom would have been "asks for a code every time,
`Bye` from the receiver in the log" — the exact fallback S35 designed for an
older peer.

**Fix.** Store the whole certificate in rtc's own PEM (`serialize_pem` /
`from_pem`, its documented persistent-identity path) and, on Windows, wrap
that file with DPAPI in user scope as `identity.key` (application entropy,
UI forbidden). A key-only `identity.pem` from r12–r17 is upgraded in place
on first read; a plaintext certificate file is wrapped and removed. A blob
copied to another PC or account fails to unwrap and is replaced loudly —
the copier holds nothing, which is the point of §4. 7 tests, one of which
(`the_same_key_comes_back_across_runs`) now asserts the fingerprint, so this
cannot come back silently.

**Second cause, found the same day by running it.** With the fingerprint
fixed, a headless loopback code pairing still wrote no `peers.json`:
`signal::sdp_fingerprint` scanned for an `a=fingerprint:` line, but what
`Offer.sdp` / `Answer.sdp` carry is the JSON `RTCSessionDescription`
(`{"type":"offer","sdp":"v=0\r\n…"}`), one line, no such prefix — so it
returned `None` at every call site, `remember` was never reached, and the
errors it would have raised were `let _ =` anyway. Its unit test used raw
SDP. Now it takes either form, with a test on the JSON form, and
`scripts/trusted-check.sh` proves the whole path on one PC, headless: code
pairing → `paired trusted=false` and an entry written; a second share with
`--trusted <fp>` and no code → `paired trusted=true` on the receiver,
`connected trusted=true` on the sender; a fingerprint nobody holds →
refused before DTLS, zero paired events. Run it before every r-build that
touches signalling.

**Owed to two PCs (r18):** the S35 script as written — code pairing, then a
second share with no code — was never actually possible before r18 and is
now the first thing to run.

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

---

## Build log — what each installer contains, and what only two PCs can prove

Kept here because the second PC is the only place most of it can be checked,
and a result nobody wrote down gets re-tested. Rule: ship from `main`, state
the contents, state what is owed.

| Build | `main` | Adds | Verified on two PCs | Owed to two PCs |
|---|---|---|---|---|
| r12 | `2d47572` | S30 loss recovery + S33 clock/audio/teardown, first time with S29 in one binary | — | S30's keyframe request and 1 s withholding under real loss; B14/B16/B8 confirmation |
| r13 | `5c060c4` | S31 stream health; S35 identity foundation | — | chip stays quiet on a good stream; latency never negative |
| r14 | `0fa9776` | S35 remembered devices (Option A) | — | code pairing → reconnect with no code → consent check (receiver not listening) → Forget → reboot; install-over-the-top on both PCs |
| r15 | `4cdda0f` | S38 stream resilience | — | kill the sender's engine by PID and watch it return; same for the receiver; Stop is a Stop; give-up time; reboot one PC mid-share; crash line shown once; close notice on/off |
| r16 | `dff9b33` | S37 audio mixer (three tracks, faders both ends) | Loopback on the main PC, headless, 2026-09-22: exclude-mode capture activates (`audio pipeline up track=Rest`, 48 kHz stereo), three tracks travel (`rest_packets` in lockstep with `audio_packets` and `mic_packets`, 1,048 each in 10 s), receiver classifies `relay-audio-rest` as `Rest`. Nothing was playing, so `rest_peak` = 0: *that* the track flows is proven, *what* it carries is not. | **listening check, speech and music, never a tone**: mute the game and hear only the rest; the reverse; move a fader mid-share, no click; an older receiver hearing the rest track as the mic |
| r17 | `b23a85c` | S36 Relay Camera on the sending PC (camera while sharing, video only) | — | OBS on the **main** PC picking up its own outgoing share (camera must be registered there first — Jake's UAC click); the reverse path, PC 2 → main PC → OBS, is M5 as built and is the OBS test Jake queued |

| r18 | `2da791f` | B17, both causes: identity is the stored certificate (DPAPI-wrapped `identity.key`) and the fingerprint is read from the real wire form; first build where a remembered peer can actually match — proven headless on one PC by `scripts/trusted-check.sh` | — | **the S35 script, now possible for the first time**: code pairing, then a second share with no code, on both directions; consent check; Forget; reboot; install-over-the-top keeps the identity (log says "identity upgraded" / "wrapped", never "generated" on an updated PC) |

| r19 | `2091050` | S19 call-audio return route: the receiver sends the call app's audio back (`relay-audio-return`), the sender hears it behind a Call fader; "Send the call back" card on Receive | One-PC headless `scripts/return-check.sh`: 647 packets sent, 599+ received and played, peak 0.33 with a `SoundPlayer` as the call app | a real call on PC 2 (Discord, second participant): the main PC hears them, they never hear themselves; then Game + "everything else" once to hear the echo the Share screen warns about |
| r20 | `0aafc2d` (branch `fix/core-version`) | Two-PC fixes: still screen no longer ends a share (sender re-encodes the last frame every empty 250 ms); receiver restarted by S38 keeps its host window; clean stop reported as `sender_stopped`; `relay-core --version`; `share-start` takes `RELAY_PEER_ID` | 2026-09-25: no-code share streams 24 s, trusted, embedded, aus=presented=1445, 2.9 ms; update r19→r20 keeps `peers.json` and `identity.key` byte-identical | — |
| r21 | `19eadba` | Shell forwards `ended_by_sender`; reused identity logs "loaded" | 2026-09-25: "The share from jake ended." after a clean stop; "loaded this PC's DTLS identity" on both PCs | — |
| r22 | `eb19637` | A no-code refusal is final (sender: one attempt, message, no crash record); receiver refuses an unremembered no-code peer and keeps its port and code | 2026-09-25: **S35 done on two PCs**: pair with code, reconnect with no code, Forget, two refusals (code unchanged, WARN with fingerprint), re-pair gives one fresh entry | reboot of either PC |
| r22 | `eb19637` | (same build) | 2026-09-25: **S38 kill tests pass**: sender engine killed by PID, back in 4.5 s with no code, embedded; receiver engine killed by PID, core restarts it in 1.5 s, share back in 9.6 s, embedded | reboot of one / both mid-share; give-up time; crash line shown once |
| r23 | `8ea5fcd` | Receiver's codec status keeps the sender and trusted flag ("remembered, no code" was wiped 0.4 s into every trusted share) | — | the live screen during an outage ("dropped — waiting"), "remembered, no code" throughout |
| r24 | `5f7099d` | Core replays the newest ReceiveStatus on Subscribe; `restarting` flag on the drop push | 2026-09-26: close/reopen shows the live receive (same code, Stop receiving); outage reads "dropped" 0.29 s after the kill, no Idle/ended flash; reboot of PC 2 then opening Relay resumes receiving with no press; remembered store and identity survive the reboot | reboot with the shortcut-launched UI sampled untouched |
| r25 | `4d6b862` | Engine drops a dead host HWND; shell embeds an unhosted stream window; receive episode resets on first stats, not paired; installer keeps `active-stream.json` across its shutdown | 2026-09-26: install over a live receive resumes it; unhosted receiver embedded by the shell in 168 ms; receiver kill → back embedded in 10 s; Stop receiving stays stopped (no restart, record cleared); sender gives up 3 min 06 s after the receiver stops (18 attempts, 1/2/5/10 s) | — |
| r26 | `50ae1c1` | Receiver hears `stop`/`host` while waiting for a sender (the installer's stop was killing it → spurious "did not shut down cleanly" on every update); "You stopped receiving from X." | Installing r26 still logged "did not exit gracefully; killing" -- **expected**: the preinstall stop runs the *old* (r25) core. Only an install over r26 tests it | install over r26 with no kill line and no crash banner |
| r27 | `093d2c9` | (r26 + docs) | 2026-09-26: **update path clean**: install over a live receive -- "stop command received while waiting", no kill line, no crash banner, resume with no press; mid-share Stop reads "You stopped receiving from jake." | -- |
| r28 | `a681abc` | Resumed receive keeps its pairing code; tao's session-end panic not recorded as a crash | 2026-09-28: second reboot -- UI launched from the shortcut shows the live receive untouched; a receiver kill keeps the code; settings toggles persist across a UI restart without resetting each other | -- |
| r29 | `1c2f5b4` | Wrong code is non-fatal to the receiver (rotates, keeps waiting, notice); receive episode ends after 30 s healthy; sender treats a wrong code as a refusal; close notice logged | 2026-09-28: 000000 → receiver stays up on the same port, code rotates and is stored, sender stops after one attempt with "did not accept that code"; pairing with the new code works; close notice logged with close_notice=true. **S35 and S38 closed on two PCs** | audio items (S37, S19, B16) parked on the shared Rodecaster |
| r30 | `053bcfe` | `RELAY_NO_CAPTURE_EXCLUDE` (test-only) so a meter can read the received picture | 2026-09-28, measured, no ears: **S37** mixer -- mute game −101 dB, mute rest −84 dB, **no game leak into Rest** (exclude loopback holds), fader −60 = mute, sweep tracks dB exactly, no clicks, faders reset on a new share. **S19** call return -- 2500 Hz returns at −13.9 dB, follows the call app's on/off edge for edge, silent (−112 dB) when it is; mix-minus holds with Game-only capture. **B16** lip-sync -- audio +43 ms late (stdev 6.3, 55/55 flashes), identical with the return on (+43.7); `audio_ms` is the audio queue, not A/V offset (relabelled "Audio buffer") | per-frame A/V offset inside the engine (the durable B16 meter); call-app picker should list audio sessions, not windows; the Call card shows the UI's choice, not the receiver's `return_pid` |
| (sender `0b37c23`) | `0b37c23` | `share-start` takes `RELAY_SIZE` / `RELAY_FPS` | 2026-09-28: **S32 matrix on two PCs, full motion**: 4K60 at 60 Mb/s, 4K30 at 40, 1440p60 at 40 and 1080p60 at 25 all held their frame rate with 0 lost, p95 latency 10 / 11 / 4 / 3 ms, receiver decode 33 % at 4K60. See `docs/dev/resolution-matrix.md` | 4K from a real 4K source and display; Wi-Fi; the first latency sample after a connect (100–290 ms warm-up) should be skipped by the health card |
| r32 | `0efbf9a` | Call card names the return app by exe | 2026-09-29: health card hides latency for the first ~1.5 s, then 2-4 ms | **Fail**: after a UI close/reopen the card read "Call app: None" (core still returned the pid; the shell's stream status did not carry it); share.log printed latency_ms=0.0 in warm-up; an exited pid showed "process N" |
| r33 | `e611345` | Shell stream status carries return_pid/return_exe; no PID fallback; log says "warming up" | 2026-09-29: live pid names python.exe before and after a UI kill+relaunch; exited pid reads "an app that has closed", no PID; share.log warm-up lines say warming up, then 2.1-3.5 ms, lost 0; card shows no Latency for 1.5 s | -- |

Each build supersedes the one before; install over the top without
uninstalling, on both PCs — that is the standing-rule check itself.
