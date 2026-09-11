# Relay roadmap (agreed 2026-09-09)

Milestones are sized so one Claude Code session can complete a milestone or a
clearly bounded slice of one. Each milestone has its own plan file under
`docs/plans/` with a kickoff prompt; one chat session per milestone. This file
is the index, the ordering, and the decision log.

## Decisions (2026-09-09)
1. **Share first.** M4 starts right after M0. Display (M2) and audio DSP (M3) run in parallel after that; M1 (hardware probe) lands before M2 needs it.
2. **Latency and efficiency are the top priority for share.** Media Foundation is the first encoder path; if it cannot hold the <50 ms / ~0 CPU targets, direct NVENC follows immediately rather than later.
3. **Receiver is a mode of the same app**, not a separate binary.
4. **EV certificate procurement starts now**, in parallel with M0. APO (M3b) and the virtual mic (M5) are gated on it.

## Sequence
```
M0 foundation ──► M4 share MVP ──► M1 hardware ──► M2 display ──► M3b APO ──► M5 vdevices ──► M6 ──► M7
                                  └► M3 audio DSP (parallel with M1/M2)
EV cert (external) ─────────────────────────────────────────────► unblocks M3b, M5
```

Milestone numbers are stable identifiers; the sequence above is the order of work.

Status legend: `[ ]` not started · `[~]` in progress · `[x]` done

---

## M0 — Foundation hardening
Small items that unblock everything else. **Done 2026-09-09** (session log in `docs/plans/M0-foundation.md`; CI green on the first run; the autostart console flash is deferred to M7).

- [x] Initial commit; GitHub Actions on `windows-latest`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `pnpm build`.
- [x] Release-footprint gate: script that builds `relay-core` in release, runs it for 30 s idle, asserts RSS ≤ 10 MB and CPU ≈ 0. Runs in CI.
- [x] Single-instance guard for the core (named mutex) and a `--foreground/--tray`-less headless mode that is the default.
- [x] Log to `%LOCALAPPDATA%\Relay\logs\core.log` with size-based rotation; `--verbose` flag.
- [x] Opt-in autostart at login (Run key under HKCU only; removable from Settings and by the uninstaller).
- [x] IPC hardening: pipe DACL limited to the current user; reject messages > 1 MB.
- [x] Profile CRUD in the UI: New / Edit / Delete profile forms wired to `save_profile` / `delete_profile`. Today the UI is read-only.
- [x] Crash-restore test harness: apply a profile, kill the core with `taskkill /F`, restart, assert restore ran. Automated, uses the `Recorder` backends.

## M1 — Hardware library and probe
Profiles only match "Any" until the core knows what is plugged in.

- [ ] `relay-core::hardware`: real `HardwareProbe`. Default WASAPI render endpoint (IMMDeviceEnumerator) → stable `HeadsetId`; monitors via `QueryDisplayConfig` + EDID (manufacturer, product code, serial) → stable `MonitorId`.
- [ ] Hardware library file (`hardware.json`): headsets with measured curves, monitors with panel type and DDC/CI capability. Importer for the AutoEQ results format (oratory1990 / crinacle live there).
- [ ] Device-change events (WM_DEVICECHANGE / IMMNotificationClient) → re-select profile without a focus change.
- [ ] UI: "Add" flows on the Profiles screen, live "Plugged / Main / Second" pills, headset/monitor pickers on the Games screen.

## M2 — Display profiles (first real apply/restore)
- [ ] `relay-display::ddc`: DDC/CI over `dxva2` (`GetVCPFeatureAndVCPFeatureReply`, `SetVCPFeature`), capabilities string parsing, per-monitor handle from the monitor the game window is on.
- [ ] `relay-display::gpu`: NvAPI bindings for digital vibrance, hue, and per-display gamma/contrast/brightness; Windows `SetDeviceGammaRamp` as the vendor-neutral fallback for gamma, contrast and shadow lift. ADLX after NVIDIA works.
- [ ] `DisplayControl` adapter with true capture-before-apply; snapshot includes every VCP code touched.
- [ ] Multi-monitor: only the game's monitor changes; game moves monitors → restore old, apply new.
- [ ] Manual test log on the LG 27GP850 and LG C2 (VCP codes that actually work per model go in the hardware library).
- [ ] UI: Display section reads/writes the real profile; "Applied via" reflects which path was used.

## M3 — Audio DSP and detection (no signing needed)
**Done 2026-09-10** (branch `m3-audio-dsp`; plan + measurements in `docs/plans/M3-audio-dsp.md`). HRTF set: SADIE II D1 (KU100), Apache 2.0. Full chain benchmarks at 0.60 % of a core @48 k / 1.29 % @96 k (< 2 % budget); allocation-free `process` proven by a counting-allocator test; footprint gate stays green because the DSP lives in the on-demand `relay-preview` child, not the core. The listening session itself and a real-game exclusive spot check are deferred (plan's Deferred).
- [x] `relay-audio::dsp`: biquad cascade (peaking, low/high shelf, LP/HP), soft limiter with band-split, partitioned-convolution HRTF. Real-time safe: no allocation after `prepare()`. Golden-response unit tests.
- [x] `process()` bypass path is a plain copy (bit-exact test); benchmark at 48 kHz / 96 kHz stereo.
- [x] WASAPI-exclusive detection: `IAudioSessionManager2` enumeration + device-in-use probe; `AudioChainState::ExclusiveBypassed` set within 1 s and the Games › Audio banner says the game bypasses the APO. Verified live against this machine's endpoint.
- [x] Offline listening test: apply the profile to a WAV (or a synthesized demo clip) and play A/B from the UI so tuning works before the APO ships.

## M3b — Endpoint APO (needs EV cert)
- [ ] Start EV certificate + Hardware Dev Center attestation as soon as M0 lands. External dependency; track in this file.
- [ ] APO `cdylib` hosting `relay-audio::dsp`, parameters via shared memory, registered on one endpoint only.
- [ ] Install/uninstall that edits only that endpoint's FX property store and restores the exact prior chain (brief risk #2). Verified by a before/after registry diff test.
- [ ] `AudioControl` adapter; Settings opt-in card goes live.

## M4 — Share MVP (headline)
**Done 2026-09-10** (branch `m4-share`; plan + measurements in `docs/plans/M4-share.md`). Full capture→encode→transport→decode→present pipeline built and green on loopback: capture→present ~4–6 ms, zero AU loss; MF HEVC encode 4K60 p99 10.8 ms (« 20 ms gate, so direct NVENC not needed); core RSS unchanged while sharing. Decision 2026-09-10: dual-PC testing is not currently possible, so the pipeline logic is unit-tested thoroughly instead (101 workspace tests; see the plan's "Unit coverage") and **live two-PC integration testing moves to the future MVP validation pass** (runbook in the plan's Deferred).
- [x] `relay-capture::source`: Windows.Graphics.Capture (monitor, cursor toggle, border off), DXGI Desktop Duplication fallback. (Window capture: monitor only for now.)
- [x] `relay-capture::encode`: Media Foundation HEVC hardware encoder (NVENC/QSV/AMF via vendor MFTs), low-latency CBR, B-frames off, keyframe-on-request; no software path (hardware-only enum bound to the capture adapter). Decision gate passed — MF holds the budget.
- [x] Audio: WASAPI loopback + process loopback (game-only) + mic, Opus 48 kHz stereo 10 ms.
- [x] `relay-capture::transport`: webrtc-rs, mDNS discovery, six-digit pairing (HMAC over SDP → DTLS fingerprint pin), DTLS-SRTP, host candidates only. LAN only.
- [x] Receiver mode in the same app: "Receive" screen; native D3D11 swapchain window, DXVA HEVC decode, WASAPI playback (virtual camera is M5).
- [x] Instrument strip fed by real `ShareStats` events over IPC (bitrate, latency, drops, encoder load, audio level).
- [x] Share engine runs as a child process of the core, spawned per share and fully torn down after; core RSS unchanged.
- [x] Wi-Fi detection → wired / 6 GHz recommendation; AIMD bitrate step-down on receiver-reported loss.
- [x] Hotkeys Ctrl+Alt+S (toggle share) / Ctrl+Alt+P (preview) wired in the core.

## M5 — Virtual devices on the receiver
- [ ] Virtual camera via the Windows 11 frame-server API (`MFCreateVirtualCamera` + a registered media source). OBS VirtualCam detection as fallback.
- [ ] Virtual mic: signed audio-class driver (second signing dependency). Interim: detect VB-Cable and route to it.
- [ ] First-run consent screen: two opt-ins, "what we install / how to remove", with a dry-run listing.

## M6 — Recording, replay buffer, presets
- [ ] Local high-bitrate MP4/MKV recording alongside the share, replay buffer with hotkey save.
- [ ] Share presets Game / DAW / Desktop map to encoder settings, audio sources, cursor.
- [ ] Multi-source switching (display, window, region) without restarting the share.

## M7 — Installer and uninstaller
- [ ] NSIS per-user install of core + UI; autostart opt-in; Settings "Uninstall" button.
- [ ] Uninstaller restores the APO chain, removes drivers and the Run key, offers to delete data. Tested from a clean VM snapshot.

## v1.1 backlog
- Relay Send VST3 plugin over shared memory (DAW audio under ASIO).
- Call-audio return route and mix-minus.
- Stream Deck plugin; NDI-compatible output.
- AI tuning loop with user-supplied API key (headset curve + measured game bands → profile → A/B tests).

---

## Plan files
| Milestone | Plan | Session status |
|---|---|---|
| M0 Foundation hardening | `docs/plans/M0-foundation.md` | done 2026-09-09, CI green |
| M4 Share MVP | `docs/plans/M4-share.md` | done 2026-09-10; pipeline complete, measured on loopback, logic unit-tested; live two-PC run → MVP validation pass |
| M1 Hardware library & probe | `docs/plans/M1-hardware.md` | not started |
| M2 Display profiles | `docs/plans/M2-display.md` | not started |
| M3 Audio DSP & detection | `docs/plans/M3-audio-dsp.md` | done 2026-09-10; listening session + real-game exclusive check deferred to MVP validation |
| M3b Endpoint APO | `docs/plans/M3b-apo.md` | blocked: EV cert |
| M5 Virtual devices | `docs/plans/M5-vdevices.md` | blocked: EV cert (mic) |
| M6 Recording & presets | `docs/plans/M6-recording-presets.md` | not started |
| M7 Installer | `docs/plans/M7-installer.md` | not started |

Update the status column when a session starts or finishes a milestone.
