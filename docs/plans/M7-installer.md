# M7 — Installer and uninstaller

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M7-installer.md. Work on branch `m7-installer`. The uninstaller is the product's promise: test it from a clean VM snapshot after every change. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

**Status: code-complete 2026-09-12** (branch `m7-installer`); the elevated
phase was completed and live-verified 2026-09-14 by S6. The NSIS per-user
installer ships all nine files, the uninstall engine plans from the records
the components wrote rather than from a hard-coded list, and the full
install → share → uninstall → diff cycle was run four ways on the dev machine
with an empty diff every time. Deferred: the clean-VM broad-tier pass (no
hypervisor) and the signed installer (EV certificate still not ordered). The
two opt-in components' live removal is no longer deferred — see Deferred
item 2.

## Depends on
M0 (autostart), M3b and M5 (components that need removal). Can start earlier for the core + UI only.

## Definition of Ready
- [x] M0 complete; for the full uninstaller, M3b and M5 complete.
      **M0 yes. M3b and M5 are code-complete but their live passes are still
      blocked** (EV cert, hypervisor, elevation), so their removal paths are
      implemented, planned and unit-tested here but not live-verified. Scoped
      per the kickoff instruction: core + UI proven end to end, components
      recorded as Deferred.
- [ ] Hyper-V (or other) clean Windows VM with a checkpoint available for the diff test.
      **Not available.** The Hyper-V role and its PowerShell module are not
      installed on this machine (`Get-VM` absent, `Get-WindowsOptionalFeature`
      needs elevation to even query), and enabling it needs an elevated
      session plus a reboot. `HypervisorPresent` is true only because of VBS.
      Mitigation below.
- [ ] EV cert available for the signed-installer item. **Still not ordered as
      of 2026-09-12** — same blocker as M3b and M5.

### What replaced the clean VM
The harness was built first and then run on this machine, which gives a real
install/uninstall diff, just a narrower one than a VM would:

* `scripts/machine-snapshot.ps1` captures every location Relay can write —
  the Run key, Add/Remove Programs, the camera CLSID in all three registry
  views, **every** render endpoint's `FxProperties` store, the frame-server
  camera list, the data root, the install directory, Start Menu, and any
  Relay service or driver. `-Broad` adds whole-hive exports and a file index
  for the VM pass.
* `scripts/snapshot-diff.ps1` compares two snapshots and PASSes only on an
  empty difference (or, with `-KeepData`, a difference consisting only of
  entries under the data root). Exits non-zero on FAIL so it can gate a
  release.
* `scripts/vm-cycle.ps1` drives the phases and asserts at each one.

The targeted tier is authoritative for the promise and is stable on a working
machine. What a VM still adds is breadth (whole-hive diffs are pure noise
here) and the two HKLM components. Runbook: `docs/dev/uninstall-vm.md`.

## Checklist
- [x] NSIS per-user installer (Tauri bundler, `installMode: currentUser`) shipping `relay-core.exe`, `relay-ui.exe`, `relay-share.exe`; Start Menu entry; optional autostart checkbox mapped to `relay-core autostart on`.
      Ships nine files, not three: the three exes above plus
      `relay-svc.exe` (the windowless launcher, see below),
      `relay-elevate.exe` (the elevated install helper — the only binary that
      writes HKLM; see `docs/dev/elevation-live.md`),
      `relay-preview.exe` (the on-demand A/B renderer), `relay_apo.dll` and
      `relay_vdevice.dll` (the two opt-in components, installed but not
      registered), and NSIS's `uninstall.exe`. The five helper exes ride as
      Tauri `externalBin` sidecars; the two cdylibs go through `resources`
      mapped to the install root, because the sidecar naming convention only
      covers executables. `scripts/stage-bundle.ps1` builds and stages all of
      them (both DLLs need their `com` feature explicitly — the workspace
      takes those crates with `default-features = false` through the core).
      The payload is declared in `ui/src-tauri/tauri.bundle.conf.json`, an
      overlay passed to `tauri build --config`, **not** in the main
      `tauri.conf.json` — see the decision below.
      **Autostart: see the decision below** — it is a first-run screen toggle
      plus an `/AUTOSTART` installer switch, not a wizard checkbox.
- [x] Core started by the installer; UI opens to the first-run consent screen.
      `NSIS_HOOK_POSTINSTALL` starts it with `RunAsUser` so it runs as the
      user whose profiles it manages. The consent screen already gates on
      `installed.json` having no recorded decision (M5), so a fresh install
      lands there.
- [x] Uninstaller order: stop UI → `relay-core shutdown` (restores state) → APO uninstall → virtual device unregister / driver removal → firewall rule → Run key → files; asks whether to keep `%LOCALAPPDATA%\Relay` (profiles, hardware library).
      `crates/core/src/uninstall.rs`. The order is the `StepKind` enum's
      declaration order and a test asserts the generated plan is sorted by it,
      so a step inserted in the wrong place fails the build rather than the
      VM. The question about data comes from the uninstaller's own "Delete the
      application data" checkbox rather than a second MessageBox.
- [x] `relay-core uninstall --dry-run` prints everything the uninstaller will touch, used by the Settings "what we installed" card — and, since S6, by the two opt-in cards as well: `elevate::plan_lines` narrows the same `Plan` to one step and renders it with the same `Plan::lines`, so the listing shown before a UAC prompt is generated by the code that does the removing.
      Same function on both paths: `Plan::lines()`. The card renders it over
      IPC (`Method::UninstallPlan`), so the listing the user reads before
      uninstalling is generated by the code that does the uninstalling. The
      Settings card also has the "Uninstall Relay" button, which hands over to
      the one Windows uninstaller (`Method::LaunchUninstaller`) instead of
      doing its own removal.
- [x] Upgrade path: installer over an existing install preserves data and re-registers components.
      Measured below. `NSIS_HOOK_PREINSTALL` shuts a running core down first
      (which also restores the user's audio and display settings) so the
      binaries can be replaced.
- [x] Clean-VM test script (Hyper-V checkpoint): install → opt in to both components → share → uninstall → registry and file diff against the checkpoint is empty except the optional data folder.
      Script written and run; the checkpoint itself is Deferred (no
      hypervisor). Results below.
- [x] **Windows Firewall rule for `relay-share.exe` (S22, 2026-09-15).** The
      installer's `/FIREWALL` switch and the app's own banner add one inbound
      Allow rule on private + domain profiles; the uninstaller removes it.
      See the decision and the S22 section below.
- [ ] Signed installer and binaries with the EV cert. **Deferred** — the
      certificate has still not been ordered. `bundle.windows.certificateThumbprint`
      plus `signCommand` is the one-line change once it exists; the NSIS
      template already threads `UNINSTALLERSIGNCOMMAND` through.

## Decisions

**The install directory and the data root are the same folder, deliberately.**
Tauri's per-user NSIS template installs to `$LOCALAPPDATA\<productName>`,
which for Relay is `%LOCALAPPDATA%\Relay` — the documented data root. Rather
than fork the bundler's template to move the program elsewhere, the
uninstaller stopped treating the data root as a directory to delete: it
removes the paths `Paths::data_paths()` names (`data\`, `logs\`, `previews\`,
`apo-backup\`, `installed.json`) and leaves the folder itself to NSIS, which
removes it non-recursively once its own files are gone. Deleting the root
recursively would have deleted the running `relay-core.exe` mid-uninstall.
A test (`the_data_step_never_targets_the_install_directory_itself`) pins this.

**The autostart opt-in is on the first-run screen, not in the installer.**
Tauri's NSIS hooks are macros inserted inside sections; they cannot add a
wizard page, so a real checkbox would have meant vendoring and maintaining a
copy of the bundler's 950-line template. It is also a better place to ask: the
installer's job is to copy files, and Relay's first-run screen is already
where the user answers every other "may we change something" question.
Silent and managed installs get `Relay_0.1.0_x64-setup.exe /S /AUTOSTART`,
which runs the same `relay-core autostart on` the checklist asked for.

**Packaging config lives in an overlay, not in `tauri.conf.json`.**
`tauri-build` fails the `relay-ui` build script when a declared `externalBin`
is missing, and the staged payload is a build artifact that is not in the
repo. With the payload in the main config, a fresh checkout could not run
`cargo build`, `cargo clippy` or `cargo test` at all — CI would have died
before compiling a line. `ui/src-tauri/tauri.bundle.conf.json` holds
`externalBin`, `resources` and `installerHooks`, and is merged in only by the
packaging step:

```
pwsh scripts/stage-bundle.ps1
cd ui; pnpm tauri build --bundles nsis --config src-tauri/tauri.bundle.conf.json
```

Verified by moving the staging directory aside and confirming
`cargo build -p relay-ui` still succeeds, then confirming the overlay build
still emits all five payload files and the hook include.

**The console flash is fixed by a launcher, not by moving the CLI.** Windows
decides whether to capture a process' output from the PE subsystem field, so a
console-subsystem `relay-core.exe` gets a console allocated before `main`
runs — which is the flash — while a GUI-subsystem one would make
`relay-core status` print nothing into a PowerShell pipe. The sketch in the
first draft of this plan was the usual split (GUI service + console CLI shim);
the inverse turned out to be better. `relay-svc.exe` is a 224 KB
GUI-subsystem binary that spawns `relay-core.exe` with `CREATE_NO_WINDOW` and
exits, and the Run key and the installer go through it. Nothing stays
resident, and `relay-core`'s command line, output, and every script and test
that reads it are untouched. `autostart::command_line()` delegates the choice
to `launcher::autostart_target`, which falls back to the core itself in a
`cargo run` tree where the launcher was not built — a flash on a dev machine
beats silently failing to configure autostart.

**The firewall rule is asked for, never taken (S22).** Windows prompts the
first time a given *path* listens, and dismissing that prompt writes a Block
rule that is permanent, invisible and never mentioned again -- after which
every symptom points at the network rather than at a firewall. That makes
"zero network config for the user" false, so Relay adds one inbound Allow rule
for `relay-share.exe`, the only binary that opens a socket.

Three constraints shaped how:

* *It cannot be silent.* Firewall policy is machine-wide, and this installer
  is per-user and unelevated. Reaching for an administrator token the user did
  not offer is exactly the behaviour Relay promises not to have, so the rule
  goes through `relay-elevate.exe` like the APO and the camera: a listing the
  user reads, then one UAC prompt they can decline. `/FIREWALL` exists for
  silent and managed installs where a deploying admin has already decided.
  Interactively the app asks at the moment it matters instead -- the Share and
  Receive screens detect the state and offer the fix -- which is a better
  place to ask than a wizard page nobody reads.
* *Adding an Allow rule is not enough.* Windows applies deny before allow, so
  a machine that already has a Block rule stays broken no matter what we add.
  The install therefore removes the Block rules **for our own binary**, after
  backing each one up verbatim into `firewall.json`. The uninstall does not
  re-create them: they named a program that is being deleted, and restoring
  one would silently re-break Relay for anyone who reinstalls. A clean VM has
  no such rules, so the diff is unaffected either way.
* *It has to come off.* The rule is recorded and removed by
  `StepKind::RemoveFirewallRule`, in the same plan, with the same elevation
  reporting, as everything else.

**Scope: private + domain, never public.** Private is obvious -- Relay is
LAN-only by design. Domain is in scope because a managed work machine reports
its network as Domain, not Private, and a private-only rule would leave
exactly the silent failure this work exists to remove. Public stays blocked:
Windows classifies unknown networks as Public by default, and a coffee-shop
network has no business reaching a screen share. On a public-only network the
banner says so and offers no button, because offering a fix Relay will not
apply would be a lie.

**Reads are a registry parse; only writes use COM.** Detection runs behind a
UI banner and must not drag a COM rule enumeration (and `IEnumVARIANT`) into
the always-on core's 10 MB budget. Windows stores every rule as one
pipe-delimited string under
`...\SharedAccess\Parameters\FirewallPolicy\FirewallRules`; reading needs no
elevation and parsing it is a pure function, fixture-tested against strings
captured from this machine and covered live by
`crates/core/tests/firewall_live.rs`. Mutation goes through `INetFwPolicy2` in
the elevated helper, and only ever `Add` one rule or `Remove` rules *by name*
-- so nothing enumerates rules over COM anywhere. `INetFwRules::Remove` matches
on name alone, which is why Relay's rule has a distinctive one, and why
re-running the install replaces rather than accumulates (the dev machine had
24 duplicate rules from `scripts/firewall-rules.ps1` across worktrees).

### S22 measurements

The probe was run against this machine's real rule store:

```
parsed 705 rules            (402 inbound Allow)
policy: active_profiles=6 (Private|Public), firewall on, inbound blocked
status: verdict=WillPrompt, our rule absent, 0 blocking, 24 stale
```

"24 stale" is 24 rules for a `relay-share.exe` in another worktree or the
installed copy -- correctly *not* counted as governing this binary, which is
the distinction the verdict depends on.

The harness was then proved to enforce the promise rather than just describe
it. `machine-snapshot.ps1` captures every firewall rule naming a Relay binary
and `snapshot-diff.ps1` diffs them as a new section; a planted leftover rule
produces:

```
| firewall rules | 24 | 25 | 1 |

**FAIL -- 1 leftover difference(s):**
- `added` v2.33|Action=Allow|...|App=...\Relay\relay-share.exe|Name=Relay (relay-share)|...
```

with exit code 1. Identical snapshots still PASS. `vm-cycle.ps1`'s opt-in
phase now adds the rule, asserts `our rule: present`, and asserts
`firewall.json` was written -- so the uninstall phase has something real to
remove and the final diff has something real to catch.

### Gates (S22)
- `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -D warnings` clean.
  (`opusic-sys` needs `cmake`, which is installed on this machine but not on
  `PATH`; prepend `C:\Program Files\CMake\bin` before running the workspace
  gates.)
- Workspace tests: **446 passed, 0 failed**, including 19 new `firewall` unit
  tests, 5 in `firewall_live` and 2 new `uninstall` tests.
- UI tests: **178 passed** (8 new, covering blocked / will-prompt / declined
  UAC / public network / unreadable probe / permissive).
- Footprint gate **PASS**: `relay-core.exe` 1.57 MB (up from 1.31 -- the
  firewall COM and registry code), idle RSS 6.87 MB peak, private working set
  0.89 MB, CPU 0 %. Budget 10 MB / 0.5 %.

**The uninstall plan is a pure function of a probed `MachineState`.** Only
`probe()` reads the machine; the plan, its ordering and its rendering are pure,
which is what lets the promise be tested without a VM at all.

## Measurements — the acceptance cycle

Run on the dev machine (Windows 11 Pro 26200), `-SkipOptIn` (the two HKLM
components need elevation this session could not obtain). Each run started
from a machine with no Relay traces: no data root, no `HKCU\Software\relay`,
no Run key, no Add/Remove Programs entry.

### Run 3 — full cycle, data deleted: the diff must be completely empty

```
### Clean-VM uninstall diff

| | before | after | diff |
|---|---|---|---|
| registry values | 1659 | 1659 | 0 |
| file entries | 0 | 0 | 0 |
| relay services | 0 | 0 | 0 |
| relay drivers | 0 | 0 | 0 |

**PASS -- the diff is empty.** Nothing Relay installed is left on the machine.
```

Phases asserted along the way: all 7 payload files present in one folder; a
Start Menu shortcut; autostart defaulting to **off**; `uninstall --dry-run`
after install showing nothing registered (a fresh install must not have
touched the machine); autostart toggled on and then removed by the uninstall;
a loopback share started and torn down; no Relay process surviving the
uninstall; core footprint 5.1 MB idle, 6.1 MB while sharing.

### Run 4 — data kept: empty except the data folder

```
| | before | after | diff |
|---|---|---|---|
| registry values | 1659 | 1659 | 0 |
| file entries | 0 | 6 | 6 |
| relay services | 0 | 0 | 0 |
| relay drivers | 0 | 0 | 0 |

**PASS -- the diff is empty except the data folder the user chose to keep**
(6 entries under `C:\Users\stern\AppData\Local\Relay`).

  added  ...\Relay||.
  added  ...\Relay||data
  added  ...\Relay||data\m7-marker.json     <- written between install and uninstall
  added  ...\Relay||logs
  added  ...\Relay||logs\core.log
  added  ...\Relay||previews
```

Zero registry difference: keeping the data does not mean keeping registry
state.

### Upgrade pass
Installed over a live install with a marker file in `data\` and autostart on.
Installer exit 0; the marker survived byte-for-byte; the Run key was untouched;
the core was stopped before the binaries were replaced and restarted after.

### The leftover the harness caught
The first keep-data run FAILed on one entry:

```
added  HKCU\Software\Relay\Relay || (default) = "C:\Users\stern\AppData\Local\Relay"
```

Tauri's template records the install location under `MANUPRODUCTKEY` so a
reinstall can offer the same folder, and only deletes it when its own "delete
application data" box is ticked. That is installer bookkeeping, not the user's
profiles, and Relay's promise makes no exception for it — so
`NSIS_HOOK_POSTUNINSTALL` now removes it on every uninstall. Re-running the
cycle gave the zero-registry-difference results above. This is exactly what
the harness is for and it is worth keeping in mind that it was found by the
diff, not by reading the template.

### The windowless launcher
Measured directly from the PE headers and the process list:

```
relay-core.exe subsystem=3 (CONSOLE)  -> CLI output is still captured by PowerShell
relay-svc.exe  subsystem=2 (GUI)      -> Windows allocates it no console at all

relay-svc.exe run --data-dir <tmp>
  returns immediately; 0 relay-svc processes remain
  1 relay-core process, MainWindowHandle=0 (no visible window)
  relay-core status answers over the pipe; shutdown is clean
```

`relay-svc.exe` is 224 KB and exits before the core finishes starting, so it
costs nothing at runtime. The cycle now asserts the Run value:

```
run value: "C:\Users\stern\AppData\Local\Relay\relay-svc.exe" run
```

— absolute, inside the install directory, and naming the launcher rather than
the console binary. The uninstall removes it, as the zero-registry-difference
results below show.

### Gates
- `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -D warnings` clean
  (this also fixed four lints in `relay-vdevice` and one in `relay-core` that a
  newer toolchain started flagging after M5/M2 landed).
- Full workspace test suite green: **272 passed, 0 failed** across 33 suites,
  including 9 new `uninstall` tests, 3 `launcher` tests and 2 new
  `config`/`autostart` tests.
- Footprint gate with the uninstall engine in the always-on core:
  `relay-core.exe` 1.31 MB, idle RSS 6.16 MB peak, private working set
  0.84 MB, CPU 0.044 % over 35.8 s. **PASS** (budget 10 MB / 0.5 %). The
  launcher adds nothing resident — it has exited by the time the core is up.

## Definition of Done
- [x] Every checklist item checked or moved to Deferred with a reason.
- [x] Clean-VM diff after uninstall is empty — met on the dev machine's
      targeted tier, both with and without the data folder. The broad tier on
      a real checkpoint is Deferred.
- [x] Install → uninstall never leaves the endpoint APO chain modified —
      every render endpoint's `FxProperties` store is in the snapshot and the
      registry diff was zero on every run. Note that with `-SkipOptIn` the APO
      was never registered, so this proves the installer does not disturb the
      chain; it does **not** yet prove the *restore* path live. That is the
      first thing the VM pass has to check.

## Deferred
1. **Clean-VM broad-tier pass** — needs the Hyper-V role (elevated install +
   reboot) or another hypervisor. Runbook and scripts are ready:
   `docs/dev/uninstall-vm.md`, then `scripts/vm-cycle.ps1`. Run it four ways
   (`-SkipOptIn`, delete-data, `-KeepData`, and install-over-install), from a
   fresh checkpoint each time.
2. ~~**Live removal of the two opt-in components**~~ — **done 2026-09-14
   (S6)**. Both components were registered and removed live on this dev
   machine through `relay-elevate.exe`, and the endpoint's `FxProperties`
   export came back byte-identical. What remains gated on M3b's VM pass is the
   *loading* of the APO into audiodg, not its removal.
   `docs/dev/elevation-live.md`.

   S6 also fixed the elevated phase itself, which could not have worked as
   written: `relaunch_elevated` re-ran `relay-core uninstall
   --components-only` under `runas` with the live-write gates set in the
   *parent*, but elevation starts the child from the user's logon environment
   block, so the gates never reached it and both HKLM steps failed the gate
   check every time. `uninstall::finish_elevated` now goes through the helper,
   which needs no inherited environment because it arms each gate itself,
   around one vetted call. The `-SkipOptIn` runs in the measurements above
   never exercised this path, which is why the harness did not catch it — the
   clean-VM pass (item 1) should run *without* `-SkipOptIn` now that it can.
3. **Signed installer and binaries** — EV certificate not ordered.
4. **The loopback share in the cycle does not complete pairing** on a single
   machine, so the `share` phase proves the engine spins up and tears down
   rather than that a share ran end to end. Two-PC validation is already
   M4's deferred item; the cycle will pick it up for free once that runs.
_(The autostart console flash that M0 deferred here is **done** — see the
launcher decision above and the measurements below.)_
