# S50 — Share and go

Branch `feat/s50-share-and-go`, cut from `feat/s46-learned-game-eq` (50167bb).

## Goal

The owner's words: users "share and go". On the receiving PC, the stream
Relay is showing should be pickable like any other window in Discord's Go
Live, Zoom's, Teams' and Meet's screen share, and OBS Window Capture, and
carry sound where the app shares a window's or an app's sound. No virtual
device needed for that path; Relay Camera stays as the webcam path (picture
only).

## What changed

1. **Capturable by default.** B9 made the stream window invisible to every
   capture, always (`WDA_EXCLUDEFROMCAPTURE`), which also hid it from every
   share picker. The guard is now conditional: the window is excluded only
   while *this same PC* is sending a share whose area the window is on — a
   shared display or region it overlaps, or the stream window itself as a
   window share. The decision is `render::placement::should_exclude` (pure)
   and is re-taken when the core reports a local share starting, switching
   source or stopping (`local_share` engine command, `--local-share` at
   spawn), after every move or resize of the window, on a display change,
   and after every hosting change. The core tells the receiver *before* the
   share engine is spawned, so the first captured frame is already clean,
   and says "not sharing" only after the share engine has stopped. When the
   exclusion is on, the Receive screen says: "Hidden from screen capture
   while this PC is also sharing its screen." `RELAY_NO_CAPTURE_EXCLUDE`
   still means "never exclude" (test only, B16 meter).
2. **A title pickers can show.** `Relay — from <sender>` (control characters
   dropped, names over 48 characters cut), `Relay — receiving` before a
   sender is known. Class `RelayReceiver` in every mode (OBS remembers a
   Window Capture source by title, class and exe).
3. **Clean feed.** A third hosting mode, `clean`: borderless, no Relay
   chrome, unowned with a taskbar button, a fixed client size of 1920×1080 or
   2560×1440 that never changes, centred on the monitor the stream came from
   (pinned top-left when larger than the monitor; it keeps its size). The
   render thread rebuilds the back buffer at that size and letterboxes the
   stream into it (`placement::letterbox`, video-processor dest rect, black
   bars), so a call app never rescales or crops. Esc or close re-embeds it;
   a drag anywhere moves it. The Receive screen's **Share to a call** opens it
   at 1920×1080; a size chip switches live.
4. **Call presets.** Built-ins `discord` (1920×1080 @ 60, 20 Mb/s, system
   audio) and `discord-720` (1280×720 @ 30, 8 Mb/s). Call apps re-encode
   whatever they capture and none sends above 1080p60, so a 4K or native feed
   only costs bandwidth and a downscale. `presets.json` goes to version 2; a
   version-1 file gains the two once, then a deletion sticks.
5. **Guide.** A "Use with Discord / Zoom / Teams / Meet / OBS" card on the
   Receive screen: pick the window "Relay — from …" (full quality plus the
   sound where the app shares it), one line per app, or Relay Camera as the
   webcam (picture only).

Embedded mode is unchanged (`WS_EX_TOOLWINDOW`, owned): it is part of the
app window and is not offered to pickers. The pick-me paths are the clean
feed and the popped-out window.

## Tests (one PC, no window)

- `relay-capture`: `render::placement` — capture decision (same monitor,
  other monitor, straddling, region, window share, unknown target, empty
  window), start / switch / move / stop transitions, title formatting, clean
  feed size and position (incl. bigger than the monitor), letterbox;
  `render::host` mode/feed round trip through `wparam`; `command` wire shapes
  for `host` + `feed` and `local_share`; `relay-share recv --local-share`.
- `relay-core`: `share::local_share_sync` once-per-change, `source_target_of`,
  `--local-share` on `recv` and `host-stub` lines and not client-settable,
  wire shapes matching the engine; `presets` call preset values, existing
  presets untouched, v1 → v2 migration.
- `relay-ui` shell: the foreground goes to a new own window once, not on a
  repeated `host` event.
- UI (Vitest): guide content and title mirror, guide follows the paired
  sender, capture note on/off with the guard, Share to a call → 1080p clean
  feed → size change → back, call preset values and note.

## Two-PC test plan

PC1 = JAKE (Win11 main, sends). PC2 = second PC (Win10, receives, runs the
call apps). Never run a windowed receiver on PC1 (it captures the owner's
screen).

Setup: PC2 Start receiving; PC1 shares its desktop with the **Discord**
preset; PC2 Receive screen shows the stream; press **Share to a call**.

| # | Check | Pass |
|---|---|---|
| 1 | PC2 `stream-window-check.ps1` | `RelayReceiver`, title `Relay — from JAKE`, unowned, no caption, client exactly 1920×1080, `excluded=false` |
| 2 | Discord Go Live → Applications | lists "Relay — from JAKE"; a viewer on a third device sees a sharp 1080p picture, no Relay chrome, and hears PC1's audio |
| 3 | Zoom → Share Screen | window listed; with Share sound ticked the far end hears PC1 |
| 4 | Meet in Chrome/Edge → Present → A window | window listed by title; picture sharp (no sound expected) |
| 5 | Teams → Share → Window | listed; Include sound carries PC1 audio |
| 6 | OBS Window Capture | `[relay-share.exe]: Relay — from JAKE` listed; source is 1920×1080; Application Audio Capture on the same window has signal |
| 7 | Size chip → 2560×1440 | OBS source becomes 2560×1440 and stays there; stream letterboxed, not stretched |
| 8 | Esc on the clean feed | back in the app's video area, Receive screen shows "Share to a call" again |
| 9 | B9 still holds: while receiving, PC2 also shares its own display (the one the clean feed is on) to PC1, where PC1 runs only a headless receiver (`relay-share recv --headless`, no window) | Receive note "Hidden from screen capture while this PC is also sharing its screen"; `stream-window-check` reads `excluded=true`; OBS on PC2 shows the window black; PC2's screen does not smear |
| 10 | Move the clean feed to PC2's other monitor (if any) while 9 runs | note disappears; window capturable again; back onto the shared monitor → hidden again |
| 11 | Stop the PC2 share | note disappears within a second; Discord/OBS see the picture again |
| 12 | PC1 share with **Discord 720p30** | PC2 stats read 1280×720 @ ~30 fps, ~8 Mb/s; the 1080p clean feed letterboxes nothing (16:9) and upscales cleanly |

### Automating the OBS rows (obs-websocket)

OBS 28+ ships obs-websocket v5 (Tools → WebSocket Server Settings, port
4455). On PC2, a small script (Node `obs-websocket-js` or Python
`obsws-python`) can, without touching the mouse:

1. `CreateInput` kind `window_capture` in a scratch scene, then
   `GetInputPropertiesListPropertyItems` for `window` — assert an item whose
   name contains `Relay — from JAKE` (row 6, title and class).
2. `SetInputSettings` with that item's value, wait 1 s,
   `GetSourceScreenshot` (PNG, full size) — assert width×height is
   1920×1080 (row 1/6), then 2560×1440 after the chip (row 7), and that the
   image is not uniformly black (sharp picture present; for row 9 assert it
   *is* black while PC2 shares its own display).
3. `CreateInput` kind `wasapi_process_output_capture` on the same window and
   read `InputVolumeMeters` events for a non-zero level while PC1 plays
   audio (row 6 sound).
4. Clean up with `RemoveInput`.

Discord, Zoom, Teams and Meet have no comparable local API; those rows stay a
human check (a second account or device watching the call).

## Not done / owed

- The two-PC pass above.
- Embedded mode stays out of pickers by design; if users want to pick the
  in-app picture directly, that is a separate decision (it would give the
  embedded window a taskbar/Alt+Tab presence).
