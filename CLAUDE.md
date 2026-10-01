# Relay — project brief for Claude Code

Working name: **Relay**. Single Windows desktop app that (1) shares one PC's screen + audio to another PC for Discord/Zoom/Meet as a software capture-card replacement, and (2) applies per-game audio EQ / spatial and display color profiles, keyed to the user's headset and monitor. One installer, one uninstaller, one UI.

## Non-negotiables
- **Never trip anti-cheat.** OS/hardware-layer only: DXGI Desktop Duplication / Windows.Graphics.Capture for video, WASAPI process loopback + endpoint APO for audio, NvAPI/ADLX + DDC/CI for color. No DLL injection, no game hooks, no memory reads, no kernel driver except the signed audio-class virtual device.
- **Never touch global config.** No default-device changes, no global EQ, no edits to other apps. Profiles apply only while the target app has focus and restore on blur, exit, crash, or reboot. Original state is written to disk before any change.
- **Zero network config for the user.** WebRTC (ICE/STUN, mDNS discovery, DTLS-SRTP). LAN-first; pairing by code. Relay makes exactly two *other* outbound requests: (1) the user-initiated AutoEQ curve fetch in `hardware/catalog.rs` (https, one host, cached, skippable by pasting a curve); (2) the update check in `update.rs` (https, GitHub only: the Releases API for jakesterns/relay, at most daily, never during a share or game profile, user-controllable — "Check for updates automatically" switch, default on; downloads the installer only when the user chooses Install now or turned on auto-install, default off; SHA-256 + signature verified). Same WinHTTP path, no extra HTTP/TLS crate.
- **Low footprint.** Always-on core ≤ ~10 MB RAM, ~0% idle CPU. Audio engine, capture/encode, and UI load only on demand and tear down fully. Hardware encode only (NVENC/QSV/AMF). Target 4K60 HEVC at 40–80 Mb/s on LAN, <50 ms latency.
- **Two explicit opt-ins** at first run: the endpoint APO and the virtual camera/mic. Clear "what we install / how to remove" screen.

## Architecture
- `core/` Rust service: focus watcher, profile apply/restore, hotkeys, state backup. Tiny, always resident.
- `audio/` Rust DSP + signed APO DLL: biquad EQ cascade, partitioned-convolution HRTF, soft limiter. Bypass = pass-through, zero allocations. Match endpoint rate; never resample.
- `capture/` DXGI/WGC capture → HW encoder → webrtc-rs. Spun up per share only.
- `vdevice/` virtual camera (prefer Win11 MediaFrameSource; fallback OBS VirtualCam detection) + signed virtual mic/audio driver for the receiver side.
- `display/` NvAPI / ADLX color (vibrance, gamma, contrast, hue, LUT) + DDC/CI monitor controls (brightness, contrast, black equalizer, etc.).
- `daw-plugin/` "Relay Send" VST3 (later AU/CLAP) that ships DAW master/bus audio to the app over shared memory. This is how DAW audio is captured under ASIO exclusive mode.
- `ui/` Tauri. Separate process; closing the window costs nothing.
- `ai/` optional: user-supplied API key. Takes headset measured curve + measured footstep/explosion bands from live game audio + user goal → starting profile → short A/B listening/screenshot tests → refine. Not on the hot path.

## Data model
- **Hardware library**: headsets/IEMs (measured curves from AutoEQ / oratory1990 / crinacle), monitors (panel type, DDC/CI capability), audio interfaces.
- **Profile** = game × headset × monitor → {EQ bands, HRTF, limiter, GPU color, monitor settings, share preset}. Multiple rows per game allowed; auto-select by currently connected hardware.
- **Share presets**: Game / DAW / Desktop (encoder + audio sources + cursor).

## Feature scope
v1: share (LAN, 4K60, virtual cam/mic on receiver), presets, instrument strip (bitrate/latency/drops/load/audio), local high-bitrate recording + replay buffer, multi-source switching, per-game audio EQ + HRTF, per-game GPU/monitor color, profiles + hardware library, first-run consent.
v1.1: Relay Send DAW plugin, call-audio return route, mix-minus, Stream Deck, NDI-compatible output, AI tuning loop.
Out of scope: WAN sharing, Twitch streaming, overlays, in-game shaders (ReShade-style).

## UI direction
Dark, restrained, hardware-inspired. Warm black `#0E0D0C`, surfaces `#151312`/`#1C1917`, ivory text `#ECE6DC`, one champagne accent `#C9A96A`. Geist for UI, Geist Mono for readouts (tabular figures). Hairline dividers, slow damped motion, no glow/gradient/gamer palette. Signature element: the instrument strip. Mocks in `mocks/` (open the HTML files; PNGs are 2× renders). Every screen carries a plain-English "nothing on your PC was changed" line.

## Key risks to solve early
1. Signed APO + audio-class driver (EV cert, Hardware Dev Center attestation).
2. Rock-solid uninstall that restores the endpoint APO chain.
3. WASAPI-exclusive games bypass the APO — detect and tell the user.
4. Restore-on-crash for display settings; multi-monitor handling.
5. Wi-Fi at 4K60 is unreliable — surface a wired/6E recommendation, degrade gracefully.

## Roadmap and milestone sessions
`docs/ROADMAP.md` is the agreed order and decision log. Each milestone has a plan file in `docs/plans/` with a kickoff prompt; one chat session per milestone. Agreed order: M0 → M4 share → M1 hardware → M2 display (with M3 audio DSP in parallel) → M3b APO → M5 → M6 → M7. Latency and efficiency come first for share.

**`main` is the trunk** (2026-09-14: M0–M7 all merged into it). Cut feature branches from `main`, never from the old `m*` milestone branches, which are kept only as history. **`docs/plans/SESSIONS.md` is the work queue** — every remaining task as a session with its branch, worktree, DoR, DoD and kickoff prompt. Parallel sessions each get a git worktree — `docs/dev/parallel-sessions.md` has the layout, the merge-back steps, and the one sharp edge: the post-commit installer runs in every tree and installs over the same `%LOCALAPPDATA%\Relay`, so the installed app follows whichever tree committed last (`RELAY_NO_INSTALL=1` opts a tree out).

## Repo layout (scaffolded 2026-09-09)
```
Cargo.toml            workspace (resolver 2, size-optimised release profile)
crates/core/          relay-core  — lib + three binaries: `relay-core` (the always-on
                      service and the CLI), `relay-svc` (GUI-subsystem launcher, no console
                      flash), `relay-elevate` (the elevated install helper — the ONLY Relay
                      binary that writes HKLM; see src/elevate.rs and docs/dev/elevation-live.md).
crates/audio/         relay-audio — DSP (biquad EQ, band-split limiter, partitioned-conv HRTF),
                      WASAPI session/exclusive probing, offline A/B render, `relay-preview` bin.
                      `fit.rs` turns a measured AutoEQ curve into a shelf/peaking cascade within
                      MAX_BANDS (shelves at fixed corners, then greedy peaking placement against
                      the actual cascade response) — this is the headset-correction path.
                      `dsp` feature (default on) holds the FFT; the core links default-features=false
                      (params + sessions only) and spawns `relay-preview` on demand — keep it that way
                      or the footprint gate fails. Bundled HRIRs: SADIE II D1 (assets/hrtf, Apache 2.0).
crates/audio/apo/     relay-apo — the endpoint APO cdylib (COM, feature "com" pulls the DSP) plus the
                      FX property-store install/uninstall engine (regfile/fxstore/livereg — pure model,
                      fixture-tested; live writes double-gated on RELAY_APO_ALLOW_LIVE_WRITE + elevation,
                      VM only). The core links it default-features=false (no FFT). Test-sign runbook:
                      docs/dev/apo-testsign.md.
crates/capture/       relay-capture — the share engine (`relay-share` child process, spawned
                      per share, torn down after). WGC + DXGI Desktop Duplication capture,
                      Media Foundation HEVC or H.264 hardware MFT, negotiated per share in
                      `codec` (HEVC when both ends decode it; no CPU path), `resample` (any endpoint
                      rate/channels → Opus 48 kHz stereo, one track for desktop and one for mic),
                      webrtc-rs transport (mDNS, six-digit pairing, DTLS-SRTP, LAN only),
                      DXVA decode + D3D11 present on the receiver, fMP4/MKV recording + replay
                      buffer, 480×270 JPEG preview tap, instrument-strip stats.
crates/vdevice/       relay-vdevice — virtual camera: NV12 frame ring (seqlock slots; receiver
                      writes, camera reads), frame-server media source cdylib (feature "com" —
                      core/capture link default-features=false), MFCreateVirtualCamera control,
                      HKLM registration planner + gated livereg (RELAY_VDEVICE_ALLOW_LIVE_WRITE
                      + elevation), installed.json model (consent + registered components),
                      OBS VirtualCam / VB-Cable detection. Live runbook: docs/dev/vcam-live.md.
crates/display/       relay-display — primitive display controls, no relay-core dependency:
                      `vcp` (opcodes + evidence-typed per-model quirks; a `VerifiedCode` is the
                      only thing `SetVCPFeature` accepts, so a guess cannot reach a monitor),
                      `gamma` (SetDeviceGammaRamp, vendor-neutral), `ddc` (dxva2 with retries),
                      `nvapi` and `amd` (vibrance + hue, both dynamically loaded). Snapshots are
                      vendor-tagged so a restore goes back through the API that captured it.
ui/                   Vite + React + TypeScript frontend (ported from mocks/)
ui/src-tauri/         relay-ui — Tauri 2 shell, workspace member; talks to core over IPC
mocks/                design mocks (HTML + 2× PNG) and Geist fonts
relay-handoff/        original handoff bundle; do not edit
```

### Core crate map (`crates/core/src`)
- `types.rs` — Profile, EQ/HRTF/limiter, GPU + monitor settings, share preset. Serialised as JSON.
- `profiles.rs` — on-disk store + `select()` (game exe × connected headset × monitor scoring).
- `backup.rs` — original-state snapshot written atomically to disk *before* any apply; restored on start if left pending (crash/reboot path).
- `apply.rs` — `AudioControl` / `DisplayControl` traits and the `Applier` that enforces backup-then-apply and restore-on-blur. Real backends live in `audio/` and `display/` later; `Noop` today.
- `winloop.rs` — one Win32 message-loop thread: `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` focus watcher + `RegisterHotKey`. Emits `CoreEvent`.
- `ipc.rs` — newline-delimited JSON over named pipe `\\.\pipe\relay-core`. Server in core, client used by the Tauri shell. Pipe DACL = current user only, remote clients rejected, lines capped at 1 MB, idle and request timeouts.
- `config.rs` — data root (`%LOCALAPPDATA%\Relay`; `--data-dir` overrides), pipe and mutex names. `RELAY_INSTANCE=<suffix>` namespaces both so tests and the footprint gate can run beside a live core.
- `instance.rs` — single-instance guard (named mutex `Local\RelayCore`).
- `logging.rs` — `logs/core.log`, 1 MB × 3 rotation, plus stderr when attached. `--verbose` or `RELAY_LOG=` sets the level (no `EnvFilter`: its regex engine is too big for the budget).
- `autostart.rs` — the one Run-key value (`HKCU\...\Run\Relay`); `relay-core autostart on|off`.
- `processes.rs` — windowed processes for the exe picker (`Method::ListProcesses`).
- `status.rs` — human summary for `relay-core status` (`--json` for the raw state).
- `audio_bridge.rs` — `AudioSettings` → `relay_audio::ChainParams`; `chain_params_with` prepends the fitted headset-correction bands (budgeted by `CORRECTION_BUDGET`) ahead of the profile's own, so profile EQ is taste on top of a corrected headset. Spawns `relay-preview` for the A/B render.
- `hardware/` — the probe and library: endpoint/monitor ids (`mod.rs`, `edid.rs`), `probe_win.rs`, device-change watcher (`watch_win.rs`), DDC/CI capability parse (`ddc.rs`), AutoEQ file/paste importer (`autoeq.rs`), and `catalog.rs` — the bundled 8,849-model index (`autoeq-index.tsv`, read and dropped, not `include_str!`) plus the on-demand WinHTTP curve fetch. The index ships; the CC BY-NC-SA measurements never do.
- `vdevice.rs` — virtual-device glue: status probe, consent record, camera install/uninstall (record-then-apply into `installed.json`), receive routing (service decides `--vcam`/`--mic-route` from consent + registration, never the client); `relay-core vdevice` CLI.
- `elevate.rs` — the elevated install helper's protocol and rules: a six-variant op allow-list, requests that carry no registry path or DLL path (the helper re-derives them), CLSID-scope and GUID vetting, versioned/expiring/location-checked requests, and the gate armed around one vetted call. `relay-core elevate plan|run <op>`; `Method::ElevationPlan` / `RunElevated`.
- `audio_apo.rs` — the production `AudioControl`: params to the APO over `relay_audio::shm` (apply = write + un-bypass, restore = bypass); read-only `apo_status()` probe; gated `install_live`/`uninstall_live` (backup-then-apply to `apo-backup\<endpoint>.json`); `relay-core apo` CLI.
- `service.rs` — wires the above; single-threaded tokio runtime. 1 s tick runs the WASAPI-exclusive watcher (only while a profile with audio processing is active) → `AudioChainState::ExclusiveBypassed`.
- `firewall.rs` — the inbound Windows Firewall rule for `relay-share.exe`, the only Relay binary that listens (`relay-core` is named-pipe only, so it gets no rule). Reads are a pure parse of the firewall policy store's rule strings (no COM, no elevation, fixture-tested); writes go through `INetFwPolicy2` in the elevated helper only, gated on `RELAY_FIREWALL_ALLOW_LIVE_WRITE`, and only ever add one rule or remove rules by name. Scope is private + domain, never public. `status()` classifies the live state — `Blocked` is the one that otherwise looks like a dead network — and the Share/Receive `FirewallBanner` renders it. Recorded in `firewall.json`; removed by `uninstall::StepKind::RemoveFirewallRule`. `relay-core firewall` CLI.
- `footprint.rs` — RSS + CPU self-measurement for the "9 MB / 0.0 %" readouts.

### Build & run
```
cargo build                         # whole workspace
cargo test -p relay-core
cargo run -p relay-core -- run      # start the service (foreground, logs to stderr)
cargo run -p relay-core -- status   # human summary; --json for raw state
cargo run -p relay-core -- autostart on|off
cargo test -p relay-core --test crash_restore   # spawns a real core, taskkill /F, checks restore
pwsh scripts/footprint.ps1          # release footprint gate (<=10 MB WS, <=0.5 % CPU); also in CI
cd ui && pnpm install && pnpm tauri dev   # UI (needs the service running for live data; falls back to mock data otherwise)
cd ui && pnpm test                  # UI component tests (Vitest + jsdom); also in CI
```
UI tests never drive a real window — component tests in jsdom only, so nothing
can move the user's cursor. `ui/src/test/README.md` has the harness notes.
IPC contract lives in `crates/core/src/ipc.rs`; the TypeScript mirror is `ui/src/lib/ipc.ts`. Keep them in sync.
CI (`.github/workflows/ci.yml`, windows-latest) builds the UI first because `tauri::generate_context!` embeds `ui/dist` at compile time. Test backend: `RELAY_RECORDING_BACKEND=<file>` swaps in `apply::FileRecorder`.
