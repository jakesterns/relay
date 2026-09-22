# S38 — Stream resilience: crash record, auto-reconnect, resume

**Branch** `feat/stream-resilience` · **Worktree** `C:\Users\stern\Documents\Code\relay-resilience`

Jake, 2026-09-18 and again 2026-09-20: if Relay or a share dies, restore the
session by default rather than leaving the user to rebuild it; keep the crash
log; tell the user what is happening during a stream rather than degrading
silently. **On by default, off by a Settings toggle.** Closing the window exits
to the tray and keeps running, **with a notification-area message saying so**,
also toggleable.

Evidence it is needed (relay-pc2, 2026-09-20): the receiver twice ended a
share by itself with nothing on screen explaining why; the app kept a stale
"Paired" status after the share was dead; every stream-health number lived in
`share.log`. S31 fixed the last two symptoms; this is the cure for the first.

## What is already true, and shapes the design

- **S35 made reconnect possible without a code.** After one code pairing both
  PCs remember each other, and a remembered sender connects with no code —
  provided the receiver is on Start receiving (Jake's Option A). So resuming a
  share after a crash or reboot needs nothing typed, *as long as the receiver
  is listening*. That is the design's one hard dependency and it is why S35
  had to land first.
- **The core already re-spawns a share from its last request** (the
  Ctrl+Alt+S hotkey path: `Inner::last_share` → `spawn_share`). Reconnect is
  that, driven by an exit instead of a keypress, with backoff.
- **The receiver simply exits when its sender vanishes** (`gone_rx` →
  `render::run` ends → process exits 0). It does not go back to waiting. So
  the *core* re-spawns `relay-share recv` — with the **same pairing code**
  (`ReceiveRequest::code`), so a sender that was paired by code can still
  return with the code it already typed, and a remembered one needs none.
- **`ShareState` is `Off | Sharing{peer}`**; the UI reads it from
  `core://state`. It needs a `Reconnecting { peer, attempt }` arm.
- **The tray has no balloon support** — `NIF_ICON | NIF_MESSAGE | NIF_TIP`
  only. `NIF_INFO` + `NIM_MODIFY` adds one, on the winloop thread that owns
  the icon.
- **No binary installs a panic hook.** `share.log` / `ui.log` / `core.log`
  persist (S29), and the core sees the engine's exit code, but a panic's
  message and location go to stderr and are lost.
- **`settings.json` (`uiprefs.rs`) has one field**, `close_action`, with a
  versioned file and a Settings toggle. Two more fields fit the same shape.
- **The close path** (`ui/src-tauri/src/lib.rs` `CloseRequested`): keep-running
  just lets the window close. The notice has to come from the core, which is
  what stays alive.

## Design

### 1. Intent: `active-stream.json`
`crates/core/src/resilience.rs`. One record in the data root, written when a
share or a receive *starts by the user's hand* (UI, hotkey), removed when it
*stops by the user's hand* (Stop button, hotkey off, tray Quit) or when the
supervisor gives up.

```
{ "version": 1, "kind": "send" | "receive",
  "request": <ShareRequest | ReceiveRequest, minus `host`>,
  "peer": "<name>", "started_unix": …, "attempts": 0 }
```

`peer` is the receiver's name for a send. At resume time it is resolved
through `peers::Store` to a `peer_id` — so a share that began with a code
resumes with none, because the code pairing remembered the peer. If the name
is not in the store (Forgotten meanwhile), resume is impossible and says so.

The record is the difference between "the engine exited" and "the user
stopped it": a deliberate Stop clears the record **before** killing the
engine, so the exit that follows finds no intent and does nothing.

### 2. Supervisor
In `service.rs`, on the existing 1 s tick plus the engine pump:

- Engine exit (`ShareEvent::Exited`, any `ok`) with an intent still recorded →
  `ShareState::Reconnecting { peer, attempt }`, schedule the next attempt.
- Backoff **1 → 2 → 5 → 10 → 10 … s**, a pure `fn delay_for(attempt) ->
  Duration` with a unit test. Give up after **3 minutes** of failing: clear
  the intent, `Notice`, tray balloon, state `Off`.
- Each attempt is `spawn_share` / `spawn_receive` with the recorded request
  (send: `peer_id` resolved fresh, `code` empty; receive: same `code`,
  `host: None` — see §6). A successful `Connected` / `Paired` resets
  `attempts` to 0 and the state to `Sharing`.
- **Receiver first, then sender.** A receiver that lost its sender goes
  straight back to waiting on the next tick (nothing to back off from; the
  code is unchanged). A sender that lost its receiver backs off, because the
  receiver may be rebooting and mDNS will not find it for a while.

### 3. Resume after reboot / power loss
`Service::run` start-up, after the crash-restore of audio/display (which
comes first — it always has): if `active-stream.json` exists and
`prefs.resilience` is on, treat it as attempt 1 of the schedule above, and
say so (balloon: "Relay is restoring your share to <peer>"). Requires
autostart or the user opening Relay; either way the core acts on the record
when it starts. If the receiver's PC also rebooted, *its* record restarts
`recv`, so both ends come back without a hand on either.

### 4. Crash record
`std::panic::set_hook` in `relay-core run`, `relay-share`, and the Tauri
shell, writing `crash\<ts>-<binary>.txt`: panic message, location, thread,
and (core) the intent record if any. The core also writes one for a
non-zero engine exit (`crash\<ts>-relay-share-exit<code>.txt`, with the last
50 lines of `share.log`). On the next start the core emits **one** `Notice`
naming the newest file, then writes `crash\.seen`; nothing nags twice.

### 5. Loud, not silent — the vocabulary is `docs/dev/stream-notices.md`
- **Reconnecting**: Share screen's `Live` pill reads "Reconnecting (n)…",
  amber; Receive's video area reads "The share from <peer> dropped — waiting
  for it to come back…". Tray balloon once per episode, not per attempt.
- **Resumed without a click** (after a restart): balloon "Relay restored your
  share to <peer>", and the strip is visible when the window opens.
- **Gave up**: video area "The share from <peer> dropped and did not come
  back."; balloon; `Notice`. Rule 5 of the vocabulary: the recovery message
  clears, it never spins forever.
- **Window closed, core running**: balloon "Relay is still running here.
  Your profiles keep applying; right-click to stop." — every close, until
  the user turns it off.

### 6. The stream window on a receiver respawn
The recorded `ReceiveRequest` drops `host`: the HWND that embedded the
stream may belong to a window that no longer exists. The engine comes up in a
window of its own; if the app window is open, the shell already knows how to
embed (`Method::SetStreamMode { embedded, owner }`, S29) and does so when it
sees the `StreamWindow` event with mode `none`. Verified with the S29 harness.

### 7. Settings
`UiPrefs` gains `resilience: bool` (default `true`) and `close_notice: bool`
(default `true`), file version unchanged (serde defaults; the standing rule —
an update never resets anything — holds because a missing field reads as the
default). Two `Toggle`s under the close-action one, in the same voice.

## Definition of Ready
- [x] S35 merged (`0fa9776`): a remembered sender reconnects with no code.
- [x] `docs/dev/stream-notices.md` fixes the words; this session adds one
      state ("dropped, and Relay is reconnecting"), not a second vocabulary.
- [x] The core supervises the engine and sees its exit code; `last_share`
      exists; the 1 s tick exists.

## Definition of Done
- [x] `active-stream.json`: written on user start, cleared on user stop and
      on give-up, versioned, round-trip and migration tests
      (`crates/core/src/resilience.rs`, 7 tests).
- [ ] Reconnect in-session: kill the sender's `relay-share` **by its own
      PID** (never by image name — see the S30 note) and the share is back
      within one backoff step on a LAN, with no code typed. Same for the
      receiver's process. **Built; needs the two-PC run.**
- [x] Backoff is the stated schedule and gives up at 3 minutes; unit-tested
      as a pure function (`Episode`).
- [ ] Resume after a reboot of the sender, with the receiver on Start
      receiving; and after a reboot of both. **Built; needs the two-PC run.**
- [x] Every state in §5 is visible where §5 says, and clears (rule 5). One
      deviation, for the better: the Receive video area's "dropped — waiting"
      needed no protocol field — Start receiving by hand clears the last
      sender's name and an automatic restart does not, so the screen tells
      the two apart on its own.
- [x] A panic in any of the three binaries leaves a `crash\` file naming the
      binary, message and location; the next start says so exactly once
      (`crash.rs`, `CoreState.last_crash`, `CrashBanner`, `AckCrash`).
- [x] Two toggles in Settings, both default on, both honoured, both survive
      an update (a pre-S38 `settings.json` reads them as on; tested).
- [x] Closing the window shows the tray balloon when `close_notice` is on,
      and not when it is off (`Method::WindowClosed`; the shell's close path
      calls it, bounded to 500 ms).
- [x] Nothing here changes behaviour when `resilience` is off: an engine exit
      is reported as today and nothing respawns.
- [ ] All gates green (**yes**: fmt, clippy `-D warnings`, 292 UI / 214 core /
      176 capture, footprint); two-PC pass with relay-pc2 recorded in
      `BUGS.md` against the build hash — **owed**.

## Kickoff prompt
```
You are session S38 (stream resilience) for Relay. Read CLAUDE.md,
docs/plans/S38-stream-resilience.md (the design; follow it), docs/plans/SESSIONS.md
(section S38 and the standing rules), and docs/dev/stream-notices.md.

Your worktree already exists: C:\Users\stern\Documents\Code\relay-resilience on
branch feat/stream-resilience, cut from main with S35 merged. pnpm install in ui/.
RELAY_NO_INSTALL=1 for commits and pushes.

Build it in the order the design lists: the intent record and backoff (pure,
tested), the supervisor in the core, resume at start, the crash record, the tray
balloon, the two Settings toggles, then the UI states. Jake's decisions are not
open: on by default, off by a toggle; closing the window keeps running and says
so in the tray.

Never kill a Relay process by image name; kill only PIDs you started. Never
write the registry. Do not block on a question: take the most reversible option,
write down why, continue. relay-pc2 is a Claude session on Jake's second PC,
reachable with SendMessage (ListAgents); builds ship from main through the
main-tree session with commit, contents and SHA-256. Record every two-PC result
in docs/dev/BUGS.md against the hash. Finish by updating docs/ROADMAP.md.
```
