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
- ~~**Console window flash on autostart.**~~ **Closed 2026-09-12 in M7** by `relay-svc.exe` (`crates/core/src/bin/relay-svc.rs`), a GUI-subsystem launcher that starts the core with `CREATE_NO_WINDOW` and exits; the Run value points at it. Original reasoning: `relay-core` is a console-subsystem binary so the CLI (`status`, `autostart`, ...) behaves like a normal tool. When Explorer or the Run key launches it, Windows allocates a console that `run` hides immediately, but with Windows Terminal as the default host that can still flash for a frame. The proper fix is a tiny GUI-subsystem launcher or `conhost --headless` in the Run value; it belongs with the installer work in M7.
- ~~**Driving the Tauri UI automatically.**~~ **Closed 2026-09-14** by session S8 (`feat/ui-test-harness`). See "UI test harness" below.

## UI test harness (added 2026-09-14, session S8)
`pnpm test` in `ui/` — Vitest + jsdom + Testing Library, 158 tests across 9
files, ~12 s. Wired into CI as its own step next to `pnpm build`. Full notes in
`ui/src/test/README.md`.

**Component tests, not WebDriver.** The standing rule is that no test may move
the real mouse or send synthetic keystrokes to the desktop — an earlier attempt
drove a real Tauri window and its clicks landed in the user's browser. Testing
Library dispatches DOM events inside a jsdom document in-process, so there is
no host cursor to move and no window to steal focus. `ui/src/test/safety.test.ts`
enforces this mechanically: it fails if a WebDriver or input-synthesis package
(`tauri-driver`, `playwright`, `robotjs`, …) appears in `package.json`, or if
the Tauri module aliases stop pointing at the in-process fake.

What this does not cover, and where that coverage lives instead: the Tauri
shell booting and `ui/dist` being embedded (`cargo build -p relay-ui`), the IPC
wire shapes (Rust tests on both sides of `ipc.rs`), and a real core over the
real pipe (`crash_restore.rs`). What had no coverage at all was the screens.

**How it is wired.** `ui/src/lib/ipc.ts` reaches the desktop in exactly three
places — dynamic imports of `@tauri-apps/api/{core,event,window}`.
`vitest.config.ts` aliases all three to `ui/src/test/tauriMock.ts`, which gives
each test one of three situations: browser mock data (`ipc.ts` serves its own
fixtures), an offline core (every `invoke` rejects), or `makeFakeCore()` — a
scripted core implementing every command in `ipc.ts` over mutable state, so a
test can click Save and then assert the core holds the new value.

**Coverage.** Every screen renders under all three modes (`screens.smoke`),
plus the regression suites this session was asked for: share preset start/stop
with its arguments, catalogue search and import, uninstall plan rendering, and
the consent flow. Also the shell (first-run gate, rail, title bar), the Games
audio/display serialisation, and Receive.

One real bug fell out on the first run: the Share preset editor's **encode size
field could not be typed into**. Its value was derived from `draft.size`, which
stays `undefined` until the whole `WxH` string parses, so React reset the box
on every keystroke. Fixed by holding the typed text in its own state
(`crates`-side unaffected; `ui/src/screens/Share.tsx`).

## Verification log (2026-09-09)
- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: 27 unit tests + 2 integration tests pass (`crash_restore.rs`: hard kill mid-apply restores on restart and clears `original-state.json`; a second instance exits 0 with "already running").
- `pnpm build` in `ui/`: passes (`tsc --noEmit` + Vite).
- `scripts/footprint.ps1`: PASS with the numbers above.
- First GitHub Actions run on a fresh clone (`windows-latest`, Windows Server 2025): **green**. Same 27 + 2 tests; footprint on the runner 0.69 MB exe, 9.79 MB peak working set (1.12 MB private), 0 % CPU. Only 0.21 MB of headroom on total working set there, so treat that gate as the first thing to revisit if it ever flakes.
- CLI against a live release core: `status` summary, `status --json`, `autostart` -> `on` (Run value written with the quoted exe path) -> `off` (value removed), a second `run` prints "relay-core is already running", `shutdown` restores and exits, `logs/core.log` written.
