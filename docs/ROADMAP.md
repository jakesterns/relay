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
Small items that unblock everything else. **Done 2026-09-09** (session log in `docs/plans/M0-foundation.md`; CI green on the first run). The autostart console flash deferred from here was closed in M7 by `relay-svc.exe`, a GUI-subsystem launcher that starts the core with `CREATE_NO_WINDOW` and exits.

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
**Done 2026-09-13** (branch `m2-display`, merged into `m7-installer`; plan + live pass log in `docs/plans/M2-display.md`). All logic unit-tested (two-monitor fake covers "second monitor untouched", unplug-mid-apply, stale-handle restore); footprint gate green at 5.95 MB / 0 %. **Live pass done** on the LG ULTRAGEAR+ once the monitor was awake: DDC/CI reads on 0x10/0x12/0x87, NvAPI ids correct on this driver, crash-restore returns the exact original values after `taskkill /F`, and apply-on-focus / restore-on-focus-loss measured at 137–148 ms (mean 144, budget 200) over six runs. Still deferred: the second physical monitor, the LG C2, vendor OSD opcodes, ADLX, and the Win+L lock/unlock check.
- [x] `relay-display::ddc`: DDC/CI over `dxva2` with retries + per-model write delays; capability parsing from M1; per-monitor handle from the probe's `HMONITOR`.
- [x] `relay-display`: NvAPI vibrance + hue (dynamic `nvapi64.dll`, raw levels snapshotted); `SetDeviceGammaRamp` per monitor DC for gamma/contrast/shadow lift as the vendor-neutral path (raw original ramp preserved). ADLX after NVIDIA is live-verified.
- [x] `DisplayControl` adapter (`display_backend.rs`) with true capture-before-apply; snapshot carries every VCP code touched, the raw ramp and raw NvAPI state, keyed by stable monitor id for crash/reboot restore.
- [x] Multi-monitor: only the game's monitor changes; game moves monitors → restore old, apply new (focus hmonitor + move hook + tick safety net).
- [x] Manual test log on the LG ULTRAGEAR+ (brightness 0x10, contrast 0x12, sharpness 0x87 all answer; NvAPI vibrance 0–63) — logged in the plan's Live pass section. The LG C2 and the second-panel checks still need the second monitor.
- [x] Live apply/restore proof: crash-restore and focus-change restore both verified against real hardware, with an automated test for each (`crash_restore_display`, `focus_apply_restore_live`; both `--ignored`, run by hand with the monitor awake).
- [x] UI: Display section reads/writes the real profile; "Applied via" reflects NvAPI / gamma ramp / DDC-CI actually used; unsupported controls disabled.

## M3 — Audio DSP and detection (no signing needed)
**Done 2026-09-10** (branch `m3-audio-dsp`; plan + measurements in `docs/plans/M3-audio-dsp.md`). HRTF set: SADIE II D1 (KU100), Apache 2.0. Full chain benchmarks at 0.60 % of a core @48 k / 1.29 % @96 k (< 2 % budget); allocation-free `process` proven by a counting-allocator test; footprint gate stays green because the DSP lives in the on-demand `relay-preview` child, not the core. The listening session itself and a real-game exclusive spot check are deferred (plan's Deferred).
- [x] `relay-audio::dsp`: biquad cascade (peaking, low/high shelf, LP/HP), soft limiter with band-split, partitioned-convolution HRTF. Real-time safe: no allocation after `prepare()`. Golden-response unit tests.
- [x] `process()` bypass path is a plain copy (bit-exact test); benchmark at 48 kHz / 96 kHz stereo.
- [x] WASAPI-exclusive detection: `IAudioSessionManager2` enumeration + device-in-use probe; `AudioChainState::ExclusiveBypassed` set within 1 s and the Games › Audio banner says the game bypasses the APO. Verified live against this machine's endpoint.
- [x] Offline listening test: apply the profile to a WAV (or a synthesized demo clip) and play A/B from the UI so tuning works before the APO ships.

## M3b — Endpoint APO (needs EV cert)
**Code-complete 2026-09-11** (branch `m3b-apo`; plan + session log in `docs/plans/M3b-apo.md`). All development done unsigned and fixture-driven: this machine's registry was never modified (read-only baseline exports only; live writes double-gated). Deferred, blocked on the EV cert and on having a hypervisor: production signing, and the live audiodg-hosted VM pass (runbook: `docs/dev/apo-testsign.md`). 176 workspace tests green; footprint gate 5.3 MB / 0 % with the registration engine in the core.
- [ ] Start EV certificate + Hardware Dev Center attestation as soon as M0 lands. External dependency; track in the M3b plan (**still not ordered as of 2026-09-11 — this now blocks M3b's VM pass and M5's virtual mic**).
- [x] APO `cdylib` hosting `relay-audio::dsp`, parameters via shared memory (seqlock, bypass-first-word, event-driven rebuilds off the RT path), registered on one endpoint only (EFX via the composite key). In-process COM test proves output bit-identical to the direct DSP chain.
- [x] Install/uninstall that edits only that endpoint's FX property store and restores the exact prior chain (brief risk #2). Verified by a byte-for-byte before/after export diff against the real RODECaster baseline fixture; the same diff re-runs live in the VM pass (deferred).
- [x] `AudioControl` adapter (now the production audio backend; live-tested over the real endpoint's section name); Settings opt-in card live with real status + consent copy; Games Route/Processing readouts real; `relay-core apo` CLI.

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
- [x] In-app share preview: the video pipeline taps a 480×270 JPEG thumbnail (GPU scale on the existing NV12 video processor, ~200 KB readback, WIC 24bppBGR encode) at a preset-chosen rate — 2 fps from the UI, off from the CLI. Measured 17–32 KB per frame, ≈41 KB/s. Added 2026-09-14; Ctrl+Alt+P now shows a picture instead of a notice.

## M5 — Virtual devices on the receiver
**Code-complete 2026-09-12** (branch `m5-vdevices`; plan + session log in `docs/plans/M5-vdevices.md`). The whole chain short of the frame server is proven by tests (media source serves ring frames byte-for-byte in-process; GPU NV12 texture → staging → ring byte-for-byte); footprint gate 4.21 MB / 0.044 % with the vdevice engine in the core. Deferred: the live frame-server pass + Discord/Zoom/Meet verification (needs one elevated registry write — UAC unavailable in the session; runbook `docs/dev/vcam-live.md`), the signed mic driver (EV cert still not ordered), and the interim-mic live check (VB-Cable not installed).
- [x] Virtual camera via the Windows 11 frame-server API (`MFCreateVirtualCamera` + a registered media source reading the receiver's shared NV12 ring). OBS VirtualCam detection as fallback (feeding it: deferred by decision — all target receivers are 22H2+).
- [x] Virtual mic interim: detect VB-Cable / VoiceMeeter and route decoded audio to it (`recv --mic-route`); signed audio-class driver deferred on the EV cert.
- [x] First-run consent screen: two opt-ins, "what we install / how to remove", exact dry-run key listing from the core; nothing registers before opt-in and everything registered lands in `installed.json` (record-then-apply, empty after opt-out).

## M6 — Recording, replay buffer, presets
**Done 2026-09-11** (branch `m6-recording`; plan + loopback measurements in `docs/plans/M6-recording-presets.md`). Recording tees the share's own bitstream through a non-blocking channel into a pure-Rust fragmented-MP4 muxer (golden-fixture tested) — measured 0 ms added latency and +0.06 pt median CPU on loopback; 60 s replay saved in 55 ms (≈ 0.7 s extrapolated at a full 60 Mb/s, « 2 s gate); region↔display switch mid-share held 60 fps with no renegotiation; footprint gate still green (5.1 MB idle). MKV, a 1-hour roll soak and full-motion 4K60 numbers are deferred with runbooks (plan's Deferred).

- [x] Local high-bitrate fMP4 recording alongside the share (same bitstream, hourly keyframe-aligned rolls, 50 GB cap / 10 GB free-floor budget, `%USERPROFILE%\Videos\Relay`), replay buffer with Ctrl+Alt+R hotkey save. (MKV deferred — see plan.)
- [x] Share presets Game / DAW / Desktop as editable data in `presets.json` → encoder settings (bitrate/fps/size), audio source (system / game-process / mic / off), cursor, record-on-start, replay window; `StartSharePreset` resolves them, DAW = 1440p60 with untouched default-endpoint audio.
- [x] Multi-source switching (display, window, region) without restarting the share — same encoder, track and peer connection; one forced IDR per swap.

## M7 — Installer and uninstaller
**Code-complete 2026-09-13** (branch `m7-installer`; plan + diff summaries in `docs/plans/M7-installer.md`). The install → share → uninstall → diff cycle ran seven times on the dev machine with a zero-difference registry diff every time, and an empty file diff except the data folder when the user keeps it. The harness caught one real leftover (Tauri's `HKCU\Software\relay\Relay` install-location record, now removed on every uninstall). Also closes M0's deferred console flash via `relay-svc.exe`. Footprint gate 6.16 MB / 0.044 % with the uninstall engine in the core; 272 workspace tests green. Deferred: the clean-VM broad-tier pass (no hypervisor on this PC), the live removal of the two opt-in components (needs elevation; APO restore also wants audiodg), and the signed installer (EV cert still not ordered).
- [x] NSIS per-user install of core + UI; autostart opt-in; Settings "Uninstall" button. Ships eight files in one folder (four sidecar exes + the two opt-in cdylibs, installed but not registered). Autostart is a first-run-screen toggle plus an `/AUTOSTART` installer switch — Tauri's NSIS hooks cannot add a wizard page, and the first run is the better place to ask — and its Run value points at `relay-svc.exe` so signing in never flashes a console. Build with `scripts/stage-bundle.ps1` then `pnpm tauri build --bundles nsis --config src-tauri/tauri.bundle.conf.json`; the payload lives in that overlay so a checkout without staged binaries still builds.
- [x] Uninstaller restores the APO chain, removes drivers and the Run key, offers to delete data. The plan is a pure function of a probed `MachineState` and is driven by what the components recorded (`apo-backup\<endpoint>.json`, `installed.json`), not a hard-coded list; `relay-core uninstall --dry-run` and the Settings card render the same plan the uninstaller executes.
- [ ] Clean-VM snapshot test. Scripts written and green on the dev machine (`scripts/vm-cycle.ps1`, `machine-snapshot.ps1`, `snapshot-diff.ps1`); the checkpoint run itself is deferred — runbook in `docs/dev/uninstall-vm.md`.
- [ ] Signed installer and binaries (EV cert).

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
| M2 Display profiles | `docs/plans/M2-display.md` | done 2026-09-13; live pass complete on the LG ULTRAGEAR+ (crash-restore + focus restore at 144 ms mean); 2nd monitor / C2 / vendor opcodes / ADLX / Win+L deferred |
| M3 Audio DSP & detection | `docs/plans/M3-audio-dsp.md` | done 2026-09-10; listening session + real-game exclusive check deferred to MVP validation |
| M3b Endpoint APO | `docs/plans/M3b-apo.md` | code-complete 2026-09-11; VM pass + signing deferred (EV cert not ordered, no hypervisor on dev PC) |
| M5 Virtual devices | `docs/plans/M5-vdevices.md` | code-complete 2026-09-12; live pass → `docs/dev/vcam-live.md` (needs elevation); mic driver still EV-cert-blocked |
| M6 Recording & presets | `docs/plans/M6-recording-presets.md` | done 2026-09-11; measured on loopback (0 ms / +0.06 pt CPU / 55 ms replay save); MKV + 1 h roll soak + full-motion 4K60 → MVP validation pass |
| M7 Installer | `docs/plans/M7-installer.md` | code-complete 2026-09-12; install/uninstall cycle green on the dev machine (empty diff); clean-VM + component live removal + signing deferred |

Update the status column when a session starts or finishes a milestone.
