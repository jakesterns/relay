# S36 — Direct send to streaming software: the decision

**Status: a decision for the owner, not yet a plan.** Written 2026-09-22 so the
choice is in front of him with its costs; nothing is built.

The owner, 2026-09-18: beyond a second PC, send the feed and audio straight into
OBS, Streamlabs, TikTok Live Studio, or any other streaming program, with
little to no setup on the user's part.

## Two shapes

### (a) Local — Relay appears as a camera and microphone
The streaming program runs on the *same* PC as the game. Relay's capture,
which today goes to the encoder, also goes into the M5 virtual camera; the
program picks "Relay Camera" as a webcam, like any other. Audio goes to a
virtual microphone the same way.

What exists: the virtual camera (`MFCreateVirtualCamera`, the NV12 frame
ring, `RingWriter`) is built and registered through the elevated helper —
today fed by the *receiver's* decoded frames. Feeding it from the *sender's*
captured frames is the same `RingWriter::push` on the texture the encoder
already gets: one extra GPU→CPU copy per frame, the same cost the receiver
pays now.

What does not exist:
- **The virtual microphone.** It is the signed audio-class driver, which is
  behind the EV certificate that is still not ordered. The interim is the
  same one the receive side uses: render Relay's mix to a VB-Cable input
  endpoint (`mic_route`), and the user picks "CABLE Output" in OBS. Works,
  but it is a third-party install and a step to explain.
- **Windows 10.** The frame-server camera needs Windows 11 22H2+. On Windows
  10 there is no camera to feed — which is exactly PC 2's OS. For Windows 10
  the honest answer is NDI (v1.1 S20) or shape (b).

Cost: small. A day for the video path and the UI ("Also show this share as
Relay Camera on this PC"), plus the VB-Cable route for audio. Zero network.
Works with every program that takes a webcam.

### (b) Remote — Relay pushes to the platform itself
Relay takes a stream key and sends RTMP or SRT to Twitch/YouTube/TikTok
ingest directly. No OBS at all.

What exists: the encoder output is already an Annex B bitstream and the
recorder already muxes it into fMP4/MKV; an FLV/RTMP muxer is the same kind
of code. What does not: the outbound network path. The brief's "zero network
config" rule is fine (ingest is outbound, no ports), but its **"exactly one
other outbound request"** rule (the AutoEQ fetch) would need revisiting, and
a stream key is a secret Relay would then hold — a new class of thing to
protect, alongside the identity key.

Cost: a week plus. A new consumer of the encoder, a muxer, reconnect logic
of its own (S38's does not transfer: the far end is a platform, not a
remembered PC), a keys store, and platform-specific quirks (TikTok's ingest
is not public RTMP for most accounts). And it competes with OBS rather than
feeding it, which changes what Relay is.

## Recommendation
**(a) first, scoped to what is already there:** the sender feeds the virtual
camera; audio through the VB-Cable route until the driver is signed; Windows
11 only, said plainly in the UI on Windows 10. That is "little to no setup"
for OBS-style software on the same PC, and it is mostly wiring.

**(b) is a product decision**, not a session: it makes Relay a streaming
client. If the owner wants it, it gets its own plan with the outbound-request
rule rewritten first.

## What the owner needs to answer
1. Is (a) with the Windows 11 + VB-Cable caveats worth shipping now?
2. Is (b) wanted at all in v1.1, given what it changes about the brief?
3. If both: (a) first is the recommendation; say if the order should differ.
