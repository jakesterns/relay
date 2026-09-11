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
**Done 2026-09-10** (branch `m1-hardware`; plan + live measurements in `docs/plans/M1-hardware.md`). Re-selection on a default-endpoint switch measured live at 26–41 ms (« 1 s gate), no focus change; footprint gate still green (idle RSS 4.4 MB after a post-probe working-set trim). Second-monitor and physical-unplug live passes deferred with runbooks (one monitor attached this session).

- [x] `relay-core::hardware`: real `HardwareProbe`. Endpoint keys prefer the device container GUID (port-stable for serialised USB gear), library headsets are user-named objects bound to endpoint keys; monitors get EDID-derived `mon:<PNP><product>:<serial>` ids (pure function of the panel, fixture-tested) with `HMONITOR` mapping + DDC/CI VCP lists for M2.
- [x] Hardware library file (`hardware.json`): headsets with measured curves, monitors with panel type and DDC/CI capability. AutoEQ results importer (local file/paste only; real oratory1990 fixture checked in).
- [x] Device-change events (IMMNotificationClient + hidden-window WM_DISPLAYCHANGE/WM_DEVICECHANGE) → re-select without a focus change.
- [x] UI: "Add" flows on the Profiles screen, live "Plugged / Main / Second" pills, headset/monitor pickers on the profile form and Games screen.

## M2 — Display profiles (first real apply/restore)
**Code complete 2026-09-10/11** (branch `m2-display`, worktree `stream-share-m2`; plan in `docs/plans/M2-display.md`). All logic unit-tested (two-monitor fake covers "second monitor untouched", unplug-mid-apply, stale-handle restore); footprint gate green at 5.95 MB / 0 %. **Live pass pending:** the physical monitor was powered off the whole session, so the real-hardware runbook (live read → real crash-restore test → alt-tab timing → lock/unlock) is queued in the plan's Deferred and gates closing this milestone.
- [x] `relay-display::ddc`: DDC/CI over `dxva2` with retries + per-model write delays; capability parsing from M1; per-monitor handle from the probe's `HMONITOR`.
- [x] `relay-display`: NvAPI vibrance + hue (dynamic `nvapi64.dll`, raw levels snapshotted); `SetDeviceGammaRamp` per monitor DC for gamma/contrast/shadow lift as the vendor-neutral path (raw original ramp preserved). ADLX after NVIDIA is live-verified.
- [x] `DisplayControl` adapter (`display_backend.rs`) with true capture-before-apply; snapshot carries every VCP code touched, the raw ramp and raw NvAPI state, keyed by stable monitor id for crash/reboot restore.
- [x] Multi-monitor: only the game's monitor changes; game moves monitors → restore old, apply new (focus hmonitor + move hook + tick safety net).
- [ ] Manual test log on the LG ULTRAGEAR+ and LG C2 (VCP codes that actually work per model go in the hardware library) — needs the monitor awake / second panel; runbooks in the plan.
- [x] UI: Display section reads/writes the real profile; "Applied via" reflects NvAPI / gamma ramp / DDC-CI actually used; unsupported controls disabled.

## M3 — Audio DSP and detection (no signing needed)
- [ ] `relay-audio::dsp`: biquad cascade (peaking, low/high shelf), soft limiter with band-split, partitioned-convolution HRTF. Real-time safe: no allocation after `prepare()`. Golden-response unit tests.
- [ ] `process()` bypass path is a plain copy; benchmark at 48 kHz / 96 kHz stereo.
- [ ] WASAPI-exclusive detection: `IAudioSessionManager2` enumeration; flag the session and surface "this game bypasses the APO" in the UI (`AudioChainState::ExclusiveBypassed` already exists).
- [ ] Offline listening test: apply the profile to a WAV and play A/B from the UI so tuning works before the APO ships.

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
| M1 Hardware library & probe | `docs/plans/M1-hardware.md` | done 2026-09-10; re-selection measured at 26–41 ms live; 2nd-monitor + physical-unplug passes deferred (runbooks in plan) |
| M2 Display profiles | `docs/plans/M2-display.md` | code + tests done 2026-09-11; live hardware pass pending (monitor was off) — runbook in plan |
| M3 Audio DSP & detection | `docs/plans/M3-audio-dsp.md` | not started |
| M3b Endpoint APO | `docs/plans/M3b-apo.md` | blocked: EV cert |
| M5 Virtual devices | `docs/plans/M5-vdevices.md` | blocked: EV cert (mic) |
| M6 Recording & presets | `docs/plans/M6-recording-presets.md` | not started |
| M7 Installer | `docs/plans/M7-installer.md` | not started |

Update the status column when a session starts or finishes a milestone.
