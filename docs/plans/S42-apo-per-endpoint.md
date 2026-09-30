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
3. **Load in audiodg.** Play speech/music to that output (Jake's rule: never a
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
