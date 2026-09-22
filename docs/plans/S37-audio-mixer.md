# S37 — Audio mixer on Share and Receive

**Branch** `feat/audio-mixer` · **Worktree** `C:\Users\stern\Documents\Code\relay-mixer`

Jake, 2026-09-18: when sharing a window or one application rather than the
whole screen, choose what goes out — that app's sound, the rest of the PC,
the microphone — each on its own fader with mute, live while sharing. The
same on the receiving side: mute or drop system sounds, app sound, the mic,
in real time. Under the Start/Stop button on each screen.

## The shape, and why it is small

Relay already does the hard half. The sender runs **one Opus track per audio
source** (`audio_pipeline` in `sender.rs`: the program mix or one process,
and the microphone as a second track since S2). The receiver decodes each
track separately and sums them in exactly one place — `playback.rs`
`render_loop`, one `mix_sum` per sample — which S19 (mix-minus) was always
going to cut into. So:

- **"That app's sound"** is the existing process-loopback capture
  (`AudioSource::Process`, `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`).
- **"Everything else on the PC"** is the *same* API with the other flag:
  `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` — the whole endpoint
  minus the app's process tree, delivered by the audio engine already mixed.
  One new `AudioSource::Rest { pid }` variant, one line in `activate_client`.
  No WASAPI session walking, no per-session mixing of our own.
- **The microphone** is the second track it already is.

Three sources, **three tracks**, each with the machinery it has today. The
mixer is then two small things:

1. **A fader per track on each end.** Sender: gain and mute applied to the
   10 ms frame just before it is encoded (`OpusStream::next`), so a muted
   track still sends packets — silence is cheaper to reason about than a
   track that comes and goes, and the receiver's meters stay honest. Receiver:
   gain and mute applied per decoded stream in `render_loop`, before
   `mix_sum`. Gains ramp linearly across the frame they change in, so a
   fader move never clicks.
2. **A live command.** `EngineCmd::Mixer { faders }` on the existing stdin
   channel, on both engines, so nothing restarts; `Method::SetMixer { side,
   faders }` in the core; `api.setMixer` in the UI. Fader state is
   per-share and starts at unity. (Persisting it per preset is a follow-up;
   the standing rule means it would need a migration, and nobody has asked.)

Not a mixing engine. Not DSP on the share path. Relay's own mix only; no
endpoint volume, no default device, nothing global — the non-negotiables
hold by construction, because every source is a shared-mode WASAPI capture
Relay already opens.

## Wire

```
// stdin, both engines
{ "cmd": "mixer", "faders": { "app": { "gain": 1.0, "mute": false },
                              "rest": { "gain": 0.5, "mute": false },
                              "mic":  { "gain": 1.0, "mute": true } } }
```
`gain` is linear 0.0–2.0 (unity 1.0; +6 dB at 2.0); missing faders are
left as they are. The sender ignores faders for tracks it is not sending;
the receiver ignores faders for tracks that never arrived.

Track ids (the msid contract the receiver classifies on, `audio_role`):
`relay-audio` = app *or* the whole desktop mix (unchanged, so an older
receiver hears what it always heard), `relay-mic` (unchanged),
**`relay-rest`** (new). An older receiver that does not know `relay-rest`
falls into the arrival-order fallback and treats it as the mic — wrong but
audible; the receiver in the same build classifies it correctly. Worth one
line in the release note, not a compatibility layer.

## Preset

`PresetAudio` gains `rest: bool` (default `false`): with `desktop: "game"`,
also send everything else on the PC as its own track. Meaningless with
`system` (that already *is* everything) and with `off`; the UI only offers
it beside "Game only". Old `presets.json` files read it as `false` — the
standing rule holds with no migration.

## Recording

`Recorder::push_audio(track_index, …)` takes the track index the wire uses.
The muxer's audio-track count is the one thing to check before committing to
three tracks on disk; if it is fixed at two, the recording keeps app + mic
and drops rest, and the plan says so in the UI copy. Check first, then
decide — do not widen the muxer in this session unless it is one number.

## UI

- **Share**: a "Mixer" card directly under Start/Stop, visible while sharing
  (before that, the preset's Audio chips are the choice). One row per track
  being sent: name, `Slider` (−∞…+6 dB, shown in dB, unity marked), mute.
  Rows for tracks not in the preset do not appear. The existing dB meters
  gain a third bar for `rest`.
- **Receive**: the same card under Start/Stop receiving, visible once paired;
  one row per track that has arrived. Same look, same words.
- Live: a slider move sends within ~50 ms (debounced) and never restarts.
- jsdom component tests only; a range input is driven with `fireEvent`, never
  the real cursor.

## Stats

The sender's `stats` line gains `rest_packets` / `rest_peak` beside the
existing `audio_*` and `mic_*`; the receiver's gains `rest_packets`. The
strip reads them like the others.

## Definition of Ready
- [x] Process loopback exists for one process and the *exclude* flag exists
      in the `windows` crate (0.62: `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`).
- [x] Two Opus tracks already travel and are classified by msid id.
- [x] The engines already take live commands on stdin (`Preview`, `Switch`,
      `Host`).
- [ ] The recorder's audio-track count is known (see Recording).

## Definition of Done
- [ ] `AudioSource::Rest { pid }`: everything on the endpoint except the
      target's process tree, via the exclude flag; unit test on the params.
- [ ] Three tracks on the wire when the preset asks (`rest: true` with
      `game`); an older receiver still hears app + mic.
- [ ] `EngineCmd::Mixer` on both engines; gains ramp within the frame; a
      muted track keeps sending silence.
- [ ] `Method::SetMixer`, Tauri command, `api.setMixer`; mixer state is
      per-share and resets to unity.
- [ ] Mixer cards on Share and Receive as described; rows only for tracks
      present; live while sharing; tests in jsdom.
- [ ] Levels for all three tracks in the strip.
- [ ] Recording: tracks on disk match what the muxer supports, and the UI
      says which.
- [ ] Listening check on the second PC with relay-pc2: **speech and music,
      never a tone.** Mute the game and hear only the rest; mute the rest and
      hear only the game; move a fader mid-share and hear no click. Recorded
      in `BUGS.md` against the build hash.
- [ ] All gates green.

## Kickoff prompt
```
You are session S37 (audio mixer on Share and Receive) for Relay. Read CLAUDE.md,
docs/plans/S37-audio-mixer.md (the design; follow it) and docs/plans/SESSIONS.md
(section S37 and the standing rules).

Worktree: git worktree add -b feat/audio-mixer ..\relay-mixer main; pnpm install
in ui/; RELAY_NO_INSTALL=1 for commits and pushes.

The design is three tracks, not a mixing engine: "everything else on the PC" is
WASAPI process loopback with the EXCLUDE flag, one new AudioSource variant. Fader
gain and mute apply per track just before encode on the sender and per decoded
stream before mix_sum on the receiver, ramped across the frame. A live "mixer"
command on the existing stdin channel on both engines. Check the recorder's
audio-track count before deciding what lands on disk.

Non-negotiables: shared-mode WASAPI only, Relay's own mix only, no endpoint
volume or default-device change, no DSP on the share path. UI tests in jsdom
only. relay-pc2 is a Claude session on Jake's second PC (SendMessage; ListAgents);
the listening check there uses speech and music, never a tone. Builds ship from
main through the main-tree session. Do not block on a question: take the most
reversible option, write it down, continue. Finish by updating docs/ROADMAP.md.
```
