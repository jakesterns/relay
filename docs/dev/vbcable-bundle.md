# Bundled VB-CABLE (the virtual microphone)

Status: **plumbing done, payload pending VB-Audio's written permission.**
No build carries any VB-Audio file. `crates/core/src/vbcable.rs::MANIFEST` is
empty, and an empty manifest means "not bundled" whatever is on disk, so
nothing unpinned can ever run.

## Why

Owner, 2026-10-12: everything a user needs ships in the one installer. The
receiving PC needs a virtual microphone so call apps can pick up the shared
sound, and VB-CABLE is the only option that works without an EV certificate
(Relay will never have one). Permission request drafted 2026-10-21 (sent by
the owner through VB-Audio's contact form).

## How it works

| Piece | Where |
|---|---|
| Payload location | `<install dir>\drivers\vbcable\` (VBCABLE_Setup_x64.exe + its driver files, unmodified) |
| Pinned files | `vbcable::MANIFEST` (file name + SHA-256) |
| Opt-in | `Consent::vbcable` in `installed.json`; first run and Settings → "What Relay installs" → "Install the virtual microphone (VB-CABLE by VB-Audio)". Off by default. Credit + https://vb-cable.com on both. |
| Install / uninstall | elevated helper ops `install_vbcable` / `uninstall_vbcable` (request version 4). Requests carry no path or argument. |
| Record | `installed.json` component `vbcable-driver`, written before the setup runs, dropped if no CABLE device appears |
| Uninstaller | `StepKind::RemoveVbcable`, present only when Relay's record exists |
| Receive routing | `vdevice::pick_mic_route`: VB-CABLE first, VoiceMeeter otherwise |

The helper reads every pinned file into memory, checks its SHA-256, writes
those bytes to `%ProgramFiles%\Relay\drivers\vbcable` (administrators-only),
and runs the setup from there. The pack beside Relay is in the user's
`%LOCALAPPDATA%`; running it in place would let anything running as the user
swap it between the check and the elevated launch. The launch is also behind
`RELAY_VBCABLE_ALLOW_LIVE_RUN`, armed only around the one call.

An existing VB-CABLE is used, not reinstalled, and gets no record, so the
uninstaller never removes a VB-CABLE the user installed themselves.

## To do when permission arrives

1. Quote VB-Audio's terms in `THIRD_PARTY_NOTICES.md` §5 and drop "PENDING".
2. Add the unmodified pack to the bundle step (`scripts/stage-bundle.ps1`)
   under `drivers\vbcable\`, fetched and hash-checked like the NDI runtime
   (no VB-Audio file in the repository).
3. Pin every file of the pack in `vbcable::MANIFEST`; update the
   `this_build_bundles_nothing_until_permission` test accordingly.
4. **First live run on PC2** (never the main PC): verify the silent command
   lines `INSTALL_ARGS` = `-i -h` and `UNINSTALL_ARGS` = `-u -h` (VB-Audio's
   documented switches, not yet run by Relay), the exit codes, whether a
   reboot is requested, and that "CABLE Input" appears and goes. Fix the
   constants if they differ.
5. Fresh install/uninstall check on PC2 with the opt-in on and off, and with
   a pre-existing VB-CABLE (must be left alone).

## Tests

- `crates/core/src/vbcable.rs`: manifest/hash verification (good, absent,
  incomplete, altered, unpinned), name vetting, the install decision table,
  install/uninstall flows against a fake machine (staged copy is what runs,
  record-then-apply, rollback on failure, the user's own VB-CABLE untouched).
- `crates/core/src/elevate.rs`: the ops round-trip and cannot carry a path.
- `crates/core/src/uninstall.rs`: the step is present only with Relay's record.
- `crates/core/src/vdevice.rs`: routing prefers VB-CABLE; first run keeps the opt-in.
- `ui/src/screens/Settings.vbcable.test.tsx`: the row, the prompt order, a
  declined prompt, removal, first run.
