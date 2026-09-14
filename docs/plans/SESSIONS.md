# Post-M7 session catalogue

Every remaining piece of work in the project, broken into sessions. M0–M7 are
done and merged into `main`; what follows is the deferred work from their plans
plus the v1.1 backlog.

One session per entry. Each gives the branch, the worktree, what blocks it,
a Definition of Ready, a Definition of Done, and a kickoff prompt to paste into
a fresh Claude Code chat.

**Shared rules for every session** (do not repeat them in the prompt — they are
inherited):

- The non-negotiables in `CLAUDE.md` bind absolutely: no anti-cheat surface, no
  global config changes, backup-then-apply with restore on blur/exit/crash, the
  always-on core stays ≤ ~10 MB and ~0 % idle, both opt-ins stay explicit.
- Never drop scope silently. Anything unfinished moves to that plan's
  `## Deferred` with a reason and a runbook.
- Never write the registry autonomously. Elevated `HKLM` writes are the user's
  to perform; the live-write gates (`RELAY_APO_ALLOW_LIVE_WRITE`,
  `RELAY_VDEVICE_ALLOW_LIVE_WRITE`) exist so this cannot happen by accident.
- Every session ends green on: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, `pnpm build` in `ui/`, and `scripts/footprint.ps1`.
- IPC changes land in `crates/core/src/ipc.rs` and its TypeScript mirror
  `ui/src/lib/ipc.ts` **in the same commit**. That pair is the known collision
  point between parallel sessions — land those commits small and early.
- Worktree mechanics, and the fact that the post-commit installer is shared
  across trees: `docs/dev/parallel-sessions.md`. Set `RELAY_NO_INSTALL=1` in a
  feature tree unless you want the installed app to follow it.

**Concurrency.** Groups 1 and 2 are all independent; the practical limit is
three at once, because they converge on `ipc.rs` / `ipc.ts` and the UI screens.
S5 landed 2026-09-14 and answered its question **no**: per-user registration
does not work, so S12 still needs the elevated shell and S6 is required.

| # | Session | Branch | Worktree | Blocked on |
|---|---|---|---|---|
| S1 | ADLX display backend | `feat/adlx-display` | `relay-adlx` | — |
| S2 | Second Opus track | `feat/dual-audio` | `relay-dual-audio` | — |
| S3 | Vendor VCP opcodes | `feat/monitor-vcp` | `relay-monitor-vcp` | partly: OSD eyes |
| S4 | MKV container | `feat/mkv-container` | `relay-mkv` | — |
| S5 | Per-user vcam registration | `feat/hkcu-vcam` | `relay-hkcu-vcam` | **done 2026-09-14 — HKCU does not work** |
| S6 | Elevated install helper | `feat/elevated-install` | `relay-elevation` | — (**required**, not optional: S5 said no) |
| S7 | Codec robustness | `feat/codec-robustness` | `relay-codec` | — |
| S8 | WebView UI test harness | `feat/ui-test-harness` | `relay-uitest` | — |
| S9 | Documentation truth pass | `chore/docs-truth` | main tree | — |
| S10 | Human verification pass | `chore/human-pass` | main tree | you, 45 min |
| S11 | Soak and measurement pass | `chore/soak-pass` | main tree | ~2 h wall clock |
| S12 | Live virtual-camera pass | `chore/vcam-live` | main tree | one elevated shell |
| S13 | Clean-VM uninstall diff | `chore/vm-uninstall` | main tree | a hypervisor |
| S14 | Live APO test-sign pass | `chore/apo-vm` | main tree | a hypervisor |
| S15 | Two-PC share validation | `chore/two-pc` | main tree | a second PC |
| S16 | Second-monitor display pass | `chore/second-monitor` | main tree | a second panel |
| S17 | EV certificate and signing | `chore/signing` | main tree | the certificate |
| S18 | Relay Send VST3 | `feat/vst3-send` | create when started | — (v1.1) |
| S19 | Call-audio return and mix-minus | `feat/mix-minus` | create when started | S2 (v1.1) |
| S20 | Stream Deck and NDI output | `feat/streamdeck-ndi` | create when started | — (v1.1) |
| S21 | AI tuning loop | `feat/ai-tuning` | create when started | — (v1.1) |

---

# Group 1 — Code, unblocked

Worktrees for S1–S8 already exist and sit on `main` with `pnpm install` done.

---

## S1 — ADLX (AMD) display backend
**Branch** `feat/adlx-display` · **Worktree** `C:\Users\stern\Documents\Code\relay-adlx`

Display control is NVIDIA-only. `DisplayIo` is the seam; NvAPI is the reference
implementation. This dev machine reports an AMD encoder, so some of this is
verifiable here.

### Definition of Ready
- [x] M2 merged and NVIDIA live-verified (2026-09-13, restore at 144 ms mean).
- [x] `DisplayIo` trait exists as the seam (`crates/display`).
- [ ] Confirm whether an AMD GPU drives any display on this machine, or whether ADLX can only be exercised against fixtures. If fixtures only, say so in the plan and unit-test the mapping.

### Definition of Done
- [ ] ADLX backend behind `DisplayIo`, selected at runtime by which vendor owns the target monitor; NVIDIA path unchanged.
- [ ] Backup-then-apply and restore-on-blur behave identically on both vendors — proven by running the existing crash-restore harness against the ADLX backend.
- [ ] Vibrance/gamma/contrast/hue mapping unit-tested against fixtures; the mapping is *not* one-to-one with NvAPI, so record the curve you chose and why.
- [ ] A machine with neither vendor degrades to "unsupported" in the UI, never a panic.
- [ ] `docs/plans/M2-display.md` ADLX line checked off; ROADMAP M2 row updated.

### Kickoff prompt
```
You are starting session S1 (ADLX/AMD display backend) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S1) and docs/plans/M2-display.md. Work in the worktree C:\Users\stern\Documents\Code\relay-adlx on branch feat/adlx-display.

The display path is NVIDIA-only today. Add the AMD equivalent behind the existing DisplayIo seam so an AMD PC gets the same per-game colour an NVIDIA one does.

1. Verify the Definition of Ready in SESSIONS.md. Tell me if no AMD GPU drives a display here and build against fixtures instead — do not fake a live result.
2. Backup-then-apply and restore-on-blur must behave identically on both vendors. Run cargo test -p relay-core --test crash_restore against the ADLX backend and record the numbers.
3. The gamma/vibrance mapping is not one-to-one with NvAPI. Record the curve you chose and why, and unit-test it.
4. Finish only when the Definition of Done is met. Update docs/plans/M2-display.md and docs/ROADMAP.md, then summarise.
```

---

## S2 — Second Opus track (mic *and* desktop audio)
**Branch** `feat/dual-audio` · **Worktree** `C:\Users\stern\Documents\Code\relay-dual-audio`

The sender ships one audio track, so choosing Microphone *replaces* the desktop
mix. This is the deferred item most likely to embarrass someone mid-call.

### Definition of Ready
- [x] Mic path built and benchmarked (`AudioSource::Microphone`).
- [x] M4 measurements table is the latency baseline to beat.
- [ ] Decide where mixing happens: receiver-side mix, or two tracks the receiver routes separately (the virtual mic wants them separate; a plain call wants them mixed). Record the decision before writing code.

### Definition of Done
- [ ] Sender can carry desktop mix **and** microphone simultaneously as two Opus tracks; existing single-track presets are unchanged.
- [ ] Receiver handles one track or two, and an older peer sending one track still works.
- [ ] Added latency measured against M4's baseline and recorded; still inside the budget.
- [ ] The warning note under the Audio chips in `ui/src/screens/Share.tsx` is removed, and the chips become a source *set* rather than a single choice.
- [ ] Recording carries both tracks (closes `docs/plans/M6-recording-presets.md:53`).
- [ ] M4 and M6 Deferred entries struck; ROADMAP rows updated.

### Kickoff prompt
```
You are starting session S2 (simultaneous mic and desktop audio) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S2), the Deferred section of docs/plans/M4-share.md and docs/plans/M6-recording-presets.md. Work in the worktree C:\Users\stern\Documents\Code\relay-dual-audio on branch feat/dual-audio.

The sender ships one audio track, so picking Microphone drops the desktop mix. Add a second Opus track so both travel together.

1. Verify the Definition of Ready. Decide receiver-side mixing versus two separately routed tracks and write the decision down before coding — the virtual mic wants them separate, a plain call wants them mixed.
2. This is on the hot path. Measure added latency against M4's baseline table and record both numbers.
3. A peer sending a single track must still work; do not break the existing presets.
4. Remove the warning note under the Audio chips in ui/src/screens/Share.tsx once it is no longer true, and make the chips a source set rather than a single choice.
5. Finish only when the Definition of Done is met. Update docs/plans/M4-share.md, docs/plans/M6-recording-presets.md and docs/ROADMAP.md, then summarise.
```

---

## S3 — Verified vendor VCP opcodes
**Branch** `feat/monitor-vcp` · **Worktree** `C:\Users\stern\Documents\Code\relay-monitor-vcp`

Black equalizer and Response are greyed on every panel: `quirks_for` in
`crates/display/src/vcp.rs` has no verified vendor opcode for any PNP prefix.
This is the monitor-side answer to the headphone catalogue — per-model data
instead of a category-wide guess.

### Definition of Ready
- [x] `UnsupportedReason::NoKnownOpcode` already plumbs through to the UI.
- [x] A real MCCS capability dump from the LG ULTRAGEAR+ is a checked-in fixture (47 codes).
- [ ] Accept that set-and-readback cannot prove on-screen meaning: each opcode needs either vendor documentation or a human watching the OSD. Unverified entries must be marked unverified, not shipped as fact.

### Definition of Done
- [ ] A per-model quirks table keyed by PNP ID and model string, each entry carrying its evidence (vendor doc URL, or "observed on OSD by <person> <date>").
- [ ] Verified entries for at least the LG ULTRAGEAR+ on this desk; everything else marked unverified and **not** used to enable a control.
- [ ] Never guesses: writing an unknown code is impossible by construction, and there is a test proving an unverified entry does not enable the slider.
- [ ] The UI note in `ui/src/screens/Games.tsx` shrinks to match what is now supported.
- [ ] `docs/plans/M2-display.md:132` updated; ROADMAP M2 row updated.

### Kickoff prompt
```
You are starting session S3 (verified vendor VCP opcodes) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S3) and docs/plans/M2-display.md. Work in the worktree C:\Users\stern\Documents\Code\relay-monitor-vcp on branch feat/monitor-vcp.

Black equalizer and Response are greyed out on every panel because quirks_for in crates/display/src/vcp.rs has no verified vendor opcode for any PNP prefix.

1. Build a per-model quirks table keyed by PNP ID and model, where every entry records its evidence — a vendor document, or an OSD observation with who and when.
2. Never guess an opcode. Writing the wrong VCP code changes a setting the user did not ask for. Make guessing structurally impossible and prove with a test that an unverified entry does not enable the slider.
3. I can watch the OSD on the LG ULTRAGEAR+ on this desk — ask me and I will confirm what each candidate code does. Anything I cannot see stays unverified.
4. Shrink the explanatory note in ui/src/screens/Games.tsx to match what is actually supported now.
5. Finish only when the Definition of Done is met. Update docs/plans/M2-display.md and docs/ROADMAP.md, then summarise.
```

---

## S4 — MKV recording container
**Branch** `feat/mkv-container` · **Worktree** `C:\Users\stern\Documents\Code\relay-mkv`

Deferred by *decision*, not by a blocker: fragmented MP4 already covers
crash-safety. Worth doing for the reason OBS defaults to MKV, but the lowest
priority in Group 1 — start it only if a real compatibility gap appears.

### Definition of Ready
- [x] fMP4 muxer with golden-fixture tests is the pattern to follow.
- [ ] A named reason to do it now: a player or editor that rejects the current Opus-in-fMP4 output. Record it, or leave this session unstarted.

### Definition of Done
- [ ] MKV selectable per preset; fMP4 remains the default.
- [ ] Golden-fixture byte tests to the same standard as the fMP4 muxer.
- [ ] Still a tee of the share's existing bitstream — no second encode, no added latency; re-measure and record.
- [ ] Replay save works in both containers.
- [ ] `docs/plans/M6-recording-presets.md:50` updated; ROADMAP M6 row updated.

### Kickoff prompt
```
You are starting session S4 (MKV recording container) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S4) and docs/plans/M6-recording-presets.md. Work in the worktree C:\Users\stern\Documents\Code\relay-mkv on branch feat/mkv-container.

Recording writes fragmented MP4. Add MKV as a per-preset option, for the reason OBS defaults to it: an MKV survives a crash mid-file.

1. Check the Definition of Ready first. This was deferred by decision, not by a blocker — if there is still no concrete player or editor that rejects our fMP4 output, say so and stop rather than doubling the muxer surface for nothing.
2. Match the existing golden-fixture byte-test approach; the fMP4 muxer is the pattern.
3. This must stay a tee of the share's existing bitstream. No second encode path, no added latency — re-measure and record both numbers.
4. Finish only when the Definition of Done is met. Update docs/plans/M6-recording-presets.md and docs/ROADMAP.md, then summarise.
```

---

## S5 — Per-user (HKCU) virtual-camera registration — **done 2026-09-14**
**Branch** `feat/hkcu-vcam` · **Worktree** `C:\Users\stern\Documents\Code\relay-hkcu-vcam`

**Answer: no.** An HKCU-only registration resolves in the calling process but
`IMFVirtualCamera::Start` fails `0x80070003` inside the Frame Server, which runs
as `NT AUTHORITY\LocalService` and never loads the DLL. Evidence, control arms
and a reproducible probe: `docs/dev/vcam-live.md` (last section). The HKLM write
and its one elevation stay; **S6 is required and S12 keeps its blocker.**

It was the highest-leverage session in Group 1: the virtual camera needs one
elevated write to `HKLM\SOFTWARE\Classes\CLSID`, which is why M5's live pass has
never run and why the installer needs an elevation story at all. Had COM
activation resolved from `HKCU\Software\Classes\CLSID` for the Frame Server, the
elevation requirement would have disappeared — and with it most of S6 and all of
S12. It does not, so both stand.

### Definition of Ready
- [x] Registration planner and `installed.json` bookkeeping exist and are tested.
- [x] Live writes double-gated on `RELAY_VDEVICE_ALLOW_LIVE_WRITE` + elevation.
- [x] The experiment is already written down at the end of `docs/dev/vcam-live.md`.
- [x] Understand before writing anything: the Frame Server is a *service*, so it may not see per-user registrations at all. That is the question this session answers. (Confirmed: `svchost -k Camera`, running as `NT AUTHORITY\LocalService`.)

### Definition of Done
- [x] A clear, evidenced answer to "can the Relay camera register per-user?" — **no**, with three control arms and DLL-load evidence written up so nobody retries it.
- [—] If it works: HKCU is the default path… — it does not work, so nothing moved: `reg.rs` still plans HKLM keys and `installed.json` still records them.
- [x] If it does not: `docs/dev/vcam-live.md` records the negative result and S6's elevated helper becomes required rather than optional.
- [x] Either way, no `HKLM` write happens in this session without the user performing it — none was written at all; the HKCU key and the `C:\ProgramData` DLL copy used for the experiment were removed afterwards.
- [x] `docs/plans/M5-vdevices.md` Deferred item 5 resolved; SESSIONS.md S12 updated to match.

### Kickoff prompt
```
You are starting session S5 (per-user virtual-camera registration) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S5), docs/plans/M5-vdevices.md and docs/dev/vcam-live.md. Work in the worktree C:\Users\stern\Documents\Code\relay-hkcu-vcam on branch feat/hkcu-vcam.

The virtual camera currently needs one elevated write to HKLM\SOFTWARE\Classes\CLSID. That single requirement is why M5's live pass has never run. Find out whether registering under HKCU\Software\Classes\CLSID works instead — the Frame Server is a service, so it may not see per-user registrations at all, and that is exactly the question.

1. Answer the question with evidence before changing any default. A documented negative result is a perfectly good outcome for this session.
2. Never write HKLM yourself. If a step needs elevation, stop and tell me exactly what to run.
3. If HKCU works: make it the default, record which hive was used in installed.json, and make uninstall remove from the right one.
4. If it does not: write the reason into docs/dev/vcam-live.md clearly enough that nobody tries it again, and note in docs/plans/SESSIONS.md that S6 is now required.
5. Finish only when the Definition of Done is met. Update docs/plans/M5-vdevices.md and docs/ROADMAP.md, then summarise.
```

---

## S6 — Elevated install helper
**Branch** `feat/elevated-install` · **Worktree** `C:\Users\stern\Documents\Code\relay-elevation`

The core runs unelevated by design. The Settings cards for the APO and the
virtual camera are wired and honest about failing, but there is no production
path to actually install them. S5 has run and did **not** shrink this to the APO
only: the helper covers both components.

### Definition of Ready
- [x] S5 finished (2026-09-14): the camera **still needs elevation** — per-user registration does not work, so this session covers the camera as well as the APO.
- [x] Both components already record what they installed (`installed.json`, `apo-backup\<endpoint>.json`), so an elevated helper has a manifest to act on.
- [x] The uninstall planner is a pure function of probed state and needs no new design.

### Definition of Done
- [ ] One elevated helper, launched on demand with a UAC prompt, that performs exactly the recorded install/uninstall steps and nothing else.
- [ ] The user sees what will be changed *before* the prompt — the same dry-run listing the uninstall card already renders.
- [ ] Helper refuses to run anything not in the plan; the live-write gates stay in force.
- [ ] Declining UAC leaves the machine untouched and says so plainly.
- [ ] Settings "Install APO" and the camera card work end to end on this machine.
- [ ] `docs/plans/M3b-apo.md` Deferred item 3 closed; M7 plan updated.

### Kickoff prompt
```
You are starting session S6 (elevated install helper) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S6), docs/plans/M3b-apo.md and docs/plans/M7-installer.md. Work in the worktree C:\Users\stern\Documents\Code\relay-elevation on branch feat/elevated-install.

S5 has finished: per-user camera registration does not work (docs/dev/vcam-live.md), so this session covers the camera as well as the APO.

The core runs unelevated by design, so the Settings cards for the APO and camera cannot actually install anything. Build the elevated helper that can.

1. The helper performs exactly the steps the components recorded and nothing else. It must refuse anything not in the plan, and the existing live-write gates stay in force.
2. Show the user what will change before the UAC prompt — reuse the dry-run listing the uninstall card already renders.
3. Declining UAC must leave the machine untouched and say so plainly.
4. You may prompt me for elevation and I will approve it; never attempt to write HKLM without that prompt.
5. Finish only when the Definition of Done is met, including Settings "Install APO" working end to end on this machine. Update docs/plans/M3b-apo.md, docs/plans/M7-installer.md and docs/ROADMAP.md, then summarise.
```

---

## S7 — Codec and capture robustness
**Branch** `feat/codec-robustness` · **Worktree** `C:\Users\stern\Documents\Code\relay-codec`

Three documented hard edges that all fail the same way — a machine unlike this
one hits a `bail!` instead of a graceful path.

### Definition of Ready
- [x] `Method::ShareCapabilities` already warns before a share or receive fails (added 2026-09-14).
- [x] The three edges are known: `crates/capture/src/encode/mf.rs:230` (MFT allocator path unimplemented), `mf.rs:294` (no software HEVC encode), `crates/capture/src/audio.rs:258,280,285` (48 kHz stereo only).
- [ ] Decide which are worth fixing versus documenting as hard limits. Not all three deserve code.

### Definition of Done
- [ ] MFT allocator path either implemented or turned into a clear, actionable message naming the adapter.
- [ ] Non-48 kHz and non-stereo endpoints produce a specific, fixable message rather than a generic failure — or are supported.
- [ ] No-hardware-encoder machines are told the truth early (S7 should confirm the S1-era banner covers this).
- [ ] The HEVC Video Extension question is resolved one way: bundle it, detect-and-link to the Store (already done), or document why a DXVA-direct decoder is not worth it.
- [ ] `docs/plans/M4-share.md:125` updated.

### Kickoff prompt
```
You are starting session S7 (codec and capture robustness) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S7) and the Deferred section of docs/plans/M4-share.md. Work in the worktree C:\Users\stern\Documents\Code\relay-codec on branch feat/codec-robustness.

Three documented hard edges all fail the same way — a machine unlike this dev PC hits a bail! instead of a graceful path: the MFT allocator branch at crates/capture/src/encode/mf.rs:230, no software HEVC encode at mf.rs:294, and 48 kHz stereo only at crates/capture/src/audio.rs:258/280/285.

1. Decide which deserve code and which are honestly hard limits. Not all three need fixing; say which and why.
2. Every remaining limit must produce a specific, actionable message naming what is wrong, not a generic failure.
3. Resolve the HEVC Video Extension question one way and write the decision down: bundle it, keep the detect-and-warn we added, or document why a DXVA-direct decoder is not worth building.
4. Finish only when the Definition of Done is met. Update docs/plans/M4-share.md and docs/ROADMAP.md, then summarise.
```

---

## S8 — WebView UI test harness
**Branch** `feat/ui-test-harness` · **Worktree** `C:\Users\stern\Documents\Code\relay-uitest`

No automated test has ever clicked through the actual UI. Every screen bug
found so far was found by a human reading the code. Note the standing rule:
**do not drive the real desktop with synthetic mouse or keyboard input** — a
previous session's stray clicks landed in the user's browser.

### Definition of Ready
- [x] `pnpm build` runs `tsc --noEmit` and catches type drift already.
- [x] IPC wire shapes are covered by Rust tests on both sides.
- [ ] Pick the approach: component tests against a mocked `api` (safe, fast, no window) versus WebDriver against a real Tauri window (higher fidelity, moves a real cursor). Default to the former unless there is a reason.

### Definition of Done
- [ ] Every screen renders against both mock data and an offline core without throwing.
- [ ] The paths that were mocks and got wired this month have regression tests: share preset start/stop, catalogue search and import, uninstall plan rendering, consent flow.
- [ ] The suite runs in CI on `windows-latest` alongside `pnpm build`.
- [ ] No test moves the real mouse or sends synthetic keystrokes to the desktop.
- [ ] `docs/plans/M0-foundation.md:59` updated.

### Kickoff prompt
```
You are starting session S8 (WebView UI test harness) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S8) and docs/plans/M0-foundation.md. Work in the worktree C:\Users\stern\Documents\Code\relay-uitest on branch feat/ui-test-harness.

No automated test has ever exercised the UI; every screen bug so far was found by reading code. Build the harness.

1. Hard rule: no test may move the real mouse or send synthetic keystrokes to the desktop. A previous session did that and its clicks landed in my browser. Prefer component tests against a mocked api module over WebDriver for exactly this reason, and justify it if you choose otherwise.
2. Cover every screen against both mock data and an offline core, then add regression tests for the paths that were mocks until recently: share preset start/stop, catalogue search and import, uninstall plan rendering, and the consent flow.
3. Wire the suite into CI on windows-latest next to pnpm build.
4. Finish only when the Definition of Done is met. Update docs/plans/M0-foundation.md and docs/ROADMAP.md, then summarise.
```

---

# Group 2 — Bookkeeping

## S9 — Documentation truth pass
**Branch** `chore/docs-truth` · **Worktree** main tree

An audit found plan files that no longer match the code. These are small, but
they are the files every future session reads first, so a stale line becomes a
wrong decision.

### Definition of Ready
- [x] The specific drifts are known (listed in the DoD below).

### Definition of Done
- [ ] `docs/plans/M3-audio-dsp.md:65` — "headset-correction curves waits on M1's hardware library" is stale; M1 landed and the correction path was built (`crates/audio/src/fit.rs`, `audio_bridge::chain_params_with`, the Games correction card). Verify against the code, then strike it.
- [ ] `docs/plans/M1-hardware.md:36-37` — "Out of scope: online curve download" is stale; `crates/core/src/hardware/catalog.rs` fetches measurements over WinHTTP with attribution. Correct it and state the licensing position (AutoEQ data is CC BY-NC-SA: index ships, measurements fetch on demand, never redistributed).
- [ ] `docs/ROADMAP.md` — M7 is dated 2026-09-12 in the status table and 2026-09-13 in its section. Pick one.
- [ ] Every ROADMAP status-table row re-read against its plan's Deferred section, and this catalogue cross-linked from `docs/plans/README.md` and `KICKOFF-PROMPTS.md`.
- [ ] No claim in any plan file asserts something the code does not do.

### Kickoff prompt
```
You are starting session S9 (documentation truth pass) for Relay. Read CLAUDE.md and docs/plans/SESSIONS.md (section S9). Work in the main tree C:\Users\stern\Documents\Code\Stream Share on branch chore/docs-truth.

Several plan files no longer match the code. They are the first thing every session reads, so a stale line becomes a wrong decision.

1. Work through the specific drifts listed in S9's Definition of Done. Verify each against the current code before editing — do not take my list on faith, and tell me if one of them is wrong.
2. Then re-read every ROADMAP status row against its plan's Deferred section and fix any that overstate completeness.
3. No claim in any plan file may assert something the code does not do. Where a claim is aspirational, mark it so.
4. Finish only when the Definition of Done is met, then summarise what was stale.
```

---

# Group 3 — Live passes on this PC

These need a person at this keyboard, not new hardware.

## S10 — Human verification pass
**Branch** `chore/human-pass` · **Worktree** main tree · **Needs you for ~45 minutes**

Four deferred checks that only a human can sign off.

### Definition of Ready
- [x] A/B material staged at `%LOCALAPPDATA%\Relay\previews\original.wav` / `processed.wav`; the Games › Audio card renders pairs on demand.
- [x] Two headset rows can be configured for the unplug test.
- [ ] You are available and willing to lock the machine mid-session (Win+L) and pull a USB cable.

### Definition of Done
- [ ] **A/B listening session** (`M3-audio-dsp.md:63`) — you listen, verdict recorded in the plan with what you heard, not just "passed".
- [ ] **Win+L lock/unlock** (`M2-display.md:130`) — display settings survive a lock/unlock cycle; result recorded.
- [ ] **Physical headset unplug** (`M1-hardware.md:41`) — with two Ready rows, pulling the default endpoint's cable re-selects the surviving row within 1 s; measured.
- [ ] **Visual UI pass with the live core** (`M1-hardware.md:42`) — every screen walked in a running Tauri window against a live core; anything wrong filed or fixed.
- [ ] Those four Deferred entries struck and ROADMAP rows updated.

### Kickoff prompt
```
You are starting session S10 (human verification pass) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S10), and the Deferred sections of docs/plans/M1-hardware.md, M2-display.md and M3-audio-dsp.md. Work in the main tree on branch chore/human-pass.

Four deferred checks need me at the keyboard: the A/B listening session, Win+L lock/unlock, a physical headset unplug, and a visual walkthrough of every screen against a live core.

1. Set each one up, tell me exactly what to do, and wait for my answer. Do not record a result I did not give you.
2. For the listening test, ask what I actually heard, not whether it passed — write that down.
3. For the unplug test, configure two Ready rows first and measure the re-selection time.
4. Never drive my mouse or keyboard synthetically; ask me to click things.
5. Fix or file anything the walkthrough turns up. Finish only when the Definition of Done is met, then update the three plan files and docs/ROADMAP.md.
```

---

## S11 — Soak and measurement pass
**Branch** `chore/soak-pass` · **Worktree** main tree · **~2 hours wall clock, mostly unattended**

### Definition of Ready
- [x] `scripts/m6-loopback.ps1` drives record and replay runs.
- [x] Hourly rolling implemented; unit-level logic covered.
- [ ] A real game available for the WASAPI-exclusive spot check, or agreement to keep using the in-process exclusive stream.

### Definition of Done
- [ ] **One-hour roll soak** (`M6:52`) — `-Record -Secs 4000` with `roll_secs` 3600; two files, both playable in ffprobe; drop counters recorded.
- [ ] **Full-motion 4K60 numbers** (`M6:51`) — using the `RELAY_BITRATE_MBPS=80` upscale workaround given the 1440p panel; recorded with the caveat stated.
- [ ] **Real-game WASAPI-exclusive spot check** (`M3:64`) — a shipping title with exclusive output is detected and the UI warns; or recorded as still-deferred with the reason.
- [ ] Measurements tables in M6 and M3 updated with real numbers, not extrapolations.

### Kickoff prompt
```
You are starting session S11 (soak and measurement pass) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S11), docs/plans/M6-recording-presets.md and docs/plans/M3-audio-dsp.md. Work in the main tree on branch chore/soak-pass.

Three deferred measurements need wall-clock time rather than new hardware: the one-hour roll soak, full-motion 4K60 recording numbers, and a real-game WASAPI-exclusive spot check.

1. Start the one-hour soak first and let it run while you do the rest. Expect two playable files; record the drop counters.
2. This panel is 1440p, so use the RELAY_BITRATE_MBPS=80 upscale workaround for the 4K60 numbers and state that caveat next to them.
3. For the exclusive-mode check, ask me to launch a title with exclusive output. If I have none, record it as still deferred rather than passing it on the in-process stream.
4. Put real measured numbers in the Measurements tables, never extrapolations. Finish only when the Definition of Done is met, then update both plans and docs/ROADMAP.md.
```

---

# Group 4 — Blocked, one unblock each

Each of these is written and ready; each waits on one external thing.

## S12 — Live virtual-camera pass
**Branch** `chore/vcam-live` · **Worktree** main tree · **Blocked on: one elevated shell** (~20 min)

S5 has run (2026-09-14): per-user registration does **not** work, so the elevated shell is still required. Start with the two-minute positive control at the end of `docs/dev/vcam-live.md` — run `vcam_reg_probe` once the key is in HKLM and confirm `Start` returns `S_OK`.

### Definition of Ready
- [x] S5 resolved 2026-09-14: HKCU does not work → elevation required.
- [ ] If elevation is still required: you are present to approve one UAC prompt for a write to `HKLM\SOFTWARE\Classes\CLSID`.
- [x] Runbook written: `docs/dev/vcam-live.md` (~20 min, includes the removal check).

### Definition of Done
- [ ] Results table at `docs/plans/M5-vdevices.md:45-49` filled in: Discord, Zoom and Meet × 1080p60 and 4K30.
- [ ] The 5 s DoD check met: opt in, "Relay Camera" appears in a Discord call within 5 s of sharing.
- [ ] Clap test for sample-accurate A/V alignment recorded.
- [ ] Removal check: unregistering leaves no trace, verified with the snapshot harness.
- [ ] M5 Deferred items 1 and 3 struck; ROADMAP M5 row updated.

### Kickoff prompt
```
You are starting session S12 (live virtual-camera pass) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S12), docs/plans/M5-vdevices.md and docs/dev/vcam-live.md. Work in the main tree on branch chore/vcam-live.

S5 has answered the hive question: HKCU does not work, so one elevated write to HKLM is still needed. Run the positive-control probe in the runbook first.

1. Verify the Definition of Ready. Tell me exactly what needs approving and wait — never write HKLM yourself.
2. Follow docs/dev/vcam-live.md and fill in the results table in docs/plans/M5-vdevices.md with what actually happened in Discord, Zoom and Meet at 1080p60 and 4K30. Do not fill a cell you did not observe.
3. Run the clap test for A/V alignment and the removal check with the snapshot harness.
4. Finish only when the Definition of Done is met. Update docs/plans/M5-vdevices.md and docs/ROADMAP.md, then summarise.
```

---

## S13 — Clean-VM uninstall diff
**Branch** `chore/vm-uninstall` · **Worktree** main tree · **Blocked on: a hypervisor**

The uninstaller is the product's promise. It has been proven seven times on this
machine with an empty diff, but never from a clean checkpoint.

### Definition of Ready
- [ ] Hyper-V enabled (elevated install + reboot) or another hypervisor installed.
- [ ] A clean Windows VM with a checkpoint taken *before* any Relay install.
- [x] Harness written and green on the dev machine: `scripts/vm-cycle.ps1`, `machine-snapshot.ps1`, `snapshot-diff.ps1`; runbook `docs/dev/uninstall-vm.md`.

### Definition of Done
- [ ] The cycle run **four ways**, each from a fresh checkpoint: `-SkipOptIn`, delete-data, `-KeepData`, and install-over-install.
- [ ] `-Broad` tier used, so whole-hive registry exports and a full file index are compared.
- [ ] The APO *restore* path proven live — it has never been, because `-SkipOptIn` meant the APO was never registered. This is the first thing to check.
- [ ] Diff summaries pasted into `docs/plans/M7-installer.md`; a non-empty diff is a bug to fix, not a result to record.
- [ ] ROADMAP:101 checked off.

### Kickoff prompt
```
You are starting session S13 (clean-VM uninstall diff) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S13), docs/plans/M7-installer.md and docs/dev/uninstall-vm.md. Work in the main tree on branch chore/vm-uninstall.

The uninstaller is the product's promise: after uninstall a clean VM must show no differences except the optional data folder. It has been proven seven times on the dev machine but never from a clean checkpoint.

1. Verify the Definition of Ready. Enabling Hyper-V needs elevation and a reboot — tell me what to run, do not attempt it yourself.
2. Run the cycle four ways from a fresh checkpoint each time: -SkipOptIn, delete-data, -KeepData, and install-over-install. Use the -Broad tier.
3. Check the APO restore path first. It has never run live, because -SkipOptIn meant the APO was never registered.
4. A non-empty diff is a bug to fix, not a result to record. Paste the diff summaries into docs/plans/M7-installer.md.
5. Finish only when the Definition of Done is met. Update docs/ROADMAP.md and summarise.
```

---

## S14 — Live APO test-sign pass
**Branch** `chore/apo-vm` · **Worktree** main tree · **Blocked on: a hypervisor** (and test-signing, or the EV cert)

### Definition of Ready
- [ ] A Windows VM with test-signing enabled and a checkpoint taken before any APO registration.
- [x] Runbook written: `docs/dev/apo-testsign.md` (83 lines, one-time setup then an install/verify/uninstall/diff loop).
- [x] Baseline FX property-store export checked in as a fixture.
- [ ] **Never on this dev machine.** The double gate exists precisely to prevent that.

### Definition of Done
- [ ] APO registers, `audiosrv` restarts, endpoints re-enumerate, audio plays through the chain.
- [ ] Before/after `reg export` diff is byte-identical after uninstall (step 7 of the runbook).
- [ ] Single-endpoint scoping confirmed live: other endpoints untouched.
- [ ] M3b Deferred item 2 struck; ROADMAP M3b row updated.

### Kickoff prompt
```
You are starting session S14 (live APO test-sign pass) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S14), docs/plans/M3b-apo.md and docs/dev/apo-testsign.md. Work in the main tree on branch chore/apo-vm.

This registers an audio processing object with the Windows audio engine. It runs in a test-signing VM only.

1. Absolute rule: no APO registration on this dev machine, ever. The RELAY_APO_ALLOW_LIVE_WRITE plus elevation double gate exists for this. If you cannot confirm you are in the VM, stop.
2. Verify the Definition of Ready, including the checkpoint taken before any registration.
3. Follow docs/dev/apo-testsign.md. Step 7's before/after reg export diff must be byte-identical after uninstall; anything else is a bug.
4. Confirm live that only the one target endpoint was touched.
5. Finish only when the Definition of Done is met. Update docs/plans/M3b-apo.md and docs/ROADMAP.md, then summarise.
```

---

## S15 — Two-PC share validation
**Branch** `chore/two-pc` · **Worktree** main tree · **Blocked on: a second PC**

The named "MVP validation pass". Everything it needs is built and green on
loopback.

### Definition of Ready
- [ ] A second Windows PC on the same wired LAN, with the HEVC Video Extension installed (check with `relay-core`'s capability probe before travelling to it).
- [ ] A camera and stopwatch, or a phone, for the glass-to-glass measurement.

### Definition of Done
- [ ] Glass-to-glass latency measured with camera and stopwatch, wired; target under 50 ms.
- [ ] 10-minute 4K60 run with zero drops; `capture_to_present_ms` p50 and p99 recorded.
- [ ] M7's uninstall cycle re-run so its `share` phase proves a real end-to-end share, not just engine spin-up (`M7:293`).
- [ ] If S12 is done, the receiver's virtual camera verified from the sending PC's perspective too.
- [ ] M4 Deferred item 1 struck; ROADMAP M4 row updated.

### Kickoff prompt
```
You are starting session S15 (two-PC share validation) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S15) and the Deferred section of docs/plans/M4-share.md. Work in the main tree on branch chore/two-pc.

This is the MVP validation pass. Everything it needs is built and green on loopback; it has never run across two machines.

1. Verify the Definition of Ready. Check the second PC's HEVC decoder support with the capability probe before relying on it.
2. Measure glass-to-glass with a camera and stopwatch on a wired link, and run 10 minutes at 4K60 looking for drops. Record p50 and p99 of capture_to_present_ms.
3. Re-run M7's uninstall cycle now that a real share can complete pairing — its share phase has only ever proven the engine starts and stops.
4. Record what you measured, including anything that missed target. Finish only when the Definition of Done is met, then update docs/plans/M4-share.md, docs/plans/M7-installer.md and docs/ROADMAP.md.
```

---

## S16 — Second-monitor and LG C2 display pass
**Branch** `chore/second-monitor` · **Worktree** main tree · **Blocked on: a second panel**

### Definition of Ready
- [ ] A second display attached (the LG C2 is the one named in the plans).
- [x] Multi-monitor id stability and `HMONITOR` mapping unit-tested against fixture EDIDs.

### Definition of Done
- [ ] Only the monitor hosting the game window changes — the second panel provably untouched.
- [ ] Dragging the game across restores the first panel within a tick.
- [ ] Monitor id stability confirmed live across reboot and replug with two panels.
- [ ] Per-model VCP codes that work land in the hardware library / quirks table (feeds S3).
- [ ] M1 and M2 second-monitor Deferred entries struck; ROADMAP rows updated.

### Kickoff prompt
```
You are starting session S16 (second-monitor display pass) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S16), docs/plans/M1-hardware.md and docs/plans/M2-display.md. Work in the main tree on branch chore/second-monitor.

Multi-monitor behaviour is unit-tested against fixture EDIDs only; one physical panel has ever been attached.

1. Verify a second display is actually connected before starting, and tell me if not.
2. Prove the second panel is untouched when a profile applies to the first — that is the non-negotiable here, not a nice-to-have.
3. Check that dragging the game across restores the first panel within a tick, and confirm monitor id stability across a reboot and a replug.
4. Any per-model VCP codes you verify go into the quirks table; coordinate with session S3 if it is running.
5. Finish only when the Definition of Done is met, then update both plans and docs/ROADMAP.md.
```

---

## S17 — EV certificate and signing
**Branch** `chore/signing` · **Worktree** main tree · **Blocked on: the certificate being ordered**

**The long pole.** Four separate deliverables wait on this one purchase, and it
has weeks of identity-verification lead time. Ordering it is the single
highest-leverage action in the project — it does not need a session, only a
decision.

### Definition of Ready
- [ ] EV code-signing certificate ordered (date: ____)
- [ ] EV certificate received and the token in hand
- [ ] Hardware Dev Center account created and attestation signing working

### Definition of Done
- [ ] **Signed installer and binaries** — `bundle.windows.certificateThumbprint` + `signCommand`; the NSIS template already threads `UNINSTALLERSIGNCOMMAND`, so this is close to a one-line change.
- [ ] **Signed production APO DLL** with attestation, so it loads outside a test-signing VM.
- [ ] **Signed audio-class virtual microphone driver**, replacing the VB-Cable interim route.
- [ ] SmartScreen behaviour on a clean machine recorded — that is the user-visible reason this matters.
- [ ] ROADMAP:66, ROADMAP:102, M3b:13-15, M5:28 and M7:95 all checked off.

### Kickoff prompt
```
You are starting session S17 (EV certificate and signing) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S17), docs/plans/M3b-apo.md and docs/plans/M7-installer.md. Work in the main tree on branch chore/signing.

Four deliverables wait on this one certificate: the signed installer, the signed APO DLL, the signed virtual mic driver, and attestation.

1. Verify the Definition of Ready first. If the certificate is not in hand, stop immediately and tell me — there is nothing useful to do without it.
2. Start with the installer: bundle.windows.certificateThumbprint plus signCommand, with the NSIS template's UNINSTALLERSIGNCOMMAND already threaded.
3. Record SmartScreen behaviour on a clean machine before and after. That is the user-visible reason any of this matters.
4. Finish only when the Definition of Done is met. Check off the five tracking lines named in the DoD and update docs/ROADMAP.md.
```

---

# Group 5 — v1.1 backlog

Out of v1 scope (`docs/ROADMAP.md:104-108`). Create the worktree when the
session actually starts; these are sketches, not ready plans, and each needs its
own plan file written first.

## S18 — Relay Send VST3 · `feat/vst3-send`
The `daw-plugin/` crate in the brief, unbuilt. A VST3 that ships DAW master/bus
audio to Relay over shared memory — the only way to capture DAW audio under
ASIO exclusive mode. **First task of that session: write `docs/plans/v11-vst3.md`
with a real DoR and DoD.**

## S19 — Call-audio return route and mix-minus · `feat/mix-minus` · depends on S2
Audio back from the call to the sending PC, minus your own voice. S2's second
Opus track is the foundation; this is the feature it was always heading toward.

## S20 — Stream Deck plugin and NDI output · `feat/streamdeck-ndi`
Two separate integrations sharing one session only because both are outbound
control/output surfaces. Split if either grows.

## S21 — AI tuning loop · `feat/ai-tuning`
The `ai/` crate in the brief. User-supplied API key; headset measured curve plus
measured footstep/explosion bands from live game audio plus a stated goal → a
starting profile → short A/B tests → refinement. **Never on the hot path.**
The Games screen currently says this is planned and not built — that promise is
this session's to keep or remove.

---

## Recommended order

1. **Order the EV certificate today.** It gates four deliverables and has weeks
   of lead time. Everything else can proceed meanwhile.
2. **S5** (per-user camera registration) — may delete S12 and shrink S6.
3. **S9** (docs truth pass) — cheap, and every later session reads those files.
4. **S1, S2, S3** in parallel — the three highest-value code sessions.
5. **S10 + S11** — closes most of the "deferred to MVP validation" backlog
   without new hardware.
6. **S6, S7, S8** as capacity allows.
7. **S13, S14** once a hypervisor exists; **S15, S16** once the hardware does;
   **S17** once the certificate arrives.
8. **S4** only if a real compatibility gap appears; **S18–S21** after v1 ships.
