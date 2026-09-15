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
- [x] Biquad: peaking, low shelf, high shelf, low/high pass; RBJ cookbook coefficients; f64 state, f32 I/O; cascade of up to 16 bands; per-band bypass. (Unsafe is `deny`ed crate-wide; only `sessions` opts back in for COM.)
- [x] Soft limiter with band split (< `below_hz` limited, rest passed) for the "explosion tamer"; look-ahead ≤ 1 ms; release curve tests. LR4 crossover (magnitude-flat recombination test), running-min wedge + soft knee, hard ceiling via per-sample clamp, exponential release verified at t63 ≈ configured release.
- [x] Partitioned-convolution HRTF: uniform partitions, FFT via `realfft`/`rustfft`, stereo → binaural with a bundled default IR set ("Relay Arena" = SADIE II D1/KU100 ±30°, 44.1/48/96 k, Apache 2.0); latency = one 128-frame partition (verified). Engine matches a naive time-domain reference on random IRs and awkward block sizes.
- [x] `Chain { prepare(rate, max_block), process(in, out) }`: zero allocation in `process` (proved with a counting allocator test over 2000 blocks of the heaviest chain), bypass = `copy_from_slice` (bit-exactness test). Denormals flushed from all recursive state each block (10 s silence test).
- [x] Golden tests: response of each stage vs analytic magnitude within 0.1 dB; end-to-end chain equals sum of stages. (Measured with stepped sine probes rather than white noise — same assertion, far lower estimator variance; landmarks like peaking = gain_db at f0 and −3.01 dB at Butterworth corner also checked in closed form.)
- [x] Benchmark at 48 k and 96 k, 256-sample blocks; target < 2 % of one core for the full chain. See Measurements.

### Detection
- [x] `relay-audio::sessions`: enumerate render sessions (`IAudioSessionManager2` → active PIDs) and detect exclusive-mode streams on the target endpoint (probe: shared-mode `IAudioClient::Initialize` → `AUDCLNT_E_DEVICE_IN_USE`; exclusive streams don't appear as enumerable sessions). Attribution to the foreground game happens in the core, which only watches while a game profile with audio processing is active.
- [x] Core: `AudioChainState::ExclusiveBypassed` set when the active game's session is exclusive (1 s watcher + immediate probe on focus change; restores the applier's state when exclusivity clears); UI banner in Games › Audio: "This game opens the headset exclusively, so Relay's EQ is bypassed" with the advice to switch the game's exclusive/WASAPI output option to shared. End-to-end integration test (`crates/core/tests/exclusive_watch.rs`) runs a real core against a real endpoint.

### Offline listening test
- [x] `relay-audio::offline`: render a WAV through the chain; core exposes `RenderPreview { profile, wav }` (optional wav; a synthesized demo clip — footsteps, explosion, reference beep — is used without one); UI "A/B" buttons play original vs processed via the webview `<audio>` element (Tauri asset protocol scoped to `Relay/previews` only). Rendering runs in the on-demand `relay-preview` child so the FFT never enters the always-on core (footprint gate stayed green).

## Definition of Done
- [x] Every checklist item checked or moved to Deferred with a reason.
- [x] Allocation-free `process` proven by test (`crates/audio/tests/alloc_free.rs`).
- [x] Golden tests pass; benchmark within budget.
- [x] Exclusive-mode "game" (the in-process exclusive stream from the DoR) is detected and reported in the UI state within 1 s of launch — integration test observes the flip well inside the bound; the probe on focus change makes the common path immediate.

## Measurements (2026-09-10, this dev PC, release build)
`cargo run -p relay-audio --release --example dsp_bench` — 256-frame stereo blocks:

| configuration | µs/block | % of one core |
|---|---|---|
| 48 kHz · full chain (16-band EQ + limiter + HRTF) | 32.1 | **0.60 %** |
| 48 kHz · EQ + limiter only | 27.3 | 0.51 % |
| 48 kHz · bypass (plain copy) | 0.02 | 0.000 % |
| 96 kHz · full chain | 34.4 | **1.29 %** |
| 96 kHz · EQ + limiter only | 27.4 | 1.03 % |
| 96 kHz · bypass | 0.02 | 0.001 % |

Budget < 2 % ✓. Chain latency: 1 ms limiter look-ahead + 128-frame HRTF partition (2.7 ms @48 k).

Footprint gate after integration: relay-core 0.86 MB exe, idle RSS 9.57 MB, 0 % CPU — **PASS** (first attempt linked the DSP into the core and failed at 11.71 MB; fixed by the params/sessions feature split + `relay-preview` child process).

Live detection on this machine: `exclusive_detect` and `exclusive_watch` tests ran against the real default endpoint (not skipped) — exclusive stream opened, detected, and cleared.

## Out of scope
The APO itself, installer, signing (M3b).

## Deferred
- **The listening session itself** — needs Jake's ears. Material is staged: `%LOCALAPPDATA%\Relay\previews\original.wav` / `processed.wav` (demo clip through a 2-band EQ + explosion tamer + HRTF), and the Games › Audio → "A/B listening test" card renders/plays pairs on demand. No code work left.
- **Real-game exclusive-mode spot check** — the DoR decision replaced it with the in-process exclusive stream for automation; verifying against a shipping title (e.g. a player with WASAPI-exclusive output) moves to the MVP validation pass alongside the two-PC share run.
- ~~**Headset-correction curves in the chain**~~ — **done 2026-09-13** (`Headset correction curves now reach the audio chain`), extended 2026-09-14 by the catalogue fetch. A measured curve is fitted to a shelf/peaking cascade by `crates/audio/src/fit.rs` (two fixed-corner shelves plus greedy peaking placement, half-octave minimum spacing, evaluated against the actual cascade response), and `relay_core::audio_bridge::chain_params_with` prepends that fit to the profile's own bands — correction first, taste on top — under a `CORRECTION_BUDGET`. The Games screen carries a "Headset correction" card bound to `audio.headset_correction`. Coverage: `crates/core/tests/headset_correction.rs` plus the unit tests in `audio_bridge.rs`; the real oratory1990 HD 560S curve fits to 2.2 dB worst case / 0.8 dB RMS in eight bands.
