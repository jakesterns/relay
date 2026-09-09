# M7 — Installer and uninstaller

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M7-installer.md. Work on branch `m7-installer`. The uninstaller is the product's promise: test it from a clean VM snapshot after every change. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Depends on
M0 (autostart), M3b and M5 (components that need removal). Can start earlier for the core + UI only.

## Definition of Ready
- [ ] M0 complete; for the full uninstaller, M3b and M5 complete.
- [ ] Hyper-V (or other) clean Windows VM with a checkpoint available for the diff test.
- [ ] EV cert available for the signed-installer item.

## Checklist
- [ ] NSIS per-user installer (Tauri bundler, `installMode: currentUser`) shipping `relay-core.exe`, `relay-ui.exe`, `relay-share.exe`; Start Menu entry; optional autostart checkbox mapped to `relay-core autostart on`.
- [ ] Core started by the installer; UI opens to the first-run consent screen.
- [ ] Uninstaller order: stop UI → `relay-core shutdown` (restores state) → APO uninstall → virtual device unregister / driver removal → Run key → files; asks whether to keep `%LOCALAPPDATA%\Relay` (profiles, hardware library).
- [ ] `relay-core uninstall --dry-run` prints everything the uninstaller will touch, used by the Settings "what we installed" card.
- [ ] Upgrade path: installer over an existing install preserves data and re-registers components.
- [ ] Clean-VM test script (Hyper-V checkpoint): install → opt in to both components → share → uninstall → registry and file diff against the checkpoint is empty except the optional data folder.
- [ ] Signed installer and binaries with the EV cert.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- Clean-VM diff after uninstall is empty.
- Install → uninstall never leaves the endpoint APO chain modified.

## Deferred
_(none yet)_
