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

### B8 — A windowed `relay-share recv` never exits  |  MITIGATED 2026-09-17, not cured
After the sender stops it logs "connection closed" and keeps running. Found by
S27 during loopback work and reproduced against a pre-S27 binary, so it
predates the codec work.

Hidden in normal use because the core kills the child, which is exactly why it
survived this long. Anyone running the binary by hand — as we did for the
Windows 10 diagnosis — leaves a process holding an open render window.

### B9 — Relay will happily capture its own render window  |  FIXED 2026-09-17, unverified on hardware
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
### B13 — The received stream played in a window of its own  |  S29, local pass done, two-PC pass pending
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
worktree at the S29 commit:** embedded placement exact to the pixel;
owner/styles as designed; pop-out, close-to-re-embed, two shell resizes,
navigate-away (hidden) and back (shown, page state restored), stop (window
gone, "The share from host-stub ended."); `excluded_from_capture=true` after
every transition; present counter climbing throughout. What the stub cannot
show: a real modal drag of the app window, real decode and audio, Windows 10.

**Two-PC pass:** see the S29 run log below once it has happened.

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
