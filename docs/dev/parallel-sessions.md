# Running several Relay sessions at once

`main` is the trunk. As of 2026-09-14 it carries every milestone M0–M7; the
`m0`…`m7-*` branches are kept on the remote as history and nothing should be
cut from them any more.

Each parallel session gets its own **git worktree** — a separate checkout
sharing this repository's history and its single `.git`. Separate folders
matter for more than tidiness: two sessions in one tree would fight over
`target/`, over `ui/dist` (which `tauri::generate_context!` embeds at compile
time), and over the post-commit installer.

## The trees

| Folder | Branch | Session |
| --- | --- | --- |
| `Stream Share` | `main` | trunk; integration, releases, the installed app |
| `relay-adlx` | `feat/adlx-display` | S1 — AMD display backend behind the `DisplayIo` seam |
| `relay-dual-audio` | `feat/dual-audio` | S2 — second Opus track, mic *and* desktop audio |
| `relay-monitor-vcp` | `feat/monitor-vcp` | S3 — verified vendor VCP opcodes |
| `relay-mkv` | `feat/mkv-container` | S4 — MKV alongside the fragmented-MP4 recorder |
| `relay-hkcu-vcam` | `feat/hkcu-vcam` | S5 — per-user camera registration (kills the elevation need) |
| `relay-elevation` | `feat/elevated-install` | S6 — elevated install helper |
| `relay-codec` | `feat/codec-robustness` | S7 — MFT allocator path, codec hard edges |
| `relay-uitest` | `feat/ui-test-harness` | S8 — WebView test harness |

`docs/plans/SESSIONS.md` is the full catalogue: what each session does, its
Definition of Ready and Done, and the prompt to start it. These eight were
given their own trees because they touch nearly disjoint files and so merge
back without fighting. Sessions S9–S17 are validation and bookkeeping and run
in the main tree on a `chore/` branch.

## Adding one

```
cd "<repo root>"
git worktree add -b feat/<name> ..\relay-<name> main
cd ..\relay-<name>\ui && pnpm install
```

`pnpm install` is per-tree; `node_modules` is not shared.

## Finishing one

```
cd "<repo root>"
git merge --no-ff feat/<name>          # or open a PR
git push
git worktree remove ..\relay-<name>
```

`git worktree remove` fails with "Directory not empty" when `node_modules` or
`target` is present. Delete the folder yourself afterwards and run
`git worktree prune`.

## Environment gotchas that cost sessions time

Two things bit real sessions on this machine. Both are environment, not code,
so nothing in the repo reveals them.

**CMake is required to build `relay-capture` and is not on the default PATH.**
`opusic-sys` needs it. Without it the crate does not build at all, which looks
like a broken checkout. Two copies exist on this machine, neither on PATH, and
both work — verified 2026-09-14:

```
C:\Program Files\CMake\bin                                                    (4.4.3)
C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin   (3.31.6-msvc6)
```

Prepend either to `PATH` for the session before building anything touching
`crates/capture`. The standalone install is the tidier choice; the BuildTools
one is a working fallback if it is ever missing.

**WASAPI loopback of a silent endpoint delivers no packets at all.** This is
the dangerous one, because it does not fail — it quietly produces a zero-filled
audio track and plausible-looking latency numbers measured over nothing. S2
nearly recorded a bogus p99 from two passes that were measuring silence. If you
are benchmarking or validating any audio path, play actual audio through the
default endpoint for the duration and assert the packet count, the way
`scripts/dual-audio-check.ps1` does with a generated 440 Hz tone.

**Windows Firewall blocks `relay-share.exe` by path.** Windows prompts the
first time a given executable path opens a socket, and dismissing that prompt
writes a permanent Block rule. Every worktree and every build profile is a
different path, so the prompt returns and Block rules accumulate — ten of them
had piled up by 2026-09-14, with no Allow rule anywhere. Symptom: loopback
share and pairing fail with what look like network errors.

`scripts/firewall-rules.ps1 -List` shows the state without elevation. Clearing
and allowing needs an elevated shell and belongs to the user:

```
powershell -NoProfile -File scripts\firewall-rules.ps1 -Clean -Allow
```

**Never change firewall rules from a session.** It needs elevation the user
owns. Report the block and let them run it. The product-side fix — the
installer adding the rule and the uninstaller removing it — is session S22.

## The shared local install

`scripts/install-local.ps1` runs from the post-commit hook in **every** tree
and installs over the same `%LOCALAPPDATA%\Relay`. Its lock, marker and log
live in the common git dir, so only one install runs at a time and the log
names the tree that produced each line:

```
[18:13:38] (Stream Share) installed b6ae8b66 -> %LOCALAPPDATA%\Relay
[18:20:02] (relay-mkv) an install is already running; skipping
```

Consequences worth knowing:

- **The installed app follows whichever tree committed last.** If you are
  testing the app by hand, commit from the trunk, or pass `RELAY_NO_INSTALL=1`
  in the feature trees.
- **It builds the working tree, not the commit.** Editing files while a
  background install runs compiles a half-finished tree and fails. The failure
  now names the commit that is still installed.

## Never kill Relay processes by name

Two sessions testing on one PC share the process list. `taskkill /IM
relay-share.exe` (or `Get-Process relay-share | Stop-Process`) in one session
ends the other's loopback run with exit code 1 and no log line, which reads
exactly like a crash in the engine; S33 lost an hour to that on 2026-09-18
(`docs/dev/BUGS.md`, B8). Kill the PIDs you started, and set `RELAY_INSTANCE`
so pipes and mutexes do not collide either.
