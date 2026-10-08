# S51 — NDI® output

Branch `feat/s51-ndi-output`, cut from `feat/s46-learned-game-eq`. PR into
that branch. NDI® is a registered trademark of Vizrt NDI AB;
https://ndi.video/.

**Goal.** Relay publishes the received stream (video and the mixed audio) as
an NDI source, "Relay (from <sender>)", so OBS (with the NDI plugin), vMix,
Streamlabs, Resolume, NDI Studio Monitor and others can use it with no camera
or window capture. Optionally the sending PC publishes its own share too
("Relay share"). Off by default on both sides.

## Licence summary

Full reading, sources and verdict: `docs/dev/ndi-licensing.md`.

- **Dynamic loading, nothing vendored.** The engine loads
  `Processing.NDI.Lib.x64.dll` by full path with `LoadLibraryExW` and resolves
  seven named exports with `GetProcAddress`. The C declarations are written by
  hand (`crates/capture/src/ndi/ffi.rs`, layouts pinned by tests). No SDK
  header, library or binary is in the repo or needed to build.
- **Bundled (RC2, owner's decision 2026-10-07: option B).** Release
  installers carry NDI's runtime DLL and notice file in Relay's folder,
  fetched from NDI's redistributable at build time and hash-pinned
  (`scripts/ndi-runtime.psd1`, `scripts/fetch-ndi-runtime.ps1`), with NDI's
  terms as an installer licence page. Relay loads the bundled copy first, then
  a runtime installed from NDI (http://ndi.link/NDIRedistV6). Dev builds
  without it still build.
- **Notices.** "NDI®" on first use, the trademark line and an ndi.video link
  in the NDI card on Receive and Share, the trademark line in Settings' About
  card, `THIRD_PARTY_NOTICES.md` §4, the README. No NDI in Relay's name or the
  source names.

## Design

```
                      relay-share recv (per share)                       relay-share send
 decoded NV12 ─ present ─┬─ vcam tee (existing)                capture NV12 ─┬─ vcam tee
                         └─ ndi::video::VideoProducer                        ├─ ndi::video::VideoProducer
                              2 staging textures: copy N, map N-1            └─ encoder
                              with DO_NOT_WAIT; pack_nv12 → pool buffer
                              ─try_send→ tee (2 deep) ─→ "relay-ndi-video" worker ─→ NDIlib_send_send_video_v2
 Opus → jitter → mix ─ WASAPI buffer                          program-track OpusStream frame
                    └─ ndi::AudioProducer                       └─ ndi::AudioProducer
                         deinterleave → planar float
                         ─try_send→ tee (8 × 10 ms) ─→ "relay-ndi-audio" worker ─→ NDIlib_send_send_audio_v2
```

- **`NdiOutput`** (`crates/capture/src/ndi/mod.rs`) is the switch, made per
  engine at start and cheap until used. `set(true)` loads the runtime (first
  time only; a `OnceLock`), creates one NDI sender (no groups, clocking off),
  starts two workers and installs the producers; `set(false)` clears the
  producers, which ends the workers, which drops the sender. `apply()` wraps
  it in `ndi_up` / `ndi_down` / `ndi_error` NDJSON lines and never fails the
  share. Called on a blocking task, never on a real-time thread.
- **The tee** (`ndi/tee.rs`): a `Tap` the real-time thread checks with one
  relaxed atomic load (the whole cost while off), then `try_lock` only. Full
  queue, no free buffer, or a dead worker = the frame is dropped and counted.
  Buffers are allocated at most `capacity + 2` times, then recycled.
- **Video** stays NV12: NDI takes the `NV12` FourCC, so no colour conversion.
  The staging copy's pitch and aligned height (1088 for 1080) are packed out
  (`convert::pack_nv12`). Frame rate is measured from pts and snapped to a
  standard fraction (`FrameRate`, 59.94 → 60000/1001). The read-back is one
  frame behind the window so the render thread never waits on the GPU.
- **Audio** is the mix exactly as it goes to the endpoint, after the faders,
  48 kHz stereo float planar. On the sender, the program track as encoded
  (the mic when the share is mic-only).
- **Timecodes**: one 100 ns clock per output. Video is stamped at present;
  audio at when it will be heard (now + what is already queued in the
  endpoint), advanced by sample count so a steady stream is gapless, and
  re-anchored only when it drifts > 20 ms from the clock (`AudioClock`).
- **Connections**: `NDIlib_send_get_no_connections(…, 0)` each stats tick, in
  the stats line's `ndi` object with sent/dropped counters and the source name.
- **Control**: two saved settings, `UiPrefs::ndi_receive` and `ndi_share`
  (off by default). The service sets `--ndi` at spawn from them (never from
  the client) and, when one changes, sends `{"cmd":"ndi","on":…}` to the
  running engine (`service::sync_ndi`). The receiver carries a toggle made
  while waiting into the share.
- **Runtime status for the UI**: `Method::NdiStatus` → `Reply::Ndi`, a file
  check in the core (`relay_core::ndi::locate_runtime`): nothing is loaded
  into the core, so the footprint gate is untouched. Search order:
  `RELAY_NDI_RUNTIME` (tests; replaces the rest), `NDI_RUNTIME_DIR_V6`, the
  runtime's default folder (a core started before the install has no such
  variable), the engine's folder. Full paths only, never `PATH`.
- **Failure**: a D3D error in the read-back turns NDI output off with the
  reason in the stats line; the stream carries on. A missing or broken
  runtime is `ndi_error` + `runtime_missing` in the stats line; the card says
  "NDI output needs the NDI runtime" and links NDI's download.
- **LAN only**: NDI's own mDNS discovery on the local network. Relay sets no
  discovery server and no groups and does not touch NDI's configuration.

**UI.** `components/NdiCard.tsx` on Receive and Share: the switch, the source
name, "NDI receivers" while live, frames skipped if any, the runtime note, the
ndi.video link and the trademark line. Links open through the shell's
`open_ndi_link` command, which takes `"ndi"` or `"runtime"` and holds the
URLs itself. Existing controls and tokens only (one new `.linkbtn` style).

**IPC.** `Method::NdiStatus`, `Reply::Ndi { runtime }`, `UiPrefs.ndi_receive`
/ `ndi_share`, `EngineCmd::Ndi { on }` (both mirrors), `ShareStats.ndi` in
`ui/src/lib/ipc.ts`. Wire shapes locked by `ipc::tests::ndi_wire_shape`,
`share::tests::ndi_command_and_flags` and `command::tests::ndi_wire_shape_is_locked`.

**Not in this session.** Separate NDI sources per received track (their
audio / their mic / the rest); NDI|HX; NDI receive (Relay as an NDI input);
NDI tally. Each is a small addition on `NdiOutput` (a second `NdiOutput` per
track) if wanted.

## Tests

- `relay_core::ndi` — the runtime search (variable, default folder, override,
  missing, empty values) and source-name cleaning.
- `relay_capture::ndi::ffi` — every struct size and field offset, the FourCC,
  error classification.
- `relay_capture::ndi::convert` — NV12 packing (pitch, aligned rows, short
  buffers, odd sizes, 1080p), de-interleave, frame-rate snapping and learning,
  audio timecodes (gapless under jitter, re-anchor after a stall).
- `relay_capture::ndi::tee` — a 30 ms consumer under a 1 ms producer: worst
  push < 10 ms, most frames dropped, at most 4 buffers allocated; recycling;
  worker shutdown; dead worker = drop, not hang.
- `relay_capture::ndi` — on/off/on with a mock backend, nothing created until
  asked, planar audio with a timecode reaching the sender, missing runtime
  reported not raised, a 40 ms mock NDI sender never holding up the producer.
- `tests/ndi_runtime.rs` — the real loader in a child process
  (`relay-share ndi-probe`) pointed at an empty folder (runtime missing, with
  the download link) and at a file that is not a DLL (fails cleanly).
- UI `components/NdiCard.test.tsx` — off by default, the missing-runtime note
  and both links, Receive and Share saved separately, no stale write-back of
  other settings, the live source and receiver count from stats, the engine's
  load failure shown, the About attribution.

## Two-PC test plan (owed)

Rig: PC1 (Win11, sender) and PC2 (Win10, receiver), wired LAN. Installing the
NDI runtime, NDI Tools or the OBS NDI plugin (DistroAV) on either PC is the
**owner's call**; nothing in Relay's tests installs them.

1. **Absent runtime (no installs).** PC2: Receive → NDI output on → Start
   receiving, share from PC1. Expect the card's "needs the NDI runtime" note,
   the stream unaffected, `share.log` with one `NDI output unavailable` line,
   `relay-share ndi-probe` printing `runtime_missing: true`.
2. **Install the NDI 6 Runtime on PC2** (owner). Without restarting Relay,
   start a new share: the runtime is found in its default folder. The card
   shows Source "Relay (from PC1)", NDI receivers 0.
3. **NDI Studio Monitor** (NDI Tools) on PC2, or on a third NDI app on the
   LAN: the source appears as "PC2 (Relay (from PC1))"; picture and sound
   play; the card's receiver count goes to 1. Toggle NDI output off and on
   mid-share: the source disappears and returns within ~2 s, the Relay window
   never stutters (`presented` keeps climbing at the share rate).
4. **OBS with DistroAV on PC2** (owner installs): add an NDI Source → "Relay
   (from PC1)". Automatable through obs-websocket (OBS 28+, port 4455):
   `GetSourceActive` true; `GetSourceScreenshot` twice 1 s apart differ while
   PC1's screen moves; the `InputVolumeMeters` event shows a non-zero level
   for the source while PC1 plays speech/music. Record
   10 s in OBS and check A/V offset with the existing lip-sync clip (S39
   method), target within ±1 frame of Relay's own window.
5. **4K60 load.** PC1 shares 4K60 HEVC; PC2 publishes NDI with Studio Monitor
   watching. Record `presented` rate, `capture_to_present_ms`, and the `ndi`
   counters for 2 minutes: present rate unchanged vs NDI off; `ndi.video.dropped`
   reported honestly if the CPU cannot compress 4K60 (NDI encodes on the CPU).
6. **Sender publish.** PC1: Share → "Publish my share as NDI too" → share.
   On PC2 (or PC1) Studio Monitor shows "PC1 (Relay share)" with the program
   audio; the share to PC2 is unaffected.
7. **LAN only.** During steps 3-6, a `pktmon` (or Wireshark) capture on PC2
   shows NDI traffic only to LAN addresses (mDNS 224.0.0.251:5353 and the
   peers); nothing to the internet.
8. **Teardown.** Stop receiving: the source leaves the NDI list within a few
   seconds and `relay-share` exits as before (no added exit delay).

Footprint: the core links no NDI code (a file check only); the gate
(`scripts/footprint.ps1`) is unchanged by this branch.
