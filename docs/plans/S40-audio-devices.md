# S40 — Audio devices: follow the default, pick per track

Branch `feat/s40-audio-devices`, cut from `fix/r34` (b4c1120).

## Product rule

Relay uses the OS default audio input and output unless told otherwise. The
mixer lets the user pin a device-backed track to one endpoint. Nothing needs
Windows Sound settings, and Relay never changes the default.

## What was done

**Engine (`crates/capture`)**
- `devices.rs`: `DeviceSlot` (a track's pick: `None` = System default, with a
  generation counter), `DeviceSlots` (mic + output), a process-wide
  `IMMNotificationClient` watcher that counts `OnDefaultDeviceChanged` for
  `eConsole` per direction, and the pure `should_reopen` rule (the pick
  changed, or the track follows the default and the default moved; a pinned
  device ignores default changes).
- `audio.rs`: desktop loopback and the mic run in a reopen loop. Mid-share they
  reopen in place on a pick or a default change, and they also reopen after a
  lost endpoint, retrying every 500 ms. If a pinned device will not open, the
  track falls back to the default. `OpusStream` follows a change in rate or
  channel count between blocks. Process loopback (game, rest, call return)
  does not depend on an endpoint and behaves as before.
- `playback.rs`: the same reopen loop for render. The jitter queues and fader
  state carry across a reopen. `run` takes an `Arc<DeviceSlot>`.
- `command.rs`: `{"cmd":"device","track":"mic"|"output","device":<id>|null}`.
  The sender applies `mic` and `output` (the call return). The receiver
  applies `output` both while waiting and while playing, and ignores it while
  the virtual-mic route (`--mic-route`) is active.
- New flags: `send --mic-device <id> --output-device <id>` and
  `recv --output-device <id>`. With no flag, the track follows the System
  default.
- Every open, reopen, default change and pick is logged in `share.log`.

**Core**
- `Method::ListAudioDevices` returns `Reply::AudioDevices { render, capture }`,
  where each entry has an id, a friendly name and `is_default`.
- `Method::SetAudioDevice { side, track, device }` saves the pick in
  `settings.json` (`UiPrefs.audio_devices`: `send_mic`, `send_output`,
  `receive_output`), then forwards it to the running engine if there is one.
- `spawn_share` and `spawn_receive` start the engine on the saved picks.
- The Tauri commands are `list_audio_devices` and `set_audio_device`.

**UI**
- `MixerCard` takes a `devices` prop. On the sender, the mic row gets
  "Microphone input" and the call row gets "Call output". On the receiver,
  an "Output" row stands on its own.
- The first option in each list is "System default (<current name>)". A saved
  device that is unplugged is listed as not connected.
- The endpoint list is fetched again when a picker takes focus.

## Tests run
- `cargo test -p relay-core -p relay-capture --lib`: all pass. The new tests
  cover slot/reopen rules, command and IPC wire shapes, args and prefs.
- `cargo test -p relay-capture --bin relay-share`: flag parsing.
- `cd ui && pnpm exec tsc --noEmit && pnpm test`: 321 pass. Six of those are
  new picker tests.

No share was run and no audio was played. Everything below still needs a
live pass.

## Two-PC test list (owed)

Setup: sender on the Win11 main PC, receiver on the Win10 second PC. Run a
share with the mic track enabled and a call-return app. Use speech or music
as the source, or tones measured by FFT, per the automated-audio rule.

| # | Sender does | Receiver meters / checks |
|---|---|---|
| 1 | Start a share with nothing picked | Program and mic meters move. `share.log` on both ends shows "audio playback up device=default" |
| 2 | In Windows, change the default **output** mid-share | Program audio keeps flowing. The sender log has "OS default audio endpoint changed" then "audio capture reopened". The receiver's program meter has at most a short gap, not a stall |
| 3 | Change the default **input** mid-share | The mic meter keeps moving and now carries the new mic. The sender log shows the reopen |
| 4 | Mixer: set Microphone input to a specific non-default mic | The mic meter follows that device. Changing the Windows default input now does **not** move it |
| 5 | Mixer: set Microphone input back to System default | The mic follows the default again |
| 6 | Unplug the pinned mic mid-share | The share continues. The log shows the endpoint lost, then a fallback to the default. The mic meter resumes |
| 7 | Receiver: Output set to a second device (e.g. HDMI) | Audio moves to that device with no restart. `peak` in the receiver stats stays live |
| 8 | Receiver: Windows default output changed while Output = System default | Playback follows the new default |
| 9 | Sender: Call output set to a specific device with a call-return running | The call return plays there |
| 10 | Stop, then start a new share | The picks from 4, 7 and 9 are still selected (`settings.json`), and the engines start on them (look for `--mic-device` / `--output-device` in the core log) |
| 11 | Endpoint at 44.1 kHz or mono as the new pick | The conversion note is logged once. A/V sync holds (the receiver's latency figure does not drift) |

## Two-PC results

2026-09-29, r43 (`6985e20`), receiver = PC2, 1 kHz tone at amplitude 0.2, loopback meters:
- **Row 1 pass**: nothing picked, `audio playback up device="default"`, -14.0 dB on the default output.
- **Row 7 pass**: Output set to Realtek mid-share over IPC; reopened 56 ms after the command, tone moved from the Rodecaster to Realtek at -14.0 dB with no measurable gap, same relay-share pid, no reconnect.
- **Row 10 pass**: the next share opened straight on Realtek (`recv ... --output-device {...}`); setting System default again removes the flag after a core restart.
- Fixed after the run: the Output picker only showed while a sender was connected; it now shows whenever the Receive page is open.
- Sender rows (2-6, 9, 11) are owed: they change this PC's default devices, which the owner uses for other work.

## Not done / follow-ups
- A pinned device that is unplugged and plugged back in is not picked up
  again on its own. The track stays on the default until the user picks the
  device again or the share restarts. Fix: reopen on `OnDeviceStateChanged`
  or `OnDeviceAdded` for the pinned id.
- The desktop program mix follows the default output but has no picker. The
  task scope only asked for mic input, receiver output and call-return
  output.
- The receiver's call-return capture is a process loopback, not a device, so
  it has no picker.
- While the virtual-mic route is on, the receiver's Output pick is saved but
  not applied. The UI does not yet say so.
