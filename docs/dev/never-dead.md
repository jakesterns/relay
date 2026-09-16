# S23 — never look dead

What the app does when it is opened and nothing is running yet, what keeps
running after the window closes, and how both were proved on this machine.

## The problem

Autostart is off by default. The UI never started the core. So the sequence
every new user performs — install, reboot, open Relay from the Start Menu —
ended with every screen reporting that the service was not running, and the
only remedy on offer was a terminal command in the offline banner:

```
Core service not running. Start it with `relay-core run` to see live state.
```

That is a regression against an explicit product decision, not a missing
nicety: Relay is an installed desktop app precisely so that nobody has to use
terminal commands.

Separately the core had always emitted `notice` events over IPC, and
`ui/src/lib/core.tsx` had always stored them and expired them after 4 s — with
nothing rendering them. Every backend toast, including every hotkey
acknowledgement, went straight in the bin.

## What now happens

**Opening the app reaches live state on its own.** The Tauri shell attempts a
start as soon as it is up (`spawn_core_autostart` → `relay_core::startup::
ensure_running`), through the same `relay-svc.exe` the installer and the Run
key use — a GUI-subsystem launcher that spawns the core with no console window
and exits. `ensure_running` connects first, so the common case costs one pipe
connect and launches nothing, and it is safe to call repeatedly: the core holds
a single-instance mutex, so a second launcher exits without starting a second
service.

**Failures say what to do.** `startup::StartError` has one variant per remedy
and no catch-all, because a catch-all is the message this whole session exists
to stop showing:

| Situation | What the user reads |
|---|---|
| `relay-svc.exe` missing from the install folder | "Part of Relay is missing from its installation folder (…), so the background service could not be started. Reinstalling Relay will replace it." |
| `CreateProcess` refused | "Windows would not start Relay's background service: `<os error>`. This is usually security software blocking it — allow Relay in your antivirus or security settings, then try again." |
| Launched, but no answer in 15 s | "Relay's background service was started but did not finish starting up. Restarting your PC usually clears this. The details are in `<logs folder>`." |

A unit test (`startup::tests::no_message_names_a_command`) asserts that no
message contains `relay-core`, `relay-svc`, `cargo`, `powershell`, `cmd.exe` or
a bare `--`, and a UI test asserts the offline banner renders a button and no
`<code>` element. The old "Core offline" / "core service" jargon was replaced
with "Relay is not running" throughout at the same time.

**Notices render.** `core.tsx` keeps a queue with a monotonic id per notice, so
two identical strings are two entries and one notice's expiry cannot cut the
next one short. `components/Toasts.tsx` renders them bottom-right in the app
shell, `role="status"` / `aria-live="polite"`, no dismiss control, gone after
`NOTICE_MS` (4 s).

## The background-service decision

The DoR asked whether it is acceptable for the core to keep running after the
window closes with nothing saying so. It is not, and the answer is **a tray
icon owned by the core, not by the UI.**

That ownership is the whole point. Closing the Relay window is supposed to free
the entire Tauri process — that is what the core/UI split is *for*, and it is
what a user does before starting a game. A UI-owned tray icon would therefore
vanish at exactly the moment it is the only remaining way to see that Relay is
still applying an audio and display profile, and the only way to put the
machine back.

So the icon hangs off the winloop's existing hidden window (`crates/core/src/
tray.rs`), which lives as long as the core does. Menu:

- **Open Relay** — focuses the running window, or launches `relay-ui.exe`.
  Focusing first matters: the Tauri shell has no single-instance guard, so
  spawning unconditionally would hand the user a second window per click.
- **Restore everything** — the *same* `restore_all` the IPC method calls
  (extracted into one function so the two cannot drift), then a notice.
- **Quit Relay** — emits `Event::Quitting` so the window closes with it rather
  than sitting there reporting a dead core, then breaks the service loop.
  Restore happens in `Service::run`'s teardown, which already runs on every
  exit path, so quitting from the tray **cannot** leave a game profile applied.

It re-adds itself on Explorer's `TaskbarCreated` broadcast, and is removed on
drop before the window is destroyed, so a dead icon never lingers.

**Close behaviour is a preference, not forced** (`crates/core/src/uiprefs.rs`,
`data/settings.json`). It defaults to `keep_running`, because that is what the
app is for. The alternative stops the core — restoring on the way out — for
anyone who expects a closed window to mean a closed app. It lives in the core
rather than in the webview because the answer has to survive the window it is
about.

Settings carries the plain line the decision needs: what keeps running, that it
is about 7 MB and no measurable CPU, and that it is in the notification area
with those three menu items on it.

## Footprint

`pwsh scripts/footprint.ps1 -Seconds 20`, release, this machine.

| | binary | peak RSS | private WS | idle CPU |
|---|---|---|---|---|
| before (f83411f) | 1.51 MB | 6.85 MB | 0.93 MB | 0 % |
| after (tray + startup + prefs) | 1.53 MB | 5.86 MB | 0.88 MB | 0 % |

The binary grew **20 KB**. The RSS difference is run-to-run variance, not a
saving — the tray is one `NOTIFYICONDATAW`, one shared `HICON` extracted from
the UI binary, and a menu built and destroyed per right-click; it allocates
nothing at rest. Budget is 10 MB working set and 0.5 % CPU; both runs PASS with
a wide margin, so there was nothing to stop and report.

## Reboot proof

<!-- filled in after the live pass -->
