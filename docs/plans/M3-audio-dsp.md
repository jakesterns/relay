# M3 — Audio DSP and detection

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M3-audio-dsp.md. Work on branch `m3-audio-dsp`. The DSP must be real-time safe: no allocation after prepare(), bypass is a plain copy. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Goal
The signal-processing core that the APO (M3b) will host, testable and usable
today via an offline A/B listening test, plus detection of games that bypass
the APO with WASAPI-exclusive streams.

## Depends on
M0. Uses headset curves from M1 when available; not required.

## Definition of Ready
- [x] M0 complete (done 2026-09-09).
- [x] A default HRTF impulse-response set chosen with a licence that allows bundling (candidates: SADIE II, MIT KEMAR, HUTUBS); decision recorded here: **SADIE II, subject D1 (Neumann KU100), Apache 2.0 — confirmed by Jake 2026-09-10.** Attribution ships in `crates/audio/assets/hrtf/LICENSE`.
- [x] A game known to use WASAPI exclusive mode identified for the detection test: **our own test helper** (`relay-audio` opens an `AUDCLNT_SHAREMODE_EXCLUSIVE` stream in-process for the automated test) — confirmed by Jake 2026-09-10. A real game spot-check moves to the MVP validation pass.

## Checklist
### DSP (`relay-audio::dsp`, `#![forbid(unsafe_code)]` where possible)
- [ ] Biquad: peaking, low shelf, high shelf, low/high pass; RBJ cookbook coefficients; f64 state, f32 I/O; cascade of up to 16 bands; per-band bypass.
- [ ] Soft limiter with band split (< `below_hz` limited, rest passed) for the "explosion tamer"; look-ahead ≤ 1 ms; release curve tests.
- [ ] Partitioned-convolution HRTF: uniform partitions, FFT via `realfft`/`rustfft`, stereo → binaural with a bundled default IR set ("Relay Arena"); latency = one partition.
- [ ] `Chain { prepare(rate, max_block), process(in, out) }`: zero allocation in `process` (proved with a counting allocator test), bypass = `copy_from_slice`.
- [ ] Golden tests: white-noise response of each stage vs analytic magnitude within 0.1 dB; end-to-end chain equals sum of stages.
- [ ] Benchmark at 48 k and 96 k, 256-sample blocks; target < 2 % of one core for the full chain.

### Detection
- [ ] `relay-audio::sessions`: enumerate render sessions (`IAudioSessionManager2`) and detect exclusive-mode streams on the target endpoint; map session PID → foreground game.
- [ ] Core: `AudioChainState::ExclusiveBypassed` set when the active game's session is exclusive; UI banner "This game opens the headset exclusively; Relay's EQ is bypassed" with the game's own audio setting to change if known.

### Offline listening test
- [ ] `relay-audio::offline`: render a WAV through the chain; core exposes `RenderPreview { profile, wav }`; UI "A/B" buttons play original vs processed via the webview `<audio>` element.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- Allocation-free `process` proven by test.
- Golden tests pass; benchmark within budget.
- Exclusive-mode game is detected and reported in the UI within 1 s of launch.

## Out of scope
The APO itself, installer, signing (M3b).

## Deferred
_(none yet)_
