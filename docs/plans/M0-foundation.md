# M0 — Foundation hardening

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M0-foundation.md, then work through the M0 checklist in order. Make the initial commit first. Check items off in the plan file as you complete them and update the status column in docs/ROADMAP.md when done.

## Goal
Make the scaffold safe to build on: version control, CI, a footprint gate that
enforces the ≤10 MB / ~0 % CPU budget, a hardened always-on core, and a UI
that can actually create and edit profiles.

## Depends on
Nothing. Unblocks every other milestone.

## Definition of Ready
- [ ] Workspace builds: `cargo build --workspace` and `pnpm build` in `ui/` succeed on this machine.
- [x] GitHub remote decided: private repo `jakesterns/relay`, already added as `origin` (2026-09-09). Push `main` after the initial commit.
- [ ] No open questions on the checklist below.

## Checklist
- [ ] Initial commit on `main` (everything currently untracked; `relay-handoff/` included as the design record).
- [ ] `.github/workflows/ci.yml` on `windows-latest`: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm install --frozen-lockfile && pnpm build` in `ui/`. Cache cargo and pnpm.
- [ ] `scripts/footprint.ps1`: release-build `relay-core`, run it headless for 30 s with a temp `--data-dir`, sample working set and CPU time via `Get-Process`, fail if RSS > 10 MB or CPU > 0.5 %. Wire into CI.
- [ ] Single-instance guard in `relay-core run`: named mutex `Local\RelayCore`; second instance prints "already running" and exits 0.
- [ ] Logging: `tracing-appender` (or hand-rolled) to `%LOCALAPPDATA%\Relay\logs\core.log`, 1 MB × 3 rotation, `--verbose` flag switches to debug. Keep stderr output when attached to a console.
- [ ] Autostart: `relay-core autostart on|off` writes/removes only `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Relay`. Off by default. Settings screen toggle.
- [ ] IPC hardening: create the pipe with a DACL granting access only to the current user SID (`SECURITY_ATTRIBUTES` via `ServerOptions::create_with_security_attributes_raw`); reject lines > 1 MB; per-connection request timeout.
- [ ] `Method::DeleteProfile` exposed as a Tauri command; UI gets New / Edit / Delete profile with a form (name, exe picker from running processes via a new `Method::ListProcesses`, headset/monitor free text until M1, share preset, status).
- [ ] Crash-restore harness as an integration test in `crates/core/tests/crash_restore.rs`: spawn `relay-core run` with a temp data dir and an env var that swaps in a file-backed recording backend, apply a profile over IPC, `taskkill /F`, restart, assert the recording shows restore ran and `original-state.json` has `applied=false`.
- [ ] `relay-core status` prints a human summary by default, `--json` for the current output.
- [ ] Release binary size and RSS recorded in this file under "Measurements".

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- CI green on a fresh clone.
- Footprint gate passes in release.
- Killing the core mid-apply and restarting restores state (test proves it).
- A profile can be created, edited, and deleted from the UI and survives a core restart.

## Out of scope
Any real audio/display backend, hardware detection, share engine.

## Measurements
_(fill in)_ release `relay-core.exe` size, idle RSS, idle CPU.

## Deferred
_(none yet)_
