# Verifying a vendor VCP opcode

Black equaliser and response time are not in MCCS. Every monitor vendor puts
them somewhere in the vendor-reserved range 0xE0–0xFF, and none of the vendors
Relay cares about publishes which code is which. So the opcode has to be
*verified*, and the only two things that count as verification are:

- **`Evidence::VendorDoc`** — published documentation, with the URL it was read
  from.
- **`Evidence::Osd`** — a named person watched the monitor's own on-screen
  display change while Relay wrote the code, on a named date, on a named panel.

Everything else is `Evidence::Unverified`, which means the control stays off.

## Why set-and-read-back is not proof

The obvious shortcut is to write a vendor code, read it back, and conclude the
monitor "supports" it. It does not follow. A panel will happily store and
return a value for a control whose on-screen meaning is something else
entirely — a picture mode, a PIP source, a factory ageing counter. Read-back
proves the *transport* worked, not the *meaning*. Meaning needs eyes.

The cost of being wrong is asymmetric. A greyed-out slider disappoints
somebody. A wrong write silently changes a setting the user never asked for,
and Relay then faithfully backs it up and restores it, so the user cannot even
tell which of their settings moved.

## How the code enforces it

`crates/display/src/vcp.rs`:

- `QUIRKS` is a static table of `ModelQuirks`, keyed on the EDID manufacturer
  id (`GSM`) plus the four-hex-digit product code (`5C7C`) — together, one
  model. A row with `product: None` is a manufacturer-wide fallback and may
  carry write timing only.
- Each vendor opcode in the table is a `Candidate` / `ResponseCandidate`: the
  code *plus its `Evidence`*. There is no way to write a row without evidence,
  because the field is not optional.
- `quirks_for_key` converts a candidate into a `VerifiedCode` via
  `VerifiedCode::attest(code, evidence)`, which returns `None` for
  `Evidence::Unverified`.
- `VerifiedCode`'s inner `u8` is private to its own module and `attest` is its
  only constructor. `plan_writes` and `vendor_controls` take `VerifiedCode`,
  not `u8`. So an unverified opcode cannot reach a `SetVCPFeature` call, and
  cannot enable a slider, no matter what the calling code does.

**Do not add a `From<u8>`, a public field, or a `new(u8)` to `VerifiedCode`.**
That single type is the whole guarantee.

Tests that hold the line (`cargo test -p relay-display`):

| test | what it pins |
| --- | --- |
| `unverified_table_entry_does_not_enable_the_slider` | the LG row has candidates for *both* controls, the panel advertises both, the profile asks for both — and nothing is written and no slider is enabled |
| `no_unverified_row_ever_resolves_to_a_code` | the same, for every row in the table |
| `vendor_wide_rows_carry_no_vendor_opcodes` | a `product: None` row cannot smuggle an opcode onto models nobody has looked at |
| `verified_entries_carry_traceable_evidence` | a doc entry has a URL, an OSD entry has an observer, an ISO date and a panel |
| `model_rows_sort_before_their_vendor_wide_row` | the fallback row never shadows a model row |

The core mirrors this: `hardware::vendor_controls` computes what the UI is
allowed to enable and sends it as `Reply::Hardware.vendor_controls`. The client
is told, never asked — a UI that inferred a control from the advertised opcode
list would defeat the whole thing.

## The OSD runbook

`crates/display/tests/vendor_probe.rs` sweeps one vendor opcode at a time and
holds each value long enough to read the OSD. It reads the original value
first, and skips any code it cannot read — without a read there is no restore.
It refuses anything below 0xE0, so a typo cannot land on 0x04 "restore factory
defaults". The original is written back after every code, including on panic.

1. Find the candidates. The panel's advertised list comes from the capability
   dump — `cargo test -p relay-core --lib live_ddc_caps -- --ignored --nocapture`
   — and the ones worth probing are the codes in 0xE0–0xFF, preferring those
   with an explicit value list, since the list length hints at how many OSD
   steps the control has.
2. Open the monitor's OSD on the page you want to watch **before** starting,
   and then keep your hands off the joystick: many panels NAK DDC/CI writes
   while the menu is being navigated.
3. Run one code at a time, so there is no ambiguity about what moved:

   ```powershell
   $env:RELAY_VCP_PROBE = "F5:1|2|3|4"
   cargo test -p relay-display --test vendor_probe -- --ignored --nocapture
   ```

   Syntax is `CODE:v1|v2|…`, comma-separated for several codes, hex code and
   decimal values. `RELAY_VCP_DWELL_MS` (default 4000) sets the hold, and
   `RELAY_VCP_GDI` (e.g. `\\.\DISPLAY1`) limits the sweep to one output.
4. Write down, per code: which OSD label moved, and which written value
   corresponds to which OSD level. Both halves matter — knowing 0xF5 is
   overdrive is no use if we do not know whether `2` means "fast" or "off".
5. Turn that into a row. An opcode whose OSD effect you could not see stays
   `Unverified`. That is not a failed run; it is the honest answer, and it is
   the state the table ships in until someone sees otherwise.

```rust
response: Some(ResponseCandidate {
    code: 0xF5,
    levels: &[("off", 1), ("normal", 2), ("fast", 3), ("faster", 4)],
    evidence: Evidence::Osd {
        observer: "Jake Sterns",
        date: "2026-09-14",
        monitor: "LG UltraGear OLED 32GS95UE (GSM 5C7C)",
    },
}),
```

## Current table

| Manufacturer | Product | Models | Black equaliser | Response | Evidence |
| --- | --- | --- | --- | --- | --- |
| GSM (LG) | 5C7C | WK95U, LG ULTRAGEAR+, 32GS95UE | 0xF6 | 0xF5 | **unverified** — candidates only, both controls stay off |
| GSM (LG) | any | — | — | — | timing only (60 ms write delay), measured 2026-09-13 |

Every other monitor falls back to `Quirks::default()`: a 50 ms write delay,
standard codes only.
