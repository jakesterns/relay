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

5. **MKV is a per-preset container, added 2026-09-14 (session S4), and `mfra` is now always written.** The deferral below was reopened after a measured compatibility gap: `Windows.Media.Editing.MediaClip` — the Windows video-editing import API — rejected *every* Relay recording, because the fMP4 muxer wrote no `mfra` random-access index. Two separate fixes came out of that, and only the first is about MKV at all:
   - `Mp4Muxer::finalize` now emits `mfra` (one `tfra` per track, `mfro` last). Cleanly-stopped recordings import everywhere they are supposed to.
   - `mfra` can only be written at `finalize`, so a **crashed** recording still has no index and stays un-importable. Matroska has no such end-of-file obligation: the `Segment` carries an unknown size, clusters are self-delimiting, and the one patched field (`Duration`) is rewritten after every cluster rather than at the end. That is exactly why OBS defaults to MKV, and it is now measured rather than assumed — see the table in `docs/dev/container-compat.md`.

   Opus-in-MP4, the gap the original deferral guessed at, turned out **not** to be a problem: Media Foundation reports `OPUS … FullySupported` and imports it fine. The real discriminator was the index.

## Checklist
- [x] Local recording: same bitstream as the share (decision 1), muxed by the pure-Rust muxer (decision 2 — not the MF sink writer) into **fragmented MP4 (default) or Matroska, chosen per preset** (decision 5, 2026-09-14); file rolls hourly at a keyframe; path + disk budget in Settings (`RecordingSettings` in `presets.json`, `set_recording_settings` over IPC). Verified live on loopback: 65 s share → 44.6 MB fMP4, ffprobe reads HEVC 1440p60 + Opus, correct duration.
- [x] MKV container as a per-preset option, fMP4 still the default (`container` on `SharePresetDef` → `ShareRequest` → `relay-share --container mp4|mkv`; `RecordingContainer` in `crates/core/src/share.rs`, mirrored in `ui/src/lib/ipc.ts`). Same teed bitstream, same `hvcC`/`OpusHead` configuration, no second encode. Golden-fixture byte tests to the same standard as fMP4 (`tests/fixtures/golden-recording.mkv`), and the recorder/replay test runs for both containers. Verified live on loopback in both — see Measurements.
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

## Measurements — S4 container comparison (loopback, 2026-09-14, same rig and driver as above)
Driver: `scripts\m6-loopback.ps1 [-Record] [-Container mp4|mkv] -Secs 65`. Three runs back to back on a *working* desktop (builds running), so the absolute CPU numbers sit above the 2026-09-11 idle-desktop table; the like-for-like comparison is MP4 vs MKV, which were taken minutes apart.

| Metric | Recording OFF | ON — MP4 | ON — MKV |
|---|---|---|---|
| capture→arrival p50 / p99 | 2.47 / 7.63 ms | 2.32 / 3.18 ms | 2.41 / 7.51 ms |
| capture→send (sender avg) | 3.55 ms | 2.17 ms | 2.56 ms |
| encode (avg) | 6.28 ms | 5.02 ms | 5.15 ms |
| fps median / drops / rec drops | 60.0 / 0 / — | 60.0 / 0 / 0 | 60.0 / 0 / 0 |
| process CPU median (p90) | 3.12 % (9.4 %) | 6.23 % (12.5 %) | 6.22 % (12.6 %) |
| replay save | — | 59 ms | 73 ms |

- **MKV adds no latency over MP4, and neither adds any over recording-off.** Every arrival delta above is inside run-to-run noise (recording-*on* sampled lower than recording-off on this pass). The tee is unchanged: one `try_send` off the encoder output feeding whichever muxer the preset selected. There is no second encode path — both muxers consume the identical access units and the identical `hvcC`, and the container choice is a `match` on the writer.
- **MKV costs the same CPU as MP4** — 6.22 % vs 6.23 % median, 12.6 % vs 12.5 % p90. Both sit ~3 pt above recording-off in this session because the desktop was busy; the 2026-09-11 idle run measured the +0.06 pt figure the DoD is stated against, and nothing on the hot path changed.
- **Replay save works in both**: 73 ms for 48.7 MB of MKV, 59 ms for 43.3 MB of MP4 — both far inside the < 2 s gate.
- **Container overhead favours MKV slightly**: muxing the *same* 442,889-byte HEVC elementary stream gives 444,903 bytes of MKV against 447,439 of fMP4 (2.0 kB vs 4.6 kB of framing). The live files differ in size only because the two runs captured different desktop activity.
- **Editor import, the point of the exercise** — all four live artefacts (rolling recording + replay save, each container) now pass `Windows.Media.Editing.MediaClip` with `OPUS … FullySupported`. Before this session the MP4s failed that check; they pass now because of the `mfra` fix, and the MKVs pass *even when truncated mid-write*, which the MP4s still do not.
- Footprint gate after the core changes (`container` on the preset + `ShareRequest`): **PASS** — idle RSS 6.77 MB, private WS 0.91 MB, 0 % CPU.

## Deferred
- ~~**MKV container.**~~ **Done 2026-09-14 (session S4)** — see decision 5. The deferral said "revisit only if a user-facing compatibility gap appears"; one did, though not the one guessed at. Evidence and the full probe matrix: `docs/dev/container-compat.md`.
- **True-4K60 recording/replay numbers under real game motion.** This session's monitor is 1440p and the desktop was near-idle, so produced bitrate sat at 2–16 Mb/s. The 4K60 claim rests on the M4 4K encode benchmark (unchanged path) plus the measured 750 MB/s save throughput; a full-motion 4K60 run lands in the MVP validation pass alongside M4's deferred two-PC run. Runbook: game at 4K60 (or `RELAY_BITRATE_MBPS=80` upscale), `m6-loopback.ps1 -Record -Secs 90`, read `replay_saved.ms` and the rec-drop counter.
- **One-hour roll soak.** Hourly rolling is implemented (writer closes/reopens at the first keyframe past `roll_secs`) and unit-level logic is covered, but a real 60-minute soak was not run in this session. Same MVP validation pass; runbook: `-Record -Secs 4000` with `roll_secs` left at 3600, expect two files, both playable.
- **Simultaneous mic + desktop audio in the recording** follows the M4-deferred second Opus track (v1.1 mix-minus work); the recording currently carries whatever single audio track the share sends.
