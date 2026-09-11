# M6 — Recording, replay buffer, presets, source switching

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M6-recording-presets.md. Work on branch `m6-recording`. Recording must not add latency or CPU to the live share. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Depends on
M4.

## Definition of Ready
- [x] M4 complete with Measurements filled, so the "no added latency" claim has a baseline. (ROADMAP: M4 done 2026-09-10; baseline table in `docs/plans/M4-share.md` — loopback capture→arrival p50 4.6 ms / p99 7.4 ms, encode p99 10.8 ms @4K60.)
- [x] Recording location and disk budget decided (user, 2026-09-10): recordings and replay saves default to `%USERPROFILE%\Videos\Relay` (path changeable in Settings); disk budget = 50 GB cap with oldest-file pruning, and recording stops before free space drops below 10 GB.

## Decisions (2026-09-10)
1. **Same bitstream, not a second encoder.** The recording tees the exact AUs the share encoder produces (the checklist's "same bitstream when settings match" option). The tee is a bounded channel written with `try_send` from the pipeline threads: a slow disk drops recording fragments (counted) but can never block or delay the live path — 0 ms added latency by construction.
2. **Pure-Rust fragmented-MP4 muxer instead of the MF sink writer.** The kickoff requires golden-byte-fixture tests of the muxer state machine; MF sink writer output is not byte-deterministic across machines/driver versions, and it would put a COM object on the recording thread. A small fMP4 muxer for HEVC (`hvc1`+`hvcC`) and Opus (`dOps`) is deterministic, crash-safe (every closed fragment is playable), and is also what makes the < 2 s replay save possible (slice the ring → write fragments, no re-encode). MKV output deferred (see Deferred).
3. **Replay ring holds encoded AUs in RAM**, budgeted in bytes and seconds; memory ≈ bitrate × window (60 s @ 60 Mb/s ≈ 450 MB) and lives only in the per-share `relay-share` process, never the core. Eviction and save-slicing happen at keyframe boundaries; with the share's 10 s GOP a save may include up to one extra GOP before the requested window.
4. **Source switching swaps the capture source inside the running pipeline** (new WGC/DXGI source + converter feeding the *same* encoder and RTP track, then a forced IDR). The peer connection, tracks and SDP are untouched — no renegotiation. Region capture = monitor capture + a source-rect crop in the D3D11 video processor.

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
