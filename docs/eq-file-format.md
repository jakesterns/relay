# Relay game EQ file (`relay-game-eq`, schema 1)

A game EQ file carries **one game's EQ layer** so people can share it: the
game's identity, the curve, and an optional note. It never carries the
headphone correction (that belongs to the listener's hardware, and Relay
applies the importer's own correction underneath), and never anything about
the person or PC that made it.

Reader and writer: `crates/audio/src/learn/file.rs` (`GameEqFile::parse`,
`GameEqFile::export`). Relay exports through the same validator it imports
with, so it never writes a file it would refuse.

## Example

```json
{
  "format": "relay-game-eq",
  "schema": 1,
  "game": { "exe": "game.exe", "name": "Some Game", "version": "1.2.3.4" },
  "curve": [[20, -4.0], [100, -3.0], [1000, 0.0], [3150, 3.0], [16000, 0.0]],
  "bands": [
    { "kind": "lowshelf", "freq_hz": 105.0, "gain_db": -3.9, "q": 0.7 },
    { "kind": "peaking", "freq_hz": 3150.0, "gain_db": 3.0, "q": 1.4 }
  ],
  "goal": "awareness",
  "note": "Quieter booms, clearer steps"
}
```

## Fields

| Field | Required | Meaning |
|---|---|---|
| `format` | yes | Always `"relay-game-eq"`. |
| `schema` | yes | `1`. A newer schema is refused with "made by a newer Relay". |
| `game.exe` | yes | Executable file name, e.g. `game.exe`. No folders, drive letters or wildcards. Import refuses a file whose exe is not the profile's. |
| `game.name` | no | Display name, at most 128 characters. |
| `game.version` | no | The game version the curve was made on (informational). |
| `curve` | one of `curve` / `bands` | Ascending `[hz, dB]` points, 2–64 of them, 20 Hz – 20 kHz, each gain from −9 to +6 dB. This is what Relay reads back. |
| `bands` | one of `curve` / `bands` | 1–8 `peaking` / `lowshelf` / `highshelf` filters (`freq_hz` 20 Hz – 20 kHz, `q` 0.1–10, `gain_db` −9 to +6). Written for other EQ tools; when there is no `curve`, Relay samples the bands' combined response on 1/3 octaves, and that combined response must also stay within −9 to +6 dB. |
| `goal` | no | `"awareness"`, `"dialogue"` or `"immersion"`: what the curve was made for. Adopted only if the importing profile has no goal yet. |
| `note` | no | The creator's note, at most 500 characters. |

## Validation (all refusals say why)

- The whole file must be at most 64 KiB and valid JSON.
- **Unknown fields are refused**, at the top level and inside `game` and each
  band — a file cannot smuggle extra data in.
- Every number must be finite; frequencies ascend; ranges as above.
- Control characters are refused in every text field (newlines allowed in the
  note).

## What happens on import

The layer applies at once, exactly as imported, and the profile shows
**Applied (imported)**. Learning is **off** for that game by default. The
"Keep learning to fine-tune for my setup" switch turns it on; once Relay's own
curve for this game has converged it offers the import moved halfway
(`IMPORT_BLEND` = 0.5) towards it, always measured from the original import
so repeated fine-tuning never drifts. Export writes whatever layer is applied —
learned, imported, or imported-and-tuned.
