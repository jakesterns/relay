# Kickoff prompts — one per milestone chat session

> **All eight milestones (M0–M7) are complete and merged into `main` as of
> 2026-09-14.** The prompts below are kept for the record. For work that is
> still outstanding, use **`docs/plans/SESSIONS.md`** — it carries every
> remaining task as a session with its own branch, worktree, DoR, DoD and
> kickoff prompt.


Copy the block for the milestone into a new Claude Code chat in the Stream
Share project. Each prompt tells the session where the plan is, how to treat
Definition of Ready (DoR) and Definition of Done (DoD), and how to report.

Shared rules every prompt relies on live in `docs/plans/README.md`.

Standing decision (2026-09-10, from M4): dual-PC and other currently
impossible live verification is replaced by thorough unit tests of the logic,
with the live runbook recorded under the plan's Deferred section for the MVP
validation pass. Every prompt below inherits this: **unit-test everything that
runs without the missing hardware; never silently skip verification — either
do it live on this PC or write the unit tests and defer the live run with a
runbook.**

Completed: **M0** (2026-09-09) and **M4** (2026-09-10) — their original
prompts are kept at the bottom for the record.

---

## What can run in parallel

Remaining dependency graph (M0 and M4 are done, so four tracks are unblocked
immediately):

```
Track A:  M1 hardware ──► M2 display
Track B:  M3 audio DSP ──► M3b APO   (M3b also gated on the EV cert, external)
Track C:  M5 virtual devices         (virtual mic part gated on the EV cert)
Track D:  M6 recording/presets
Final:    M7 installer               (core+UI scope any time; full scope needs M3b + M5)
```

- **M1, M3, M5, M6 can all start now, in any combination.** They live in
  different crates (core hardware store / `crates/audio` / `crates/vdevice` /
  `crates/capture`) and none depends on another.
- **M2 must wait for M1** (monitor ids, HMONITOR mapping, DDC/CI capability
  list). M3 does not need M1 — headset curves plug in later.
- **M3b must wait for M3** and for the EV certificate; if the cert is the
  long pole, M3b's DSP-in-APO work can proceed test-signed in a VM.
- **M7 runs last** for the full uninstaller; a core+UI-only installer slice
  can be pulled earlier if needed.
- Practical limit: run **at most two or three sessions concurrently** and
  merge often. The known collision points are `crates/core/src/ipc.rs` + its
  TypeScript mirror `ui/src/lib/ipc.ts` (M1, M2, M5, M6 all add methods or
  events) and the Tauri shell/UI screens. Keep each session on its own
  branch, rebase on main before touching the IPC contract, and land IPC
  changes in small, early commits.
- Suggested pairing if running two at a time: **M1 + M3** first (they feed
  M2 and M3b), then **M2 + M5**, then **M6**, then M3b when the cert lands,
  then M7.

---

## M1 — Hardware library and probe
```
You are starting milestone M1 (hardware library and probe) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M1-hardware.md. Work on branch m1-hardware.

M0 and M4 are complete; the core, IPC pipe, and UI shell all exist. M2 (display) consumes your monitor ids, HMONITOR mapping, and DDC/CI capability list, so treat those as contract surfaces.

1. Verify every Definition of Ready item. If the test hardware list or an AutoEQ result file is not recorded in the plan, ask me once; if I cannot supply hardware right now, build against fixture data (checked-in probe dumps and AutoEQ files), unit-test the parsing/id/selection logic thoroughly, and record the live-hardware verification under Deferred with a runbook.
2. Work through the checklist in order and check items off in the plan file. Ids must be stable across reboots and USB port changes — encode why in the id scheme and unit-test it against fixtures.
3. The core must not make network requests; the AutoEQ importer reads local files or pasted text only.
4. Unit-test the library store, curve parsing, id stability, and profile auto-selection scoring (the game × headset × monitor logic in profiles.rs is the consumer). The two-headset Call of Duty case must be a unit test, not just a manual check.
5. Any IPC additions go in crates/core/src/ipc.rs and must be mirrored in ui/src/lib/ipc.ts in the same commit.
6. Unfinished items go under "Deferred" with a reason.
7. Finish only when the Definition of Done is met: re-selection within 1 s on a device change (measured live on this PC — plug/unplug is available), and the selection tests pass. Update M1's status in docs/ROADMAP.md and summarise.
```

## M2 — Display profiles
```
You are starting milestone M2 (display profiles) for Relay. Work on branch m2-display. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M2-display.md. Requires M1 merged (monitor ids, HMONITOR mapping, DDC/CI capability list) — verify that first and stop if it is not.

This milestone changes real monitor and GPU settings on this PC, so live verification IS possible here — do it. Original state must be captured and written to disk before any change (backup.rs already enforces the pattern), and you must implement and test restore-before-apply for each control.

1. Verify every Definition of Ready item, including a manual note of the current monitor settings as a fallback.
2. Work through the checklist: DDC/CI, GPU colour, core integration, UI. Check items off in the plan file.
3. Only the monitor hosting the game window may change. Prove the second monitor is untouched.
4. Run the existing crash-restore harness (cargo test -p relay-core --test crash_restore) against the real display backend and record the result. Unit-test the pure parts (VCP code mapping, gamma-ramp math, LUT generation, capability parsing) against fixtures.
5. Multi-monitor edge cases (mixed DDC/CI support, monitor unplugged mid-apply) get unit tests; anything needing hardware I don't have goes under Deferred with a runbook.
6. Finish only when the Definition of Done is met. Update M2's status in docs/ROADMAP.md and summarise.
```

## M3 — Audio DSP and detection
```
You are starting milestone M3 (audio DSP and exclusive-mode detection) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M3-audio-dsp.md. Work on branch m3-audio-dsp. M0 is the only dependency; M1's headset curves plug in later, so use fixture curves if M1 has not merged yet.

The DSP must be real-time safe: no allocation after prepare(), bypass is a plain copy, no resampling. Prove the allocation-free property with a test.

1. Verify every Definition of Ready item; ask me to confirm the HRTF impulse-response set and licence if it is not recorded.
2. Work through the checklist: DSP stages with golden tests, benchmark, exclusive-mode detection, offline A/B listening test. Check items off in the plan file.
3. This is the most unit-testable milestone in the project: biquad coefficients and frequency response, convolution correctness against a reference implementation, limiter attack/release envelopes, bypass bit-exactness, and denormal handling all get golden tests. The benchmark numbers go in the plan's Measurements.
4. The offline A/B listening test needs my ears; stage the material and defer the listening session itself if I am not available.
5. Unfinished items go under "Deferred" with a reason.
6. Finish only when the Definition of Done is met. Update M3's status in docs/ROADMAP.md and summarise.
```

## M3b — Endpoint APO
```
You are starting milestone M3b (endpoint APO) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M3b-apo.md. Work on branch m3b-apo. Requires M3 merged (the DSP the APO hosts).

This milestone touches the Windows audio engine and is partly gated on the EV certificate.

1. Verify every Definition of Ready item, including the EV cert tracking checklist in the plan. If the cert is not available, do all APO development test-signed in a VM and leave the production install/uninstall signing items as Deferred with that reason.
2. Take a registry export of the target endpoint's FX property store before any registration and keep it as the baseline.
3. Work through the checklist and check items off. The uninstall must restore the property store byte-for-byte; prove it with a diff, and unit-test the property-store read/modify/restore logic against exported fixtures before ever touching the live registry.
4. Only one render endpoint may ever be modified; the WASAPI-exclusive detection from M3 must warn instead of silently doing nothing.
5. Finish only when the Definition of Done is met. Update M3b's status in docs/ROADMAP.md and summarise.
```

## M5 — Virtual devices on the receiver
```
You are starting milestone M5 (virtual camera and microphone on the receiver) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M5-vdevices.md. Work on branch m5-vdevices. M4 is complete: receive mode renders to a native D3D11 window and the loopback path (send and receive on this one PC) is the proven test rig — use it, since the second PC is not currently available.

1. Verify every Definition of Ready item, including this PC's Windows build for the MFCreateVirtualCamera path. Confirm with me whether the EV cert is available for the virtual mic driver; if not, implement the VB-Cable interim route and defer the signed driver with that reason (tracking lives in the M3b plan).
2. Virtual camera first (no signing needed), then consent screen, then microphone.
3. Nothing is registered before the user opts in, and everything registered is recorded in installed.json so it can be removed. Unit-test the installed.json bookkeeping and the register/unregister argument construction; the frame path from the receiver's decode loop to the camera source gets a loopback test.
4. Verify locally: run receive in loopback and select the virtual camera in Discord/Zoom/Meet on this PC; record results in the plan. The true two-PC end-to-end joins the deferred MVP validation runbook from M4.
5. Finish only when the Definition of Done is met. Update M5's status in docs/ROADMAP.md and summarise.
```

## M6 — Recording, replay buffer, presets, source switching
```
You are starting milestone M6 (recording, replay buffer, presets, source switching) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M6-recording-presets.md. Work on branch m6-recording. M4 is complete and its Measurements table is the baseline.

Recording must add no latency and under 1 % CPU to the live share; re-measure the M4 loopback numbers with recording on and put both in the plan.

1. Verify every Definition of Ready item; ask me for the recording location and disk budget if not recorded.
2. Work through the checklist and check items off in the plan file.
3. Source switching must not renegotiate the peer connection.
4. Unit-test the pure logic: MP4/MKV muxer state machine against golden byte fixtures, replay-buffer ring accounting (eviction, save-marker slicing), preset serialization and preset→SendOpts mapping, and the source-switch state machine. The live encode+record path runs in loopback on this PC.
5. Unfinished items go under "Deferred" with a reason.
6. Finish only when the Definition of Done is met. Update M6's status in docs/ROADMAP.md and summarise.
```

## M7 — Installer and uninstaller
```
You are starting milestone M7 (installer and uninstaller) for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M7-installer.md. Work on branch m7-installer. Run this last: the full uninstaller needs M3b and M5 merged. If they are incomplete, scope this session to core + UI + share engine and record the rest as Deferred.

The uninstaller is the product's promise: after uninstall, a clean VM must show no differences except the optional data folder.

1. Verify every Definition of Ready item, including the clean VM checkpoint.
2. Work through the checklist and check items off in the plan file.
3. Unit-test the manifest logic (everything installed is recorded, uninstall order is the reverse, partial-failure recovery) so the promise does not rest on the VM run alone.
4. Run the clean-VM install → opt-in → share → uninstall → diff cycle and paste the diff summary into the plan. The VM runs on this PC, so this is live-verifiable now.
5. Finish only when the Definition of Done is met. Update M7's status in docs/ROADMAP.md and summarise.
```

---

## Completed milestones (prompts kept for the record)

## M0 — Foundation hardening (done 2026-09-09)
```
You are starting milestone M0 for Relay. Read CLAUDE.md, docs/ROADMAP.md, and docs/plans/M0-foundation.md.

1. Verify every Definition of Ready item in the plan. If one is not met, stop and tell me what you need.
2. Make the initial commit on main first, then work through the checklist in order. Check items off in the plan file as you complete them.
3. Do not drop scope: anything you cannot finish goes under "Deferred" with a reason.
4. Finish only when the Definition of Done is met: clippy clean, all tests pass, pnpm build passes, footprint gate passes in release. Record the release binary size, idle RSS, and idle CPU under "Measurements".
5. Update the status column for M0 in docs/ROADMAP.md and end with a short summary of what changed and what was deferred.
```

## M4 — Share MVP (done 2026-09-10; live two-PC run deferred to the MVP validation pass)
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
