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

| Folder | Branch | Scope |
| --- | --- | --- |
| `Stream Share` | `main` | trunk; integration, releases, the installed app |
| `relay-adlx` | `feat/adlx-display` | AMD display backend behind the existing `DisplayIo` seam |
| `relay-dual-audio` | `feat/dual-audio` | second Opus track so mic and desktop audio ship together |
| `relay-mkv` | `feat/mkv-container` | MKV alongside the fragmented-MP4 recorder |
| `relay-monitor-vcp` | `feat/monitor-vcp` | verified vendor VCP opcodes (black equalizer, response) |

These four were chosen because they touch nearly disjoint files, so they
merge without fighting each other.

## Adding one

```
cd "C:\Users\stern\Documents\Code\Stream Share"
git worktree add -b feat/<name> ..\relay-<name> main
cd ..\relay-<name>\ui && pnpm install
```

`pnpm install` is per-tree; `node_modules` is not shared.

## Finishing one

```
cd "C:\Users\stern\Documents\Code\Stream Share"
git merge --no-ff feat/<name>          # or open a PR
git push
git worktree remove ..\relay-<name>
```

`git worktree remove` fails with "Directory not empty" when `node_modules` or
`target` is present. Delete the folder yourself afterwards and run
`git worktree prune`.

## The shared local install

`scripts/install-local.ps1` runs from the post-commit hook in **every** tree
and installs over the same `%LOCALAPPDATA%\Relay`. Its lock, marker and log
live in the common git dir, so only one install runs at a time and the log
names the tree that produced each line:

```
[18:13:38] (Stream Share) installed b6ae8b66 -> C:\Users\stern\AppData\Local\Relay
[18:20:02] (relay-mkv) an install is already running; skipping
```

Consequences worth knowing:

- **The installed app follows whichever tree committed last.** If you are
  testing the app by hand, commit from the trunk, or pass `RELAY_NO_INSTALL=1`
  in the feature trees.
- **It builds the working tree, not the commit.** Editing files while a
  background install runs compiles a half-finished tree and fails. The failure
  now names the commit that is still installed.
