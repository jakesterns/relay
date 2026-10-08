# S47 — Learned game display

Branch `feat/s47-learned-game-display`, cut from `main`. Mirrors S46 (learned
game EQ) on the display side.

**Owner's requirement.** No per-game presets. Relay learns each game's look
from its own frames and tunes the display per game and per monitor, within
that panel's verified capabilities, so it works for new games and after
updates. Users can import and export.

**Owner decisions recorded during the session.**

1. Tournament wording is a notice only, no confirmation, exactly: *"Relay's
   visual enhancements may not be allowed in some tournaments or
   professional environments. Check with your tournament host or rules."*
   Shown on the card and in `docs/display-file-format.md`.
2. System input idle time (`GetLastInputInfo`) is an extra signal: frames
   sampled after more than `INPUT_IDLE_MAX_MS` (20 s) without input are not
   gameplay.
3. The learned look is stored as aggregates only, panel-neutral; a monitor
   switch re-fits it to the new panel with no relearn.
4. An import applies at once ("Applied (imported)"), learning is off for that
   game by default, and "Keep learning to fine-tune for my monitor" learns
   under the same rules. Export writes the current game layer.

## Design

```
game focused + profile applied + learning on
      │
core: sync_look ──spawn──▶ relay-share look --hmonitor H --fps 1   (own process, BELOW_NORMAL)
      │                         │ DXGI Desktop Duplication, latest frame once a second
      │                         │ GPU video processor → 480×270 NV12 (preview tap) → BGR
      │                         │ relay_display::learn::Analyser → FrameReport (numbers)
      │                         │ + GetLastInputInfo → Idle
      │  ◀── NDJSON stdout ─────┘ frame dropped; nothing stored, nothing sent
      │
learn_tick (1 s): Learner::observe → Aggregate (rolling) → checkpoint every 120
      │            → derive_look → convergence → freeze rule
      │
select_and_apply: effective look (this monitor's, else another monitor's,
      else imported) → realize(look, PanelCaps) → overlay onto the profile's
      DisplaySettings → the ordinary Applier (backup → apply → restore)
```

### Where the code lives

| Piece | File |
|---|---|
| Per-frame statistics, classification, input-idle rule | `crates/display/src/learn/analyse.rs` |
| Aggregate, panel-neutral look, panel-aware adjustments | `crates/display/src/learn/derive.rs` |
| Readiness, convergence, rolling window, freeze, build change | `crates/display/src/learn/converge.rs` |
| Synthetic-frame tests | `crates/display/src/learn/tests.rs` |
| Sampler process (`relay-share look`) | `crates/capture/src/look.rs`, `Preview::bgr` in `preview.rs` |
| Store, overlay, file format, sampler child, view | `crates/core/src/learned_display.rs` (+ `learned_display/tests.rs`) |
| Service wiring | `service.rs`: `learned_profile`, `sync_look`, `learn_tick`, `reapply_learned`, IPC handlers |
| IPC | `ipc.rs` `LearnDisplay*` methods, `Reply::LearnDisplay` / `GameDisplayFile`; mirrored in `ui/src/lib/ipc.ts`; Tauri commands `learn_display_*` |
| UI | `ui/src/components/LearnLookCard.tsx` on Games → Display |

### Analysis per frame (`analyse.rs`)

Gamma-encoded BT.709 luma Y′ on the 480×270 frame, letterbox bars excluded:
mean and median (APL), standard deviation, crushed fraction (Y′ < 0.05),
clipped fraction (Y′ ≥ 0.98), dark fraction (Y′ < 0.15), mean local gradient
among crushed pixels (*is there detail down there?*) and among dark pixels,
mean and P90 saturation, saturation-weighted 12-bin hue histogram, motion
(mean absolute change of a 64×36 block-mean grid against the previous sample).

Classification, in order: **Loading** (near-uniform) → **Cutscene**
(letterbox ≥ 8 % top and bottom) → **Outlier** (> 60 % clipped) → **Warmup**
(no previous sample) → **Static** (motion < 0.004: menus, pause, map, HUD
over a frozen world) → **Gameplay**; then **Idle** if no input for > 20 s.
Only Gameplay is learned from; the others are counted for the UI.

### Derivation (`derive.rs`)

Look (panel-neutral, each 0–1, rounded to 0.05):

- `shadow = (crush − 0.04) / 0.20`, but **0 if the crushed pixels are flat**
  (mean gradient < 0.004): true black is not hidden detail.
- `saturation = (0.35 − sat_mean) / 0.25`, but 0 if P90 saturation ≥ 0.85
  (a boost would clip colour).
- `highlight = (clip − 0.02) / 0.10`.

Realisation per panel (only controls Relay already drives):

| Panel | Shadows | Saturation | DDC/CI |
|---|---|---|---|
| OLED | gamma up to ×1.15 (code 0 stays 0: no black lift) | vibrance +8 max | never |
| IPS / VA / TN, verified black equalizer | black equalizer up to half its range + gamma up to ×1.10 | vibrance +12 max | one code |
| IPS / VA / TN, no verified code | ramp shadow lift up to 20 + gamma up to ×1.10 | vibrance +12 max | never |
| Unknown | gamma only, up to ×1.10 | vibrance +12 max | never |

Highlight caution scales the gamma boost down by up to 50 % (don't brighten
a game that already clips; on OLED, spare ABL). Brightness and contrast over
DDC/CI are **never learned** (`LEARNED_DDC_WRITES_MAX = 1`, black equalizer
only), see the restore budget below. Learned values are folded onto the
profile (gamma multiplies, shadow lift takes the larger, vibrance adds the
offset, a profile's own black-equalizer value wins), so the profile stays
the user's taste.

Panel type comes from the hardware library's panel field (EDID cannot say
OLED vs IPS; see `hardware/edid_color.rs`). The black-equalizer path is
inert today: no quirks row is a `VerifiedCode` with a level range, so
`panel_caps` returns `black_equalizer_max: None` for every model.

### Confidence and convergence (`converge.rs`)

> **S48** samples at 2 fps with 30 s checkpoints, two agreeing, and a
> confident one-scene budget of 4 min; see `S48-fast-learning.md`. The S47
> table below is kept for the record.

| Constant | Value | Meaning |
|---|---|---|
| `MIN_GAMEPLAY_FRAMES` | 600 | 10 min of gameplay at 1 fps |
| `MIN_SCENES` / `MIN_FRAMES_PER_SCENE` | 3 / 30 | APL buckets (edges 0.10, 0.25, 0.45, 0.65) visited |
| `CHECKPOINT_FRAMES` | 120 | a checkpoint every 2 min of gameplay |
| `CONVERGE_CHECKPOINTS` / `CONVERGE_TOLERANCE` | 3 / 0.05 | three checkpoints within 0.05 on every axis |
| `ROLLING_WINDOW_FRAMES` | 3600 | exponential window (~1 h) after that, to follow updates |
| `MEANINGFUL_CHANGE` | 0.15 | the applied look moves only for a change this big |
| `INPUT_IDLE_MAX_MS` | 20 000 | no input this long = not gameplay |
| `SAMPLE_FPS` / `look::MAX_FPS` | 1 / 2 | sampler rate and its ceiling |

A new game build (exe size + modified time, read from the file on disk)
relearns from scratch; the applied look stays in use until the new one
settles and differs meaningfully. Every constant has a test.

### HDR

The sampler checks the output's colour space (`IDXGIOutput6::GetDesc1`).
PQ/BT.2020 (`DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`) → it prints
`look_hdr` and exits; the card says "HDR was on. Relay only learns from SDR".
Analysing scRGB/PQ correctly would need a different crush/clip model and is
out of scope.

### Footprint

The always-on core gains only the pure maths (`relay_display::learn`, no
FFT, no capture) and a reader thread while the sampler runs. The sampler is
`relay-share look`, a separate process spawned on focus and killed (by its
own handle) on blur, exit, Reset, import, or core shutdown. Per sample: one
`AcquireNextFrame`, one GPU blt to 480×270 NV12, a ~190 KB readback and a
single pass over 129 600 pixels. Release footprint gate on this branch
(`scripts/footprint.ps1`, 2026-10-01): relay-core.exe 2.24 MB, idle RSS
8.2 MB peak, private working set 1.02 MB, idle CPU 0 % over 31 s — PASS.

### Restore budget (200 ms)

Learned looks use the gamma ramp and vendor vibrance only on every shipped
model (no learned DDC writes), so the restore path is the one the M2 live
pass measured: 144 ms mean (137–148 ms) for a profile with one DDC write,
of which the DDC/CI write is ~100 ms on its own and the ramp + NvAPI paths
are "far quicker" (`docs/plans/M2-display.md`, §3). If a profile otherwise
had no display change, the learned look adds one `SetDeviceGammaRamp` and
one vendor vibrance restore — GPU-side — and no DDC write, so the estimate
is well under the 144 ms figure. A second DDC write would put a profile at
~245 ms, over budget: that is why the learner is capped at one DDC code
(a future verified black equalizer) and only when the profile itself writes
none, and why brightness/contrast are never learned. **To measure in the live pass**
(step 6 below); not measurable without the owner's monitor.

## Tests (all synthetic; no screen capture, no live monitor/GPU change)

- `relay-display` `learn::tests`: dark scene with crushed-but-detailed
  shadows, flat black, bright scene (hue peak), washed-out scene, menu/static,
  loading, letterboxed cutscene (bars excluded from stats), white-out,
  BGR/RGB parity, short buffers, every classification constant, input idle;
  derivation thresholds, quantisation, OLED vs IPS vs VA (+verified BEQ) vs
  unknown, highlight damping, neutral → neutral, out-of-range input never
  extreme; state machine (frames, scenes, convergence, exclusions, NaN,
  freeze rule, apply needs convergence, build change, reset, rolling window,
  JSON round-trip); an end-to-end synthetic session.
- `relay-core` `learned_display::tests`: store round-trip and corrupt/future
  files, effective-look precedence, monitor switch re-fit across two panel
  capability sets, overlay maths, no DDC on any shipped model, import applies
  at once with learning off, fine-tuning an import under the same rules,
  export writes the current game layer, export/import round-trip with no
  monitor id or build fingerprint in the file, every file rejection, wrong
  game, status walk, exact notices, sampler wire lines, build fingerprint.
- `relay-core` `ipc::tests::learn_display_wire_shape`; `relay-share`
  `look_args`.
- UI `LearnLookCard.test.tsx`: off by default with the privacy line and the
  exact tournament notice (no dialog), enable, progress, Apply, Relearn,
  Reset (confirm), import → "Applied (imported)" with learning off and the
  fine-tune toggle, rejected import, export note, HDR, no game.

## Live test plan (owner present, LG OLED)

Run on the main Win11 PC with the LG 32GS95UE (WOLED). Mark the monitor's
panel as "WOLED" in the hardware library first. Owner at the keyboard
throughout; stop on anything unexpected.

1. **Baseline.** Profile for a dark game (one with night maps), display
   follow-focus on, no other display changes. `relay-core status` shows
   display Default. Note the OSD picture mode.
2. **Sampler cost.** Enable "Learn this game's look". Focus the game. In
   Task Manager, `relay-share.exe` appears; record its CPU (target < 1 % of
   one core) and the GPU engine column; record the game's frame rate with
   and without learning for 2 minutes each (target: no measurable change).
   Alt-tab out: `relay-share.exe` is gone within 1 s.
3. **HDR.** Turn Windows HDR on, focus the game: card says HDR was on;
   sampler exits. HDR off again.
4. **Learn.** Play ~15 minutes across dark and bright areas. Progress fills;
   skipped count rises in menus, cutscenes and when idle > 20 s. The card
   reaches Ready. Nothing on screen changed yet.
5. **Apply.** Press Apply; on next focus the game opens up shadows. Check a
   black frame stays black (OLED: no grey floor), check vibrance with the
   owner's eye. `Applied via` shows gamma ramp + NVIDIA.
6. **Restore budget.** Alt-tab out 10 times with the learned look applied;
   time restore with the M2 method (`core.log` timestamps around
   `restore`). Record mean and max; must stay under 200 ms.
7. **Freeze.** Keep playing 10 more minutes: the applied values do not move.
8. **Export → Reset → Import.** Export with a note; open the file and check
   it holds no monitor name, serial or path. Reset (profile back to as
   written). Import the file: "Applied (imported)", learning off.
9. **Crash.** With the look applied, `taskkill /F` the core **by PID**
   (never by image name); the next start restores the original ramp and
   vibrance.
10. **Anti-cheat titles.** Follow `docs/dev/anti-cheat.md` test plan step 2
    with learning on, on the owner's chosen alt accounts only.

Record results in this file under "Live results".

## Live results

Pending (owner's LG OLED pass).
