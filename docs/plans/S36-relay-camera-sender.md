# S36 — Relay Camera on the sending PC

**Branch** `feat/stream-out` · **Worktree** `..\relay-camsend`

The local half of `v11-stream-out.md`, started 2026-09-22 on the assumption
The owner takes the recommendation there (local first; RTMP/SRT push remains his
decision). Scope: while this PC shares, it can *also* present the same
picture as "Relay Camera" on this PC, so OBS, Streamlabs, TikTok Live
Studio or anything else that takes a webcam here can use it with no setup
beyond picking the camera.

## What already exists
- **The camera** (M5): `MFCreateVirtualCamera`, the NV12 frame ring, and
  `vcam_sink::VcamSink` / `RingWriter`, registered through the elevated
  helper with the user's consent. Today only the *receiver* feeds it, from
  decoded frames (`render.rs`).
- **The other direction is already covered**: PC 2 sharing to this PC, with
  OBS here picking "Relay Camera", is M5 as built and needs only the test
  The owner has queued.

## Design
- The sender's capture loop already holds each frame as an NV12 texture on
  the GPU before it reaches the encoder. Push that texture into the same
  `RingWriter` the receiver uses: one extra `CopySubresourceRegion` + a
  mapped row copy per frame, the cost the receiver already pays. No second
  capture, no second encode.
- **Audio: none, on purpose.** On the same PC the streaming program captures
  the game's audio itself (OBS's application audio capture), which is better
  than anything Relay could route to it. Said in the UI so nobody looks for
  a "Relay Microphone" that is not there.
- **One feeder at a time.** The ring has one writer. The service refuses
  `vcam` on a share while a receive holds it, and the reverse, with a
  sentence saying which.
- **Windows 11 22H2+ only**, like the receiver's camera: on Windows 10 the
  toggle explains itself and stays off.
- **Service decides, never the client**: `ShareRequest::vcam` is set from
  the preset's wish *and* consent *and* registration, exactly like
  `receive_routing`. The preset gains `vcam: bool` (default `false`; old
  files read as off — the standing rule).
- Engine: `relay-share send --vcam`; the sink starts when the video pipeline
  is up at the capture size, restarts on a source switch (size can change),
  and is best-effort — a camera failure is reported and the share carries on.
- Stats: `vcam_frames` on the sender's line, so the strip can say the camera
  is being fed.

## Definition of Done
- [x] `--vcam` on the sender feeds the ring from the NV12 the encoder is
      about to take, at the encode size (fixed for the share, so a source
      switch needs nothing); best-effort, with the same Windows-11 guard as
      the receiver's camera; a failure is one `error` line and the share goes
      on.
- [x] Preset `vcam` field (old files read it as off); Share preset editor
      toggle saying Windows 11, "installed in Settings" and "video only — the
      streaming program captures the game's audio itself"; the service
      resolves it with consent + registration and refuses, in words, when a
      receive already holds the camera (and the reverse).
- [x] `vcam_frames` in stats; "Relay Camera — Live on this PC" in the
      connection card only while frames are actually reaching it.
- [x] Tests: args (`--vcam` only when the service left it set), preset
      round trip via the existing preset tests, UI toggle and readout. The
      service's one-feeder refusal has no unit test — it needs a live engine
      on each side; it is a four-line rule and is proven live.
- [ ] Live: OBS on this PC shows the share while it is being sent to PC 2.
      **Owed** — needs the camera registered here (the owner's UAC click).
- [x] All gates green: fmt, clippy `-D warnings`, 306 UI / 217 core /
      181 capture, footprint.

## Not built: Relay Camera with no second PC
A share still needs a paired receiver: the engine connects, then captures.
Game and OBS on one machine with nothing sent anywhere would be a
capture-only mode — `relay-share send` without a peer, or a `relay-share
camera` command — and it is not a few lines: the engine's lifecycle is
signalling → connect → capture, and the core's share supervision (intent,
reconnect, stop) is built around a peer. Same frame path, different
lifecycle. **A decision for the owner, then its own session**, not a footnote
here. What shipped is "Relay Camera while sharing", which is the
capture-card use case with OBS on the sending side.
