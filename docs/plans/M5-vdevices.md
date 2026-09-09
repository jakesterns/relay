# M5 — Virtual devices on the receiver

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M5-vdevices.md. Work on branch `m5-vdevices`. The virtual camera has no signing dependency and comes first; the virtual mic driver is gated on the EV cert. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Goal
On the receiving PC, the incoming stream appears as "Relay Camera" and "Relay
Microphone" in Discord / Zoom / Meet, after explicit opt-in.

## Depends on
M4 (receiver mode). Virtual mic blocked on EV cert (see M3b tracking).

## Definition of Ready
- [ ] M4 complete (receiver mode renders to a window).
- [ ] Receiver PC is Windows 11 22H2 or later (frame-server virtual camera API); build noted here: ____
- [ ] Discord, Zoom, and a browser for Meet installed on the receiver for verification.
- [ ] Virtual mic driver items only: EV cert and attestation (see M3b tracking).

## Checklist
### Virtual camera
- [ ] `relay-vdevice::camera::frameserver`: Windows 11 virtual camera via `MFCreateVirtualCamera` with a registered media source COM DLL that reads frames from shared memory written by the receiver; NV12 at stream resolution, 60 fps.
- [ ] Fallback: detect OBS VirtualCam (registry/DirectShow filter); if present, offer to feed it; otherwise explain the Win11 requirement.
- [ ] Register/unregister the media source only after opt-in; unregister on uninstall; record what was registered in `%LOCALAPPDATA%\Relay\installed.json`.
- [ ] Verified in Discord, Zoom, Meet (browser) at 1080p60 and 4K30.

### Virtual microphone
- [ ] Interim: detect VB-Cable / VoiceMeeter and route decoded audio to it via WASAPI render.
- [ ] Signed audio-class virtual device (KMDF / AVStream or Windows Audio Virtual Driver sample as base) — only after EV cert + attestation. Installed through `pnputil` with the INF recorded in `installed.json`.
- [ ] Sample-accurate audio/video alignment on the receiver (measure with a clap test).

### Consent
- [ ] First-run consent screen: two opt-ins, dry-run listing of every file, registry key, and driver each installs; "how to remove" links to the Settings uninstall action.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- Fresh receiver PC: opt in, "Relay Camera" appears in a Discord call within 5 s of sharing.
- Opt out removes every registered component; `installed.json` is empty.

## Deferred
_(none yet)_
