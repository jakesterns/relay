# Game display file format

A *game display file* carries one game's learned look so it can be shared:
one person learns how a game looks, another imports it and gets the same
correction fitted to their own monitor. Introduced in S47.

The file holds the **game layer only**. It never names a monitor, a GPU, a
PC, a user or a path; the panel-specific part is worked out on the importing
PC from that PC's own monitor.

## Example

```json
{
  "format": "relay-game-display",
  "version": 1,
  "game": { "exe": "game.exe", "name": "A Game" },
  "look": { "shadow": 0.35, "saturation": 0.1, "highlight": 0.0 },
  "evidence": { "frames": 1440, "scenes": 4 },
  "note": "dark maps; learned on an OLED"
}
```

## Fields

| Field | Type | Required | Meaning |
|---|---|---|---|
| `format` | string | yes | Always `relay-game-display`. |
| `version` | integer | yes | `1`. A Relay refuses versions it does not read and says which one it found. |
| `game.exe` | string | yes | The game's executable **file name**, e.g. `game.exe`. At most 64 characters, must end in `.exe`, no `\`, `/` or `:` (no folder, so no user name can ride along). Stored lower-case. |
| `game.name` | string | no | A display name. At most 100 characters, no control characters. |
| `look.shadow` | number 0–1 | yes | How much crushed shadow detail to bring back. 0 = none. |
| `look.saturation` | number 0–1 | yes | How much saturation help a washed-out game wants. 0 = none. |
| `look.highlight` | number 0–1 | yes | How much the game already clips highlights; holds back brightening. |
| `evidence.frames` | integer | no | Gameplay frames the look was learned from. 0 for a hand-made or re-exported import. |
| `evidence.scenes` | integer | no | Distinct brightness levels ("scenes") seen. |
| `note` | string | no | The creator's note. At most 500 characters, no control characters except newline. |

Unknown fields anywhere are **rejected**, not ignored, so nothing such as a
monitor serial can be added and travel silently. Files over 16 KB are
rejected.

## What happens on import

- The look applies at once. The game's status reads **Applied (imported)**.
- Learning is switched **off** for that game. The user can turn on
  **Keep learning to fine-tune for my monitor**: Relay then learns under the
  normal readiness and convergence rules, starting from the imported look,
  and only replaces it when its own result differs by at least
  `MEANINGFUL_CHANGE` (0.15 on some axis).
- The file must be for the same game (`game.exe` matches the profile's exe,
  case-insensitive).
- The look is fitted to the monitor on that PC: OLED panels get gamma only
  (black stays black), IPS/VA/TN get a small shadow lift or a verified black
  equalizer level, unknown panels get gamma only. See
  `crates/display/src/learn/derive.rs::realize`.

## What export writes

The current game layer: the look in use (an applied learned look, the one
with the most evidence if several monitors have one; else the imported look),
or, if nothing is applied yet, the settled result waiting for Apply. Export
fails with a plain sentence if nothing has settled.

## Validation summary (enforced in `crates/core/src/learned_display.rs`)

| Rule | Rejection text |
|---|---|
| not JSON / wrong shape / unknown field | "this is not a Relay game display file" |
| `format` differs | "this is not a Relay game display file" |
| `version` ≠ 1 | "this file is version N; this Relay reads version 1" |
| bad `game.exe` | "the game must be an exe file name like game.exe, with no folder" |
| look value outside 0–1 or not finite | "the look values must be numbers from 0 to 1" |
| note too long / control characters | "the note is longer than 500 characters" / "the note contains control characters" |
| file for another game | "this file is for X, not Y" |
| over 16 KB | "this file is too large to be a Relay game display file" |

Relay's visual enhancements may not be allowed in some tournaments or
professional environments. Check with your tournament host or rules.
