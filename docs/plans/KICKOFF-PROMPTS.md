# Kickoff prompts — one per milestone chat session

Copy the block for the milestone into a new Claude Code chat in the Stream
Share project. Each prompt tells the session where the plan is, how to treat
Definition of Ready (DoR) and Definition of Done (DoD), and how to report.

Shared rules every prompt relies on live in `docs/plans/README.md`.

---

## M0 — Foundation hardening
```
You are starting milestone M0 for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M0-foundation.md.

1. Verify every Definition of Ready item in the plan. If one is not met, stop and tell me what you need.
2. Make the initial commit on main first, then work through the checklist in order. Check items off in the plan file as you complete them.
3. Do not drop scope: anything you cannot finish goes under "Deferred" with a reason.
4. Finish only when the Definition of Done is met: clippy clean, all tests pass, pnpm build passes, footprint gate passes in release. Record the release binary size, idle RSS, and idle CPU under "Measurements".
5. Update the status column for M0 in docs/ROADMAP.md and end with a short summary of what changed and what was deferred.
```

## M4 — Share MVP
```
You are starting milestone M4 (Share MVP) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M4-share.md. Work on branch m4-share.

Latency and resource efficiency are the top priorities: glass-to-glass under 50 ms wired, sender CPU around 1 %, hardware encode only, GPU-resident frames from capture to encoder. Measure at every stage and fill the Measurements table in the plan as you go.

1. Verify every Definition of Ready item. If the second PC, GPU details, or the Media Foundation HEVC encoder MFT are not confirmed, stop and ask me.
2. Work through the checklist in order: capture, encode, audio, transport, receiver mode, process model and UI. Check items off in the plan file.
3. At the encoder decision gate, if capture plus encode p99 exceeds 20 ms after tuning Media Foundation, implement direct NVENC in this milestone and record why.
4. The share engine is a separate process spawned per share; core RSS must not change while sharing.
5. Never drop scope silently: unfinished items go under "Deferred" with a reason.
6. Finish only when the Definition of Done is met, including a 10-minute 4K60 zero-drop run with numbers recorded. Update M4's status in docs/ROADMAP.md and summarise results and deferrals.
```

## M1 — Hardware library and probe
```
You are starting milestone M1 (hardware library and probe) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M1-hardware.md. Work on branch m1-hardware.

1. Verify every Definition of Ready item; ask me for the test hardware list and an AutoEQ result file if they are not recorded in the plan.
2. Work through the checklist in order and check items off in the plan file. Ids must be stable across reboots and USB port changes.
3. The core must not make network requests; the AutoEQ importer reads local files or pasted text only.
4. Unfinished items go under "Deferred" with a reason.
5. Finish only when the Definition of Done is met: re-selection within 1 s on a device change, and the two-headset Call of Duty case picks correctly. Update M1's status in docs/ROADMAP.md and summarise.
```

## M2 — Display profiles
```
You are starting milestone M2 (display profiles) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M2-display.md. Work on branch m2-display.

This milestone changes real monitor and GPU settings. Original state must be captured and written to disk before any change, and you must implement and test restore before apply for each control.

1. Verify every Definition of Ready item, including a manual note of the current monitor settings as a fallback.
2. Work through the checklist: DDC/CI, GPU colour, core integration, UI. Check items off in the plan file.
3. Only the monitor hosting the game window may change. Prove the second monitor is untouched.
4. Run the crash-restore harness against the real backend and record the result.
5. Unfinished items go under "Deferred" with a reason.
6. Finish only when the Definition of Done is met. Update M2's status in docs/ROADMAP.md and summarise.
```

## M3 — Audio DSP and detection
```
You are starting milestone M3 (audio DSP and exclusive-mode detection) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M3-audio-dsp.md. Work on branch m3-audio-dsp.

The DSP must be real-time safe: no allocation after prepare(), bypass is a plain copy, no resampling. Prove the allocation-free property with a test.

1. Verify every Definition of Ready item; ask me to confirm the HRTF impulse-response set and licence if it is not recorded.
2. Work through the checklist: DSP stages with golden tests, benchmark, exclusive-mode detection, offline A/B listening test. Check items off in the plan file.
3. Record benchmark numbers in the plan.
4. Unfinished items go under "Deferred" with a reason.
5. Finish only when the Definition of Done is met. Update M3's status in docs/ROADMAP.md and summarise.
```

## M3b — Endpoint APO
```
You are starting milestone M3b (endpoint APO) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M3b-apo.md. Work on branch m3b-apo.

This milestone touches the Windows audio engine and is partly gated on an EV certificate.

1. Verify every Definition of Ready item. Confirm with me whether the EV cert is available; if not, do all APO development in the test-signed VM and leave the install/uninstall signing items as Deferred with that reason.
2. Take a registry export of the target endpoint's FX property store before any registration and keep it as the baseline.
3. Work through the checklist and check items off. The uninstall must restore the property store byte-for-byte; prove it with a diff.
4. Only one render endpoint may ever be modified.
5. Finish only when the Definition of Done is met. Update M3b's status in docs/ROADMAP.md and summarise.
```

## M5 — Virtual devices on the receiver
```
You are starting milestone M5 (virtual camera and microphone on the receiver) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M5-vdevices.md. Work on branch m5-vdevices.

1. Verify every Definition of Ready item, including the receiver's Windows build. Confirm with me whether the EV cert is available for the virtual mic driver; if not, implement the VB-Cable interim route and defer the driver with that reason.
2. Virtual camera first (no signing needed), then consent screen, then microphone.
3. Nothing is registered before the user opts in, and everything registered is recorded in installed.json so it can be removed.
4. Verify in Discord, Zoom, and Meet and record results in the plan.
5. Finish only when the Definition of Done is met. Update M5's status in docs/ROADMAP.md and summarise.
```

## M6 — Recording, replay buffer, presets, source switching
```
You are starting milestone M6 (recording, replay buffer, presets, source switching) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M6-recording-presets.md. Work on branch m6-recording.

Recording must add no latency and under 1 % CPU to the live share; use the M4 Measurements table as the baseline and re-measure with recording on.

1. Verify every Definition of Ready item; ask me for the recording location and disk budget if not recorded.
2. Work through the checklist and check items off in the plan file.
3. Source switching must not renegotiate the peer connection.
4. Unfinished items go under "Deferred" with a reason.
5. Finish only when the Definition of Done is met. Update M6's status in docs/ROADMAP.md and summarise.
```

## M7 — Installer and uninstaller
```
You are starting milestone M7 (installer and uninstaller) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M7-installer.md. Work on branch m7-installer.

The uninstaller is the product's promise: after uninstall, a clean VM must show no differences except the optional data folder.

1. Verify every Definition of Ready item, including the clean VM checkpoint. If M3b or M5 are incomplete, scope this session to core and UI and record the rest as Deferred.
2. Work through the checklist and check items off in the plan file.
3. Run the clean-VM install → opt-in → share → uninstall → diff cycle and paste the diff summary into the plan.
4. Finish only when the Definition of Done is met. Update M7's status in docs/ROADMAP.md and summarise.
```
