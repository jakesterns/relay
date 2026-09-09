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
- [x] Workspace builds: `cargo build --workspace` and `pnpm build` in `ui/` succeed on this machine.
- [x] GitHub remote decided: private repo `jakesterns/relay`, already added as `origin` (2026-09-09). Push `main` after the initial commit.
- [x] No open questions on the checklist below.

## Checklist
- [x] Initial commit on `main` (everything currently untracked; `relay-handoff/` included as the design record).
- [x] `.github/workflows/ci.yml` on `windows-latest`: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm install --frozen-lockfile && pnpm build` in `ui/`. Cache cargo and pnpm.
- [x] `scripts/footprint.ps1`: release-build `relay-core`, run it headless for 30 s with a temp `--data-dir`, sample working set and CPU time via `Get-Process`, fail if RSS > 10 MB or CPU > 0.5 %. Wire into CI.
- [x] Single-instance guard in `relay-core run`: named mutex `Local\RelayCore`; second instance prints "already running" and exits 0.
- [x] Logging: `tracing-appender` (or hand-rolled) to `%LOCALAPPDATA%\Relay\logs\core.log`, 1 MB × 3 rotation, `--verbose` flag switches to debug. Keep stderr output when attached to a console.
- [x] Autostart: `relay-core autostart on|off` writes/removes only `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Relay`. Off by default. Settings screen toggle.
- [x] IPC hardening: create the pipe with a DACL granting access only to the current user SID (`SECURITY_ATTRIBUTES` via `ServerOptions::create_with_security_attributes_raw`); reject lines > 1 MB; per-connection request timeout.
- [x] `Method::DeleteProfile` exposed as a Tauri command; UI gets New / Edit / Delete profile with a form (name, exe picker from running processes via a new `Method::ListProcesses`, headset/monitor free text until M1, share preset, status).
- [x] Crash-restore harness as an integration test in `crates/core/tests/crash_restore.rs`: spawn `relay-core run` with a temp data dir and an env var that swaps in a file-backed recording backend, apply a profile over IPC, `taskkill /F`, restart, assert the recording shows restore ran and `original-state.json` has `applied=false`.
- [x] `relay-core status` prints a human summary by default, `--json` for the current output.
- [x] Release binary size and RSS recorded in this file under "Measurements".

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason.
- CI green on a fresh clone.
- Footprint gate passes in release.
- Killing the core mid-apply and restarting restores state (test proves it).
- A profile can be created, edited, and deleted from the UI and survives a core restart.

## Out of scope
Any real audio/display backend, hardware detection, share engine.

## Measurements
Measured 2026-09-09 with `scripts/footprint.ps1` (release profile: `opt-level = "s"`, fat LTO, `panic = "abort"`, stripped) on the dev machine, Windows 11 Pro 26200, 30 s idle after a 3 s warm-up:

| Metric | Value | Budget |
|---|---|---|
| `relay-core.exe` size | 0.69 MB (727,040 bytes) | - |
| Idle working set (peak / end) | 9.51 MB / 9.50 MB | <= 10 MB |
| Idle private working set (Task Manager "Memory" column) | 1.1 MB | - |
| Idle CPU (share of one core) | 0.000 % | <= 0.5 % |

Notes:
- Total working set is dominated by shared pages of system DLLs (ntdll, kernel32, user32, advapi32, ...); the process itself holds about 1.1 MB private. The gate is on total working set as the plan specified, so the margin to the 10 MB line is only about 0.5 MB and depends on the Windows build. If CI on `windows-latest` trips it, compare the private figure before touching code.
- Dropping the `env-filter` feature of `tracing-subscriber` (a regex engine) took the binary from 1.03 MB to 0.69 MB and the working set from 11.0 MB to 9.5 MB. Log level is now `--verbose` or `RELAY_LOG=trace|debug|info|warn|error|off`.
- Before that change the gate correctly **failed** at 11.0 MB, which is the first proof the gate bites.

## Deferred
- **GitHub remote and "CI green on a fresh clone".** No remote existed when M0 ran, so `.github/workflows/ci.yml` is written and every step it runs (`fmt --check`, `clippy -D warnings`, `test --workspace`, `pnpm install --frozen-lockfile && pnpm build`, `scripts/footprint.ps1`) was executed locally and passes, but the workflow itself has not run on GitHub. The first push to a private `main` will verify it; watch the footprint margin noted above.
- **Console window flash on autostart.** `relay-core` is a console-subsystem binary so the CLI (`status`, `autostart`, ...) behaves like a normal tool. When Explorer or the Run key launches it, Windows allocates a console that `run` hides immediately, but with Windows Terminal as the default host that can still flash for a frame. The proper fix is a tiny GUI-subsystem launcher or `conhost --headless` in the Run value; it belongs with the installer work in M7.
- **Driving the Tauri UI automatically.** New / Edit / Delete are implemented and type-checked, the window renders, and the create -> kill -> restart -> still-listed path is proven over IPC by `crash_restore.rs`, but no automated test clicks through the WebView. Manual UI testing stays with each milestone smoke check.

## Verification log (2026-09-09)
- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: 27 unit tests + 2 integration tests pass (`crash_restore.rs`: hard kill mid-apply restores on restart and clears `original-state.json`; a second instance exits 0 with "already running").
- `pnpm build` in `ui/`: passes (`tsc --noEmit` + Vite).
- `scripts/footprint.ps1`: PASS with the numbers above.
- CLI against a live release core: `status` summary, `status --json`, `autostart` -> `on` (Run value written with the quoted exe path) -> `off` (value removed), a second `run` prints "relay-core is already running", `shutdown` restores and exits, `logs/core.log` written.
