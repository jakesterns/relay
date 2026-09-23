# Dual audio: where the mix happens

Decision record for session S2 (`feat/dual-audio`). Written **before** the
code, as the Definition of Ready requires.

## The question

The sender ships one Opus track, so a preset that asks for the microphone
sends the mic *instead of* the desktop mix. Carrying both raises one design
question that everything else follows from: **does the sender mix the two
sources into one track, or does it send two tracks and let the receiver
decide?**

The two consumers pull in opposite directions:

- A **plain call** (the receiver is in Discord/Zoom/Meet) wants one stream.
  Whoever is on the call has to hear the game *and* the person talking.
- The **virtual mic** (M5) and the mix-minus work (S19) want them apart:
  routing the program mix to the call while the local mic stays local, or
  ducking one against the other, is impossible once they are summed.
- **Recording** (M6) wants them apart for a different reason: an editor
  balancing commentary against game audio after the fact cannot unmix.

## Decision

**Two Opus tracks on the wire. The receiver mixes, and mixing is the default.**

The sender never sums. It sends up to two tracks:

| Track | msid track id | Source |
|---|---|---|
| program | `relay-audio` | desktop endpoint loopback, or one process tree |
| microphone | `relay-audio-mic` | default capture endpoint |

The receiver decodes both and sums them in `playback.rs` immediately before
the WASAPI render buffer. A plain call therefore behaves exactly as it does
today — one audible stream on the chosen endpoint, whether that endpoint is
the speakers or the interim virtual-mic route — while the separation survives
all the way to the last op, which is where S19 needs to break it apart.

*2026-09-23:* S19 used the seam, though not by breaking the sum apart —
the call comes *back* as a fourth track (`relay-audio-return`, receiver →
sender), and the receiver's sum stays as it was. What changed at the last
op is that `playback::run` now takes any list of `(stream, fader)` pairs,
so the sender runs the same loop with its one incoming track. Record:
`docs/plans/S19-call-return.md`.

### Why not sender-side mixing

It is cheaper on the wire (one Opus stream, ~160 kb/s saved) and simpler. It
is also irreversible, and it throws away the thing the next two milestones
need. 160 kb/s against a 40–80 Mb/s video budget is 0.2–0.4 % — not a reason
to foreclose a feature. And a sender-side mix would have to run a limiter to
avoid summing two full-scale sources into clipping, which is DSP on the share
hot path: exactly what `CLAUDE.md` says the share audio must not carry ("the
engine never resamples or applies DSP to share audio").

### Why not receiver-side routing as the default

Two endpoints by default means a user who picks "Microphone" hears nothing
different until they also configure a route. The failure mode of the default
should be "it works", not "it is silent". Routing is the opt-in.

## Consequences

- **Recording carries two audio tracks, not a mix.** Track 2 is the program
  mix, track 3 the microphone, both Opus, both in the same fragmented MP4.
  A player picks track 2 by default (the program mix — what the share
  sounded like); an editor sees both and can balance them. This is the one
  place the decision is user-visible in a way that can surprise: the mic is
  *in* the file but not audible in a dumb player until it is selected.
- **Presets carry a source *set*.** `PresetAudio` stops being a four-way
  choice and becomes `{ desktop: system | game | off, mic: bool }`. The old
  four-way JSON still deserializes (`"mic"` → `{desktop: off, mic: true}`),
  so nobody's `presets.json` changes meaning.
- **`--audio-mic` becomes additive.** It adds a mic track rather than
  replacing the desktop source. Mic-only is `--no-audio --audio-mic`, which
  is exactly what the core emits for a legacy `mic` preset — so behaviour
  for existing presets is byte-identical.
- **A peer sending one track still works.** The receiver classifies audio
  tracks by msid track id and falls back to arrival order; a single audio
  track is always the program mix regardless of what it is called.

## Cost

The mic track is a second WASAPI capture client, a second Opus encoder and one
extra sum per output frame on the receiver. Measured against M4's baseline in
`docs/plans/M4-share.md` (4 alternating reps, loopback, 1440p60):
**+0.05 ms p50 / +0.57 ms p99** on capture→arrival, encode mean unchanged,
60 fps and zero drops throughout. Harness: `scripts/dual-audio-check.ps1`.

One finding is worth keeping, because it was not obvious and it is the whole
reason the second encoder is configured differently from the first. The mic
track first shipped with the program mix's encoder — 160 kb/s stereo,
`Application::Audio`, libopus's default complexity — and that cost the **video**
path a clean, repeatable regression: encode mean 5.12 → 5.86 ms and
capture→arrival p99 3.3 → 7.6 ms across three reps, while the audio
packetization numbers showed no separation at all. The cost was CPU
contention on the sender, not the audio pipeline. A microphone is speech and
does not need a music-grade encoder: `OpusProfile::voice()` (64 kb/s,
`Application::Voip`, complexity 5) removed the regression entirely and cut the
track from ~160 kb/s to ~50 kb/s. The program mix deliberately leaves
complexity at libopus's default (`complexity: None`) so the single-track path
is byte-for-byte what M4 measured.

### Measurement trap

WASAPI loopback of a **silent** render endpoint delivers no packets at all —
not silence, nothing. A quiet desktop therefore makes the program track read
zero packets and any comparison against it meaningless. Two measurement passes
were thrown away before this was spotted; `scripts/dual-audio-check.ps1` now
plays a generated tone through the default endpoint throughout and asserts
every run carried the packet count it should have.

## Containers

Both recording containers carry both tracks (S4 landed MKV alongside this
work; the muxers were merged here rather than in `main`):

| | program mix | microphone | labelled by |
|---|---|---|---|
| fMP4 | track 2 | track 3 | `hdlr` name |
| MKV | track 2 | track 3 | Matroska `Name` |

Verified end to end with ffprobe on real 20 s loopback recordings in both
containers: three streams each, both audio streams Opus 48 kHz stereo, tagged
`Relay Audio` and `Relay Microphone`, zero recorder drops. The two tracks
carry genuinely different audio — the program track measured peak −20.7 dB /
RMS −23.8 dB (the test tone's sine crest factor) against the mic's −14.3 dB /
−33.1 dB.
