# S19 — Call-audio return route and mix-minus

**Branch** `feat/mix-minus` · **Worktree** `C:\Users\stern\Documents\Code\relay-mixminus`

Started 2026-09-23 as the first v1.1 item, because every v1 session is
built and only live testing remains. Jake's brief lists it under v1.1 and
S2 left the seam for it on purpose (`docs/dev/dual-audio-decision.md`).

## The problem
The gaming PC (sender) ships game + microphone to the call PC (receiver),
which plays them into Discord/Zoom/Meet through the virtual mic. The person
at the gaming PC cannot hear the *call*: the other participants' voices
exist only on the receiving PC. Today they wear a second headset or run the
call on the gaming PC too, which is what Relay exists to avoid.

**Mix-minus** is the broadcast name for the rule that what comes back must
not contain what you sent: the gaming PC must hear the others but not its
own game or voice again, and nothing it hears must go back out to the call.

## Design

### What comes back
One more Opus track, this time from receiver to sender:
`relay-audio-return`. Its source is **the call application's own output**
— WASAPI process loopback of the call app's PID, the same
`AudioSource::Process { pid }` the sender has used for game-only audio
since M4. The call app plays only the *remote* participants; the local mic
is never played back locally. So the return track is remote voices and
nothing else, and the first half of mix-minus (no game, no own voice
coming back) holds by construction, not by cancellation. The receiver
picks the app the way the sender picks a game: the existing process
picker (`Method::ListProcesses`).

Not the receiver's desktop mix: that would include Relay's own playback of
the game and the mic — the loop mix-minus forbids.

### Wire
The sender's offer gains one **`recvonly` audio transceiver** on every
share, whether or not the receiver will use it; an unused m-line costs
nothing. The receiver, when the return route is on, adds its Opus track
before `create_answer`, and webrtc-rs binds it to that transceiver (the
answer's side is `sendonly`). An older sender has no such m-line; the
receiver checks the offer for one and, finding none, logs "the sender's
Relay predates the return route" and carries on without — never an answer
with an extra m-line, which would fail the whole share. An older receiver
ignores the transceiver. Both directions of version skew are a working
share.

### Hearing it on the gaming PC
The sender's `on_track` gains an audio arm (it has only ever received
video events and RTCP): the return track's packets go to a `DecodedStream`
and the existing `playback` render loop, which so far has only run on the
receiver, to the default render endpoint — what the user is wearing. One
fader, **Call**, on the Share mixer (gain, mute, ramped like the others),
and a meter in the strip. Stats: `return_packets`, `return_peak_milli`.

### The second half of mix-minus
What the gaming PC hears must not go back out. Whether it does depends on
what the sender is *capturing*:

| sender's audio source | return audio is in it? | outcome |
|---|---|---|
| `Process { game }` (a Game preset) | no — the game's tree only | clean mix-minus |
| `Process { game }` + rest (S37) | **yes** — rest is everything but the game, which includes Relay's own playback | echo of the call back into the call |
| `Desktop` | **yes** | echo |

Relay cannot exclude two process trees from one loopback activation
(`Rest` already spends the one exclusion on the game), and there is no
echo cancellation in Relay and will not be. So the rule is stated rather
than hidden: the return route **works cleanly with app-only capture**, and
when the share also carries the rest or the desktop mix, the Share screen
says in one sentence that the call is going back out with it and suggests
sharing the game alone. No silent muting, no automatic reconfiguration —
the user chose the sources.

### Service and UI
- `ReceiveRequest.return_pid: Option<u32>` (serde default `None`; an old
  `settings.json` reads as off — the standing rule). The Receive screen
  gains a "Send the call back" control: a process picker with the note
  "pick the call app (Discord, Zoom…) — the other PC hears the others,
  never itself". Persisted so the next Start receiving keeps it.
- `recv --return-pid <pid>`; the service adds it from the request, never
  the client.
- `FaderSet` gains `call: Option<FaderLevel>`; `MixerSide::Share` rows
  gain Call when `return_packets > 0`. `ShareStats.return_*`.
- Presets are the *sender's*; nothing changes there.

### Not in scope
- A return route when the receiving PC is not running a call app (nothing
  to return).
- Echo cancellation of any kind.
- Return *video* (seeing the call). Different feature, different cost.

## Definition of Ready
- [x] S2's two-track seam and S37's three-stream playback exist; the
      receiver's decode → `mix_sum` path is per stream.
- [x] `AudioSource::Process` capture is proven (M4, S37) and the process
      picker exists (`Method::ListProcesses`).
- [x] The sender has never *received* an audio track: `on_track` in
      `sender.rs` must grow the arm, and `playback::run` must accept a
      list of streams rather than exactly three. Both are refactors with
      existing tests to keep green.
- [x] Version skew both ways ends in a working share (the m-line check
      above), decided before code.

## Definition of Done
- [ ] Receiver: `--return-pid`; the Opus track is added only when the offer
      carries a `recvonly` audio m-line; otherwise a log line and a normal
      share. Tests: args; the m-line check on raw SDP (both cases).
- [ ] Sender: the `recvonly` transceiver on every offer; the return track
      decoded and played on the default render endpoint; the Call fader;
      `return_packets` / `return_peak_milli` in stats. Tests: the offer
      contains the m-line (SDP inspection); fader wiring.
- [ ] `playback::run` takes `Vec<(Receiver, Track)>`; the receiver's three
      streams and the sender's one both go through it; existing playback
      tests green.
- [ ] Service + IPC + UI: `return_pid` on the receive request, persisted;
      the picker on Receive; the Call row and the echo sentence on Share;
      `ipc.rs` and `ipc.ts` in one commit. UI tests for the picker, the
      row, the sentence appearing only for rest/desktop capture.
- [ ] One-PC proof, headless: `scripts/return-check.sh` — the receiver
      returns a process's audio and the sender's stats show
      `return_packets` climbing and `return_peak` > 0 while that process
      plays something (speech or music, never a tone).
- [ ] Docs: this file's DoD ticked or moved to Deferred with a runbook;
      ROADMAP v1.1 row; SESSIONS.md S19 entry; `docs/dev/dual-audio-decision.md`
      gains a line saying the seam was used.
- [ ] Gates: fmt, clippy `-D warnings`, `cargo test --workspace`,
      `pnpm build`, `pnpm test`, footprint.
- [ ] Owed to two PCs: Discord on PC 2 with a real second participant;
      the gaming PC hears them, they do not hear themselves.
