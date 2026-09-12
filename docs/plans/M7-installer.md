# M7 — Installer and uninstaller

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M7-installer.md. Work on branch `m7-installer`. The uninstaller is the product's promise: test it from a clean VM snapshot after every change. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

**Status: code-complete 2026-09-12** (branch `m7-installer`). The NSIS per-user
installer ships all seven files, the uninstall engine plans from the records
the components wrote rather than from a hard-coded list, and the full
install → share → uninstall → diff cycle was run four ways on the dev machine
with an empty diff every time. Deferred: the clean-VM broad-tier pass and the
two opt-in components' live removal (no hypervisor, no elevation this
session), and the signed installer (EV certificate still not ordered).

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
      Ships seven files, not three: the three exes above plus
      `relay-preview.exe` (the on-demand A/B renderer), `relay_apo.dll` and
      `relay_vdevice.dll` (the two opt-in components, installed but not
      registered), and NSIS's `uninstall.exe`. The three helper exes ride as
      Tauri `externalBin` sidecars; the two cdylibs go through `resources`
      mapped to the install root, because the sidecar naming convention only
      covers executables. `scripts/stage-bundle.ps1` builds and stages all of
      them (both DLLs need their `com` feature explicitly — the workspace
      takes those crates with `default-features = false` through the core).
      **Autostart: see the decision below** — it is a first-run screen toggle
      plus an `/AUTOSTART` installer switch, not a wizard checkbox.
- [x] Core started by the installer; UI opens to the first-run consent screen.
      `NSIS_HOOK_POSTINSTALL` starts it with `RunAsUser` so it runs as the
      user whose profiles it manages. The consent screen already gates on
      `installed.json` having no recorded decision (M5), so a fresh install
      lands there.
- [x] Uninstaller order: stop UI → `relay-core shutdown` (restores state) → APO uninstall → virtual device unregister / driver removal → Run key → files; asks whether to keep `%LOCALAPPDATA%\Relay` (profiles, hardware library).
      `crates/core/src/uninstall.rs`. The order is the `StepKind` enum's
      declaration order and a test asserts the generated plan is sorted by it,
      so a step inserted in the wrong place fails the build rather than the
      VM. The question about data comes from the uninstaller's own "Delete the
      application data" checkbox rather than a second MessageBox.
- [x] `relay-core uninstall --dry-run` prints everything the uninstaller will touch, used by the Settings "what we installed" card.
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

### Gates
- `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -D warnings` clean
  (this also fixed four lints in `relay-vdevice` and one in `relay-core` that a
  newer toolchain started flagging after M5/M2 landed).
- Full workspace test suite green, including 9 new `uninstall` tests and one
  new `config` test.
- Footprint gate with the uninstall engine in the always-on core:
  `relay-core.exe` 1.31 MB, idle RSS 6.21 MB peak, private working set
  0.86 MB, CPU 0.0 % over 35.6 s. **PASS** (budget 10 MB / 0.5 %).

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
2. **Live removal of the two opt-in components** — the `optin` phase needs an
   elevated session, and the APO's restore is only fully meaningful once
   audiodg has loaded it. The engine, its gating and its plan are in place and
   unit-tested; what is untested is the live round trip. Gated on the same
   things as M3b's and M5's own live passes.
3. **Signed installer and binaries** — EV certificate not ordered.
4. **The loopback share in the cycle does not complete pairing** on a single
   machine, so the `share` phase proves the engine spins up and tears down
   rather than that a share ran end to end. Two-PC validation is already
   M4's deferred item; the cycle will pick it up for free once that runs.
5. **The autostart console flash** (deferred to M7 by M0) is still not fully
   gone. Building `relay-core` for the GUI subsystem was tried and reverted:
   PowerShell does not capture stdout from a GUI-subsystem process, so
   `relay-core status` printed nothing into a pipe and every script and test
   that reads it silently saw empty output — a much worse failure than a
   flash, and a quiet one. `hide_own_console()` still hides the window within
   milliseconds of start. The real fix is the split every dual-mode Windows
   tool ends up with: a GUI-subsystem `relay-core.exe` for the service and a
   small console-subsystem `relay-cli.exe` that forwards to it. Worth doing
   before v1 ships, not worth doing in the same session as the uninstaller.
