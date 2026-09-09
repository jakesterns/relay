# M2 — Display profiles

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M2-display.md. Work on branch `m2-display`. Original state must be captured and written to disk before any change; test the restore path first. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Goal
Per-game GPU colour and monitor settings that apply only while the game has
focus, only on the game's monitor, and restore on blur, exit, crash, reboot.

## Depends on
M0 (crash-restore harness), M1 (monitor ids and HMONITOR mapping, DDC/CI capability list).

## Definition of Ready
- [ ] M0 and M1 complete (crash-restore harness, monitor ids, DDC/CI capability list).
- [ ] Test monitors and GPU noted here, with a photo or note of their current settings as a manual backup: ____
- [ ] NvAPI headers/version available if the GPU is NVIDIA; otherwise the gamma-ramp path is the target.

## Checklist
### DDC/CI
- [ ] `relay-display::ddc`: physical monitor handles from `HMONITOR` (`GetPhysicalMonitorsFromHMONITOR`), `GetVCPFeatureAndVCPFeatureReply` / `SetVCPFeature` for brightness (0x10), contrast (0x12), and vendor codes for black equaliser / response time discovered per model; retries and per-call timeouts because DDC/CI is slow and flaky.
- [ ] Capture reads every code the profile will touch and stores them in the snapshot; restore writes them back in reverse order.
- [ ] Model quirks table in the hardware library (codes that work, write delays).

### GPU colour
- [ ] `relay-display::gpu::nvapi`: NvAPI bindings (nvapi64.dll dynamic load, no link-time dependency) for digital vibrance and hue per display; capture/restore.
- [ ] `relay-display::gpu::gamma`: `SetDeviceGammaRamp` per monitor DC for gamma, contrast, shadow lift; vendor-neutral fallback and the only path on non-NVIDIA until ADLX.
- [ ] ADLX (AMD) after NVIDIA is verified; keep behind the same trait.

### Core integration
- [ ] `DisplayControl` adapter implementing capture / apply / restore, targeting the monitor that hosts the game window (`MonitorFromWindow`), leaving other monitors untouched (`leave_other_monitors`).
- [ ] Game window moves to another monitor → restore old monitor, apply on new one.
- [ ] Windows session events: lock / logoff / display change trigger restore.
- [ ] Crash-restore harness extended with the real display backend on a real monitor (manual, documented).

### UI
- [ ] Display section on the Games screen reads and writes the profile's `DisplaySettings`; "Applied via" shows NvAPI / gamma ramp / DDC-CI actually used; disabled sliders for unsupported codes.
- [ ] Hardware library entry for the monitor shows which controls are available.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- Alt-tab out of the game restores within 200 ms; alt-tab back re-applies.
- Kill the core while applied, restart: monitor and GPU back to original.
- Second monitor never changes.

## Out of scope
LUT / ICC profiles, HDR.

## Deferred
_(none yet)_
