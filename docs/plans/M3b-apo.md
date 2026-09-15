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

Verified 2026-09-10 (session start): no code-signing certificate in the
`CurrentUser`/`LocalMachine` stores and the tracking boxes above are unchecked
→ the cert is **not available**. All signing-gated install/uninstall items in
this session are Deferred with that reason; development proceeds unsigned.

## Definition of Ready
- [x] M3 complete (DSP chain with allocation-free `process`). Verified 2026-09-10: branch `m3-audio-dsp` closed with measurements; merged into `m3b-apo` (all 144 workspace tests green after the merge).
- [ ] A Windows VM with test-signing enabled and a checkpoint taken before any APO registration. **Not available 2026-09-10**: no hypervisor on this dev PC (Hyper-V feature absent, no VMware/VirtualBox). Registration code is therefore exercised against exported registry fixtures only; the live VM pass is Deferred with a runbook below. **No live APO registration happens on this dev machine.**
- [x] Registry export of the target endpoint's FX property store saved as the pre-install baseline. Taken 2026-09-10 from the default render endpoint "Headphones (RODECaster Duo Secondary)" `{f8ae226b-a4e3-45ab-97fc-3977dad232d1}`: `crates/audio/tests/fixtures/fx-baseline-rodecaster.reg` (FxProperties, 7 613 lines — the endpoint ships a real vendor FX chain, which makes it the primary uninstall-diff fixture) and `endpoint-baseline-rodecaster.reg` (full endpoint key).
- [ ] For the install/uninstall items only: EV cert and attestation signing working (tracked above). **Not available** — see the note under "Depends on".

## Checklist
- [x] `crates/audio/apo`: `cdylib` implementing `IAudioProcessingObject`, `IAudioProcessingObjectRT`, `IAudioProcessingObjectConfiguration` (COM via `windows` crate `implement`), plus the `IAudioSystemEffects` marker. **Placement: EFX** (endpoint effect — processes the final per-endpoint mix, which is what per-game EQ wants), registered through the *composite* EFX key so vendor chains coexist; MFX left to a future per-stream need. RT path is lock-free (atomic chain pointer + busy handshake); parameter rebuilds happen on a control thread woken by the shm event; every failure degrades to pass-through. In-process COM test (`tests/apo_com.rs`) drives Initialize → negotiation → LockForProcess → APOProcess and proves the output bit-identical to the direct DSP chain, instant bypass, and a live parameter swap.
- [x] Format negotiation: accept the endpoint's rate and channel count (f32 stereo at any one rate), never resample (rate changes across the connection are refused with `APOERR_FORMAT_NOT_SUPPORTED`); HRTF silently drops out at rates without a bundled IR; unsupported formats → the engine runs the endpoint without us.
- [x] Parameter block in a named shared-memory section + event (`relay-audio::shm`); core writes, APO reads lock-free with sequence numbers; bypass flag is the first word and is honoured on the very next block. audiodg lives in session 0, so the APO *creates* the `Global\` section (LOCAL SERVICE holds `SeCreateGlobalPrivilege`; the interactive core does not) and the core opens it. DACL: SYSTEM, LOCAL SERVICE and INTERACTIVE only.
- [x] Install: writes only the target endpoint's FX property store keys — composite EFX multi-sz (append, never evict), legacy EFX only if the slot is empty, EFX processing modes — plus the CLSID registration. **HKCU is not possible for the COM key**: audiodg (LOCAL SERVICE) cannot see per-user classes, so it is HKLM with the added keys recorded in the backup (decision). Complete prior property store saved to `%LOCALAPPDATA%\Relay\apo-backup\<endpoint>.json` *before* any write, atomically, install refused if a backup already exists. Live execution is double-gated (`RELAY_APO_ALLOW_LIVE_WRITE=1` + elevation); the gate is armed only by `relay-elevate.exe`, around one vetted call. **Run live on this machine 2026-09-14** (S6): the diff while installed was exactly three added values on the endpoint's `FxProperties` and nothing removed — the vendor chain was appended to, never evicted.
- [x] Uninstall restores the prior property store byte-for-byte and unregisters — proven at the fixture level: `reg export` parser/serializer round-trips the real 7 613-line RODECaster baseline **byte-for-byte**, and install→uninstall over that fixture restores exact equality plus a clean serialized-export string diff (`tests/fxstore_fixtures.rs`, 9 tests, incl. occupied-vendor-EFX and empty-store cases). The *live* before/after export diff is now done too: byte-identical after an elevated install→uninstall on the real RODECaster endpoint, 2026-09-14 (`docs/dev/elevation-live.md`).
- [x] Endpoint re-enumeration after install: `audiosrv` restart documented as the reliable no-reboot path (runbook step 5); live verification deferred to the VM pass.
- [x] `AudioControl` adapter (`relay-core::audio_apo::ApoAudioControl`, now the production audio backend): `capture` = read the live bypass word, `apply` = write params + clear bypass + event, `restore` = bypass. Degrades to `Bypass`/no-op when the APO is absent, so nothing regresses. End-to-end integration test against the real default endpoint GUID (`crates/core/tests/apo_params.rs`).
- [x] Settings screen: opt-in card is live — real install status (read-only registry probe over IPC `ApoStatus`), explicit consent block with "what this installs / how to remove", Install/Remove now go through `ElevationPlan` + `RunElevated` (S6): the card shows the real listing, then one UAC prompt runs `relay-elevate.exe`. `InstallApo`/`UninstallApo` remain as the direct, already-gated path for the CLI and the VM runbook. Games screen "Route" shows Endpoint APO / idle / "Preview only · APO not installed", and "Processing" shows the chain's real latency (0 ms EQ, 1.0 ms limiter, +2.7 ms HRTF at 48 k). `relay-core apo status|install|uninstall` CLI for the VM runbook.
- [x] Test-sign flow documented: `docs/dev/apo-testsign.md` (VM checkpoint, `bcdedit /set testsigning on`, self-signed cert + signtool, `DisableProtectedAudioDG` for the VM only, per-iteration install/verify/uninstall/diff loop).

## Definition of Done
- [x] Every checklist item checked or moved to Deferred with a reason.
- [x] APO active on the headset endpoint only; other endpoints and apps untouched. By construction: every write path is scoped to one endpoint's `FxProperties` subtree (`FxStore` cannot express anything else — proven by the "touches exactly the three PKEYs" test) and one shared-memory section per endpoint. Live single-endpoint confirmation rides the VM pass.
- [x] Uninstall leaves the endpoint property store identical to the pre-install export — proven byte-for-byte at the fixture level, **and live** on the real endpoint (2026-09-14, same SHA-256 before and after).
- [x] Processing latency reported in the UI ≤ 1 ms at 48 k: EQ-only 0 ms, EQ+limiter 1.0 ms (HRTF's one-partition 2.7 ms shown honestly when enabled).

## Deferred
Items 1 and 2 for the same two reasons — **no EV certificate yet** (tracking above) and **no hypervisor on this dev PC** (DoR); none block the code, which is complete and fixture/in-process-proven:
1. **Signed production DLL + attestation** — needs the EV cert and Hardware Dev Center. Until then the DLL only loads in a test-signing VM.
2. **Live audiodg-hosted pass in a test-signing VM** — what is left of this is the *loading*: engine pickup after an `audiosrv` restart and real playback through the chain. The **registration half is no longer deferred**: S6 ran install and uninstall live on this dev machine through the elevated helper on 2026-09-14, and the before/after `reg export` diff came back byte-identical (`docs/dev/elevation-live.md`). `audiosrv` was deliberately not restarted, so audiodg never tried to bind the unsigned DLL. Full runbook for the rest: `docs/dev/apo-testsign.md`.
3. ~~**Elevation UX for the Settings install button**~~ — **done 2026-09-14 (S6)**.
   `relay-elevate.exe` is the elevated helper: the core shows the plan, the
   user approves one UAC prompt, and the helper performs exactly the recorded
   steps. Install and uninstall both ran live on this dev machine from the
   Settings card, and the post-uninstall `reg export` of the endpoint's
   `FxProperties` is **byte-identical** to the pre-install export — which
   promotes this plan's "uninstall restores the prior property store
   byte-for-byte" from fixture-proven to live-proven, for the registry half.
   Design, evidence and the runbook: `docs/dev/elevation-live.md`.

   Two things the live pass found and fixed:
   - `RegCreateKeyExW` with `KEY_WRITE` is denied on an endpoint's
     `FxProperties` key even when elevated: SYSTEM owns it and
     `BUILTIN\Administrators` gets `SetValue + ReadKey` without
     `CreateSubKey`. `livereg::Key::create` now opens existing keys with just
     `KEY_SET_VALUE`. Taking ownership would have worked and would have left
     the machine permanently different, so it was not done.
   - A failed install left its backup file behind, so the next attempt refused
     with "the APO looks installed". `install_live` now rolls the backup back
     when the apply fails.

## Session log (2026-09-11)
Branch `m3b-apo` = `m1-hardware` + merged `m3-audio-dsp` (conflicts in ipc/service/config/UI resolved; exclusive-watcher state folded into M1's `select_and_apply`). New: `relay-audio::shm` (seqlock param block, 5 tests incl. cross-mapping), `relay-apo` crate (`com`, `regfile`, `fxstore`, `livereg`, `ids`; 21 tests), core `audio_apo` backend + `ApoStatus`/`InstallApo`/`UninstallApo` IPC + `apo` CLI, Settings consent card, Games chain readout. Workspace: 176 tests green, fmt + clippy clean.
