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

### B8 — A windowed `relay-share recv` never exits
After the sender stops it logs "connection closed" and keeps running. Found by
S27 during loopback work and reproduced against a pre-S27 binary, so it
predates the codec work.

Hidden in normal use because the core kills the child, which is exactly why it
survived this long. Anyone running the binary by hand — as we did for the
Windows 10 diagnosis — leaves a process holding an open render window.

### B9 — Relay will happily capture its own render window
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
