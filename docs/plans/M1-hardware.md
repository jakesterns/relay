# M1 — Hardware library and probe

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M1-hardware.md. Work on branch `m1-hardware`. Work through the checklist, check items off as you go, and update docs/ROADMAP.md when done.

## Goal
The core knows which headset and monitors are connected, profiles match on
that, and the user can build a hardware library with measured headset curves.

## Depends on
M0. Required by M2 (per-monitor targeting) and M3 (headset curves).

## Definition of Ready
- [x] M0 complete (2026-09-09).
- [x] Test hardware recorded (probed on the dev PC 2026-09-10): audio endpoints — ASUS USB Audio 2.0 (`USB\VID_0B05&PID_1A53`), RØDE USB device (`USB\VID_19F7&PID_004E`), RØDECaster virtual endpoints, second USB audio device (`USB\VID_2E1A&PID_4C05`), NVIDIA HDMI audio through the monitor. Monitors — one physical: LG ULTRAGEAR+ (EDID `GSM5C7C`, serial `402NTCZ9E219`). Only one monitor is attached, so multi-monitor id logic is unit-tested against fixture EDID dumps; live second-monitor verification is Deferred with a runbook.
- [x] AutoEQ result file: `crates/core/tests/fixtures/autoeq-hd560s.csv` (oratory1990 Sennheiser HD 560S, downloaded 2026-09-10 from the AutoEq repo). Real EDID fixture: `crates/core/tests/fixtures/edid-gsm5c7c.bin` (dumped from this PC's registry).

## Checklist
- [ ] `HeadsetId` from the default WASAPI render endpoint: `IMMDeviceEnumerator::GetDefaultAudioEndpoint`, key = container ID or endpoint ID string; friendly name for display. Handles USB DAC → headphone chains by letting the user name the headset attached to an endpoint.
- [ ] `MonitorId` from `QueryDisplayConfig` + EDID via SetupAPI: manufacturer PNP id, product code, serial → stable key; friendly name, native resolution, refresh rate; which `HMONITOR` it maps to (needed by M2).
- [ ] DDC/CI capability probe per monitor (`GetCapabilitiesStringLength` / `CapabilitiesRequestAndCapabilitiesReply`), parsed VCP code list stored in the library.
- [ ] `hardware.json` store in `relay-core`: headsets `{ id, name, kind (headphone|iem|speakers), curve: Option<Vec<(hz, db)>>, source }`, monitors `{ id, name, panel, ddcci: Option<Vec<VcpCode>> }`, interfaces.
- [ ] AutoEQ importer: parse the `results/<source>/<headphone>` CSV format (frequency, raw dB) from a local file or pasted text; store as the headset's measured curve. No network access from the core.
- [ ] Device-change events: `IMMNotificationClient` for endpoint changes, `WM_DISPLAYCHANGE` / `WM_DEVICECHANGE` on the winloop thread → `CoreEvent::HardwareChanged` → re-run profile selection.
- [ ] IPC: `ListHardware`, `SaveHardware`, `DeleteHardware`, `ProbeHardware`.
- [ ] UI: Profiles screen right rail lists real hardware with live Plugged / Main / Second pills; Add headset / Add monitor dialogs; headset and monitor pickers on the profile form and on the Games screen "Change" links.
- [ ] Profile selection tests extended with real ids from the probe.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- Unplugging the headset or switching default endpoint re-selects the profile within 1 s without a focus change.
- Two Call of Duty rows keyed to different headsets pick correctly when swapping.

## Out of scope
Applying anything (M2/M3). Online curve download.

## Deferred
_(none yet)_
