# S54 — Gunshots that actually come down, and every change in numbers

Owner, 2026-10-24, after the first MW4 session on the main PC: gunshots were
still very loud with Spatial audio, Explosion tamer and the "Gunshots &
explosions" slider all on; the EQ graph did not show what was learned; the
display look made no visible difference and did not say what it changed.

## What we found (main PC, MW4)

- The learned dynamic stage had **one band: 63 Hz, explosions**. No gun band:
  Relay adds one only where shots stand clear of the footsteps, and in MW4's
  mix they did not. The slider scaled only the explosion rumble.
- Explosion tamer limits under 120 Hz only; Spatial audio repositions sound
  and, over MW4's own headphone mix, can blur direction. Neither touches the
  crack of a shot.
- The learned game EQ (−3.4 dB at 20–50 Hz, +2.4 dB at 2 kHz) was applied but
  the graph drew only the five manual bands (all 0 dB).
- The learned look (shadows 0.9, colour 0.3 on OLED) realises as gamma 1.14
  and vibrance 52: deliberately gentle, and the card showed only
  `gamma 1.14 · lift 0 · vibrance 52`.

## What changed

- **Gunshots strength** (`DynamicsTuning::gun`, `GameEqAction::SetDynamics.gun`).
  It scales learned gun bands; when none was learned it adds
  `learn::dynamics::gunfire_band`: 3 kHz, Q 0.5 (≈1.2–7 kHz), acting only above
  the program loudness (`DYN_REF_DB`), 6:1, 1 ms attack, 120 ms release, up
  to 6 dB at 100 % and 12 dB at 200 %. Explosions now follow `tame` alone.
  Unset `gun` (every earlier layer) behaves exactly as before.
  `GameEqStatus::dynamics_available` is now any game layer;
  `dynamics_learned` carries the stage before strengths.
- **Audio screen.** Three sliders — Gunshots, Explosions, Footsteps — each with
  a sentence from the band that runs: the range in Hz, the most it cuts or
  lifts, when it acts (dB relative to the game's average loudness), ratio and
  timing, and whether it was learned or added. The EQ graph draws your bands,
  the learned game EQ, the headset correction and **what you hear** (their
  sum), with a readout of every layer under the pointer; a table gives each
  layer at 60 Hz–12 kHz, and one sentence says what the learned EQ raises and
  lowers.
- **Display look.** `GameRecord::strength` (0–200 %, `Method::LearnDisplayStrength`)
  scales every change from neutral (`scale_adjustments`); the overlay's clamps
  still apply. The card lists each change in words with numbers (e.g. "Gamma
  1.00 → 1.14: a 5% grey shows as 7%, 10% as 13%, 20% as 24%"), explains the
  learned shadow/colour/highlight values, and points to **Ctrl+Alt+G** to
  compare in game.

## Owed (owner present)

- MW4: Gunshots 100 % then 200 %, own gunfire noticeably quieter, footsteps
  unchanged. Audio changes reach the APO only after MW4 reopens its stream.
- The APO diagnostic log had filled on 2026-10-21; it was rotated so the next
  stream records `LockForProcess` and the process-call count.
- Look strength 200 % on the LG: shadows visibly opened, black still black.
