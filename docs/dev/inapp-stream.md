# The stream inside the app (S29)

How a received share is shown in the Relay window instead of a window of its
own, why it is built the way it is, and how to check it on one PC.

## The shape

Three processes touch the picture:

| Process | Owns | Does |
|---|---|---|
| `relay-share recv --host <hwnd>` | the stream window (class `RelayReceiver`) and its styles | decodes and presents; changes hosting mode on command; keeps `WDA_EXCLUDEFROMCAPTURE` applied |
| `relay-core` | nothing | relays `host` commands down and `render_up` / `host` / `host_close` events up |
| `relay-ui` (Tauri shell) | the app window | fills in its HWND on `start_receive`; positions the stream window over the video area with `SetWindowPos`; hides it when the Receive screen is not showing |

The webview only measures. `Receive.tsx` reports the video area's
`getBoundingClientRect()` (CSS px, clipped to the viewport) through
`set_video_area` on mount, on every `ResizeObserver` tick and on unmount
(`null`). It never sees an HWND.

### Why an owned popup and not `WS_CHILD`

The kickoff suggested `SetParent` + `WS_CHILD`. That was rejected for one
reason: `SetWindowDisplayAffinity`, the guard that stops Relay capturing its
own output (B9), is honoured only on **top-level** windows and only from the
process that owns them. A child of the app window would need the app to
exclude its *whole* window from capture, and the app cannot do that for a
window another process owns either. So the stream window stays top-level in
every mode:

- **Embedded**: `WS_POPUP`, owned by the app window (`GWLP_HWNDPARENT`),
  `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`. Owned windows stay above their
  owner, hide when it minimises, and have no taskbar button, which is the set
  of behaviours a piece of the app window should have. `WM_MOUSEACTIVATE`
  returns `MA_NOACTIVATE`, so the keyboard never leaves the app; a click on
  the picture calls `SetForegroundWindow(owner)` so the app comes forward as
  it would for a click anywhere else in it.
- **Popped out**: `WS_OVERLAPPEDWINDOW`, unowned (so the app can be brought
  in front of it), `WS_EX_APPWINDOW`, sized to 90 % of the work area of the
  monitor the app is on. Close or Esc **re-embeds it on the spot**: the
  engine remembers the app window it came from, restyles itself in place
  (never hiding — see below), emits `host_close` then the `host` event, and
  the app moves it into the video area. If the app window is gone, close
  ends the receive. Nothing else ends the receive but Stop, the sender
  stopping, or the connection dropping.
- **Standalone** (`recv` from a console, no `--host`): the pre-S29 window.
  Close and Esc end the receive.

The engine reasserts the affinity after every style change and reports what
`GetWindowDisplayAffinity` says back — not what was asked — in the `host`
event as `excluded_from_capture`. The core logs a warning when it is false,
the shell passes it to the page, and the Receive screen says so.

### Since S50: capturable by default, and a clean feed

The affinity is no longer always on. The engine excludes the window only
while this PC is also sharing an area the window is on (the core sends
`{"cmd":"local_share","target":...}` on share start, source switch and stop,
and `--local-share` at spawn); otherwise the window is capturable so call
apps and OBS can pick it as "Relay — from <sender>". A change re-sends the
`host` event with the new `excluded_from_capture`. A fourth mode, **clean**
(`{"cmd":"host","mode":"clean","feed":"1920x1080"|"2560x1440"}`), is a
borderless unowned window of exactly that client size with the stream
letterboxed into a back buffer of the same size; Esc or close re-embeds it
like a popped-out window. Details and the two-PC plan:
`docs/plans/S50-share-and-go.md`.

### Why the shell positions and the engine styles

Position changes are continuous (every mouse move of a drag); style changes
are rare. A round trip through the core for each move would put the picture
visibly behind the frame. `SetWindowPos` on another process's window is an
ordinary Win32 call; the shell does it directly from its `Moved`, `Resized`
and `ScaleFactorChanged` handlers and from every page measurement. The shell
acts only on the engine's confirmed mode (the `host` event), and the engine
never hides the window across a mode change: a hide-then-show within one
DWM frame composes the window black (run 5), so a popped-out window is
restyled where it is and moved a moment later.

Both processes are per-monitor-DPI-aware (the engine sets
`DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` before creating its window), so
the coordinates they exchange are physical pixels and the swapchain is not
bitmap-stretched on a scaled display.

### Two threads in the engine

`render.rs` used to pump window messages on the thread that decodes and
presents. A modal move/size loop on that thread stalls the picture for as
long as the drag lasts, and once the window is inside the app, resizing the
app is that drag. Now `render::host::WindowThread` creates the HWND and only
pumps; `video_thread` decodes and presents through the HWND alone (the
swapchain). Rules that keep them from deadlocking:

- The window thread never waits on the render thread. Close/Esc/owner death
  set a flag and keep pumping.
- The render thread releases every D3D object *before* posting
  `WM_APP_SHUTDOWN`; DXGI may need the window's thread to answer a message
  during release.
- `MakeWindowAssociation(DXGI_MWA_NO_WINDOW_CHANGES | DXGI_MWA_NO_ALT_ENTER)`
  so DXGI does not hook the window procedure from the render thread.
- The render thread polls the AU channel (`try_recv` + 1 ms sleep) instead
  of blocking on it, so a stop always lands within a frame. The receiver now
  reads `stop` on stdin, and its teardown is bounded to 3 s, after which it
  prints `stopped` and exits — B8's hang (`pc.close()` outliving the AU
  sender) is still there underneath and still owed.
- The render thread rebuilds the swapchain after every mode change
  (`HostLink::bump_surface`), releasing the old one first: two swapchains on
  one HWND is `E_ACCESSDENIED`.

### If the app closes while embedded

Either Windows destroys the owned window with its owner (the engine sees
`WM_DESTROY`, ends the receive, the core reports it), or it does not, in
which case a 1 s timer in the engine notices `!IsWindow(owner)` and pops the
stream out so a frameless picture is never left floating over the desktop.
Which of the two Windows does was not established in S29; both are handled.

## Checking it on one PC

A real receiver on the sending PC captures itself (B9), so the mechanics are
exercised with a stand-in:

```
RELAY_RECEIVE_STUB=1 relay-core run          # StartReceive spawns `relay-share host-stub`
```

The stub prints the same `waiting` / `paired` / `codec` / `render_up` /
`host` / `host_close` lines, honours the same `host` and `stop` commands, and
paints a hue sweep with a marching bar at 1920×1080 in the real window so a
stall, a stretch or a tear is obvious. Nothing is captured.

Two harness scripts (neither sends input to the desktop):

- `scripts/webview-eval.mjs <port> <js>` — runs JavaScript in the app's
  page over the Chrome DevTools Protocol. Launch the shell with
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9223`.
  In-page `button.click()` drives the UI exactly as the jsdom tests do.
- `scripts/stream-window-check.ps1` — lists the shell and `RelayReceiver`
  windows with owner, styles, visibility and rect. `-Close` posts `WM_CLOSE`
  to the receiver window; `-MoveShell x,y,w,h` moves the shell
  (`powershell -Command "& 'scripts/stream-window-check.ps1' -MoveShell 300,200,1500,950"`
  — `-File` cannot parse the array).

Run everything under `RELAY_INSTANCE=<name>` so it sits beside the installed
core. Note the engine still writes `share.log` under the default data root,
not the core's `--data-dir`.

### What was verified locally (2026-09-17, stub, dev box)

| Check | Result |
|---|---|
| Start receiving with the Receive screen open | window created embedded, hidden, then shown at the letterboxed rect: shell client origin + area + `fit()` offset, to the pixel (`229,375 738x415` for a 738×670 area at DPR 1) |
| Owner / styles while embedded | owner = shell HWND, `WS_POPUP`, no caption, `WS_EX_NOACTIVATE`, `WS_EX_TOOLWINDOW`, `excluded=true` |
| Pop out | unowned, captioned, `1936x1119` centred on the app's monitor, `excluded=true` |
| Close the popped-out window (`WM_CLOSE`) | `host_close` → shell requests embedded → back at the same rect, `excluded=true` |
| Resize the shell to 1500×950 and to 1200×760 | stream re-fitted each time (`942x530`, then `642x361`), still centred |
| Navigate to Profiles / back to Receive | hidden / shown; page state (paired, code, codec, Stop button) restored on return |
| Stop receiving | window gone, video area reads "The share from host-stub ended." |
| Present rate during moves | `stub presenting presented=` kept climbing at ~240/s |

Not verifiable on one PC: a real modal drag of the app window (needs a
mouse), real decode, audio, and the second PC's Windows 10 build. Those are
the two-PC pass in `BUGS.md`.

## What the second PC taught (r5 -> r10, 2026-09-17/18)

Three rules, each learned from a black video area that the stub could not
show because nobody could see the stub:

1. **Never hide-then-show the stream window within a DWM frame.** The
   engine used to hide on re-embed and the app showed it 2-3 ms later; DWM
   composed the window black until something (minimise/restore) made it
   rebuild the visual. The window now keeps its visibility through a mode
   change: a popped-out window is restyled in place and moved.
2. **Release the old swapchain before creating a new one on the same
   HWND**, or `CreateSwapChainForHwnd` fails with `E_ACCESSDENIED`. The
   swapchain is rebuilt after every mode change (`HostLink::bump_surface`),
   on the render thread, after `ClearState` + `Flush`.
3. **The engine cannot take the foreground.** `SetForegroundWindow` is
   refused to a process without the last input, so the popped-out window
   sat behind the app and Esc went to the app. The shell, which has the
   input, hands the foreground over on the popout event.

And two about the process: the receiver's teardown is bounded to 3 s so the
share end always reaches the app (B8's hang is still there underneath), and
the shell now writes `logs\ui.log`, without which run 4's "the app placed
it correctly, the pixels are wrong" could not have been said.
