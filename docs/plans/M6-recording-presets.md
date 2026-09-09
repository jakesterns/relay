# M6 — Recording, replay buffer, presets, source switching

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M6-recording-presets.md. Work on branch `m6-recording`. Recording must not add latency or CPU to the live share. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Depends on
M4.

## Definition of Ready
- [ ] M4 complete with Measurements filled, so the "no added latency" claim has a baseline.
- [ ] Recording location and disk budget decided: ____

## Checklist
- [ ] Local recording: second encoder instance (or the same bitstream when settings match) muxed to fragmented MP4 / MKV via Media Foundation sink writer; file rolls hourly; path in Settings.
- [ ] Replay buffer: ring buffer of encoded HEVC + Opus (default 60 s, configurable), hotkey saves the last N seconds to disk without touching the live stream.
- [ ] Presets Game / DAW / Desktop as data: encoder (codec, bitrate, fps, GOP), audio sources (system, process-only, mic, Relay Send later), cursor, preview. Stored in `presets.json`; profile's `SharePreset` references one.
- [ ] DAW preset: 1440p60 default, audio from the default endpoint at 48 k with no processing.
- [ ] Multi-source switching: display / window / region sources listed in the Share screen; switching swaps the capture source without renegotiating the peer connection.
- [ ] Instrument strip shows recording state and buffer fill.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- Recording on adds < 1 % CPU and 0 ms latency to the live share.
- Replay save of 60 s at 4K60 completes in < 2 s.

## Deferred
_(none yet)_
