# S36 — Relay Camera on the sending PC

**Branch** `feat/stream-out` · **Worktree** `C:\Users\stern\Documents\Code\relay-camsend`

The local half of `v11-stream-out.md`, started 2026-09-22 on the assumption
Jake takes the recommendation there (local first; RTMP/SRT push remains his
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
  Jake has queued.

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
- [ ] `--vcam` on the sender feeds the ring from captured NV12 at the
      capture size; survives a source switch; best-effort.
- [ ] Preset `vcam` field, migrated by default; Share preset editor toggle
      with the Windows-11 and audio notes; service resolves it with consent +
      registration and the one-feeder rule.
- [ ] `vcam_frames` in stats and a line in the strip while it is fed.
- [ ] Tests: args, preset round trip, service refusal, UI toggle states.
- [ ] Live: OBS on this PC shows the share while it is being sent to PC 2,
      *and* OBS shows it with no receiver at all (a share needs a receiver
      today — see Open question).
- [ ] All gates green.

## Open question (build the rest first)
A share today needs a paired receiver: the engine connects, then captures.
"Relay Camera with no second PC" — game and OBS on one machine, nothing sent
anywhere — means a capture-only mode: `relay-share send` without a peer, or
a new `relay-share camera` command. The frame path is identical; what
differs is the lifecycle (no signalling, no encoder). Decide after the
feeding path works: if a capture-only command is a few lines, do it here;
if not, it is its own session, and this one ships "camera while sharing".
