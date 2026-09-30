# S41 — Listening devices per output

Branch `feat/s41-listening-devices`, cut from `fix/r34`.

## Why

People plug many things into one output: a RODECaster feeding IEMs one day and
a headset the next, a home theatre on HDMI, desk speakers on the motherboard
jack, a gaming headset whose vendor app runs its own EQ. Windows only sees the
output device (the endpoint), never what is plugged into it. The M1 model
bound one library headset to an endpoint key, so headphone correction assumed
one headset per output. It also assumed Relay was the only thing shaping the
sound.

## Design

**Model** (`crates/core/src/hardware/listening.rs`, persisted in `hardware.json`
as `listening`):

- Per output, an ordered list of `ListeningDevice`s: `Headset { id }` (a
  library headset, usually added from the AutoEQ catalogue) or `Speakers`
  ("Speakers / home theater", no correction), plus the user's `active` pick.
- `EndpointListening::active()`: one entry is active by itself; with several,
  the pick counts only while it is still listed. No pick means *no* correction
  — Relay never guesses between two headsets.
- Key: the endpoint key, except when several endpoints in the probe share it
  (all endpoints of one USB device share a container key — a RODECaster's
  System/Chat, a headset's game/chat), then `key#friendly name`. Endpoints
  Windows splits by jack (Headphones/Speakers) already have their own keys.
- `HardwareStore::connected()` resolves the default output's *active*
  listening device to `ConnectedHardware::headset`. Outputs with no list fall
  back to the M1 endpoint binding, so existing libraries keep working.
  Removing a headset from the library drops it from every list.

**Selection and correction**: `profiles::select()` is unchanged in shape —
it consumes `ConnectedHardware::headset`, which now follows the active
listening device (Speakers → only "Any" rows match). The correction curve is
chosen by `audio_bridge::correction_curve()` (profile's headset, else the
active listening device); `service::correction_for` uses it.

**Quick switch**:
- IPC `SetListeningDevices { endpoint, devices }` and
  `SetActiveListening { endpoint, device }` (mirrored in `ui/src/lib/ipc.ts`,
  Tauri commands `set_listening_devices` / `set_active_listening`).
- Tray: when the default output lists more than one device, the menu shows
  "Listening on" with each one, the active one checked. The service publishes
  a snapshot (`tray::set_listening_menu`) whenever the hardware view is
  rebuilt; a pick re-checks against the live list.
- Hotkey `CycleListening` (Ctrl+Alt+L), off by default, enabled by
  `UiPrefs::cycle_listening_hotkey` (Settings toggle); registered at core
  start, so it takes effect on the next start.
- Every switch saves, rebuilds the view and reselects, so the profile and the
  correction change at once.

**Other processing, read-only** (`hardware/other_processing.rs`):
- Each endpoint's FX store is read with `relay_apo::livereg::LiveRegistry::read_fx_store`
  (endpoint GUID from the probe: `EndpointInfo::fx_guid`). Effect CLSIDs in
  `{d04e05a6-…},n` values are classified Relay / Microsoft (small table) /
  Other; Other is named from `HKLM\SOFTWARE\Classes\CLSID\{x}` (read-only) or
  shown as "Another audio effect". Processing-mode lists are not effects.
- Running processes (toolhelp snapshot) are matched against `VENDOR_APPS`:
  SteelSeries Sonar/GG, Razer Synapse, THX, Nahimic, Dolby Access/DAX,
  Realtek Audio Console, Logitech G HUB, Corsair iCUE, RODE Central/RODECaster
  app, Voicemeeter, FxSound. A process cannot be tied to one output, so these
  are reported on every output and worded as "may also be processing".
- Result: `HardwareView::other_processing` — per output, a list of
  `{ name, kind: apo|software, advice, clsid? }`. Rescanned on probe, on
  device change and on `ListHardware`. Nothing is ever modified or disabled.

**UI**: Profiles → side column → "What are you listening on?" card
(`ui/src/screens/Listening.tsx`). Per output: listed devices with Use /
In use and Remove, a library search to add (plus the speakers entry), a line
when several are listed but none is picked, other-processing notices, the
interface-DSP note (a RODECaster's own EQ comes after Relay; set it flat), and
"Listing what you listen on changes nothing in Windows." Existing classes and
tokens only; the four new CSS rules are spacing.

## What was done

- Core: `listening.rs`, `other_processing.rs`, `EndpointInfo::fx_guid`,
  store/view fields, `connected()` via the active device,
  `audio_bridge::correction_curve`, IPC methods, service handlers
  (`rebuild_hardware_view`, `pick_listening`), tray entries, cycle hotkey,
  `UiPrefs::cycle_listening_hotkey`.
- Tauri: two commands.
- UI: `ListeningCard`, Settings toggle, TS mirror types and helpers
  (`listeningKey`, `activeListening`), fake-core commands.
- Tests: Rust — model rules, persistence and removal, selection following
  the active device, correction choosing the active device (speakers → none),
  CLSID classification and FX-store extraction, vendor table, per-output
  assembly, IPC wire shape, tray ids, hotkey off by default. Vitest — add from
  search, speakers entry, switch active, remove, notices, helper parity.

## Not done / follow-ups

- The Microsoft CLSID table is small; an unlisted inbox effect shows as
  "Another audio effect" (over-report, never a hidden vendor). Grow it from
  the live pass.
- Vendor exe names in `VENDOR_APPS` are from public knowledge, not verified on
  this rig except where noted in the live pass below.
- The hotkey needs a core restart after toggling.
- No per-output advice for vendor apps that create their own virtual outputs
  (Sonar): the finding appears on every output.

## Live test list (Jake, two-PC rig; no audio played by the session)

1. Profiles → "What are you listening on?" lists every active output, the
   default first with "Default". Nothing in Windows Sound settings changes.
2. RODECaster output: add HD 560S and Blessing 3 from the library search.
   With two listed and none picked, the line "Pick the one you are using…"
   shows and `relay-core status --json` has `hardware.headset: null`.
3. Use → HD 560S: "In use" moves, `hardware.headset` = `hd560s`, a CoD row
   bound to HD 560S becomes active on focus. Switch to Blessing 3: the other
   row takes over without a focus change.
4. Add "Speakers / home theater", Use it: only "Any" rows match; profile with
   headset correction applies with no correction bands (check the APO shm
   params or `relay-preview` render).
5. Tray right-click on the RODECaster output shows "Listening on" with the
   three entries, the active one checked; picking one switches and shows the
   notice.
6. Settings → enable Ctrl+Alt+L, restart Relay; the hotkey cycles in list
   order and shows a balloon. Disabled by default on a fresh settings file.
7. Other processing: with SteelSeries GG/Sonar running, every output shows
   "SteelSeries Sonar is running and may also be processing this output…".
   On the Win10 PC with Realtek/Nahimic enhancements, the Realtek output lists
   the vendor APO by its registered name. Relay's own APO (if installed) and
   Microsoft effects are not listed. Confirm no registry value changed
   (`reg export` of the FxProperties key before/after).
8. Headphones/Speakers split by jack on onboard audio: each endpoint has its
   own list.
9. Remove a headset from the library: it disappears from every output list.
