# M1 — Hardware library and probe

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M1-hardware.md. Work on branch `m1-hardware`. Work through the checklist, check items off as you go, and update docs/ROADMAP.md when done.

## Goal
The core knows which headset and monitors are connected, profiles match on
that, and the user can build a hardware library with measured headset curves.

## Depends on
M0. Required by M2 (per-monitor targeting) and M3 (headset curves).

## Definition of Ready
- [x] M0 complete (2026-09-09).
- [x] Test hardware recorded (probed on the dev PC 2026-09-10): audio endpoints — ASUS USB Audio 2.0 (`USB\VID_0B05&PID_1A53`), RØDE USB device (`USB\VID_19F7&PID_004E`), RØDECaster virtual endpoints, second USB audio device (`USB\VID_2E1A&PID_4C05`), NVIDIA HDMI audio through the monitor. Monitors — one physical: LG ULTRAGEAR+ (EDID `GSM5C7C`, serial `402NTCZ9E219`). Only one monitor is attached, so multi-monitor id logic is unit-tested against fixture EDID dumps; live second-monitor verification is Deferred with a runbook.
- [x] AutoEQ result file: `crates/core/tests/fixtures/autoeq-hd560s.csv` (oratory1990 Sennheiser HD 560S, downloaded 2026-09-10 from the AutoEq repo). Real EDID fixture: `crates/core/tests/fixtures/edid-gsm5c7c.bin` (dumped from this PC's registry).

## Checklist
- [x] `HeadsetId` from the default WASAPI render endpoint. Implemented as a two-layer scheme (`hardware/mod.rs`): the probe derives a stable *endpoint key* (`ep:c:<container-guid>` when the device exposes a real container — port-stable for USB gear with serials; `ep:d:<endpoint-id>` fallback), and the library `Headset` is a user-named object bound to one or more endpoint keys, which is how a USB DAC → headphone chain gets a name. Profiles reference the library `HeadsetId`, so re-binding never touches profiles. Verified live: the RØDECaster resolved to a container key, the Realtek SPDIF endpoint (null container) fell back to its endpoint id.
- [x] `MonitorId` from `QueryDisplayConfig` + EDID: `mon:<PNP><product-hex>:<serial>` as a pure function of the EDID base block (serial-string descriptor → 32-bit serial → block-hash fallback), so the id survives ports/outputs/reboots; unit-tested against the checked-in real EDID dump. EDID is read from the PnP registry cache under the instance `QueryDisplayConfig` names (equivalent to the SetupAPI dance, far less ceremony). Friendly name + native mode from EDID, refresh from the QDC path, `HMONITOR` mapped via the source GDI name (M2's contract surface: `MonitorProbe { id, hmonitor, primary, … }`).
- [x] DDC/CI capability probe (`GetCapabilitiesStringLength` / `CapabilitiesRequestAndCapabilitiesReply`) on the full-probe path only (slow); MCCS parser extracts top-level `vcp(...)` codes (nested value lists excluded); `ProbeHardware` persists the VCP list onto known library monitors. Live capture from the LG ULTRAGEAR+ (47 codes incl. 10/12/60/62) is a checked-in parser fixture (`ddc.rs`).
- [x] `hardware.json` store (`HardwareStore`): headsets `{id, name, kind, curve, source, endpoints}`, monitors `{id, name, panel, ddcci}`, interfaces. Atomic writes, round-trip tested.
- [x] AutoEQ importer (`hardware/autoeq.rs`): full results CSV (uses `frequency` + `raw` columns) or bare two-column text, from file contents or paste. Tested against the real oratory1990 HD 560S fixture (695 points). **As shipped in M1 the core made no network requests; that changed 2026-09-14** — `hardware/catalog.rs` adds a searchable bundled index and an on-demand WinHTTP fetch of the selected curve (see the note under Out of scope). The paste/file path still exists and is still what the importer parses.
- [x] Device-change events: `IMMNotificationClient` (`watch_win.rs`, render/console default + endpoint add/remove/state) plus a hidden winloop window for `WM_DISPLAYCHANGE`/`WM_DEVICECHANGE` → `CoreEvent::HardwareChanged` → re-probe, recompute connected view, re-select for the current foreground without a focus change. Bursts coalesce by comparing against the last probe. Focus changes now use the cached connected view (no probe on alt-tab).
- [x] IPC: `ListHardware`, `SaveHardware`, `DeleteHardware`, `ProbeHardware`, plus `ImportCurve`; `CoreState.hardware` carries the connected view. Mirrored in `ui/src/lib/ipc.ts` and the Tauri commands in the same commit; wire shapes locked by a serde test.
- [x] UI: Profiles right rail lists the real library with live Plugged / Main / Second pills fed by pushed state; Add headset (endpoint binding + AutoEQ paste) and Add monitor (detected-panel prefill) dialogs; library-backed headset/monitor selects on the profile form and behind the Games screen "Change" links.
- [x] Profile selection tests extended with real ids (`two_cod_rows_follow_the_default_endpoint_swap` runs the library → endpoint-swap → selection path end-to-end; probe smoke test asserts this machine's real endpoints/monitor).

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason. ✔
- Unplugging the headset or switching default endpoint re-selects the profile within 1 s without a focus change. ✔ **Measured live 2026-09-10** on this PC (debug build, real core instance, foreground unchanged): default switch RØDECaster→SPDIF: notification 29 ms after the switch call, restore + new profile applied at **41 ms**; switch back: **26 ms**. Log excerpt: `audio endpoint change what="default"` → `hardware changed` → `profile applied` within 12 ms of the notification.
- Two Call of Duty rows keyed to different headsets pick correctly when swapping. ✔ Unit test (above) and the same shape verified live with two rows bound to the two real endpoints.

Also: release footprint gate re-run and green — idle RSS 4.42 MB / CPU 0 % (the probe's COM/WASAPI/display pages are handed back via a working-set trim after startup and after each re-probe; without it RSS sat at 13 MB of shared DLL pages).

## Out of scope
Applying anything (M2/M3).

> **Superseded 2026-09-14.** "Online curve download" was out of scope *for this
> milestone* and is no longer out of scope for the product: `crates/core/src/hardware/catalog.rs`
> ships a bundled index of 8,849 measured models (`autoeq-index.tsv`, a file beside
> the binaries, read and dropped rather than `include_str!`) and fetches the one
> curve the user picks over WinHTTP, caching it under the data root.
>
> **Licensing position:** the AutoEQ measurements are CC BY-NC-SA (oratory1990,
> crinacle and 21 other contributors), so Relay **ships the index, never the
> measurements**. A curve is fetched from the upstream source at the moment the
> user picks that model, cached so it is fetched once, credited to its measurer in
> the UI as the licence requires, and never redistributed in the installer.
>
> This is the **only** outbound network request Relay makes: user-initiated, one
> host (`raw.githubusercontent.com`), https, 1 MB response cap, skipped entirely
> once cached. It does not weaken CLAUDE.md's "zero network config" rule, which is
> about the share transport. Regenerate the index with `scripts/build-catalog.ps1`.

## Deferred
- **Live second-monitor verification** — only one physical monitor (LG ULTRAGEAR+ GSM5C7C) was attached this session; multi-monitor id stability, primary/second pills and per-monitor `HMONITOR` mapping are unit-tested against fixture EDIDs only. *Runbook:* attach the LG C2 (or any second display) → `cargo test -p relay-core --lib live_probe -- --nocapture` should list both monitors with distinct `mon:` ids and correct `primary` flags; unplug/replug into a different port and re-run — ids must not change; toggle the display cable while a profile row keyed to that monitor is Ready and confirm re-selection in `logs/core.log` within 1 s (`WM_DISPLAYCHANGE` path).
- **Physical headset unplug** — DoD was measured via default-endpoint switching (which the DoD text allows and which exercises the same `IMMNotificationClient` → re-select path); a literal USB unplug additionally goes through endpoint-removal events that are wired but were not exercised by hand this session. *Runbook:* with two Ready rows for the foreground exe, pull the default endpoint's USB cable; the row bound to the surviving endpoint must go active within 1 s.
- **Visual UI pass with the live core** — `pnpm build` (tsc + vite) is green and the IPC wire shapes are covered by tests, but the new rail/dialogs were not exercised in a running Tauri window this session. *Runbook:* `cargo run -p relay-core -- run` + `cd ui && pnpm tauri dev`; add a headset bound to the default endpoint, watch the Plugged pill; switch default device and watch the pill move without touching the window.
