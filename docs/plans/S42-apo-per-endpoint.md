# S42 — the endpoint APO, per output

Branch `feat/s42-apo-per-endpoint` (cut from `origin/fix/r34`, which carries S41).

## Why

EQ is per output device: headphone correction plus the game profile. Until
S42 the install, uninstall, status and elevated ops all targeted the
*default* render endpoint only, so a user with headphones on a DAC and
speakers on S/PDIF could carry the APO on one of them at most.

## What changed

**Core (`crates/core/src/audio_apo.rs`)**
- `install_live` / `uninstall_live(backup_dir, endpoint: Option<&str>)` — the
  named endpoint, or the default when `None`. `uninstall_all_live` restores
  every recorded endpoint (the uninstaller's path).
- Backups stay `apo-backup\<endpoint>.json`; `recorded_endpoints()` lists them
  (GUID stems only; `.restored` / `.tmp` ignored).
- Multi-endpoint safety: every backup records the COM keys, and the registry
  restore deletes them. `restore_backup_for(backup, others_installed)` keeps
  the COM keys while another endpoint still carries the APO, so removing one
  output never unregisters the class under another. The failed-install
  rollback uses the same rule.
- `apo_status(backup_dir)` lists every active render endpoint (plus orphaned
  backups for unplugged ones) with `installed / backed_up / running`. The old
  top-level fields still describe the default output.
- Runtime: parameter sections were already per endpoint
  (`Global\Relay.APO.<guid>`, `params_section()`). `apply` writes the active
  profile plus the active listening device's correction (S41,
  `audio_bridge::correction_curve`) to the default render endpoint — where the
  game's audio plays — and remembers it; `restore` bypasses that endpoint and
  the current default (the default can change mid-game). Bypass is still
  pass-through.

**Elevated helper (`elevate.rs`)** — `REQUEST_VERSION` 2. Still six ops;
`InstallApo { endpoint }` / `UninstallApo { endpoint }` carry only a GUID (no
path, no DLL). `Request::vet` refuses a non-GUID endpoint before anything
else; the install op then vets the GUID against a read-only MMDevice
enumeration (`vet_apo_target`: must be an *active render* endpoint — a
recording device or an inactive one is refused). Uninstall does not require
the output to be active (an unplugged device must stay restorable); its
backup is vetted instead (endpoint matches, COM keys inside our CLSID).
`UninstallApo { endpoint: None }` = every recorded endpoint.

**Uninstaller (`uninstall.rs`)** — one `RestoreApo` step per endpoint;
`finish_elevated` sends `UninstallApo { endpoint: None }`.

**IPC / CLI / UI** — `Method::InstallApo { endpoint }`,
`UninstallApo { endpoint }`; `ApoStatus.endpoints`; TS mirror in
`ui/src/lib/ipc.ts` (`installApoOp`, `uninstallApoOp`, `EndpointApo`).
`relay-core apo status|install|uninstall [--endpoint <guid>] [--all] [--json]`,
`relay-core elevate plan|run install-apo --endpoint <guid>`. Settings APO card
lists each output with its state and its own Install… / Remove…, each through
the same plan-then-UAC flow.

## Tests (all offline; nothing live ran in this session)
- `elevate`: bad GUID / recording device / inactive endpoint refused, op wire
  shape, v1 requests refused, non-GUID endpoint in a request refused.
- `audio_apo`: backup naming, recorded-endpoint listing, COM keys kept while
  another endpoint is installed, per-endpoint section names, status merge.
- `uninstall`: one restore step per endpoint. `ipc`: APO method/reply shapes.
- UI: per-output list, plan and run for the chosen output only, two outputs at
  once, remove one leaves the other, orphaned backup, offline.

## S42b — audio-engine registration (branch `feat/s42b-apo-registration`)

**Live finding (dev PC, 2026-09-30).** Installed on an unused S/PDIF output:
FxProperties were right (`{d04e05a6-…},7` and `,15` = Relay's CLSID, modes
`{d3993a3f-…},7` has MODE_DEFAULT) and the COM class existed under
`HKLM\SOFTWARE\Classes\CLSID\{5A8E9C3B-…}`. After restarting
AudioEndpointBuilder/Audiosrv and playing to that output the APO never loaded
(no shm section, no CodeIntegrity events). Cause: no key at
`HKLM\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{5A8E9C3B-…}` —
the `RegisterAPO` / `APO_REG_PROPERTIES` registration audiodg looks APOs up
in (22 others were there).

**Fix.**
- `fxstore::plan_install` adds that key to the machine-wide keys
  (`com_keys`, recorded in the backup) with `audio_engine_values()`: in
  `RegisterAPO`'s order and types (checked against the WM audio GFX APO's
  export on the dev PC) — FriendlyName, Copyright (REG_SZ), MajorVersion 1,
  MinorVersion 0, Flags 0x0e, Min/Max Input/Output Connections 1,
  MaxInstances 0xffffffff, NumAPOInterfaces 1 (REG_DWORD), APOInterface0 =
  `{FD7F2B29-…}` IID_IAudioProcessingObject (REG_SZ). Every number comes from
  `ids` and `com::GetRegistrationProperties` uses the same constants;
  `tests/apo_com.rs` asserts they agree.
- Flags 0x0e = `APO_FLAG_DEFAULT` (samples-per-frame, frames-per-second,
  bits-per-sample must match): negotiation accepts only f32 stereo both
  sides at the opposite side's rate, frames are 1:1. Not INPLACE.
- Same lifetime rule as the COM class: created on the first install, kept
  while another endpoint carries the APO, removed with the last (also in
  the failed-install rollback and uninstall-all). `restore_backup_for` adds
  the key to a last-endpoint restore even when the backup predates S42b.
  `livereg::restore` treats an already-absent key as done.
- `elevate::vet_apo_machine_keys`: only our CLSID subtree and exactly our
  audio-engine key; another APO's key, the root, a sub-key or `..` is
  refused. Applied to on-disk backups before uninstall and to the derived
  plan inside `install_live`.
- Dry run (`plan_lines`) lists the key; `relay-core apo status` prints
  `audio-engine registration: present / values differ / missing`
  (`ApoStatus.audio_engine`, TS mirror). S41b's reader still classifies
  Relay's CLSID as Relay.
- Fixture: `crates/audio/tests/fixtures/audioengine-apos-baseline.reg`
  (read-only export of the dev PC); install → uninstall reproduces it
  byte-for-byte.
- Also fixed: `tests/elevate_helper.rs` still sent pre-S42 string ops.

**Live retest (with the owner).**
1. `reg export HKLM\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects ae-before.reg`
   plus the Render export.
2. Uninstall the S42 install on the S/PDIF output (restores FxProperties and
   the COM class), rebuild, `elevate plan install-apo --endpoint <guid>` —
   the listing names the audio-engine key.
3. Install, then `relay-core apo status` → `audio-engine registration: present`.
4. `Restart-Service AudioEndpointBuilder -Force` (restarts Audiosrv), play
   speech/music to S/PDIF; `tasklist /m relay_apo.dll /fi "imagename eq audiodg.exe"`
   and `params section: reachable`.
5. Uninstall: `reg export` again, diff against `ae-before.reg` → empty.

## S42c — load diagnostics + init audit (branch `feat/s42c-apo-diag`)

**Live symptom (2026-09-30, Win11 26200).** Registration correct (COM class,
audio-engine key Flags 0x0e / APOInterface0 = IAudioProcessingObject,
FxProperties ,7 + ,15 + MODE_DEFAULT), Audiosrv restarted, audio played to
the S/PDIF endpoint, yet `Global\Relay.APO.<endpoint>` never appeared and
CodeIntegrity was silent.

**Top suspect, fixed.** We implemented `IAudioSystemEffects` but not
`IAudioSystemEffects2`. The engine picks the Initialize payload from the
interfaces an APO exposes: no `IAudioSystemEffects2` -> `APOInitSystemEffects`
(v1). `Initialize` only read the endpoint store when `cbDataSize >=
sizeof(APOInitSystemEffects2)`, so on v1 it quietly became a wire and never
created the section — exactly the symptom. Mode-aware EFX registrations
(`{d3993a3f…},7`) also expect `IAudioSystemEffects2`.
- `Initialize` now classifies v1/v2/v3 by size (`com::init_kind`) and reads
  `pAPOEndpointProperties`, which sits at the same offset in all three.
- `IAudioSystemEffects2` implemented (`GetEffectsList` -> empty list). Not
  `IAudioSystemEffects3` (it would change the payload to v3; handled anyway).

**Audit, no change needed.**
- QI: IUnknown, IAudioProcessingObject{,RT,Configuration},
  IAudioSystemEffects{,2} all answer (`tests/apo_init.rs`); v3 says no.
- `GetRegistrationProperties`: clsid, Flags 0x0e, 1 interface
  (IAudioProcessingObject), 1/1 connections — equals the registry values
  (asserted). The registry interface list is the *APO* interface list
  (`RegisterAPO` writes IAudioProcessingObject only); IAudioSystemEffects is a
  QI marker, not listed there. Confirm against Equalizer APO on the dev PC:
  `reg query HKLM\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects /s` and
  compare its key (count, interface, flags) with ours.
- Format: float32 stereo, rate equal to the opposite side, S_OK with the same
  type. Non-float or non-stereo is refused with APOERR_FORMAT_NOT_SUPPORTED
  (no S_FALSE proposal). If the diag log shows the S/PDIF mix format is not
  float32 stereo, that refusal is the next suspect.
- Class factory refuses aggregation (CLASS_E_NOAGGREGATION, correct for APOs);
  ThreadingModel Both in the COM registration.
- Section: created by LOCAL SERVICE (holds SeCreateGlobalPrivilege) with DACL
  SY/LS/IU full — the interactive core can open it.

**Diag switch.** Create `%ProgramData%\Relay\apo-diag.on` (any content;
`RELAY_APO_DIAG` env for tests). Checked on each line, so no audiodg restart
is needed to toggle; delete it to go silent. Lines go to the first writable of
`%ProgramData%\Relay\apo-diag.log`,
`C:\Windows\ServiceProfiles\LocalService\AppData\Local\Temp\relay-apo-diag.log`
(audiodg's %TEMP%), `%SystemRoot%\Temp\relay-apo-diag.log`; 256 KB cap each.
If an admin created `C:\ProgramData\Relay`, LOCAL SERVICE may not be able to
write there — check the fallbacks. Logged: DllMain attach, DllGetClassObject,
CreateInstance (+ QI probe of every interface), Initialize (size, kind,
endpoint, discovery), GetRegistrationProperties, GetEffectsList, format
negotiation (both formats + answer), LockForProcess, shm create (name +
HRESULT). Never APOProcess.

**Next live step (with the owner).** Rebuild + reinstall the DLL on the S/PDIF
endpoint, create `C:\ProgramData\Relay\apo-diag.on`, restart
AudioEndpointBuilder, play speech/music to S/PDIF, then read the log:
- no file anywhere -> never loaded (compare the Equalizer APO registration);
- DllMain but no CreateInstance -> class/registry mismatch;
- Initialize with an endpoint but shm error -> section/privilege;
- format refusals -> negotiation;
- `shm create … S_OK` -> `relay-core apo status` should read `reachable`.

## Live test plan (VM or the owner's machine, with the owner — NOT unattended)
## S42d — MFX placement, after Equalizer APO (branch `feat/s42d-apo-mfx`)

**Live result that started it (2026-09-30, Win11 26200, unused Realtek USB
S/PDIF render endpoint).** Fully registered (COM class; AudioEngine key Flags
0x0e; FxProperties `{d04e05a6…},7` and `,15` = our CLSID; `{d3993a3f…},7` =
MODE_DEFAULT), audio services restarted, a WAV played to that endpoint: the
diag log (written from DllMain) never appeared — audiodg never loaded the DLL.
That endpoint already had `{d3993a3f…},5` and `,6` = MODE_DEFAULT (SFX/MFX
modes), no SFX/MFX CLSIDs, and `{d04e05a6…},0` = nil GUID.

**What Equalizer APO does** (SourceForge `equalizerapo/code`, commit
`bbfcc3e`, 2025-11-28):
- Value names, `DeviceAPOInfo.cpp:54-61`: LFX `,1`, GFX `,2`, SFX `,5`, MFX
  `,6`, EFX `,7`, composite `,13/,14/,15`; modes `{d3993a3f…},5/6/7`
  (`:74-76`); `PKEY_AudioEndpoint_Disable_SysFx` `{1da5d803…},5` (`:78`). No
  `{fc52a749…}` value. It only ever **writes** the legacy single slots
  (`,1/,2/,5/,6/,7`), never the composite lists.
- Default mode (`:398-413`): LFX/GFX only on pre-8.1 style stores; otherwise
  **SFX + MFX** when the endpoint has `{b3f8fa53…},41` (combined device),
  else **SFX + EFX**. So EAPO itself runs post-mix as EFX on many endpoints —
  the EFX slot is not inherently unloadable.
- Install (`:498-645`): copies the original CLSIDs of all five legacy slots to
  `HKLM\SOFTWARE\EqualizerAPO\Child APOs\{device}` (`:538-547`, also a `.reg`
  backup file), **overwrites** the SFX/MFX (or SFX/EFX) slot with its own
  CLSID (`:599-614`), deletes the other legacy slots, writes the modes value
  only if absent (`:603-605`, `:611-613`), and deletes Disable_SysFx
  (`:643-644`). The vendor APO is kept by wrapping: EAPO's APO instantiates
  the recorded "PreMixChild"/"PostMixChild" CLSID and forwards to it
  (`:436-496`, `EqualizerAPO/EqualizerAPO.cpp:228-280`). Uninstall writes the
  recorded originals back (`:647-683`).
- Registration (`EqualizerAPO/EqualizerAPO.cpp:35-40`, `DllMain.cpp:71-78`):
  two CLSIDs (pre-mix, post-mix), interface list = IAudioProcessingObject
  only, Flags = FPS_MUST_MATCH | BPS_MUST_MATCH | INPLACE (0x0d). QI also
  answers IAudioSystemEffects (`EqualizerAPO.cpp:529`). Ours (0x0e, same
  interface list, IAudioSystemEffects{,2}) is equivalent for loading.
- **Machine-wide switch**: `Setup/Setup.nsi:279` writes
  `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\DisableProtectedAudioDG
  = 1`, and DeviceSelector re-applies it on every run
  (`DeviceSelector.cpp:101`, `DeviceAPOInfo.cpp:141-153`). This is what lets
  an unsigned/test-signed APO load in audiodg, and it is the most likely
  reason ours never loaded. Relay does **not** write it: it is global config
  (non-negotiable), and it is Jake's call. Live check below.

**Relay's decision: MFX.** Per-game EQ applies to everything on the
endpoint, so post-mix. MFX vs EFX: both post-mix; MFX runs once per
processing mode (we offer DEFAULT only, so one instance in practice) and is
where EAPO goes on combined-device endpoints. SFX was rejected: it runs per
stream (one instance per app), costs N DSP passes, and can see non-float
formats; our parameters are per endpoint anyway. MFX input is the engine mix
format (float32), so the existing float32-stereo negotiation stands; a
multichannel mix is refused (the engine then drops the APO — audio still
plays, unprocessed).

**Install plan (`fxstore::plan_install`, now `Result<_, PlanError>`):**
- composite MFX `,14` present → append our CLSID (chain, vendor kept);
- else legacy MFX `,6` absent / empty / nil GUID → write our CLSID;
- else **refuse `PlanError::SlotTaken(clsid)`** — Relay never wraps or
  evicts a vendor effect the way EAPO does. Dry-run and install show
  "Slot taken: … Relay will not replace it … Nothing on your PC was changed"
  (`audio_apo::slot_taken_line`). The RODECaster baseline is such a store
  (MFX = `{13AB3EBD…}`).
- `{d3993a3f…},6` gets MODE_DEFAULT (append if missing);
- Disable_SysFx deleted when non-zero (as EAPO), restored by uninstall.
- No EFX values are written any more; `fx_has_relay` still reads them, so a
  pre-S42d install is recognised and uninstalls from its backup as before.
- Vetting: `ids::FX_WRITABLE_VALUES` (the four names above);
  `fxstore::vet_fx_diff` refuses any plan touching another value or a
  sub-key, and `livereg::apply_install` checks it before the first write.
- Backup/restore unchanged (full store, byte-identical — fixtures), first/last
  endpoint rules unchanged. Tests: `tests/fxstore_fixtures.rs` (RODECaster
  slot taken, RODECaster with MFX freed byte-for-byte, S/PDIF-shaped store
  with a vendor SFX, nil-GUID slot, composite chain, Disable_SysFx, vetting),
  `tests/apo_com.rs` (IAudioSystemEffects{,2} QI).

**Live retest (with Jake; not run in this session).**
1. Read-only first: `reg query "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio" /v DisableProtectedAudioDG`
   and `reg query HKLM\SOFTWARE\Classes\AudioEngine\AudioProcessingObjects /s`
   (an EAPO install, if any, shows what loads on this PC).
2. Uninstall the S42c install (restores the EFX values from its backup),
   rebuild, install again: the dry-run must list only `{d04e05a6…},6`
   (and `,6` modes if missing) on the S/PDIF endpoint.
3. `apo-diag.on`, restart AudioEndpointBuilder, play speech/music to S/PDIF,
   read the log. Still no DllMain line → the remaining difference from EAPO
   is `DisableProtectedAudioDG`; decide with Jake whether to test it by hand
   (a global switch Relay would then have to own, back up and restore).

## Live test plan (VM or Jake's machine, with Jake — NOT unattended)

Pre-reqs: test-signed build per `docs/dev/apo-testsign.md`; elevated flow per
`docs/dev/elevation-live.md`. Export `HKLM\...\MMDevices\Audio\Render` first
(`reg export`) for the before/after diff.

1. **Unused output first.** Pick an output nothing is playing on (S/PDIF /
   digital output, or a monitor's HDMI audio). `relay-core apo status` → note
   its GUID. `relay-core elevate plan install-apo --endpoint <guid>` — check
   the listing names only that GUID.
2. Install through Settings → that output's Install… → accept UAC. Check
   `apo-backup\<guid>.json` exists and only that endpoint's FxProperties
   changed (diff the export).
3. **Load in audiodg.** Play speech/music to that output (the owner's rule: never a
   tone for listening). `tasklist /m relay_apo.dll /fi "imagename eq audiodg.exe"`
   shows the DLL; `relay-core apo status` shows `params section: reachable`.
4. Refusals: `elevate run install-apo --endpoint {00000000-0000-0000-0000-000000000000}`
   → REFUSED (not an active output); a capture endpoint GUID → REFUSED
   (recording device). Nothing written (diff).
5. Second endpoint: install on the headphones. Both rows read Installed.
   Focus a game with a profile: only the default output's section is
   un-bypassed; blur → bypassed.
6. Remove the S/PDIF one: its FxProperties are byte-identical to the export,
   and the COM class `HKLM\SOFTWARE\Classes\CLSID\{5A8E9C3B-…}` is **still
   present** (headphones still carry it); headphone EQ still works.
7. **Restore.** Run the uninstaller: every recorded endpoint restored, COM
   keys gone, `apo-backup\*.json` renamed `.restored`, export diff empty.
8. Crash path: install, `taskkill /F /PID <core pid>` (own PID only) while a
   profile is active; restart core → chain bypassed.
