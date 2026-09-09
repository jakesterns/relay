# M3b — Endpoint APO

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M3b-apo.md. Work on branch `m3b-apo`. Confirm the EV certificate is available before touching install/uninstall; everything else can proceed unsigned in a test-signing VM. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Goal
Host `relay-audio::dsp` inside the Windows audio engine as an endpoint APO on
exactly one render endpoint, parameterised live from the core, installed and
removed without leaving a trace.

## Depends on
M3 (DSP). **Blocked on the EV code-signing certificate and Hardware Dev Center attestation** — track status here:
- [ ] EV cert ordered (date: ____)
- [ ] EV cert received
- [ ] Hardware Dev Center account + attestation signing working

## Definition of Ready
- [ ] M3 complete (DSP chain with allocation-free `process`).
- [ ] A Windows VM with test-signing enabled and a checkpoint taken before any APO registration.
- [ ] Registry export of the target endpoint's FX property store saved as the pre-install baseline.
- [ ] For the install/uninstall items only: EV cert and attestation signing working (tracked above).

## Checklist
- [ ] `crates/audio/apo`: `cdylib` implementing `IAudioProcessingObject`, `IAudioProcessingObjectRT`, `IAudioProcessingObjectConfiguration` (COM via `windows` crate `implement`). SFX or EFX placement decided by test; MFX considered for per-stream.
- [ ] Format negotiation: accept the endpoint's rate and channel count, never resample; refuse formats the DSP does not support (falls back to pass-through).
- [ ] Parameter block in a named shared-memory section + event; core writes, APO reads lock-free with sequence numbers; bypass flag is the first word.
- [ ] Install: write only the target endpoint's FX property store keys (`PKEY_FX_*`), COM registration under HKCU where possible, otherwise HKLM with a recorded diff. Save the complete prior property store to `%LOCALAPPDATA%\Relay\apo-backup\<endpoint>.json` first.
- [ ] Uninstall restores the prior property store byte-for-byte and unregisters; verified by a before/after registry export diff test in a VM.
- [ ] Endpoint re-enumeration after install so the engine picks up the APO without a reboot where Windows allows it.
- [ ] `AudioControl` adapter: `capture` = read current params, `apply` = write, `restore` = bypass.
- [ ] Settings screen: opt-in card goes live with "what this installs / how to remove"; Games screen "Route" shows Endpoint APO.
- [ ] Test-sign flow documented (`bcdedit /set testsigning on` VM) for development without the EV cert.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- APO active on the headset endpoint only; other endpoints and apps untouched.
- Uninstall leaves the endpoint property store identical to the pre-install export.
- Processing latency reported in the UI ≤ 1 ms at 48 k.

## Deferred
_(none yet)_
