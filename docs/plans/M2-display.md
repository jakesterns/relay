# M2 — Display profiles

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M2-display.md. Work on branch `m2-display`. Original state must be captured and written to disk before any change; test the restore path first. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Goal
Per-game GPU colour and monitor settings that apply only while the game has
focus, only on the game's monitor, and restore on blur, exit, crash, reboot.

## Depends on
M0 (crash-restore harness), M1 (monitor ids and HMONITOR mapping, DDC/CI capability list).

## Definition of Ready
- [x] M0 and M1 complete. Verified 2026-09-11: `MonitorProbe { id, hmonitor, primary, ddc }` and the VCP capability parser are on `m1-hardware`; `m2-display` branches from it (worktree `stream-share-m2`). Note: neither M4 nor M1 was merged to `main` — the milestone branches are stacked (main → m4-share → m1-hardware → m2-display).
- [x] Test hardware noted (2026-09-11): GPU **NVIDIA RTX 3090** (driver 32.0.16.1664; NvAPI path) + AMD Radeon iGPU (no monitor attached; ADLX stays deferred). Monitor **LG ULTRAGEAR+** (`mon:GSM5C7C:402NTCZ9E219`), DDC/CI verified in M1 with 47 VCP codes incl. 0x10/0x12/0x60/0x62.
  **Manual backup note:** the panel was *powered off for the whole coding session*, so the pre-change values could not be read or photographed up front. Mitigations: (a) `cargo test -p relay-display --test live_read -- --ignored --nocapture` prints brightness/contrast/sharpness, the raw gamma ramp and NvAPI vibrance/hue read-only — run it first thing in the live pass and paste the output here; (b) the live crash test records `before:` to stdout before changing anything; (c) last resort is the monitor OSD reset.
- [x] NvAPI: `nvapi64.dll` present (RTX 3090). Loaded dynamically per operation (`relay_display::nvapi`), no link-time dependency; gamma-ramp path is the vendor-neutral fallback.

## Checklist
### DDC/CI
- [x] `relay-display::ddc`: physical monitor handle from the probe's `HMONITOR` (`GetPhysicalMonitorsFromHMONITOR`), `GetVCPFeatureAndVCPFeatureReply` / `SetVCPFeature`, 3 retries with 50 ms pauses per call and a per-model write-settle delay (60 ms on LG). Handles are opened per transaction group and never cached (they go stale on display changes).
- [x] Capture reads every code the profile will touch (`DisplayAdapter::capture`) and stores `(code, value)` pairs in the snapshot; restore writes them back in reverse order (unit test `restore_writes_vcp_in_reverse_capture_order`). Failing to *read* an original aborts the apply — never write what you cannot put back.
- [x] Model quirks table `relay_display::vcp::quirks_for` keyed on the PNP prefix of the monitor id (write delays; slots for verified vendor opcodes). **No vendor opcode is listed yet** — black equaliser / response candidates exist in the LG capability string (0xF4–0xFF region) but writing an unverified opcode is worse than skipping: `plan_writes` skips the field and reports it in `DisplayVia::unsupported` (→ Deferred).

### GPU colour
- [x] `relay-display::nvapi`: `nvapi64.dll` loaded per operation via `nvapi_QueryInterface` (no link-time dep, nothing resident between applies — footprint), display handles matched to the probe by GDI name, digital vibrance (`GetDVCInfo`/`SetDVCLevel`, raw levels in the snapshot, profile-percent mapping unit-tested) and hue (`GetHUEInfo`/`SetHUEAngle`). Live verification pending (see Deferred).
- [x] `relay-display::gamma`: pure ramp maths (gamma 0.5–2.0, contrast ±50 % effect, cubic-falloff shadow lift; monotonicity and pivot unit-tested against corner cases) + `Get/SetDeviceGammaRamp` on the monitor's own DC. The *original* ramp is captured raw and restored raw, so f.lux / Night Light curves come back exactly.
- [ ] ADLX (AMD) — deferred until NVIDIA is live-verified; the `DisplayIo` trait is the seam it plugs into.

### Core integration
- [x] `DisplayControl` trait now takes a target (`Option<&MonitorProbe>`); `crates/core/src/display_backend.rs` implements capture/apply/restore behind a `DisplayIo` primitive trait (fake in tests, `RealIo` in production). Only the target monitor is read or written — proven by unit tests with a two-monitor fake (`apply_touches_only_the_target_monitor_and_restore_reverts_it`, plus untouched-B assertions on every failure path). Restore re-resolves handles by stable `MonitorId` (crash/reboot path, `restore_re_resolves_stale_handles_by_stable_id`); a monitor that is gone fails the restore so the snapshot stays pending for the next start.
- [x] Game moves monitors → restore old, apply new: `Applier` keys the fast path on (profile, target-id) and retargets otherwise (`moving_monitors_restores_old_then_applies_new`). Detection: `Foreground.hmonitor` on every focus event, an `EVENT_SYSTEM_MOVESIZEEND` hook for drags, and the 5 s service tick as the safety net for hook-less moves (Win+Shift+Arrow).
- [x] Session events: `WTSRegisterSessionNotification` on the hidden winloop window → `CoreEvent::SessionLock` → restore on lock, re-select on unlock. Logoff/shutdown ride the existing console-ctrl path; `WM_DISPLAYCHANGE` re-probes and re-selects (a vanished monitor deselects → restore).
- [x] Snapshot format: `DisplayStateSnapshot.targets[]` = `{ monitor id, gdi_name, hmonitor, vcp[(code,value)], raw gamma ramp, raw NvAPI {dvc,min,max,hue} }`, serde-round-trip tested. Written (fsync) before any change, as before.
- [ ] Crash-restore harness on the real backend on a real monitor — **written** (`crates/core/tests/crash_restore_display.rs`, ignored: apply real brightness/gamma/vibrance deltas over IPC, `taskkill /F`, assert live values restored on restart by reading the hardware) but **not yet run**: the monitor was powered off all session (→ Deferred/live pass). The recording-backend harness (`crash_restore.rs`) passes with the new target-aware backend.

### UI
- [x] Display section on the Games screen reads/writes the subject profile's real `DisplaySettings` (draft loaded via `get_profile`, saved via `save_profile`); sliders for unadvertised VCP codes are disabled from the library's `ddcci` list; black equaliser / response are disabled until a verified vendor opcode exists. "Applied via" renders `CoreState.display_via` (NvAPI / gamma ramp / DDC/CI actually used, plus fields the hardware could not honour). `pnpm build` (tsc + vite) green.
- [x] Hardware library monitor entry shows which controls are available ("controls: brightness, contrast" from the advertised codes).

### Gates
- [x] Workspace tests green (incl. 10 new adapter tests, 14 relay-display tests, apply/backup extensions); clippy + fmt clean.
- [x] Release footprint gate PASS 2026-09-11: idle RSS 5.95 MB, private WS 0.86 MB, 0 % CPU, exe 1.04 MB.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason. ✔ (Deferred below)
- Alt-tab out of the game restores within 200 ms; alt-tab back re-applies. — *pending live pass*
- Kill the core while applied, restart: monitor and GPU back to original. — *test written; pending live pass*
- Second monitor never changes. — proven at the unit level on every path (two-monitor fake); live proof needs a second physical monitor (runbook below).

## Out of scope
LUT / ICC profiles, HDR.

## Deferred
- **Live verification pass** — the LG was physically powered off (no EDID device, DDC dead, gamma/NvAPI calls failing on the phantom display) for the entire session (2026-09-11); software wake (input jiggle + `SC_MONITORPOWER`) did not help. *Runbook, in order, with the monitor awake:*
  1. `cargo test -p relay-display --test live_read -- --ignored --nocapture` — read-only; paste the output into the DoR note above. Confirms DDC/CI, ramp read and the NvAPI function ids/struct versions (community-documented, not yet exercised on this driver — if `GetDVCInfo` returns −9, switch to the `…Ex` variants).
  2. `cargo test -p relay-core --test crash_restore_display -- --ignored --nocapture` — the full apply → kill → restart → restored cycle with real hardware reads between phases.
  3. Alt-tab timing: Ready profile on a real exe, watch `logs/core.log` timestamps for restore-on-blur ≤ 200 ms; alt-tab back re-applies.
  4. Session lock (Win+L) restores; unlock re-applies.
- **Second-monitor live proof + LG C2 manual test log** — one physical monitor on this PC (same deferral as M1). *Runbook:* attach the C2, give it a profile row, confirm (a) only the game's monitor changes brightness/ramp, (b) dragging the game across restores the first panel within a tick, (c) per-model VCP codes that work go into the hardware library / quirks table.
- **Vendor opcodes (black equaliser, response time)** — need eyes on the OSD to verify which candidate code (LG 0xF4–0xFF) drives which OSD setting; set-and-readback alone cannot prove the on-screen meaning. Until then the fields are skipped and surfaced as unsupported in the UI.
- **ADLX (AMD)** — after NVIDIA is live-verified; plugs into `DisplayIo`.
