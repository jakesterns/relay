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

The mic track is a second WASAPI capture client, a second Opus encoder and
one extra sum per output frame on the receiver. Measured numbers are in
`docs/plans/M4-share.md`'s Measurements table, against the M4 baseline.
