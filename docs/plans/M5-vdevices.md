# M5 — Virtual devices on the receiver

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M5-vdevices.md. Work on branch `m5-vdevices`. The virtual camera has no signing dependency and comes first; the virtual mic driver is gated on the EV cert. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Goal
On the receiving PC, the incoming stream appears as "Relay Camera" and "Relay
Microphone" in Discord / Zoom / Meet, after explicit opt-in.

## Depends on
M4 (receiver mode). Virtual mic blocked on EV cert (see M3b tracking).

## Definition of Ready — verified 2026-09-11
- [x] M4 complete (receiver mode renders to a window).
- [x] Receiver PC is Windows 11 22H2 or later (frame-server virtual camera API); build noted here: **26200.9445 (25H2)** — dev PC doubles as the receiver (M4 decision: no second PC).
- [x] Discord, Zoom, and a browser for Meet installed on the receiver (Discord ✓, Zoom ✓, Chrome + Edge ✓).
- [x] Virtual mic driver items only: EV cert **still not ordered** (M3b tracking, confirmed at kickoff) → per the kickoff decision the interim VB-Cable route ships and the signed driver is deferred below.

## Checklist
### Virtual camera
- [x] `relay-vdevice::camera`: Windows 11 virtual camera via `MFCreateVirtualCamera` (session lifetime, current-user access, geometry passed as activation attributes) with a registered media source COM DLL (`relay_vdevice.dll`: `IMFActivate` + `IMFMediaSourceEx` + `IMFMediaStream2`) that reads NV12 frames from the shared ring the receiver writes; stream resolution at 60 fps, black frames until the receiver is up. In-process COM test proves activate→start→`RequestSample` serves ring frames **byte-for-byte**; a headless GPU test proves decoder-shaped NV12 texture → staging → ring byte-for-byte. Frame-server hosting itself: deferred live pass (below).
- [x] Fallback: OBS VirtualCam detection (filter CLSID + DLL path, verified live against this PC's real OBS install); UI explains the Win11 22H2+ requirement on older builds. *Feeding* OBS's queue: deferred (below).
- [x] Register/unregister only after opt-in; recorded in `%LOCALAPPDATA%\Relay\installed.json` **before** the registry is touched (record-then-apply); uninstall deletes exactly the recorded keys and empties the record. Live writes double-gated (`RELAY_VDEVICE_ALLOW_LIVE_WRITE=1` + elevation), same discipline as the APO.
- [ ] Verified in Discord, Zoom, Meet (browser) at 1080p60 and 4K30 → **Deferred** (below).

### Virtual microphone
- [x] Interim: VB-Cable / VoiceMeeter render-endpoint detection (`relay-vdevice::detect::mic_targets`) and routing of decoded audio to a named endpoint (`relay-share recv --mic-route`, WASAPI shared mode, nothing about the endpoint changed). The service picks the route (VB-Cable preferred) only when the mic opt-in is recorded. Live end-to-end: deferred (VB-Cable not installed on this PC).
- [ ] Signed audio-class virtual device → **Deferred** (below): EV cert + attestation still not ordered as of 2026-09-12.
- [ ] Sample-accurate audio/video alignment (clap test) → **Deferred**, part of the live runbook.

### Consent
- [x] First-run consent screen (UI gates on `installed.json` consent = undecided): both opt-ins (endpoint APO; virtual camera & microphone), the exact dry-run key listing served by the core (`Method::VdeviceDryRun` / `relay-core vdevice dry-run`), plain "what we install / how to remove" copy, and the note that the mic driver ships later. Recording consent installs nothing; each component keeps its own explicit install step. Settings card (`VdeviceConsentRow`) replaces the dead placeholder with live status + install/remove.

## Session log (2026-09-11 → 2026-09-12)
- New surface: `relay-vdevice` crate (frame ring, media source behind feature `com`, camera control, planner + gated livereg, installed.json model, read-only probes) · receiver `--vcam`/`--mic-route` (`RingWriter`/`VcamSink` in relay-capture) · core `vdevice` module + 5 IPC methods + `relay-core vdevice` CLI · first-run screen + Settings row in the UI.
- The service, not the client, decides receive routing from consent + registration — a request can't turn the camera on without the recorded opt-in.
- DLL-export collision (relay-apo and relay-vdevice both exporting `DllGetClassObject` into one binary) solved with uniquely named symbols aliased to the canonical names via `rustc-cdylib-link-arg` `/EXPORT:…` — cdylib only, feature `com` only.
- Gates: full workspace tests green (incl. the two new byte-for-byte pipeline tests), clippy/fmt clean, UI builds, footprint gate **4.21 MB RSS / 0.044 % CPU** with the vdevice engine in the core.
- Live registration attempted: UAC elevation was declined on this session's only prompt, and per-user (HKCU) registration was not attempted after the tool-permission denial — no registry key on this machine was written. `installed.json` was reset so the user's own first-run decision is still pending.

## Verification in Discord / Zoom / Meet — results
Not run this session (registration requires one elevated step; see Deferred).
The runbook records where results go; the table lives here once run:

| App | 1080p60 | 4K30 | within 5 s |
|---|---|---|---|
| Discord | — | — | — |
| Zoom | — | — | — |
| Meet (Chrome) | — | — | — |

## Definition of Done
- [x] Every checklist item checked or moved to Deferred with a reason.
- [ ] Fresh receiver PC: opt in, "Relay Camera" appears in a Discord call within 5 s of sharing → deferred with the live pass.
- [x] Opt out removes every registered component; `installed.json` is empty — enforced by design (uninstall deletes exactly the recorded keys, then empties `components`; unit-tested) and re-checked in the runbook.

## Deferred
1. **Live frame-server pass + Discord/Zoom/Meet at 1080p60 and 4K30 + the 5 s DoD check.** Reason: registration needs one elevated write to `HKLM\SOFTWARE\Classes\CLSID` and this session had no elevation (UAC prompt declined; no admin shell). Everything up to the frame server is proven by tests. Runbook: `docs/dev/vcam-live.md` (~20 min at a keyboard, includes the removal check).
2. **Signed virtual mic driver.** Reason: the EV certificate + Hardware Dev Center attestation are still not ordered (M3b tracking; kickoff decision 2026-09-11 was to ship the VB-Cable interim route and defer the driver on exactly this ground). The interim route installs nothing and is consent-gated.
3. **Interim mic live end-to-end + clap-test alignment.** Reason: VB-Cable is not installed on this PC and installing a third-party driver needs the same missing elevation. Steps are in the runbook.
4. **Feeding OBS VirtualCam.** Decision: detection + explanation only. Every targeted receiver runs Win11 22H2+ (the frame-server path); implementing OBS's shared-memory queue protocol is a second frame pipeline for machines the brief does not target. Revisit only if a real pre-22H2 receiver shows up.
5. **HKCU (per-user) registration experiment** — could remove the elevation requirement entirely; noted at the end of the runbook.
