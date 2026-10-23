# S53 — Learn a game's look in HDR

Owner, 2026-10-23: "users who wish to play with HDR on will still have some
sort of Relay tuning to help their display", and "ensure that the profile or
some setting flags that it is an HDR tune so users know when selecting a
profile".

## What changed

- **The choice.** Until now an HDR screen was skipped silently (`hdr_skipped`).
  Now the Display → "Learn this game's look" card says the game is running in
  HDR and offers **Learn in HDR**, with the notice `HDR_NOTICE`
  (`learned_display.rs`) explaining that HDR tuning works differently. The
  choice is per game (`GameRecord::hdr_ok`, `Method::LearnDisplayHdr`); turning
  it off skips HDR again and stops applying the HDR look (it is kept).
- **Separate looks.** HDR is learned into its own record per monitor, keyed
  `<monitor id>#hdr` (`record_key`). `effective()` never lets an SDR look stand
  in for HDR or the reverse; imports and exports carry the SDR look only.
  On apply, the core picks the look for the mode the screen will be in: the
  profile's own S52 HDR choice if it makes one, else Windows' current state.
- **Reading HDR frames** (`relay-share look --hdr`, `crates/capture/src/look.rs`).
  `DuplicateOutput1` with `R16G16B16A16_FLOAT` (scRGB), `GenerateMips` to a
  small mip on the GPU (no shaders), one ~1 MB readback per sample. The process
  must be per-monitor DPI aware v2 or `DuplicateOutput1` fails.
  `relay_display::learn::hdr::to_display_codes` encodes each channel relative to
  the panel's peak (`DXGI_OUTPUT_DESC1::MaxLuminance`, 1000 nits if unknown)
  through a 2.2 power curve, so the existing analyser reads it unchanged:
  saturation is scale-free on power-law codes, "clipped" means at or past the
  panel's peak, and the crush line is a light level (0.1 nit) instead of 5 %.
- **What an HDR look does** (`realize_hdr`). Windows ignores the gamma ramp in
  HDR and the LG locks most OSD controls, so an HDR look sets **vendor vibrance
  only** (at most +6, +4 on OLED). Shadow and highlight findings become advice
  for the game's own HDR settings (`ADVICE_SHADOWS`, `ADVICE_HIGHLIGHTS`).
  Relay never edits game settings.
- **The flag.** `ProfileSummary::hdr` (the profile turns Windows HDR on) and
  `CoreState::hdr_looks` (games with an HDR look in use) drive an **HDR** tag
  beside the profile's name on Profiles, the Games header, and the rail's
  "Game profile" line.

## Measured on the main PC (2026-10-23)

LG 32GS95UE in Windows HDR: the sampler reports `peak_nits=603.7`, mip 2
(640×360 from 2560×1440), and frames classify as normal (desktop: mean code
0.22, crush 1.7 %, clip 0 %).

## Owed (owner present)

- Does NVIDIA vibrance (DVC) visibly change the picture while Windows HDR is
  on? If not on this driver, an HDR look is advice only and the card should say
  so. Check with MW4 in HDR: Learn in HDR, Apply, compare.
- A full MW4 HDR session: the look settles, Apply, the HDR tag appears, and
  turning HDR off in Windows switches back to the SDR look on the next focus.
