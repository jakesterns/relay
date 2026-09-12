# Clean-VM install/uninstall runbook

Relay's headline promise is that after an uninstall a machine shows no
difference except the data folder the user chose to keep. This runbook is how
that claim is checked. It is the only way to sign off a change to the
installer, the uninstall planner, or either opt-in component's registration.

The harness is three scripts:

| script | what it does |
|---|---|
| `scripts/machine-snapshot.ps1` | records every place Relay could write |
| `scripts/snapshot-diff.ps1` | compares two snapshots, PASS/FAIL, writes a Markdown summary |
| `scripts/vm-cycle.ps1` | drives install -> opt in -> share -> uninstall -> diff |

`vm-cycle.ps1` exits non-zero on FAIL, so it can gate a release.

## What "clean VM" buys you

The scripts run anywhere, and the M7 session ran them on the dev machine (see
the results in `docs/plans/M7-installer.md`). A VM adds two things the dev
machine cannot:

1. **Breadth.** `-Broad` captures whole-hive registry exports and a file index
   of every location Relay can reach. On a working machine those are drowned
   in other applications' churn; on a VM sitting on a checkpoint, any
   difference is Relay's.
2. **The opt-in components.** Registering the endpoint APO and the virtual
   camera writes to HKLM and needs elevation, and the APO only really proves
   itself once audiodg has loaded it. Both are also the two places where a bad
   uninstall does lasting damage to a user's audio — so they get tested where a
   checkpoint can undo the damage.

## Setting up the VM

1. Hyper-V (Windows 11 Pro: `Turn Windows features on or off` ->
   `Hyper-V`, then reboot) or any hypervisor that takes snapshots.
2. A Windows 11 22H2+ guest — the virtual camera needs the frame-server API,
   and `relay-core vdevice status` reports the build it found.
3. Inside the guest: WebView2 (usually already present on 11), and an audio
   endpoint. A virtual audio device is enough; the APO registers against
   whatever the default render endpoint is.
4. Build the installer on the host and copy it, with the repo's `scripts/`
   folder, into the guest:

   ```powershell
   pwsh scripts/stage-bundle.ps1
   cd ui; pnpm tauri build --bundles nsis --config src-tauri/tauri.bundle.conf.json
   ```

   The `--config` overlay carries the payload and the NSIS hooks. It is kept
   out of the main `tauri.conf.json` so that a checkout without the staged
   binaries still builds — see the M7 plan.
5. **Take the checkpoint now.** Every run starts from it; the cycle is not
   idempotent because an install is not.

## The cycle

From an **elevated** PowerShell in the guest (the opt-in phase needs it; the
rest deliberately does not):

```powershell
.\vm-cycle.ps1 -Installer .\Relay_0.1.0_x64-setup.exe -WorkDir C:\relay-m7
```

Then **revert to the checkpoint** before running it again.

Useful variants:

```powershell
# Keep the profiles: the diff must then be empty except the data folder.
.\vm-cycle.ps1 -KeepData

# Without the two components, which is also the non-elevated pass.
.\vm-cycle.ps1 -SkipOptIn

# One phase at a time when something fails in the middle.
.\vm-cycle.ps1 -Phase install
.\vm-cycle.ps1 -Phase uninstall
.\vm-cycle.ps1 -Phase after
```

Phases, in order: `before`, `install`, `optin`, `share`, `uninstall`, `after`.

## What each phase asserts

- **before** — refuses to run if Relay is already installed, because a
  baseline taken over an existing install proves nothing.
- **install** — all seven payload files landed in one folder
  (`relay-ui.exe`, `relay-core.exe`, `relay-share.exe`, `relay-preview.exe`,
  `relay_apo.dll`, `relay_vdevice.dll`, `uninstall.exe`); there is a Start
  Menu shortcut and an Add/Remove Programs entry; autostart is **off**; and
  `relay-core uninstall --dry-run` shows nothing registered. That last check
  is the important one: a fresh install must not have touched the machine.
- **optin** — the APO registers and writes its pre-install FX-store backup
  *before* the registry changes, and the camera registers and is recorded in
  `installed.json`. Both refuse without their live-write gate and elevation.
- **share** — a loopback share so the capture and encode paths have run before
  the uninstall. A peer that never pairs is a warning, not a failure; that is
  M4's territory.
- **uninstall** — runs the real NSIS uninstaller silently, then asserts no
  Relay process survived.
- **after** — snapshots again and diffs. PASS requires an empty difference, or
  a difference consisting only of entries under the data root when `-KeepData`
  was passed.

## Reading a failure

The report names the exact key, value or path that changed. Typical causes:

- **A registry value the uninstaller does not know about.** Anything a
  component registers has to be recorded — the APO into
  `apo-backup\<endpoint>.json`, the camera into `installed.json` — because
  `crates/core/src/uninstall.rs` plans from those records, not from a
  hard-coded list. A leftover means something was written without being
  recorded.
- **A changed FX property store.** This is brief risk #2 and the most serious
  failure the harness can report: the user's audio chain is not what it was.
  Compare against the backup taken at install; `relay-core apo uninstall`
  restores it byte-for-byte.
- **Bookkeeping the bundler writes.** Tauri's NSIS template records the
  install location under `HKCU\Software\relay\Relay` and only removes it when
  its own "delete application data" box is ticked. Relay removes it on every
  uninstall from `NSIS_HOOK_POSTUNINSTALL` — this is exactly the class of
  leftover the harness exists to find, and it found that one.

## Before a release

Run the cycle four ways from a fresh checkpoint each time:

1. `-SkipOptIn` — the per-user scope, which must never need elevation.
2. full cycle, data deleted — the diff must be completely empty.
3. `-KeepData` — the diff must be empty except the data folder.
4. install, then install again over the top (`-Phase install` twice), then
   uninstall — the upgrade path must preserve profiles and the autostart
   setting.
