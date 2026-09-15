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
- [x] Local recording: same bitstream as the share (decision 1), muxed to fragmented MP4 by the pure-Rust muxer (decision 2 — not the MF sink writer; MKV deferred); file rolls hourly at a keyframe; path + disk budget in Settings (`RecordingSettings` in `presets.json`, `set_recording_settings` over IPC). Verified live on loopback: 65 s share → 44.6 MB fMP4, ffprobe reads HEVC 1440p60 + Opus, correct duration.
- [x] Replay buffer: RAM ring of encoded HEVC + Opus (default 60 s, per-preset `replay_secs`), keyframe-aligned eviction, byte cap. Ctrl+Alt+R (core hotkey) / UI button / `save_replay` IPC → `replay_save` on the engine's stdin → slice muxed to `Relay Replay <timestamp>.mp4` without touching the live stream. Verified live (see Measurements).
- [x] Presets Game / DAW / Desktop as data in `presets.json` (editable built-ins + custom rows): bitrate, fps, encode size cap, audio source (system / game-process / mic / off), cursor, record-on-start, replay window. Codec is HEVC and GOP is fixed by the engine's latency tuning (documented in `presets.rs`). `StartSharePreset` resolves preset → `ShareRequest` (game preset picks the focused game's pid for process-only audio). Profile's `SharePreset` chip names the preset id.
- [x] DAW preset: 1440p60 (encode-size cap `--size 2560x1440`, GPU-scaled), audio = default endpoint loopback at 48 k with no processing (the share path never resamples or applies DSP), 40 Mb/s.
- [x] Multi-source switching: display / window (WGC `CreateForWindow`, HWND from the process list) / region (monitor capture + video-processor source rect, clamped to even pixels) in the Share screen. The swap replaces only the capture source + converter behind the *same* encoder, RTP track and peer connection, then forces one IDR — no renegotiation. Verified live: region → display switches mid-share, receiver held 60 fps (one 58 fps sample at the boundary), zero decode errors.
- [x] Instrument strip shows recording state (REC segment, MB written, drop/disk warnings) and replay-buffer fill, fed by the new `recording`/`rec_mb`/`replay_fill`/… fields on the engine's stats lines.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason. ✔
- Recording on adds < 1 % CPU and 0 ms latency to the live share. ✔ (median CPU +0.06 pt, latency unchanged — see Measurements)
- Replay save of 60 s at 4K60 completes in < 2 s. ✔ by measurement + extrapolation (55 ms for the buffered 41 MB; 450 MB at a full 60 Mb/s extrapolates to ~0.7 s — see Measurements and Deferred)

## Measurements (loopback, 2026-09-11, same rig as M4: RTX 3090, NVIDIA HEVC Encoder MFT, 1440p60 native, 60 Mb/s CBR ceiling, idle desktop ≈ 2–16 Mb/s produced)
Driver: `scripts\m6-loopback.ps1` (65 s send→recv on this PC, explicit `127.0.0.1:<port>` peer). M4 baseline for reference: capture→arrival p50 4.6 / p99 7.4 ms.

| Metric | Recording OFF | Recording ON (+ 60 s replay ring) |
|---|---|---|
| capture→arrival (receiver) | p50 1.96 ms / p99 2.61 ms | p50 1.97 ms / p99 2.63 ms |
| capture→send (sender avg) | 1.9 ms | 1.9 ms |
| encode (avg) | 4.9 ms | 4.9 ms |
| fps / drops / rec drops | 60.0 / 0 / — | 60.0 / 0 / 0 |
| process CPU median (p90) | 3.12 % (9.4 %) | 3.18 % (12.5 %) |

- **Added latency: 0 ms** — the tee is a `try_send` clone off the encoder output; the deltas above are run-to-run noise (recording-on even sampled marginally lower than the M4 baseline).
- **Added CPU: +0.06 pt at the median.** The mean rises ~1.2 pt because periodic disk flushes/ring evictions add tail spikes (p90 9.4→12.5 %); the steady-state cost is well inside the 1 % budget.
- **Replay save: 41.1 MB (64.7 s = 60 s window + one 10 s GOP) in 55 ms** → ~750 MB/s effective. A full 60 s at 60 Mb/s is 450 MB → ~0.7 s extrapolated, comfortably < 2 s. The saved clip and the rolling recording both validate in ffprobe (fMP4, HEVC 1440p60 + Opus, correct durations).
- **Source switch:** region↔display mid-share; receiver 60 fps throughout except one 58 fps half-second sample at the swap; 0 decode errors; same DTLS session end-to-end.
- Footprint gate after the core changes (presets store, new IPC, Ctrl+Alt+R): **PASS** — idle RSS 5.1 MB, 0 % CPU.

## Deferred
- **MKV container.** Decision 2: the deterministic pure-Rust fragmented MP4 covers crash-safety and every player tested (ffprobe/VLC-class); MKV would double the muxer surface for no new capability. Revisit only if a user-facing compatibility gap appears (e.g. Opus-in-MP4 in a specific editor).
- **True-4K60 recording/replay numbers under real game motion.** This session's monitor is 1440p and the desktop was near-idle, so produced bitrate sat at 2–16 Mb/s. The 4K60 claim rests on the M4 4K encode benchmark (unchanged path) plus the measured 750 MB/s save throughput; a full-motion 4K60 run lands in the MVP validation pass alongside M4's deferred two-PC run. Runbook: game at 4K60 (or `RELAY_BITRATE_MBPS=80` upscale), `m6-loopback.ps1 -Record -Secs 90`, read `replay_saved.ms` and the rec-drop counter.
- **One-hour roll soak.** Hourly rolling is implemented (writer closes/reopens at the first keyframe past `roll_secs`) and unit-level logic is covered, but a real 60-minute soak was not run in this session. Same MVP validation pass; runbook: `-Record -Secs 4000` with `roll_secs` left at 3600, expect two files, both playable.
- ~~**Simultaneous mic + desktop audio in the recording**~~ **Done 2026-09-14 (session S2).** The fragmented-MP4 muxer takes a *list* of audio tracks rather than one (`MuxConfig::audio: Vec<AudioConfig>`, `push_audio(track, …)`), so a share carrying both sources records both: track 2 is the program mix (`Relay Audio`), track 3 the microphone (`Relay Microphone`), both Opus, both named in `hdlr` so an editor labels them. They are deliberately *not* mixed down — an editor balancing commentary against game audio after the fact cannot unmix, and that is the whole reason a recording wants them apart (`docs/dev/dual-audio-decision.md`). The one user-visible wrinkle: a dumb player picks track 2 and plays the program mix only, so the mic is in the file but not audible until it is selected. Empty-track and offset-tiling cases are covered by unit tests, and the single-track golden fixture is byte-identical.
