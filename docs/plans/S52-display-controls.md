# S52: display controls per game (NVIDIA, Windows, monitor OSD)

Owner decision, 2026-10-22: the monitor profile is fully customisable, with
recommended NVIDIA settings and the other controls most current monitors
have. Scope is **apply + restore**: display-level settings only, applied per
game while it has focus and restored on blur, exit, crash or reboot, through
the existing `Applier` (capture → fsync snapshot → apply). NVIDIA per-game
driver (DRS) settings such as Reflex, frame cap, G-SYNC and sharpening are
out of scope: they take effect at launch, not on focus.

Editable on the monitor itself: panel type (done in PR #22), refresh and
resolution, HDR capability, and the monitor's own OSD settings.

## What exists (mapped 2026-10-22)

- **NVIDIA (`crates/display/src/nvapi.rs`):** vibrance (DVC) and hue only.
  There is no colour format, range or bit depth, no HDR, no DRS and no mode
  setting.
- **AMD (`amd.rs`):** saturation and hue through ADL.
- **Gamma ramp (`gamma.rs`):** gamma, contrast, shadow lift; vendor-neutral.
- **DDC/CI (`vcp.rs`, `ddc.rs`):**
  - written today: brightness 0x10, contrast 0x12, sharpness 0x87;
  - vendor codes go through `VerifiedCode`, which needs `VendorDoc` or `Osd`
    evidence (`docs/dev/vcp-verification.md`);
  - the LG 32GS95UE (GSM5C7C) candidates 0xF6 (black equalizer) and 0xF5
    (overdrive) are still `Unverified`, so no vendor code is live yet.
- **Probe:** current refresh (`QueryDisplayConfig`), EDID native mode, and
  EDID HDR/gamut/bit depth (`edid_color.rs`). There is no mode list.
  `guess_panel` cannot tell OLED from LCD; `learn::derive::KNOWN_PANELS` can.
- **Not present:** `ChangeDisplaySettingsEx` / `EnumDisplaySettings`, the
  HDR toggle (`DisplayConfigSetDeviceInfo` advanced colour), colour range.

## Work, in order

1. **Monitor record.** Add refresh/resolution overrides and an HDR
   capability override to the library `Monitor`. The default is what the
   probe or EDID reports, and an override never changes the PC by itself.
   The Edit dialog shows the detected value beside each override.
2. **Windows display mode per game.** Refresh rate and resolution through
   `EnumDisplaySettingsEx` (offer only modes the panel lists) and
   `ChangeDisplaySettingsEx` on that one device without `CDS_UPDATEREGISTRY`
   (the change is never saved, so a reboot alone undoes it). Captured and
   restored like the ramp. Budget: a mode change is a visible blank of about
   1 s, so it is opt-in per profile and never part of a learned look.
3. **HDR on/off per game** through `DisplayConfigSetDeviceInfo`
   (`DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE`). Same capture and restore.
   Only offered when the panel reports HDR.
4. **NVIDIA colour.** Add NvAPI colour control (colour format RGB/YCbCr,
   dynamic range full/limited, bit depth) beside vibrance and hue. Vendor-
   tagged restore. AMD equivalents through ADL where they exist; otherwise
   listed under "Not on this hardware".
5. **Monitor OSD over DDC/CI.** Add picture mode (0x14 colour preset / 0xDC
   display mode where advertised) and verify the LG vendor codes for black
   stabiliser/equalizer and overdrive through the OSD-session procedure in
   `docs/dev/vcp-verification.md`. Read-back alone is not proof. Each code
   stays gated until it carries evidence. Restore budget: under 200 ms per
   M2, so each extra DDC write is measured.
6. **Recommendations.** A pure table keyed by panel kind (OLED / IPS / VA /
   TN / Mini-LED) and GPU vendor, giving suggested values with a one-line
   reason each (for example: full RGB range on a PC monitor; native refresh;
   on OLED, no shadow lift beyond the cap). Shown as "Recommended" beside
   each control with one click to use it. Recommendations never apply
   themselves.
7. **UI.** A Display section on the profile form with the new controls, and
   "Applied via" / "Not on this hardware" extended to them.

## Rules carried over

- Only the target monitor is read or written.
- No guessing: vendor DDC codes need evidence, and the UI is told what is
  available by the core.
- Learning stays SDR-only. A mode change or an HDR toggle is never learned.
- Every new write is captured before apply and has a crash-restore test.

## Tests

Pure planners and tables are unit-tested. Live apply and restore runs as a
two-step human check on the main PC (LG 32GS95UE, NVIDIA), with the restore
time measured.
