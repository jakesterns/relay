# M2 — Display profiles

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M2-display.md. Work on branch `m2-display`. Original state must be captured and written to disk before any change; test the restore path first. Work through the checklist, check items off, and update docs/ROADMAP.md when done.

## Goal
Per-game GPU colour and monitor settings that apply only while the game has
focus, only on the game's monitor, and restore on blur, exit, crash, reboot.

## Depends on
M0 (crash-restore harness), M1 (monitor ids and HMONITOR mapping, DDC/CI capability list).

## Definition of Ready
- [x] M0 and M1 complete. Verified 2026-09-11: `MonitorProbe { id, hmonitor, primary, ddc }` and the VCP capability parser are on `m1-hardware`; `m2-display` branches from it (worktree `stream-share-m2`). Note: neither M4 nor M1 was merged to `main` — the milestone branches are stacked (main → m4-share → m1-hardware → m2-display).
- [x] Test hardware noted (2026-09-11): GPU **NVIDIA RTX 3090** (driver 32.0.16.1664; NvAPI path) + AMD Radeon iGPU (no monitor attached; ADLX stays deferred). Monitor **LG ULTRAGEAR+** (`mon:GSM5C7C:402NTCZ9E219`), DDC/CI verified in M1 with 47 VCP codes incl. 0x10/0x12/0x60/0x62.
  **Manual backup note:** the panel was *powered off for the whole coding session*, so the pre-change values could not be read or photographed up front. Mitigations: (a) `cargo test -p relay-display --test live_read -- --ignored --nocapture` prints brightness/contrast/sharpness, the raw gamma ramp and NvAPI vibrance/hue read-only — run it first thing in the live pass and paste the output here; (b) the live crash test records `before:` to stdout before changing anything; (c) last resort is the monitor OSD reset.
- [x] NvAPI: `nvapi64.dll` present (RTX 3090). Loaded dynamically per operation (`relay_display::nvapi`), no link-time dependency; gamma-ramp path is the vendor-neutral fallback.

## Checklist
### DDC/CI
- [x] `relay-display::ddc`: physical monitor handle from the probe's `HMONITOR` (`GetPhysicalMonitorsFromHMONITOR`), `GetVCPFeatureAndVCPFeatureReply` / `SetVCPFeature`, 3 retries with 50 ms pauses per call and a per-model write-settle delay (60 ms on LG). Handles are opened per transaction group and never cached (they go stale on display changes).
- [x] Capture reads every code the profile will touch (`DisplayAdapter::capture`) and stores `(code, value)` pairs in the snapshot; restore writes them back in reverse order (unit test `restore_writes_vcp_in_reverse_capture_order`). Failing to *read* an original aborts the apply — never write what you cannot put back.
- [x] Model quirks table `relay_display::vcp::QUIRKS` keyed on the EDID manufacturer id **plus product code** (`GSM` + `5C7C`), which together name one model; a `product: None` row is a manufacturer-wide fallback carrying write timing only. Each vendor opcode is a `Candidate` = code + `Evidence` (`VendorDoc { url }` / `Osd { observer, date, monitor }` / `Unverified { note }`). **Guessing is structurally impossible** (S3, 2026-09-14): `quirks_for_key` turns a candidate into a `VerifiedCode` via `attest()`, which returns `None` for `Unverified`; `VerifiedCode`'s `u8` is private to its module and `attest` is its only constructor; `plan_writes` and `vendor_controls` take `VerifiedCode`, not `u8`. `unverified_table_entry_does_not_enable_the_slider` proves the LG case end to end — the panel advertises both candidates, the profile asks for both, and still nothing is written and neither slider is enabled. Runbook and the evidence rules: `docs/dev/vcp-verification.md`.

### GPU colour
- [x] `relay-display::nvapi`: `nvapi64.dll` loaded per operation via `nvapi_QueryInterface` (no link-time dep, nothing resident between applies — footprint), display handles matched to the probe by GDI name, digital vibrance (`GetDVCInfo`/`SetDVCLevel`, raw levels in the snapshot, profile-percent mapping unit-tested) and hue (`GetHUEInfo`/`SetHUEAngle`). Live verification pending (see Deferred).
- [x] `relay-display::gamma`: pure ramp maths (gamma 0.5–2.0, contrast ±50 % effect, cubic-falloff shadow lift; monotonicity and pivot unit-tested against corner cases) + `Get/SetDeviceGammaRamp` on the monitor's own DC. The *original* ramp is captured raw and restored raw, so f.lux / Night Light curves come back exactly.
- [x] AMD — `relay-display::amd`: saturation + hue over the AMD Display
  Library (`atiadlxx.dll`, dynamically loaded per operation like NvAPI),
  behind the same `DisplayIo` seam, selected at runtime by which vendor owns
  the target monitor. Profile → raw-unit curve recorded below; the NVIDIA
  path is unchanged. **Transport note:** ADL rather than the ADLX vtables,
  for a reason recorded in the session log below. Fixture-level only for the
  colour writes — no AMD GPU drives a display on this PC.

### Core integration
- [x] `DisplayControl` trait now takes a target (`Option<&MonitorProbe>`); `crates/core/src/display_backend.rs` implements capture/apply/restore behind a `DisplayIo` primitive trait (fake in tests, `RealIo` in production). Only the target monitor is read or written — proven by unit tests with a two-monitor fake (`apply_touches_only_the_target_monitor_and_restore_reverts_it`, plus untouched-B assertions on every failure path). Restore re-resolves handles by stable `MonitorId` (crash/reboot path, `restore_re_resolves_stale_handles_by_stable_id`); a monitor that is gone fails the restore so the snapshot stays pending for the next start.
- [x] Game moves monitors → restore old, apply new: `Applier` keys the fast path on (profile, target-id) and retargets otherwise (`moving_monitors_restores_old_then_applies_new`). Detection: `Foreground.hmonitor` on every focus event, an `EVENT_SYSTEM_MOVESIZEEND` hook for drags, and the 5 s service tick as the safety net for hook-less moves (Win+Shift+Arrow).
- [x] Session events: `WTSRegisterSessionNotification` on the hidden winloop window → `CoreEvent::SessionLock` → restore on lock, re-select on unlock. Logoff/shutdown ride the existing console-ctrl path; `WM_DISPLAYCHANGE` re-probes and re-selects (a vanished monitor deselects → restore).
- [x] Snapshot format: `DisplayStateSnapshot.targets[]` = `{ monitor id, gdi_name, hmonitor, vcp[(code,value)], raw gamma ramp, raw NvAPI {dvc,min,max,hue} }`, serde-round-trip tested. Written (fsync) before any change, as before.
- [x] Crash-restore harness on the real backend on a real monitor — written *and now run* (2026-09-13, monitor awake). `crates/core/tests/crash_restore_display.rs` passed against the LG ULTRAGEAR+: apply → `taskkill /F` → restart → live values back. See the live pass log below. The recording-backend harness (`crash_restore.rs`) passes with the target-aware backend.
- [x] Apply-on-focus / restore-on-focus-loss on real hardware — `crates/core/tests/focus_apply_restore_live.rs` (new, ignored). Launches Notepad, lets the focus hook apply the profile with no IPC nudge, closes it, and asserts the hardware is back. Measures restore latency from the core's own log rather than from DDC reads, which would swamp it.

### UI
- [x] Display section on the Games screen reads/writes the subject profile's real `DisplaySettings` (draft loaded via `get_profile`, saved via `save_profile`); sliders for unadvertised VCP codes are disabled from the library's `ddcci` list. Black equaliser / response are driven by `Reply::Hardware.vendor_controls`, which the **core** computes from `vcp::QUIRKS` and the advertised list — the client is told, never asked, because a UI that inferred a control from the advertised opcode list would defeat the type guarantee. Response levels come from the verified value map rather than a constant the UI keeps in sync. "Applied via" renders `CoreState.display_via` (NvAPI / gamma ramp / DDC/CI actually used, plus fields the hardware could not honour). `pnpm build` (tsc + vite) green.
- [x] Hardware library monitor entry shows which controls are available ("controls: brightness, contrast" from the advertised codes).

### Gates
- [x] Workspace tests green (incl. 10 new adapter tests, 14 relay-display tests, apply/backup extensions); clippy + fmt clean.
- [x] Release footprint gate PASS 2026-09-11: idle RSS 5.95 MB, private WS 0.86 MB, 0 % CPU, exe 1.04 MB.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason. ✔ (Deferred below)
- Alt-tab out of the game restores within 200 ms; alt-tab back re-applies. — **met 2026-09-13**: 137.4–148.1 ms, mean 144 ms over six consecutive runs of `focus_apply_restore_live`. See the note on where that time goes, below.
- Kill the core while applied, restart: monitor and GPU back to original. — **met 2026-09-13** on the LG ULTRAGEAR+.
- Second monitor never changes. — proven at the unit level on every path (two-monitor fake); live proof needs a second physical monitor (runbook below).

## Live pass — 2026-09-13 (LG ULTRAGEAR+, single monitor, NVIDIA)

The monitor that was powered off for the whole 2026-09-11 session was awake,
so runbook steps 1 and 2 ran, and step 3 became an automated test.

**1. Read-only state** (`relay-display --test live_read`). Everything the
backend depends on answers on this panel and driver:

```
== \\.\DISPLAY1 (HMONITOR 0x10001)
  brightness 0x10: 100 / 100
  contrast   0x12: 70 / 100
  sharpness  0x87: 70 / 100
  gamma ramp: identity=true r[0]=0 r[128]=32896 r[255]=65535
== NvAPI \\.\DISPLAY1: dvc=Ok(Dvc { current: 0, min: 0, max: 63 }) hue=Ok((0, 0))
```

So: DDC/CI works for 0x10 / 0x12 / 0x87; the NvAPI function ids and struct
versions are right on this driver (no −9, so the `…Ex` fallback is not
needed); and the ramp was untouched, i.e. no f.lux/Night Light curve was in
play during the test.

**2. Crash-restore** (`relay-core --test crash_restore_display`). Every path
the profile touches moved, and the exact original values came back after a
hard kill:

```
before:   LiveState { brightness: 100, ramp_mid: 32896, dvc: Some(0) }
applied:  LiveState { brightness: 95,  ramp_mid: 35023, dvc: Some(13) }   <- DDC + ramp + NvAPI
killed:   LiveState { brightness: 95,  ramp_mid: 35023, dvc: Some(13) }   <- taskkill /F, nobody restored
restored: LiveState { brightness: 100, ramp_mid: 32896, dvc: Some(0) }    <- recovery on restart
```

Brief risk #4 ("restore-on-crash for display settings") is closed for the
single-monitor case. An independent `live_read` afterwards matched the
before-state on all five values, including the two the profile never touched.

**3. Apply-on-focus and restore-on-focus-loss**
(`relay-core --test focus_apply_restore_live`, new). Notepad stands in for the
game; the profile applies from the focus hook alone, with no IPC nudge, and
the machine goes back when Notepad closes. Six consecutive runs:

```
137.4  148.1  141.4  148.0  144.6  144.2  ms      (mean 144, budget 200)
```

**Where that time goes, and why it matters:** the measurement is from the
core's `foreground changed` line to its `original state restored` line, and
the bulk of it is the DDC/CI write — brightness travels over I2C to the panel
and costs ~100 ms on its own. The gamma ramp and NvAPI paths are far quicker.
So a profile that sets only GPU colour restores almost instantly, and 144 ms
is close to the floor for one that also drives a monitor control. It is
inside the 200 ms budget but not by much, and the headroom is the panel's,
not ours — worth remembering before adding a second DDC write to the restore
path.

**Test robustness, learned the hard way.** The first version of the focus test
killed Notepad by pid. On Windows 11 `notepad.exe` is an app-execution alias:
the process that gets launched exits immediately and the real Notepad runs
under a different pid, so the kill hit nothing, the test timed out, and it
panicked *with the profile still applied* — leaving the monitor dimmed. The
repair was the product's own recovery path (start a core on the same data
root; it restores from the pending snapshot before opening its pipe), which
is an unplanned second confirmation that crash-restore works. The test now
kills by image name and carries a `RestoreGuard` that runs that same recovery
on unwind.

## S1 session — AMD (ADLX) backend, 2026-09-14

### Definition of Ready: no AMD-driven display on this PC

`AMD Radeon(TM) Graphics` (Raphael iGPU, `PCI\VEN_1002&DEV_164E`) is present
with a driver loaded, but `Win32_VideoController` reports no current
resolution for it — nothing is plugged into it. The only monitor, the LG
ULTRAGEAR+ (`GSM5C7C`), hangs off the RTX 3090. So the colour **writes** are
fixture-tested, as the DoR allows. What *is* live-verified is everything
below the colour call: the library loads, the entry points resolve, the
structs match, and adapters enumerate.

### Live read-only probe

`cargo test -p relay-display --test live_read live_read_amd_state -- --ignored --nocapture`

```
adapter 0 vendor=1002 present=1 exist=1 "AMD Radeon(TM) Graphics" on "\\.\DISPLAY5"
adapter 1 vendor=1002 present=1 exist=1 "AMD Radeon(TM) Graphics" on "\\.\DISPLAY6"
adapter 2 vendor=1002 present=1 exist=1 "AMD Radeon(TM) Graphics" on "\\.\DISPLAY7"
adapter 3 vendor=1002 present=1 exist=1 "AMD Radeon(TM) Graphics" on "\\.\DISPLAY8"
adapter 4 vendor=1002 present=1 exist=1 "AMD Radeon(TM) Graphics" on "\\.\DISPLAY9"
adapter 5 vendor=10   present=1 exist=1 "NVIDIA GeForce RTX 3090"  on "\\.\DISPLAY1"
adapter 6 vendor=10   present=1 exist=1 "NVIDIA GeForce RTX 3090"  on "\\.\DISPLAY2"
adapter 7 vendor=10   present=1 exist=1 "NVIDIA GeForce RTX 3090"  on "\\.\DISPLAY3"
adapter 8 vendor=10   present=1 exist=1 "NVIDIA GeForce RTX 3090"  on "\\.\DISPLAY4"
== ADL loaded; 0 AMD-driven display(s)
```

Three things only knowable by running it, which fixtures could not have told
us:

1. **The `AdapterInfo` layout is right.** Adapter names and GDI display names
   come back as clean strings, which they would not if the struct were off by
   a field.
2. **ADL enumerates every adapter the OS has, not only AMD's** — the RTX 3090
   is in that list four times. Colour calls are addressed by *adapter index*,
   so without a vendor check the AMD backend would happily aim an
   `ADL2_Display_Color_Set` at an NVIDIA adapter. There is now a filter on
   `iVendorID`, and a test pinning the constant.
3. **`ADL_VENDOR_ID` is decimal 1002, not hex `0x1002`.** Writing the natural
   `0x1002` would have filtered out every real AMD adapter and silently
   disabled the whole backend on exactly the machines it exists for.

The correct AMD answer on this PC is "0 AMD-driven displays", which is what
it reports — so an AMD-with-nothing-attached machine falls through to the
gamma-ramp path rather than claiming a display it cannot drive.

### Why ADL and not the ADLX vtables

AMD ships two interfaces to the same driver feature (the "Custom Color" block
in Radeon Software): ADLX (`amdadlx64.dll` 1.4.0.121 here), whose C binding is
COM-like and **vtable-indexed**, and ADL (`atiadlxx.dll` 7.25.10.1590 here), a
flat C API resolved by `GetProcAddress`.

With no AMD display to test against, a wrong or shifted vtable slot would not
fail — it would call *a different method*, on a stranger's monitor, in the
field. A missing flat export fails at load and degrades to "unavailable".
That is the rule the VCP quirks table already follows: never issue a command
whose meaning you cannot prove. `ADL2_Display_Color_Set` is the call Radeon
Software's own slider makes. If ADLX ever becomes the only transport it
replaces the inside of `relay-display::amd`; the `DisplayIo::gpu_*` seam above
it does not move.

### The curve, and why it is not NvAPI's

The two drivers do not expose the same control, so one mapping cannot serve
both:

| | NvAPI DVC | ADL saturation |
|---|---|---|
| range | 0..63 | 0..200 |
| neutral | `min` (0) | driver `default` (100) |
| below neutral | **impossible** | available |

**Vibrance** (profile 0..100, 50 = neutral) maps piecewise-linearly with the
knee at the driver's own default, not at the midpoint of the range:

```
profile   0 ──────────── 50 ──────────── 100
ADL      min ─────── default ───────── max
```

Two segments rather than one line across `[min, max]`, because 50 has to mean
"leave the colour exactly alone" — anything else and every game would nudge
the desktop's colour. Each side then scales to its own span, so an off-centre
default still behaves.

The deliberate divergence: **AMD honours desaturation, NVIDIA cannot.** The
bottom half of the slider has nowhere to go on a DVC range whose neutral is
its minimum, so NVIDIA clamps to neutral and now *says so* in the UI's "Not on
this hardware" line. Pinning AMD to neutral to match would throw away a
control the hardware has; the point of the vendor seam is that each GPU does
the best it can with the same profile, not that both do the worst. Pinned by
`amd_honours_desaturation_where_nvidia_clamps` and, at the seam, by
`a_desaturating_profile_moves_amd_and_is_reported_on_nvidia`.

**Hue** stays in degrees. NvAPI takes a full 0..359 rotation; ADL's hue is a
narrow signed trim (commonly ±30°). The profile angle is folded into
(−180°, 180°] — +350° and −10° are the same rotation — and then **clamped,
not rescaled**. Rescaling would make "10°" mean one thing on NVIDIA and
another on AMD for the same profile. A clamp is reported up to the UI
("hue (AMD trims to -30..30°, asked for 90°)") rather than silently
under-delivering.

**Gamma, contrast and shadow lift need no AMD code at all** — they ride the
vendor-neutral `SetDeviceGammaRamp` path, which was already the fallback and
is unchanged.

Every branch of both mappings is unit-tested, including step-grid snapping,
degenerate ranges (`min == max`), and every hue angle from −720° to +720°.

### Crash-restore against the AMD backend

`crates/core/tests/crash_restore_amd.rs`. The existing `crash_restore.rs`
proves *which calls* happened; this proves *which values* came back, by
swapping the display backend for a file-backed simulated rig
(`RELAY_DISPLAY_SIM`, the sibling of `RELAY_RECORDING_BACKEND`) that outlives
a `taskkill /F`. Everything above `DisplayIo` is production code — planning,
backup-before-apply, snapshot format, vendor dispatch, and the recovery that
runs before the pipe opens.

One profile (brightness 95 over DDC/CI + gamma 1.2 on the ramp + vibrance 75
on the vendor API), run through both vendors. Values are
`(brightness, ramp[128], raw colour)`:

```
[amd]    before (100, 32896, 100)  applied (95, 36900, 150)  killed (95, 36900, 150)  restored (100, 32896, 100)  <= 116.3 ms
[nvidia] before (100, 32896,   0)  applied (95, 36900,  32)  killed (95, 36900,  32)  restored (100, 32896,   0)  <= 111.1 ms
```

Identical behaviour, different raw units — which is the point, and is itself
asserted (`the_two_vendors_write_different_raw_values_for_the_same_profile`:
75 % vibrance is 150 of 0..200 on AMD and 32 of 0..63 on NVIDIA, while
brightness and the ramp are vendor-neutral and match exactly). The timing is
an upper bound on *recovery* — launch to a connectable core — not the alt-tab
number; the alt-tab budget is still the 144 ms measured live on NVIDIA above,
and it is dominated by the DDC/CI write, not the GPU path.

The pre-existing harness is unchanged and still green:
`cargo test -p relay-core --test crash_restore` → 2 passed.

### Snapshot compatibility

`MonitorStateSnapshot.nvapi` became `gpu`, now vendor-tagged, but **the wire
name stays `nvapi`** with `vendor` defaulting to NVIDIA. This is the one
back-compat case that actually matters: upgrade while a profile is applied,
and a snapshot that failed to load would leave someone's monitor dimmed with
no way back. Pinned by
`a_pre_adl_snapshot_still_loads_and_restores_through_nvapi`.

### Gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo test --workspace` (38 suites green), `pnpm build`, and the footprint
gate: **PASS — 7.01 MB peak RSS, 0.92 MB private WS, 0 % idle CPU, exe
1.45 MB.**

One environment note for the next session: `cmake` is not on `PATH`, so
anything that builds `opusic-sys` (i.e. `--workspace`) fails until you add
`C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`.
Nothing to do with this change.

## Out of scope
LUT / ICC profiles, HDR.

## Deferred
- ~~**Live verification pass**~~ — **done 2026-09-13**, steps 1–3 above. Re-run any time with:
  ```
  cargo test -p relay-display --test live_read               -- --ignored --nocapture
  cargo test -p relay-core --test crash_restore_display      -- --ignored --nocapture
  cargo test -p relay-core --test focus_apply_restore_live   -- --ignored --nocapture
  ```
  All three need the monitor awake; the last two change real settings briefly and restore them.
- **Session lock / unlock (Win+L)** — the one live step still outstanding. Not automated because locking the machine mid-session is disruptive and the unlock needs a human. *Runbook:* with a profile applied, press Win+L, and on returning check `logs/core.log` for `session lock change locked=true` followed by `original state restored`, then a re-apply on unlock. The code path (`WTSRegisterSessionNotification` → `CoreEvent::SessionLock`) is unit-tested; what is unverified is that Windows delivers the notification to the hidden winloop window on this machine.
- **Second-monitor live proof + LG C2 manual test log** — one physical monitor on this PC (same deferral as M1). *Runbook:* attach the C2, give it a profile row, confirm (a) only the game's monitor changes brightness/ramp, (b) dragging the game across restores the first panel within a tick, (c) per-model VCP codes that work go into the hardware library / quirks table.
- **Vendor opcodes (black equaliser, response time)** — the *machinery* is done (S3, 2026-09-14): evidence-typed table, the `VerifiedCode` guarantee, the `vendor_probe` OSD harness, core→UI plumbing and the shrunken UI note. What is still outstanding is the **observation itself**: no candidate on the LG 32GS95UE has been watched on the OSD, so both entries remain `Evidence::Unverified` and both sliders ship disabled.

  *Runbook* (`docs/dev/vcp-verification.md`): open the OSD on the page to watch, keep off the joystick, then one code at a time —
  ```powershell
  $env:RELAY_VCP_PROBE = "F5:1|2|3|4"
  cargo test -p relay-display --test vendor_probe -- --ignored --nocapture
  ```
  Candidates on this panel, from the 47-code capability dump: `F5(01 02 03 04)`, `F6(00 01 02)`, `F7(00 01 02 03)`, `F8(00 01)`, `FA(00 01)`, `FE(00 01 02)`, plus the value-less `F4`, `F9`, `FD`, `FF`. Record which OSD label moved *and* which written value maps to which level — the code alone is not enough. A code whose effect nobody could see stays `Unverified`; that is the honest answer, not a failed run.
- ~~**ADLX (AMD)**~~ — **built 2026-09-14** (session S1, above), behind the
  `DisplayIo` seam and fixture-tested. What remains is **an AMD live pass**,
  which needs a monitor plugged into a Radeon — the iGPU on this PC drives
  nothing. *Runbook:* attach a display to the AMD adapter, then
  ```
  cargo test -p relay-display --test live_read live_read_amd_state -- --ignored --nocapture
  ```
  which should list that display instead of reporting 0. Then give it a
  profile with vibrance ≠ 50 and confirm (a) `Applied via` reads "AMD ADL",
  (b) Radeon Software's Custom Color saturation slider moves with it, (c) it
  comes back on blur, and (d) `crash_restore_display.rs` passes against it.
  Until then the AMD colour *writes* have never touched a real driver.
