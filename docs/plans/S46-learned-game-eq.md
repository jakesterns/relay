# S46 — Learned game EQ

Branch `feat/s46-learned-game-eq`, cut from `main` (d3a4bc8).

## Why

The owner's rule: **no hard-coded per-game presets.** Relay learns each game's
EQ from that game's own audio — so it works for a game nobody has heard of and
keeps up when a patch remixes the audio — and stacks it on the active
listening device's headphone correction (S41). Players can import and export
game EQs. No AI service, no subscription, no account: local signal analysis
only. Nothing is recorded and nothing leaves the PC.

## Design

```
game PID ──process loopback──▶ relay-share learn (helper, only while the game is focused)
                                 Analyzer: filterbank + pitch + onsets → 9 classes, context gate
                                 LearnRecord: rolling 60-min window of aggregates, checkpoints
                                 └─ writes data\game-eq\<exe>.json (statistics only)
relay-core ─ reads the record on request, applies user actions, spawns/stops the helper
UI (Games ▸ Audio) ─ "Learn this game's sound": goal, progress, Apply/Relearn/Reset, Import/Export
DSP chain: headset correction (S41) → game layer (≤ 4 bands) → the user's own bands
```

### Where things live

| Piece | File |
|---|---|
| Analyzer (classes, gate, aggregates) | `crates/audio/src/learn/analyzer.rs` |
| Masking matrix, goals, curve, limits | `crates/audio/src/learn/derive.rs` |
| Readiness, convergence, rolling window, versions | `crates/audio/src/learn/state.rs` |
| Game EQ file, import blend | `crates/audio/src/learn/file.rs`, `docs/eq-file-format.md` |
| Synthetic test audio | `crates/audio/src/learn/synth.rs` (tests only) |
| Helper process | `crates/capture/src/learn.rs` (`relay-share learn`) |
| Core glue, IPC actions, exe version | `crates/core/src/game_eq.rs`, `Method::GameEq` |
| Chain stacking + safety limiter | `crates/core/src/audio_bridge.rs` |
| UI | `ui/src/screens/Games.tsx` (`GameEqCard`), `ui/src/lib/ipc.ts` |

The analysis never runs in the always-on core: the helper is the existing
`relay-share` binary (already shipped, signed and firewall-scoped), so no
packaging change. The core links only the record/derivation types (no FFT).

### Analysis (every 10 ms frame)

24 one-third-octave bands (50 Hz – 10 kHz), the frame's level, and a small
autocorrelation pitch tracker on a ~4 kHz decimated copy. Every level that
reaches a histogram is relative to the session loudness, so the volume slider
does not move the statistics.

**Context gate** — the frame counts only if none of these hold:

| Rule | Constant |
|---|---|
| Silence | `SILENCE_DBFS` = −70 dBFS |
| Clipping | `CLIP_MIN_SAMPLES` = 3 at `CLIP_LEVEL` = 0.999 |
| Volume change (level jump) | `LEVEL_JUMP_DB` = 10 dB for net `LEVEL_JUMP_REJECT_FRAMES` = 2 s; re-base at 4 s |
| Nobody playing (cutscene, menu, AFK) | `INPUT_IDLE_MS` = 20 s since the last system-wide input (`GetLastInputInfo`) |
| Player chat | voice whose added energy below 180 Hz *and* above 4.5 kHz is `CHAT_EDGE_DB` = −20 dB under the speech band (codec band limit) |

Voice **with** input (callouts, NPCs) is gameplay and is kept; voice or music
with no input for 20 s is a cutscene and is excluded.

**Classes** (each a feature rule with a synthetic test):

| Class | Rule (constants in `analyzer.rs`) |
|---|---|
| Footsteps | short (< `CUE_MAX_MS` 300 ms), sharp (≥ `CUE_MIN_ONSET_DB` 7 dB), bright (added energy peaks ≥ `CUE_MIN_PEAK_HZ` 1.6 kHz); *rhythmic* when the last two intervals agree within `RHYTHM_TOLERANCE` 25 % in 250–1200 ms |
| Foliage / cloth | soft onset (< `FOLIAGE_MAX_ONSET_DB` 15 dB), noise-like (spread over ≥ `FOLIAGE_MIN_SPREAD` 5 high bands), ≤ `FOLIAGE_MAX_MS` 800 ms |
| Mechanical (reloads, clicks) | ≤ `MECH_MAX_MS` 40 ms, ≥ `MECH_MIN_ONSET_DB` 12 dB, bright, not rhythmic |
| Gunshot | ≥ `GUN_MIN_ONSET_DB` 15 dB rise in ≥ 2 bands < 250 Hz *and* ≥ 4 bands ≥ 1 kHz, ≥ `GUN_LOUD_DB` 15 dB over the session, a tail ≥ 50 ms, highs ≥ `GUN_MIN_HIGH_RATIO` of lows |
| Explosion / crash | low-weighted and loud (≥ `LOUD_ABOVE_DB` 30 dB) or long; loud non-bass bangs too |
| Vehicle | 1 s of continuous pitch in 25–250 Hz that glides ≥ `VEHICLE_MIN_GLIDE` and never jumps > `VEHICLE_MAX_STEP` |
| Voice | 3–7 Hz syllable modulation ≥ `SPEECH_MOD_FRACTION` of the envelope, depth ≥ 3 dB, 250 Hz – 2 kHz dominant by 6 dB; distinct voices by pitch clusters (`VOICE_CLUSTER_SEMITONES` 5) |
| Music | tonal: band levels steady frame to frame (`TONAL_MIN_FRACTION` 0.6) |
| Ambience | stationary background with no transients (everything else) |

A bright transient landing *inside* another sound (a step during an explosion,
a reload over music) is filed as a cue on the spot. Overlaps are otherwise not
separated: each class keeps a per-band level histogram and a time share, and
that is what the masking matrix is built from.

**Kept (and only this):** per class × band level histograms (sparse JSON),
event counts, frame counts, overlap counts, voice pitch clusters, gate-reject
counters. Record files are a few tens of KB.

### Derivation (per goal)

The masking matrix: for target class T and masker class M, per band, the share
of M's material within `CLEAR_DB` (6 dB) of T's typical level, weighted by M's
share of the time. Goals weight the classes:

| Goal (default: Awareness) | Lifted | Tamed |
|---|---|---|
| **Awareness** — "Hear footsteps, reloads and callouts; tame explosions, music and engines." | footsteps 1.0, foliage 0.8, reloads 0.8, voice 0.6 | explosion 1.0, vehicle 0.8, music 0.8, gunshot 0.5, ambience 0.5 |
| **Dialogue** — "Keep voices clear over effects and music." | voice 1.0 | music 1.0, explosion 0.8, gunshot/vehicle/ambience 0.6 |
| **Immersion** — "A gentle balance that stays close to the game's own mix." | cues/voice 0.3–0.4 | maskers 0.2–0.4, whole curve × `IMMERSION_SCALE` 0.5 |

Lift = max boost × target weight × presence × how often buried. Cut (≤ 315 Hz
only, gently: 0.3 dB per dB of dominance over 6 dB, never where a target
lives). Then, for every goal: caps **+6 / −9 dB**, no boost ≤ 80 Hz, 1-2-1
smoothing, ≤ 3 dB between neighbouring bands, the **speech guard** (300 Hz –
4 kHz never cut by more than `SPEECH_GUARD_DB` = 2 dB net, so callouts and chat
stay clear), and the **hearing rule** — never louder overall: the curve is
first lowered as a whole (gain compensation, within the guard), then any excess
comes off the boosts. A game layer with boosts brings a full-band safety
limiter (−1 dB) when the profile has none. The curve is fitted to at most
`GAME_BUDGET` = 4 biquads (`fit.rs`). Any analysis error keeps the last good
curve.

Changing the goal re-derives from the saved aggregates at once — no relearn —
and a learned layer follows immediately.

### Readiness, rolling window, versions

Evidence, not a timer: ready when the window holds `MIN_CUES` = 300 target
events and `MIN_MASKERS` = 60 masker events for the goal (stationary classes
count one per 0.5 s), **and** the curve has converged: the last
`CONVERGE_CHECKPOINTS` = 3 checkpoints, one per `CHECKPOINT_SECS` = 60 s of
active play, agree within `CONVERGE_DB` = 0.5 dB in every band. Progress is
90 % evidence + 10 % convergence.

After that the applied curve is **frozen**. Learning continues in a rolling
window (`WINDOW_SEGMENTS` 6 × `SEGMENT_SECS` 600 s = the last 60 min of active
play); a newly converged curve that differs by more than `UPDATE_DB` = 1.5 dB
in any band is offered (or applied, with "Apply new curves automatically").

The record is per exe and stores the exe's file version (`GetFileVersionInfoW`
on the path, no process handle). A new version resets the evidence and shows
**needs relearn** while the old layer keeps applying.

States: off · learning · ready · applied (applied (imported), applied
(imported, fine-tuned here)) · needs relearn.

### Safety guard at every entry point (review fix)

`relay_audio::learn::derive::guard_curve` applies the same rules as the
derivation — +6 / −9 dB caps, no boost at or below 80 Hz, at most 2 dB net
cut in 300 Hz – 4 kHz, at most 3 dB between neighbouring bands, never louder
overall (uniform weighting) — to any curve. It runs on import (clamp, and the
UI is told; non-finite numbers are still refused), on every offer and the
fine-tune blend, on a record's candidate when loaded from disk, and as the
last gate in `chain_params_with` before biquad fitting, so a curve from IPC or
a hand-edited `profiles.json` cannot bypass it. Record reads are capped at
1 MiB. A per-record lock file (`<exe>.lock`, held by the helper for its whole
run) makes Reset / Relearn / goal changes and the next helper wait for a
stopping helper's final save.

### What the learner hears

Process loopback taps the game's streams in the audio engine before the
endpoint effect chain, so the learner hears the game's pre-EQ mix and cannot
chase its own curve. Live test step 9 confirms it on hardware.

### Hardware independence and imports

The game layer is stored on the profile, independent of the headset: changing
the listening device or output swaps only the S41 correction underneath it. No
relearn. An imported layer applies at once as **applied (imported)** with
learning off; "Keep learning to fine-tune for my setup" offers the import
moved `IMPORT_BLEND` = 50 % towards the local result under the same rules,
always from the original import. Export writes the applied layer, whatever its
source.

## What is verified (synthetic signals only; no audio played, no game run)

- **Every class** from its own synthetic segment (`synth.rs`): rhythmic
  footsteps, irregular clicks, soft rustles, broadband gunshots, low booms, a
  gliding engine, a held melody, two talkers at 110 / 220 Hz counted as two
  voices, band-limited noisy player chat vs full-band voice-over.
- **Mixed scene** (steps over engine + music + talker): steps still found as
  rhythmic footsteps, no false impacts.
- **Context**: voice counts with input, voice and music after 20 s without it
  do not; silence, clipping, a 20 dB volume change (statistics unchanged
  within 4 dB).
- **Derivation**: buried footsteps lifted and explosion lows cut within every
  limit; every goal on a full scene inside the limits; the speech guard holds
  against a masker pinned at full scale (curve and fitted filters); goal switch
  moves the curve the expected way (Awareness lifts 2–4 kHz more than
  Dialogue; Dialogue favours 0.5–1.6 kHz; Immersion nearest neutral);
  determinism (same input, same curve and filters).
- **State machine**: not ready before the evidence, ready only after counts
  *and* convergence, freezes against small drift, offers an audio update once
  the window rolls, version change → needs relearn with the old layer kept,
  error keeps the last good curve, goal change re-derives without relearning.
- **Files**: round-trip, every rejection case, import blend without drift,
  export of learned / imported / tuned layers, no hardware or personal fields.
- **Core**: goal-first flow, import with learning off then fine-tune, other
  game's file refused, relearn/reset, needs-relearn status, chain order
  correction → game → user bands, identical game bands under any headset,
  safety limiter rules.
- **UI** (`Games.s46.test.tsx`): goal prompt before learning, prompt shown when
  on by default, instant goal change with no relearn, progress and Apply,
  import → applied (imported) with learning off, refused import, export path,
  reset confirmation, the privacy line.

**Measured** (synthetic, `cargo test --release -p relay-audio -- --nocapture`):

- Time to ready with shipped thresholds at a realistic event rate (a footstep
  every 1.5 s, an explosion every 8 s, all play active): **9 min** of active
  play. Real games have quieter stretches, menus and idle time that do not
  count, so expect **~10–20 min of active play**.
- Analyzer cost: **0.31 % of one core** at 48 kHz stereo on a mixed scene
  (budget 1 %); zero when not learning (no helper process exists).
- Core idle footprint gate: PASS (8.2 MB RSS, 0 % CPU).

## Not verified yet — live test plan

Never run against a real game in this session. On the owner's PC:

1. A profile with audio processing for a game; Games ▸ Audio shows "Learn this
   game's sound" on, the goal prompt first. Choose Awareness.
2. Focus the game: Task Manager shows one `relay-share.exe learn` child; CPU ≤
   1 % of a core. Alt-tab away: it exits within a second; nothing remains.
3. Play ~15 min. Progress rises only while playing; leave the game idle in a
   menu for 30 s and confirm progress does not move (input-idle gate).
4. Ready → Apply; listen (speech/music, per the listening-check rule) for
   clearer steps and calmer explosions, no overall level jump.
5. Switch goal to Dialogue: the curve changes at once, no progress reset.
6. Switch headset in the listening list: the game layer is unchanged; only the
   correction changes (`relay-core status --json`).
7. Discord call running during play: `game-eq\<exe>.json` voice clusters do not
   grow from Discord; in-game proximity chat (if the game has it) shows as
   rejected chat frames.
8. Export, delete, import the file: applied (imported), learning off.
9. Confirm process loopback captures the game **before** the endpoint APO (the
   learned curve must not chase its own EQ): with the layer applied, keep
   learning for 10 min and confirm the curve does not drift towards flat.
10. Anti-cheat titles: repeat 2 with an EAC / BattlEye / Vanguard game; nothing
    new is accessed (see `docs/dev/anti-cheat.md` #28–#30).
