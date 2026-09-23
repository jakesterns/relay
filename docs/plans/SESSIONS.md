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
- **Do not block on a question.** These run unattended. If a decision is yours to make, make it, write down what you chose and why, and carry on. Stop only for something genuinely destructive or outside your brief.
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
does not work, so the HKLM write is permanent and S6 was required. S6 landed
the same day and built it, which unblocks S12 — the camera can now be
registered from the Settings card behind one UAC prompt.

## Starting a session

Either paste the kickoff prompt into a new Claude Code chat opened in that
session's worktree, or let the launcher do it:

```
pwsh scripts\start-session.ps1                      # list every session
pwsh scripts\start-session.ps1 -Session S5 -DryRun  # show the resolved prompt
pwsh scripts\start-session.ps1 -Session S1,S2,S3    # start three (commas, not spaces)
pwsh scripts\start-session.ps1 -All                 # start S1-S8
```

Session ids are **comma-separated**. Space separation binds the second id to
the next parameter and fails with a confusing complaint about
`PermissionMode`.

The launcher reads the kickoff prompts out of *this file*, so there is one copy
of each — edit the catalogue, not the script. It starts each session with
`claude --bg`, which returns immediately and prints a short id:

- `claude agents` — list running sessions
- `claude attach <id>` — open one in your terminal, to answer a question or
  take over
- `claude stop <id>` — end one

Feature trees are launched with `RELAY_NO_INSTALL=1`, so eight sessions cannot
fight over the single installed app; the main tree is left alone. Launches are
staggered 20 s apart, because eight simultaneous Rust release builds is not a
good use of the machine.

**`-All` deliberately covers only S1–S8.** The rest need you at the keyboard
(S10 is nothing *but* asking you questions) or an external unblock, so starting
them unattended burns tokens waiting. Name them explicitly if you want them.

Two things worth knowing before starting several at once. The default
`-PermissionMode acceptEdits` lets a session edit files in its own worktree
without prompting but still asks before running commands, so a background
session will sit waiting until you attach — that is the intended shape, not a
hang. And every running session costs tokens continuously, so three attentive
sessions beat eight neglected ones.

| # | Session | Branch | Worktree | Blocked on |
|---|---|---|---|---|
| S1 | ADLX display backend | `feat/adlx-display` | `relay-adlx` | — |
| S2 | Second Opus track | `feat/dual-audio` | `relay-dual-audio` | **done 2026-09-14** |
| S3 | Vendor VCP opcodes | `feat/monitor-vcp` | `relay-monitor-vcp` | partly: OSD eyes |
| S4 | MKV container | `feat/mkv-container` | `relay-mkv` | **done 2026-09-14** |
| S5 | Per-user vcam registration | `feat/hkcu-vcam` | `relay-hkcu-vcam` | **done 2026-09-14 — HKCU does not work** |
| S6 | Elevated install helper | `feat/elevated-install` | `relay-elevation` | **done 2026-09-14** (was required, not optional: S5 said no) |
| S7 | Codec robustness | `feat/codec-robustness` | `relay-codec` | — |
| S8 | WebView UI test harness | `feat/ui-test-harness` | `relay-uitest` | — |
| S9 | Documentation truth pass | `chore/docs-truth` | main tree | **done 2026-09-15** |
| S10 | Human verification pass | `chore/human-pass` | main tree | you, 45 min |
| S11 | Soak and measurement pass | `chore/soak-pass` | main tree | ~2 h wall clock |
| S12 | Live virtual-camera pass | `chore/vcam-live` | main tree | — (S6 unblocked it) |
| S13 | Clean-VM uninstall diff | `chore/vm-uninstall` | main tree | a hypervisor |
| S14 | Live APO test-sign pass | `chore/apo-vm` | main tree | a hypervisor |
| S15 | Two-PC share validation | `chore/two-pc` | main tree | a second PC |
| S16 | Second-monitor display pass | `chore/second-monitor` | main tree | a second panel |
| S17 | EV certificate and signing | `chore/signing` | main tree | the certificate |
| S22 | Firewall rules in the installer | `feat/firewall-rules` | create when started | — |
| S23 | Never look dead | `feat/never-dead` | create when started | — |
| S24 | Stop the UI lying | `feat/honest-ui` | main tree | **done 2026-09-16** |
| S25 | Keyboard, focus, destructive actions | `feat/ui-safety` | main tree | **done 2026-09-15** |
| S26 | Shell polish | `feat/shell-polish` | main tree | **done 2026-09-16** |
| S18 | Relay Send VST3 | `feat/vst3-send` | create when started | — (v1.1) |
| S19 | Call-audio return and mix-minus | `feat/mix-minus` | create when started | S2 done, so unblocked (v1.1) |
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
- [x] Confirmed 2026-09-14: **no AMD GPU drives a display here** (Raphael iGPU present, nothing attached; the LG hangs off the RTX 3090). Fixtures for the colour writes, plus a live read-only ADL probe that verifies the FFI. Recorded in `docs/plans/M2-display.md`.

### Definition of Done
- [x] `relay-display::amd` behind `DisplayIo`, selected at runtime by vendor; NVIDIA path unchanged. Transport is ADL (`atiadlxx.dll`), not the ADLX vtables — reasoning in the plan file.
- [x] Proven by `crash_restore_amd.rs`: the same profile through a real `taskkill /F`, once per vendor, asserting exact value-for-value restore. `crash_restore.rs` unchanged and green.
- [x] Curve recorded in the plan file (piecewise-linear with the knee at the driver default; hue clamped, not rescaled) and unit-tested, including the deliberate NVIDIA/AMD divergence on desaturation.
- [x] `neither_vendor_degrades_to_unsupported_and_never_panics`: gamma ramp and DDC/CI still carry what they can, vendor-only fields report unsupported.
- [x] `docs/plans/M2-display.md` ADLX line checked off (with an S1 session log); ROADMAP M2 row and checklist updated.

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

## S2 — Second Opus track (mic *and* desktop audio) — **DONE 2026-09-14**
**Branch** `feat/dual-audio` · **Worktree** `C:\Users\stern\Documents\Code\relay-dual-audio`

The sender shipped one audio track, so choosing Microphone *replaced* the
desktop mix. This was the deferred item most likely to embarrass someone
mid-call.

**Outcome.** Two Opus tracks on the wire, summed on the receiver one op before
the render buffer — decision and rationale in `docs/dev/dual-audio-decision.md`,
written before the code as the DoR required. Cost measured against M4's
baseline: **+0.05 ms p50 / +0.57 ms p99** on capture→arrival, encode mean
unchanged, 60 fps and zero drops (4 alternating reps,
`scripts/dual-audio-check.ps1`). Merged S4's MKV work into this tree so both
containers carry both tracks rather than leaving MKV silently single-track.

Two findings worth carrying forward:
- **The mic's encoder profile is load-bearing.** Giving the mic the program
  mix's music-grade encoder cost the *video* path a repeatable regression
  (encode mean 5.12 → 5.86 ms, arrival p99 3.3 → 7.6 ms) through CPU
  contention alone. `OpusProfile::voice()` removed it entirely.
- **WASAPI loopback of a silent endpoint delivers no packets at all**, so a
  quiet desktop makes an audio comparison measure nothing while looking fine.
  Two passes were thrown away before this was caught.

### Definition of Ready
- [x] Mic path built and benchmarked (`AudioSource::Microphone`).
- [x] M4 measurements table is the latency baseline to beat.
- [x] Decide where mixing happens: receiver-side mix, or two tracks the receiver routes separately (the virtual mic wants them separate; a plain call wants them mixed). Record the decision before writing code. → **Two tracks, receiver mixes, mixing is the default** (`docs/dev/dual-audio-decision.md`).

### Definition of Done
- [x] Sender can carry desktop mix **and** microphone simultaneously as two Opus tracks; existing single-track presets are unchanged. (`relay-audio` + `relay-audio-mic`; `--audio-mic` is additive, and a legacy `mic` preset still resolves to `--no-audio --audio-mic`, byte-identical to before.)
- [x] Receiver handles one track or two, and an older peer sending one track still works. (`transport::audio_role` classifies by msid track id with arrival order as the fallback; unit-tested for unnamed and unrecognised ids.)
- [x] Added latency measured against M4's baseline and recorded; still inside the budget. (+0.05 ms p50 / +0.57 ms p99, versus a 50 ms glass-to-glass budget — table in `docs/plans/M4-share.md`.)
- [x] The warning note under the Audio chips in `ui/src/screens/Share.tsx` is removed, and the chips become a source *set* rather than a single choice. (New `ChipSet` component; the two desktop sources exclude each other, the mic toggles independently. The instrument strip grows a Mic meter when a second track arrives.)
- [x] Recording carries both tracks. (Both containers: program on track 2, mic on track 3, named, unmixed. ffprobe-verified on real loopback recordings.)
- [x] M4 and M6 Deferred entries struck; ROADMAP rows updated.

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
- [x] Accept that set-and-readback cannot prove on-screen meaning: each opcode needs either vendor documentation or a human watching the OSD. Unverified entries must be marked unverified, not shipped as fact. — Accepted, and built into the types rather than left as a rule to remember: `Evidence` has exactly the two verifying variants plus `Unverified`, and read-back is not one of them.

### Definition of Done
- [x] A per-model quirks table keyed by PNP ID and model string, each entry carrying its evidence (vendor doc URL, or "observed on OSD by <person> <date>"). — `vcp::QUIRKS`, keyed on EDID manufacturer id **plus product code** (`GSM` + `5C7C`), which together name one model; `models` carries the MCCS `model(...)` strings and EDID display names for humans. Every vendor opcode is a `Candidate` = code + `Evidence` (`VendorDoc { title, url }` / `Osd { observer, date, monitor }` / `Unverified { note }`); the field is not optional, so a row without evidence does not compile.
- [ ] Verified entries for at least the LG ULTRAGEAR+ on this desk; everything else marked unverified and **not** used to enable a control. — **BLOCKED on an OSD observation session with the user.** Zero entries are verified: both LG candidates (0xF6 black equaliser, 0xF5 response) ship as `Evidence::Unverified`, so both sliders are disabled. Set-and-readback cannot close this — a panel will store and return a value for a control whose on-screen meaning is something else — so it needs eyes on the OSD. Runbook `docs/dev/vcp-verification.md`; harness `crates/display/tests/vendor_probe.rs`. Open the OSD on Game Adjust / Picture, hands off the joystick, then one code at a time:
  ```powershell
  $env:RELAY_VCP_PROBE = "F5:1|2|3|4"
  cargo test -p relay-display --test vendor_probe -- --ignored --nocapture
  ```
  Sweep `F5:1|2|3|4`, `F6:0|1|2`, `F7:0|1|2|3`, `F8:0|1`, `FA:0|1`, `FE:0|1|2`. (`F4`, `F9`, `FD`, `FF` advertise no value list — not worth sweeping blind.) Record **which OSD label moved and which written value maps to which level**; the code alone is not enough, since knowing 0xF5 is overdrive is useless without knowing whether `2` means "fast" or "off". Deliberately no prediction is published about what each code does: priming the observer with an expected label is how a wrong entry gets confirmed. Any code whose effect nobody can see stays `Unverified` and enables nothing — a legitimate result, not a failed run.
- [x] Never guesses: writing an unknown code is impossible by construction, and there is a test proving an unverified entry does not enable the slider. — Enforced by a type, not by discipline: `VerifiedCode`'s `u8` is private to its module, `attest(code, evidence)` is its only constructor and returns `None` for `Evidence::Unverified`, and `plan_writes` / `vendor_controls` take `VerifiedCode` rather than `u8`, so there is no path from an unverified entry to a `SetVCPFeature` call. Proof: `unverified_table_entry_does_not_enable_the_slider` — the LG row *has* both candidates, the panel advertises both, the profile asks for both, and still nothing is written and neither slider enables. Backed by `no_unverified_row_ever_resolves_to_a_code`, `vendor_wide_rows_carry_no_vendor_opcodes`, `verified_entries_carry_traceable_evidence` and `model_rows_sort_before_their_vendor_wide_row`.
- [x] The UI note in `ui/src/screens/Games.tsx` shrinks to match what is now supported. — Down to one sentence, and only rendered when a control is actually off. The two sliders are now driven by `Reply::Hardware.vendor_controls`, which the **core** computes from the table and the advertised opcode list — the client is told, never asked, because a UI that inferred a control from the advertised list would defeat the type guarantee. The hand-maintained `RESPONSE_LEVELS` constant is gone; levels come from the verified value map.
- [x] `docs/plans/M2-display.md:132` updated; ROADMAP M2 row updated. — Plus a new runbook, `docs/dev/vcp-verification.md`, covering the evidence rules, why set-and-readback is not proof, and the `VerifiedCode` invariant future edits must not break.

**State:** code-complete and pushed (`feat/monitor-vcp`); 24 `relay-display` + 127 `relay-core` tests green, clippy and `tsc` clean. Four of five items done; the session is not finishable without the OSD pass above.

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
- [x] A named reason to do it now: a player or editor that rejects the current Opus-in-fMP4 output. **Found and recorded 2026-09-14**: `Windows.Media.Editing.MediaClip` — the Windows video-editing import API — rejects every Relay fMP4 recording with "The parameter is incorrect." The cause is *not* Opus (Media Foundation reports `OPUS … FullySupported`) but the missing `mfra` index. Full probe matrix: `docs/dev/container-compat.md`.

### Definition of Done
- [x] MKV selectable per preset; fMP4 remains the default.
- [x] Golden-fixture byte tests to the same standard as the fMP4 muxer (`tests/fixtures/golden-recording.mkv`, `UPDATE_GOLDEN=1` to regenerate).
- [x] Still a tee of the share's existing bitstream — no second encode, no added latency; re-measured (MKV vs MP4: arrival p50 2.41 vs 2.32 ms, CPU median 6.22 % vs 6.23 %).
- [x] Replay save works in both containers (73 ms MKV / 59 ms MP4 on loopback; unit test runs for both).
- [x] `docs/plans/M6-recording-presets.md` updated (decision 5 + S4 measurements); ROADMAP M6 row updated.
- [x] Bonus, out of the DoR investigation: `Mp4Muxer` now writes `mfra`, which fixes editor import for *existing* fMP4 recordings too.

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

### Definition of Done — **met 2026-09-14**
- [x] One elevated helper, launched on demand with a UAC prompt, that performs exactly the recorded install/uninstall steps and nothing else. `relay-elevate.exe` + `crates/core/src/elevate.rs`; the request carries no registry path, value or DLL path, so there is no field capable of naming anything else.
- [x] The user sees what will be changed *before* the prompt — for the two removal ops it *is* the uninstall card's listing, narrowed to that step and rendered by `Plan::lines`. Asserted over the pipe for all four ops.
- [x] Helper refuses to run anything not in the plan; the live-write gates stay in force. Four-variant op enum, COM keys vetted against our own CLSID, endpoint ids vetted as GUIDs, versioned + expiring + location-checked requests. Each gate is armed around one vetted call and removed after; nothing else in the product arms them.
- [x] Declining UAC leaves the machine untouched and says so plainly. Verified live: *"Nothing on this PC was changed. You declined the Windows permission prompt."*, camera left registered, registry unchanged.
- [x] Settings "Install APO" and the camera card work end to end on this machine. Both installed and removed from the cards; the endpoint's `FxProperties` export is byte-identical before and after. `docs/dev/elevation-live.md`.
- [x] `docs/plans/M3b-apo.md` Deferred item 3 closed; M7 plan updated (its Deferred item 2 closed too).

### What it also fixed
The uninstaller's elevated phase could not have worked: it re-ran `relay-core
uninstall --components-only` under `runas` with the live-write gates set in the
*parent*, and elevation starts the child from the user's logon environment
block. And `RegCreateKeyExW(KEY_WRITE)` is denied on an endpoint's
`FxProperties` even when elevated — administrators get `SetValue` without
`CreateSubKey`. Both are written up in `docs/dev/elevation-live.md`.

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
- [x] Decide which are worth fixing versus documenting as hard limits. Not all three deserve code. **Done 2026-09-14:** the allocator path and the audio format get code; software HEVC encode stays a design limit.

### Definition of Done
- [x] MFT allocator path either implemented or turned into a clear, actionable message naming the adapter. **Implemented** (`alloc_output_sample`); not testable end-to-end here because both local MFTs set `PROVIDES_SAMPLES`, so it is unit-tested through the `take_output` read path and `RELAY_FORCE_MFT_ALLOCATOR=1` exists to take it on a machine that reports `CAN_PROVIDE` only.
- [x] Non-48 kHz and non-stereo endpoints produce a specific, fixable message rather than a generic failure — or are supported. **Supported**: `capture/src/resample.rs` folds any channel count to stereo and converts any rate to 48 kHz. Only a 0-channel or out-of-range endpoint still fails, and it names the number it saw.
- [x] No-hardware-encoder machines are told the truth early. Confirmed: the S1-era banner covers it, and now names the GPU; the engine-side failure also lists any other adapter in the PC that could encode.
- [x] The HEVC Video Extension question is resolved one way. **Decision: keep detect-and-warn and link to the Store; do not bundle it (Microsoft licenses it to PC makers, not for redistribution), do not build a DXVA-direct decoder (weeks of HEVC bring-up whose failure mode is corrupt video, for a receiver one free Store install away from working).** Written up in `docs/plans/M4-share.md` → Deferred.
- [x] `docs/plans/M4-share.md` updated: the Deferred entry is closed and a new "S7 - codec and capture robustness" section records the decisions and measurements.

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
- [x] Pick the approach: component tests against a mocked `api` (safe, fast, no window) versus WebDriver against a real Tauri window (higher fidelity, moves a real cursor). Default to the former unless there is a reason. **Component tests chosen** (2026-09-14) — Vitest + jsdom + Testing Library; rationale and the fidelity gap are written up in `ui/src/test/README.md`.

### Definition of Done
- [x] Every screen renders against both mock data and an offline core without throwing. (`ui/src/screens/screens.smoke.test.tsx`, plus a third mode: a scripted live core.)
- [x] The paths that were mocks and got wired this month have regression tests: share preset start/stop, catalogue search and import, uninstall plan rendering, consent flow.
- [x] The suite runs in CI on `windows-latest` alongside `pnpm build`.
- [x] No test moves the real mouse or sends synthetic keystrokes to the desktop. Enforced by `ui/src/test/safety.test.ts`, not just by convention.
- [x] `docs/plans/M0-foundation.md` updated (deferred item closed; new "UI test harness" section).

**Done 2026-09-14** on `feat/ui-test-harness`. 158 tests, 9 files, ~12 s. Found
and fixed one real bug: the Share preset editor's encode-size field could not
be typed into.

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

### Definition of Done — **all met 2026-09-15** (branch `chore/docs-truth`)
- [x] `docs/plans/M3-audio-dsp.md:65` — "headset-correction curves waits on M1's hardware library" is stale; M1 landed and the correction path was built (`crates/audio/src/fit.rs`, `audio_bridge::chain_params_with`, the Games correction card). Verify against the code, then strike it.
- [x] `docs/plans/M1-hardware.md:36-37` — "Out of scope: online curve download" is stale; `crates/core/src/hardware/catalog.rs` fetches measurements over WinHTTP with attribution. Correct it and state the licensing position (AutoEQ data is CC BY-NC-SA: index ships, measurements fetch on demand, never redistributed).
- [x] `docs/ROADMAP.md` — M7 is dated 2026-09-12 in the status table and 2026-09-13 in its section. Pick one.
- [x] Every ROADMAP status-table row re-read against its plan's Deferred section, and this catalogue cross-linked from `docs/plans/README.md` and `KICKOFF-PROMPTS.md`.
- [x] No claim in any plan file asserts something the code does not do.

### What was stale (2026-09-15)
All three listed drifts were real. Beyond them:
- **M4's Definition of Done** listed four acceptance criteria as plain bullets with no markers, two of which (the two-PC 4K60 glass-to-glass run and "works with no network configuration on either PC") have never been run. Both are now `[ ]` and labelled **Aspirational**; the discovery one is sharpened, because `scripts/m6-loopback.ps1` passes `--peer` explicitly and so bypasses the mDNS discovery that criterion is actually about.
- **M5's Deferred item 1, the ROADMAP M5 section, and `docs/dev/vcam-live.md`** all still said the elevated registry write was the blocker. S6 removed that blocker on 2026-09-14 and registered and removed the camera live. What is genuinely left is `IMFVirtualCamera::Start` returning `S_OK` and a real call — S12's scope, which SESSIONS.md already had right.
- **M7's DoD item 2** said the APO *restore* path was not live-proven and was "the first thing the VM pass has to check". S6 proved it byte-identical on 2026-09-14. The ROADMAP's M7 section listed the same closed item as deferred.
- **M0's Deferred console-flash item** was never struck through, though M7 closed it with `relay-svc.exe`.
- **The catalogue and the headset-correction chain were not recorded anywhere** outside their commits — no plan file and no ROADMAP row mentioned either, so a session reading the plans would not have known they existed.
- **`CLAUDE.md` called `crates/capture/` and `crates/display/` placeholders.** Both have been complete for days. Its core crate map also had no `hardware/` entry.
- Unqualified `[x]` checklist items in the ROADMAP for the virtual camera and the interim mic now say which half is live-proven and which is test-proven only.

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
**Branch** `chore/vcam-live` · **Worktree** main tree · **Unblocked** (~20 min, one UAC prompt)

S5 has run (2026-09-14): per-user registration does **not** work, so the HKLM write is still required — but S6 built the thing that performs it, and the camera has already been registered and removed live through it (`docs/dev/elevation-live.md`). So this session no longer needs a hand-rolled elevated shell: `relay-core elevate run install-camera`, or the Settings card, does it. What is left is the part S6 did not do — pointing a real call at the registered camera. Start with the two-minute positive control at the end of `docs/dev/vcam-live.md`: run `vcam_reg_probe` once the key is in HKLM and confirm `Start` returns `S_OK`.

### Definition of Ready
- [x] S5 resolved 2026-09-14: HKCU does not work → elevation required.
- [x] S6 landed 2026-09-14: registration is one approved UAC prompt away, and the removal path is proven.
- [ ] You are present to approve the prompt.
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

S5 answered the hive question (HKCU does not work) and S6 built the elevated helper, which has already registered and removed the camera live. Register it with `relay-core elevate run install-camera` or the Settings card — one UAC prompt, which I approve — then run the positive-control probe in the runbook.

1. Verify the Definition of Ready. Never write HKLM by hand; go through the helper and tell me when the prompt is coming.
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

## S22 — Firewall rules in the installer
**Branch** `feat/firewall-rules` · **Worktree** `C:\Users\stern\Documents\Code\relay-firewall`
**Done 2026-09-15.** `crates/core/src/firewall.rs` is the whole feature: a
pure parser and verdict over the firewall policy store (reads), `INetFwPolicy2`
in the elevated helper (writes), `firewall.json` as the record, a new
`StepKind::RemoveFirewallRule` in the uninstall plan, a `FirewallBanner` on the
Share and Receive screens, and firewall capture + diff in the snapshot harness
(verified to FAIL on a planted leftover rule). Scope decided: **private +
domain, never public**. Details in `docs/plans/M7-installer.md`.

Found 2026-09-14 on the dev machine: ten accumulated Block rules for
`relay-share.exe` and not one Allow rule. Windows prompts the first time a
given *path* listens, and dismissing that prompt writes a Block rule that never
goes away.

This is not a dev-only annoyance. **"Zero network config for the user" is a
non-negotiable in `CLAUDE.md`**, and today a real user installs Relay, starts a
share, gets a Windows firewall prompt they do not understand, clicks the wrong
button once, and Relay is permanently broken for them with no visible cause.
`scripts/firewall-rules.ps1` cleans it up after the fact; the installer should
mean nobody needs it.

### Definition of Ready
- [x] Only `relay-share.exe` listens on the network (WebRTC + mDNS); `relay-core` is named-pipe only.
- [x] `scripts/firewall-rules.ps1` exists and can list, clean and allow.
- [x] Decide the scope: Private profile only (Relay is LAN-only by design) versus Private + Domain. Public should stay blocked.
      **Private + Domain** (`firewall::RULE_PROFILES`). A managed work machine
      reports its network as Domain rather than Private, so a private-only
      rule would leave exactly the silent failure this session exists to
      remove. Public stays blocked: Windows classifies unknown networks as
      Public by default, and the banner names that case instead of offering a
      fix Relay will not apply.

### Definition of Done
- [x] The NSIS installer adds an inbound Allow rule for the installed `relay-share.exe` on the agreed profiles, and the **uninstaller removes it** — a leftover firewall rule would fail the clean-VM diff, so this must be in the uninstall plan like everything else.
- [x] Rules are added by the existing elevated path, not by a silent elevation grab; a user who declines still gets a working app on an already-permissive network, with an explanation.
      Two new `ElevatedOp` variants (`allow-firewall`, `remove-firewall`); the
      helper re-derives the exe path from its own directory, so the request
      carries no path. Declining is reported as `declined`, not an error, and
      the banner says what still works. `Verdict::Permissive` is the
      already-permissive case and shows no banner at all.
- [x] The app detects the "blocked by firewall" state and says so plainly, rather than looking like a network fault — the same warn-before-you-fail shape as the HEVC capability banner.
      `FirewallBanner` in `ui/src/screens/Receive.tsx`, rendered on Share and
      Receive next to `CodecBanner`. 8 UI tests cover blocked, will-prompt,
      declined UAC, public network, unreadable probe and permissive.
- [~] Verified in the clean-VM cycle (S13): install, share, uninstall, empty diff.
      **As far as the missing hypervisor allows.** `machine-snapshot.ps1` now
      captures firewall rules, `snapshot-diff.ps1` diffs them as its own
      section, and a planted leftover rule was confirmed to FAIL the diff with
      exit 1 while identical snapshots still PASS. `vm-cycle.ps1`'s opt-in
      phase adds the rule and asserts both the rule and `firewall.json`. The
      checkpoint run itself stays blocked on the same missing hypervisor as
      M7's Deferred item 1 — it is the only part of this DoD not met, and it
      is not blocked on anything in this session.

### Kickoff prompt
```
You are starting session S22 (firewall rules in the installer) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S22), docs/plans/M7-installer.md and scripts/firewall-rules.ps1. Create the worktree first: git worktree add -b feat/firewall-rules ..\relay-firewall main, then cd into it and run pnpm install in ui/.

Zero network config for the user is a non-negotiable, and right now a user who dismisses one Windows firewall prompt breaks Relay permanently with no visible cause. On this dev machine that produced ten Block rules and zero Allow rules.

1. Only relay-share.exe listens; relay-core is named-pipe only. Do not add rules for anything else.
2. The installer adds the rule and the uninstaller removes it. A leftover firewall rule would fail the clean-VM diff, so it belongs in the uninstall plan like every other change.
3. Never grab elevation silently. If the user declines, the app must still work where it can and explain where it cannot.
4. Add detection so the blocked state is reported as what it is, not as a network fault — copy the shape of the HEVC capability banner we added on the Share and Receive screens.
5. Finish only when the Definition of Done is met. Update docs/plans/M7-installer.md and docs/ROADMAP.md, then summarise.
```

---

# Group 6 — MVP polish

Found by an audit on 2026-09-15, after all eight code sessions merged. None of
these are missing features; they are the things that make a finished product
feel unfinished in the first ten minutes. **S23 and S24 both touch
`Share.tsx` and `Games.tsx` — do not run them concurrently.**

---

## S23 — Never look dead
**Branch** `feat/never-dead` · **Worktree** `C:\Users\stern\Documents\Code\relay-never-dead`

The worst first impression in the product. Autostart is off by default, the UI
never starts the core, and the offline banner tells a desktop user to type
`relay-core run` in a terminal. So: install, reboot, open Relay, and every
screen reports the service is not running with no way to fix it from the app.
That is also a regression against an explicit product decision — Relay exists
as an installed app precisely so nobody has to run terminal commands.

Separately, the core sends `notice` strings over IPC and `core.tsx` expires
them after 4 s, but nothing renders them. Every backend toast — including the
preview toggle — is dropped on the floor.

### Definition of Ready
- [x] `relay-svc.exe` already starts the core windowless and exits; the installer uses it.
- [x] `OfflineBanner` already knows the difference between offline and mock.
- [x] Decide the background story: Relay's core keeps running after the window closes, by design, with no tray icon and nothing saying so. For an app that changes audio and display settings, decide whether that silence is acceptable, and record the decision.
      **Decided 2026-09-15: not acceptable — a tray icon owned by the CORE, not the UI.** Closing the window must free the Tauri process during gaming, so a UI-owned tray would die exactly when it is the only thing left saying a profile is applied. Menu: Open Relay / Restore everything / Quit Relay, with Quit going through the existing restore. Plus a plain line in Settings about what keeps running, and close-behaviour as a preference rather than forced. Full record: `docs/dev/never-dead.md`.

### Definition of Done
- [x] Opening the app with no core running starts it (via `relay-svc.exe`) or offers a single button that does. No terminal command appears in any user-facing string. (Both: the shell attempts a start on setup, and the banner carries a button. `startup::tests::no_message_names_a_command` and a UI test assert the strings.)
- [x] A core that cannot be started says why, in terms a user can act on. (`startup::StartError`, one variant per remedy, no catch-all.)
- [x] `notice` events render somewhere the user will see them, and expire quietly. (`components/Toasts.tsx`; per-notice 4 s clock, no dismiss control.)
- [x] The background-service decision from the DoR is implemented — either a tray affordance or an explicit, honest line about what keeps running after the window closes. (Both: core-owned tray + the Settings line.)
- [x] Reboot test on this machine with autostart off: open Relay from the Start Menu and reach live state without touching a terminal. **PASS 2026-09-16** against the *installed* build from `Relay_0.1.0_x64-setup.exe` — `scripts/cold-start-check.ps1` with every Relay process killed and autostart off: core up in 0.32 s, pipe answering in 0.59 s, launched via the real Start Menu shortcut. Not a true cold boot (the page cache was warm), so the only thing still unproven is first-launch-after-restart disk time.

### Kickoff prompt
```
You are starting session S23 (never look dead) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S23). Create the worktree first: git worktree add -b feat/never-dead ..\relay-never-dead main, then cd into it and run pnpm install in ui/.

Autostart is off by default, the UI never starts the core, and ui/src/components/Offline.tsx tells a desktop user to run `relay-core run` in a terminal. Install, reboot, open Relay, and the app is dead with no in-app remedy. Relay is an installed app specifically so nobody has to use terminal commands, so this is a regression against a product decision, not a missing nicety.

1. Make opening the app reach live state without a terminal. relay-svc.exe already starts the core windowless; the installer uses it.
2. No user-facing string may name a CLI command. If the core cannot start, say why in terms the user can act on.
3. The core emits `notice` events that ui/src/lib/core.tsx stores and expires after 4 s, and nothing renders them. Surface them.
4. DECIDED, do not ask: build a tray icon owned by the CORE (Shell_NotifyIcon on the existing winloop HWND), not the UI -- closing the window must free the UI process during gaming, so a UI-owned tray would die exactly when it is needed. Menu: Open Relay / Restore everything / Quit Relay, with Quit going through the existing restore so it can never leave a game profile applied. Plus a plain line in the UI stating what keeps running. Close-behaviour is a Settings preference, not forced. Re-run scripts\footprint.ps1 and record the before/after cost in the plan; stop and tell me only if it approaches the 10 MB ceiling.
5. Prove it with a real reboot on this machine, autostart off, launching from the Start Menu.
6. Finish only when the Definition of Done is met, then update docs/ROADMAP.md and summarise.
```

---

## S24 — Stop the UI lying
**Branch** `feat/honest-ui` · **Worktree** main tree · **Done 2026-09-16**

Three places render invented content as if it were measured. These are worse
than blank space because a user tests them early and believes them.

### Definition of Ready
- [x] The real data exists for all three: profile colour values, the headset curve in the hardware library, and the live preset plus `ShareCapabilities.adapters`.
- [x] Decide per case: make it real, or remove it. A removed element is a perfectly good outcome — decoration that cannot be driven by real data should not be in a product whose every screen claims nothing was faked.
      **Decided 2026-09-16:** EQ graph and Share labels made real (the data exists). Display A/B made real *for the part Relay can reproduce exactly* — the gamma ramp — and says in words that vibrance and hue are not previewed, because those go through the vendor driver and a CSS `saturate()` would be another invention. The `.scene` gradients were removed outright: an empty frame is the honest placeholder.

### Definition of Done
- [x] `Games.tsx` display A/B: both halves currently render the identical `<Scene/>` differing only by a hard-coded gradient, so Vibrance/Gamma/Contrast change nothing. Either it reflects the profile's actual colour settings, or it goes. (Both halves are one reference pattern; the right goes through an SVG `feComponentTransfer` built by `lib/honest.ts::buildRamp`, a port of `relay_display::gamma::build_ramp` pinned to it by shared golden samples on both sides. A note states vibrance/hue are not shown.)
- [x] `Games.tsx` EQ graph: the dashed "Headset raw response" is a hard-coded path shown even for a headset with no curve. Drive it from the real curve, and hide it when there is none. (Log-frequency axis; dashed line is the imported curve point for point, gone with no curve. The label was wrong too — `autoeq.rs` stores the *correction*, not the raw response — so it now reads "Headset correction · measured". The gold line, previously straight segments between slider values, is the RBJ peaking cascade's actual response.)
- [x] `Share.tsx`: the overlay's `3840×2160 / 60 fps / HEVC` and `NVENC · CPU x%` are hard-coded. Derive from the live preset and the adapter name the capability probe already returns. (Idle: the selected preset. Sharing: the preset this screen started plus the engine's measured fps; a share the screen did not start shows no size, since the core does not report its preset. Encoder name from `ShareCapabilities.encoders`, falling back to `adapters`, and "Hardware encoder" when two vendors could encode. Preset chips lock during a share. The load % had a hard-coded 60 fps budget too; it uses the preset's rate now.)
- [x] `Receive.tsx` / `Share.tsx` `.scene` decoration: either a real thumbnail or an honest placeholder; not a painted gradient under a real caption. (Gradient and `.horizon` removed; an empty dark frame until the real thumbnail arrives.)
- [x] A test pins at least the EQ and share-label cases, so the next session cannot quietly re-hard-code them. (`lib/honest.test.ts`, new `Games.test.tsx` EQ-graph and A/B blocks, `Share.test.tsx` overlay block; `gamma.rs::ramp_golden_samples_shared_with_the_ui`. 241 UI tests.)

### Kickoff prompt
```
You are starting session S24 (stop the UI lying) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S24). Check first that session S23 has finished — it touches the same files, so do not run alongside it. Work in the main tree, C:\Users\stern\Documents\Code\Stream Share, on a new branch: git checkout -b feat/honest-ui. Do NOT create a worktree. Set RELAY_NO_INSTALL=1 for your commits.

Three parts of the UI render invented content as if it were measured: the Display tab's A/B comparison (both halves are the same scene, so the colour sliders change nothing), the EQ graph's "Headset raw response" (a hard-coded path, shown even when the headset has no curve), and the Share overlay's resolution/fps/encoder labels (hard-coded 4K60 NVENC regardless of preset or GPU).

1. For each, decide yourself and record why -- do not block waiting on me. Make it real where the data exists, remove it where it does not. Removal is a good outcome — this product tells the user on every screen that nothing was faked, so decoration that cannot be driven by real data does not belong.
2. The real data already exists in all three cases: the profile's colour values, the hardware library's curve, and the live preset plus ShareCapabilities.adapters.
3. Pin at least the EQ and the share labels with tests so they cannot quietly regress to hard-coded values.
4. Finish only when the Definition of Done is met, then update docs/ROADMAP.md and summarise.
```

---

## S25 — Keyboard, focus and destructive actions
**Branch** `feat/ui-safety` · **Worktree** main tree · **Done 2026-09-15**

The app was mouse-only, and the same class of destructive action was handled
four different ways. Both closed: `ConfirmButton` is now the only way Relay
asks, an unsaved per-game edit survives navigation and a focus change
(`ui/src/lib/drafts.ts`), and `errText()` (`ui/src/lib/err.ts`) replaced every
`String(e)`. 203 UI tests, still jsdom only.

### Definition of Ready
- [x] S8's harness (`pnpm test`, 170 tests) can assert keyboard interaction in jsdom without touching the desktop.
- [x] Profiles already has the good two-step delete pattern to standardise on.

### Definition of Done
- [x] Every interactive control is reachable and operable by keyboard: rail nav items are real buttons/links, `Toggle` is focusable with an accessible name, profile rows have a keyboard path to edit and apply.
- [x] Visible `:focus-visible` styling everywhere, in the existing design language.
- [x] Global `user-select: none` is relaxed for text worth copying: paths, pairing code, error messages, monitor ids.
- [x] **One** confirmation pattern for destructive actions, replacing the current four (instant preset delete, unconfirmed Restore-all, native `window.confirm` for hardware, two-step for profiles). Native dialogs go.
- [x] "Restore original state now" gets a `.catch` and success feedback; today a failure is indistinguishable from success.
- [x] Editing a profile and navigating away, or the focused game changing mid-edit, no longer discards changes silently.
- [x] Errors use the existing `errText()` helper rather than `String(e)` (which renders `[object Object]` for non-string rejections), appear near the control that failed, and are dismissible.
- [x] Keyboard paths and the confirmation pattern are covered by tests.

### Kickoff prompt
```
You are starting session S25 (keyboard, focus and destructive actions) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S25). Work in the main tree, C:\Users\stern\Documents\Code\Stream Share, on a new branch: git checkout -b feat/ui-safety. Do NOT create a worktree. Set RELAY_NO_INSTALL=1 for your commits.

The app is mouse-only — rail nav items are <a> with no href, Toggle puts role="switch" on a non-focusable inner div, profile rows are <tr onClick>, and there are no focus styles anywhere. Global user-select: none also prevents copying paths, the pairing code, and error text.

Separately, four different patterns exist for destructive actions: preset delete fires instantly, Restore-all has no confirm and no .catch so a failure looks like success, hardware removal uses a native window.confirm inside a custom-chrome window, and profiles do it properly in two steps.

1. Make everything keyboard-operable with visible focus styling in the existing design language. Standardise on one confirmation pattern — the Profiles two-step is the one to keep. Native dialogs go.
2. Unsaved profile edits are currently discarded silently when navigating away or when the focused game changes. Fix that.
3. Use the existing errText() helper instead of String(e), which renders [object Object] for non-string rejections.
4. Cover the keyboard paths and the confirmation pattern with tests. S8's harness runs in jsdom, so nothing touches the real desktop — keep it that way.
5. Finish only when the Definition of Done is met, then update docs/ROADMAP.md and summarise.
```

---

## S26 — Shell polish
**Branch** `feat/shell-polish` · **Worktree** main tree · **Done 2026-09-16**

Small Windows-integration details, none hard, all noticed.

### Definition of Ready
- [x] Icons are already custom and on-brand, not the Tauri default.
- [x] Decide the publisher string. It shows in Add/Remove Programs and, now that S6 added an elevation prompt, in the UAC dialog. Unsigned binaries will still read "Unknown publisher" there until the EV certificate lands (S17).
      **Decided 2026-09-16:** publisher `Relay`, support URL `https://github.com/jakesterns/relay`.

### Definition of Done
- [x] Window size and position persist across launches (currently resets to 1280×800 centred every time). (`ui/src-tauri/src/window_state.rs`, in the UI process only. Normal bounds + maximised flag from `GetWindowPlacement` at close — tracking move/resize events was tried first and recorded the *maximised* rectangle — into `data\window.json`, so delete-my-data covers it. The window is created hidden and shown after placement, so there is no jump. A pure `placement()` rejects a title bar that would not be reachable on any current work area (unplugged monitor, off the top) and shrinks/nudges a window saved on a bigger screen; 10 unit tests. Live on this PC: move → relaunch exact; maximised → relaunch maximised and un-maximises to the saved bounds; bounds on a missing monitor → centred default.)
- [x] A second launch focuses the existing window instead of opening a second one. The core has a single-instance mutex; the UI has none. (`Local\RelayUi` via the core's `InstanceLock`, before Tauri starts. A second launch calls `launcher::focus_ui` and exits; it retries for 6 s so a window still starting gets focus and a window still closing hands over the name rather than eating the click. `focus_ui` now only un-minimises — `SW_RESTORE` on a maximised window had been un-maximising it, from the tray too. Live: second launch exits 0, one process, first window foreground, un-minimised.)
- [x] Add/Remove Programs shows a real publisher rather than the lowercase crate name `relay`, plus a support URL. (`bundle.publisher` / `bundle.homepage` in `tauri.conf.json`; the generated `installer.nsi` writes `Publisher=Relay` and `URLInfoAbout`/`HelpLink`/`URLUpdateInfo`. The uninstall key is keyed by product name, and `HKCU\Software\relay` vs `Relay` is the same key, so an upgrade keeps one entry. `relay-core`, `relay-svc` and `relay-elevate` gained version resources — CompanyName `Relay`, a FileDescription each — from `crates/core/build.rs`, so Task Manager and, once signed, the UAC prompt have a name to show. Before S17 the UAC publisher still reads Unknown; only a signature changes that.)
- [x] Disabled sliders show their current value instead of `—`, so a locked setting is still readable. (Muted rather than dimmed out. A monitor field the profile does not set reads `Not set` — printing the thumb's resting 50 would claim a setting nobody chose, the S24 rule.)
- [x] Loading states do not pop: at minimum the pairing-code placeholder stops rendering six em-dashes at 34 px, which reads as an error. (Six hairline slots in a fixed-height box the digits then fill, so nothing moves when the code arrives. 244 UI tests.)

### Kickoff prompt
```
You are starting session S26 (shell polish) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S26). Work in the main tree, C:\Users\stern\Documents\Code\Stream Share, on a new branch: git checkout -b feat/shell-polish. Do NOT create a worktree. Set RELAY_NO_INSTALL=1 for your commits.

Small Windows-integration details that are all individually minor and collectively make the app feel unfinished: the window forgets its size and position, a second launch opens a second window (the core has a single-instance mutex, the UI has none), Add/Remove Programs shows the publisher as the lowercase crate name "relay" with no support URL, disabled sliders hide their value behind an em-dash so you cannot read a locked setting, and the pairing-code placeholder renders as six em-dashes at 34 px which reads as an error state.

1. Publisher string is "Relay" and the support URL is the GitHub repo; do not block asking me. Both appear in Add/Remove Programs and the UAC prompt.
2. Keep the footprint gate green; window-state persistence must not pull weight into the always-on core, which is a separate process from the UI.
3. Finish only when the Definition of Done is met, then update docs/ROADMAP.md and summarise.
```

---

# Group 7 — Free everywhere, everywhere

Two requirements Jake set on 2026-09-16: Relay must be **completely free for
every user** and must **run on all operating systems**, macOS first.

This supersedes `CLAUDE.md`'s "Single Windows desktop app" framing. That file
has not been rewritten; treat its scope line as stale — its non-negotiables
still bind.

Measured 2026-09-16: **80 of 139 Rust files touch Windows** (156 `cfg(windows)`
sites, 134 `windows` crate imports). So ~40% is already portable — the DSP,
profile model, webrtc-rs transport, muxers, catalogue — and four platform seams
exist already: `AudioControl`, `DisplayControl`, `HardwareProbe`, `FrameSource`.

**There is no Mac on this network.** S28 is scoped to work that needs no Mac.
Everything after it does, and that hardware is the gate.

---

## S27 — H.264 fallback: remove the paywall
**Branch** `feat/h264-fallback` · **Worktree** `C:\Users\stern\Documents\Code\relay-h264`

Relay sends HEVC only. On Windows without OEM codec entitlement the HEVC
decoder cannot be installed for free — proven 2026-09-16 on a real Windows 10
PC where the free package's Install button is greyed out and the Store search
surfaces only paid and third-party apps. So a receiver may have to pay before
Relay works at all, which is incompatible with "completely free".

H.264 decode ships with every Windows install, is standard on macOS
(VideoToolbox) and Linux (VAAPI), and every GPU that encodes HEVC also encodes
H.264. The cost is bitrate, not capability.

### Definition of Ready
- [x] `Method::ShareCapabilities` probes decoders via MFTEnumEx, not package names, so it sees any source.
- [x] The encoder path enumerates hardware MFTs bound to the capture adapter's LUID.
- [x] Confirm the RTP/SDP layer can offer two video codecs. webrtc-rs registers HEVC pt98 today; H.264 needs its own payload type and the receiver must choose. *(2026-09-16: yes, with one trap: a track described with a codec narrows the offer to that codec. H.264 is pt 102; see M4-share.md, S27.)*

### Definition of Done
- [x] Sender offers H.264 **and** HEVC; the pair negotiates HEVC only when both ends decode it, H.264 otherwise. No user-visible codec setting — this is a capability, not a preference. *(Verified on loopback with the receiver restricted to H.264 by a test hook.)*
- [ ] A receiver with no HEVC decoder completes a share end to end. That is the acceptance test, run against a machine that genuinely lacks the codec (Jake's second PC).
- [x] Both codecs measured at the same resolution: bitrate for equivalent quality, encode latency, CPU. Expect ~30–50% more bitrate for H.264; record what it actually is. *(Measured: +9–11 % by VMAF / +16–21 % by PSNR at 4K60 40–65 Mb/s, rising steeply below 20 Mb/s; H.264 encodes 1–3 ms faster; CPU indistinguishable. M4-share.md, S27.)*
- [x] The Receive banner stops being a paywall notice. Keep an honest line that HEVC gives better quality per bit where available, but never block on it.
- [x] `docs/plans/M4-share.md` updated; the HEVC Video Extension deferral there is closed by this.

### Kickoff prompt
```
You are starting session S27 (H.264 fallback) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S27) and docs/plans/M4-share.md. Work in the main tree C:\Users\stern\Documents\Code\Stream Share on a new branch: git checkout -b feat/h264-fallback. Do NOT create a worktree. Set RELAY_NO_INSTALL=1 for commits AND pushes — the pre-push hook runs the installer too.

Relay sends HEVC only, and on Windows without OEM codec entitlement the HEVC decoder cannot be installed for free — verified on a real Windows 10 PC where the Install button is greyed out. Jake has since required Relay be completely free for every user, so HEVC cannot be the only codec.

1. Offer H.264 alongside HEVC and negotiate: HEVC when both ends decode it, H.264 otherwise. Not a user setting.
2. Do not regress the latency budget. Re-measure on loopback and record both codecs side by side: bitrate for equivalent quality, encode latency, CPU. Record what you measure, not what you expect.
3. WASAPI loopback of a silent endpoint delivers no packets, so a quiet desktop makes an audio benchmark read zero and still look plausible. Use scripts/dual-audio-check.ps1, which plays a tone and asserts packet counts.
4. CMake is needed by opusic-sys and is not on PATH: prepend "C:\Program Files\CMake\bin".
5. The acceptance test needs a machine with no HEVC decoder. Jake's second PC is exactly that — tell me when you are ready and I will drive it from here.
6. Finish only when the Definition of Done is met, then update docs/plans/M4-share.md and docs/ROADMAP.md.
```

---

## S28 — Portability seam and a macOS build
**Branch** `feat/portability-seam` · **Worktree** `C:\Users\stern\Documents\Code\relay-portable`

Everything needed to make macOS *possible*, none of which needs a Mac to write.

### Definition of Ready
- [x] Four seams exist: `AudioControl`, `DisplayControl`, `HardwareProbe`, `FrameSource`.
- [x] 59 of 139 Rust files already have no Windows dependency.
- [ ] Accept the constraint: no Mac is available, so "it compiles for the target" is the bar, not "it runs". Do not claim otherwise anywhere.

### Definition of Done
- [ ] Every Windows API call sits behind a seam and a `#[cfg(windows)]` module. No `use windows::` outside a platform module.
- [ ] A `stub` platform backend that compiles everywhere and returns a clear "not supported on this platform" per capability, so the portable half builds and tests on any target.
- [ ] `cargo check --target x86_64-apple-darwin` and `--target aarch64-apple-darwin` succeed for the portable crates. Where a crate cannot yet build, name the exact API that blocks it.
- [ ] CI builds the macOS targets. Private repos consume paid Actions minutes at a higher rate — tell Jake the cost before enabling it broadly.
- [ ] `docs/dev/porting.md`: for each seam, the Windows API today, the macOS equivalent, and the honest difficulty. Include the ones with no clean answer — DDC/CI over IOKit, and the endpoint APO, which has no macOS analogue and needs a different design (an AudioServerPlugIn), not a port.
- [ ] No behaviour change on Windows: all gates stay green.

### Kickoff prompt
```
You are starting session S28 (portability seam and a macOS build) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S28). Create the worktree: git worktree add -b feat/portability-seam ..\relay-portable main, then cd into it and run pnpm install in ui/. Set RELAY_NO_INSTALL=1 for commits and pushes.

Jake has redirected Relay to run on all operating systems, macOS first. There is NO Mac on this network, so your bar is "compiles for the macOS target", never "works on macOS". Do not write or imply otherwise in any doc or commit message.

Measured today: 80 of 139 Rust files touch Windows, 156 cfg(windows) sites, 134 windows crate imports. Four seams already exist: AudioControl, DisplayControl, HardwareProbe, FrameSource.

1. Get every Windows API call behind a seam and a cfg(windows) module, add a stub backend that compiles anywhere and reports "not supported on this platform" clearly, and make cargo check succeed for the Apple targets on the portable crates.
2. Where a crate genuinely cannot build for macOS yet, name the exact API that blocks it rather than papering over it.
3. Write docs/dev/porting.md mapping each seam to its macOS equivalent with an honest difficulty. Two need real design work, not translation: DDC/CI has no clean macOS path, and the endpoint APO has no macOS analogue at all — the equivalent is an AudioServerPlugIn, a different architecture.
4. Windows behaviour must not change. All gates stay green: cargo fmt, clippy -D warnings, cargo test --workspace, pnpm build, pnpm test, scripts/footprint.ps1.
5. Finish only when the Definition of Done is met, then update docs/ROADMAP.md and summarise.
```

---

## S29 — The stream lives inside the app
**Branch** `feat/inapp-stream` · **Worktree** `C:\Users\stern\Documents\Code\relay-inapp`

Requested by Jake directly, 2026-09-17, after the first real two-PC test: the
received stream should render **inside the Relay window**, in the Receive
screen's video area where the "Press Start receiving" placeholder sits, with an
optional **pop-out** into a separate window like Discord's.

Today `relay-share.exe` owns a top-level `HWND` and its own D3D11 swapchain, and
the Receive screen paints an empty frame next to a caption reading "Playing in a
separate window". That is the thing to remove.

### Definition of Ready
- [x] Receiver renders correctly in its own window: keyframe gate, work-area
      sizing, `WDA_EXCLUDEFROMCAPTURE`, Esc to close (all shipped in r4).
- [x] End-of-share is handled: `AU_IDLE_TIMEOUT` closes the receiver when access
      units stop (`e967d60`). Without this, an embedded stream would sit on a
      dead final frame *inside* the app, which is worse than doing so in a
      window of its own.
- [x] Accept the constraint: the UI is Tauri/WebView2, so a D3D11 surface cannot
      live in the DOM. The video area is a hole in the page that a native
      window sits over — not an element. *(Accepted, with one amendment: an
      owned top-level popup rather than a `WS_CHILD`, because the B9 capture
      exclusion only holds on top-level windows of the owning process. See
      `docs/dev/inapp-stream.md`.)*

### Definition of Done
- [x] Receiving with the Relay window open renders the stream in the Receive
      screen's video area. No second top-level window appears. *(Local stub
      pass 2026-09-17; two-PC pass below.)*
- [x] The embedded surface tracks the video area through window move, resize,
      DPI change, minimise/restore, and screen navigation. It never covers UI
      chrome and never survives leaving the Receive screen. *(Move/resize/
      navigation verified to the pixel on the stub; minimise is handled by
      the owner relationship plus an `IsIconic` guard; DPI by both processes
      being per-monitor aware and the shell re-placing on
      `ScaleFactorChanged`.)*
- [x] A pop-out control reparents the surface to a top-level window and back,
      without dropping the stream or re-negotiating anything. Closing the
      popped-out window returns the stream to the app rather than ending it.
      *(Stub pass: same HWND throughout; close → `host_close` → embedded.)*
- [x] **Presentation no longer shares a thread with the message pump.** Decode
      and present move off the window thread. *(Done: `render::host`
      pumps, `video_thread` presents. The 10 s drag measurement is the two-PC
      pass.)*
- [x] End of share is visible in the app: the video area says the share ended
      and returns to its idle state. No frozen last frame anywhere.
- [x] `WDA_EXCLUDEFROMCAPTURE` still applies to whichever window hosts the
      surface, in both embedded and popped-out states. *(Reasserted after
      every mode change; the verified value travels in the `host` event and
      reached the UI as `excluded=true` in every local transition.)*
- [x] Receive screen copy updated: no more "Playing in a separate window".
- [x] Two-PC pass with relay-pc2 on the real hardware, not just locally, with
      the exchange and its results written into `docs/dev/BUGS.md`. *(Seven
      runs on r5-r10, 2026-09-17/18; B13 in BUGS.md has each against its
      build hash. Three defects were only findable there: the popped-out
      window could not take the foreground, a second swapchain on the same
      HWND fails with E_ACCESSDENIED, and a hide-then-show within one DWM
      frame composes black. Fixed in r6, r9 and r10.)*
- [x] All gates green: `cargo fmt`, `clippy -D warnings`, `cargo test
      --workspace`, `pnpm build`, `pnpm test`, `scripts/footprint.ps1`.
      *(fmt/clippy/tests/build/UI tests green 2026-09-17; footprint below.)*

### Notes for the implementer
- Likely shape: `SetParent` the receiver `HWND` into the Tauri window, style
  `WS_CHILD`, and drive its position from the UI, which knows where the video
  area is. Pop-out is the same call in reverse. This keeps the render path
  untouched, which is worth a lot — it works today.
- The UI must send the video area's rect in physical pixels on every layout
  change, and the receiver must apply it without blocking its own present.
- Do not let the child window take focus; keyboard must keep working in the app.
- macOS has no equivalent of any of this (S28). Keep the embedding behind the
  platform seam rather than assuming it.

### Talking to the second PC
Relay cannot be tested on one machine. A windowed receiver on the sending PC
recursively captures the screen (B9), so every real check needs two, and the
second PC is where the codec, firewall and freeze bugs actually surfaced.

`relay-pc2` is a Claude Code session running on Jake's second PC, reachable with
`SendMessage` (`ListAgents` shows it). Jake is physically at whichever machine he
is at, so **that session is the only set of eyes on the receiver** — treat it as
a testing partner, not a log-reading service.

- Send it the installer path and SHA-256 for every build worth trying, plus what
  you changed and what you expect it to see. It stays on the last known-good
  build otherwise.
- Say in advance what the run should look and sound like. A previous session
  played a 440 Hz test tone without saying so, and the resulting "loud consistent
  beeping" was filed as a bug and chased across both machines. Use speech or
  music for audio checks, never a tone.
- Announce start and stop times for every share. Jake's clock starts when he
  clicks, not when the stream does, which is how a normal end-of-share got
  reported as a freeze.
- Ask for the receiver's `logs/` and the on-screen symptom separately. They have
  disagreed before, and the symptom is the one that matters.
- When its report contradicts yours, settle which run each of you is describing
  before theorising. That one question has resolved more bugs here than any
  amount of instrumentation.
- Record the outcome in `docs/dev/BUGS.md` with the build hash. A result nobody
  wrote down gets re-tested.

### Kickoff prompt
```
You are starting session S29 (the stream lives inside the app) for Relay. Read CLAUDE.md and docs/plans/SESSIONS.md (section S29). Create the worktree: git worktree add -b feat/inapp-stream ..\relay-inapp main, then cd into it and run pnpm install in ui/. Set RELAY_NO_INSTALL=1 for commits and pushes.

Jake asked for this directly after the first real two-PC test: the received stream must render INSIDE the Relay window, in the Receive screen's video area where the "Press Start receiving" placeholder is now, with an optional pop-out into a separate window like Discord's.

Constraints you cannot design around:
- The UI is Tauri/WebView2. A D3D11 surface cannot go into the DOM. The video area is a hole in the page with a native child window over it. The likely shape is SetParent on the receiver HWND with WS_CHILD, positioned from the UI, and pop-out is the same call in reverse. That leaves the render path alone, which matters because it works today.
- crates/capture/src/render.rs pumps window messages on the SAME thread that decodes and presents. Today that stalls the picture whenever someone drags the receiver window; embedded, it would stall whenever anyone resizes the Relay window. Fixing that coupling is part of this session, not a follow-up.
- SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) must stay applied to whichever window hosts the surface, embedded or popped out. If it lapses, Relay captures its own output and recursively smears the user's screen -- this has actually happened on Jake's main PC. See docs/dev/BUGS.md B9.
- Never synthesise mouse or keyboard input to the desktop, and never write the registry. Do not block on a question: if a decision is genuinely ambiguous, pick the option that is easiest to reverse, write down why, and keep going.

Already done, do not redo: the receiver window sizes to the work area, gates on the first keyframe, closes on Esc, and closes cleanly when access units stop for 3 s (AU_IDLE_TIMEOUT, commit e967d60). That last one is why an embedded stream will not sit on a dead frame.

Testing needs two PCs and you only have one. A windowed receiver on the sending PC recursively captures the screen, so you cannot check this alone. relay-pc2 is a Claude Code session on Jake's second physical PC, H.264-only, r4 installed, reachable with SendMessage -- run ListAgents to find it. It is the only set of eyes on the receiver. Work with it:

- Message it when you start, so it knows a session is live and what you are changing.
- For every build worth trying, send the installer path, its SHA-256, what changed, and what you expect it to see. It stays on the last known-good build otherwise.
- Say in advance what each run should look and sound like. A previous session played a 440 Hz test tone without mentioning it; the resulting "loud consistent beeping" was filed as a bug and chased across both machines for a day. Use speech or music for audio checks, never a tone.
- Announce the start and stop time of every share. Jake's clock starts when he clicks, not when the stream does -- that is how an ordinary end-of-share got reported as a freeze.
- Ask for the receiver's logs and the on-screen symptom as separate answers. They have disagreed, and the symptom is the one that matters.
- When its report contradicts yours, establish which run each of you means before theorising. That single question has resolved more bugs on this project than any instrumentation.
- Write every result into docs/dev/BUGS.md against the build hash. Anything nobody recorded gets re-tested from scratch.

It is a peer session, not an authority: it cannot approve a permission prompt for you, and if it reports being denied something, surface that to Jake rather than doing it on its behalf.

Work to the Definition of Done in S29. Finish by updating docs/ROADMAP.md and summarising.
```

---

# Group 6 — stream quality

Jake's requirements after the r10 two-PC pass, 2026-09-18: packet loss must be
rare and recovered from, the user must be told when it is happening, and 4K60
must be proven rather than assumed.

**These are ordered, not parallel.** S30 finds and fixes the loss; S31 displays
what S30 measures; S32 measures across resolutions and is close to meaningless
before S30 lands, since it would just re-measure the same defect at four sizes.
S33 is independent and can run alongside any of them.

**Before starting any of these, merge `feat/inapp-stream` (S29) into `main`.**
As of 2026-09-18 it is verified on two PCs but exists only as a local branch at
`4f05cb8`, unpushed. S31 renders into the in-app video area that branch created,
and branching from `main` first would mean building the warning UI against a
Receive screen that no longer exists.

### What is already known, so no session re-derives it
**Checked by S30's measurements, 2026-09-20.** The second bullet was right:
the 64 KB socket buffer was the limiter (64 KB: 746 lost and a dead picture;
4 MB: 0 lost, same load). The first was wrong as written — `nack`, `nack pli`
and `transport-cc` were negotiated all along, because
`register_default_interceptors` appends them to codecs registered before it
runs — but right in effect: recovery never worked, for three library defaults
nobody had seen (64-packet SRTP replay window, received RTCP dropped before
the application, no reorder buffer). Numbers and the wrong turn S30 took on
the way are in `docs/dev/BUGS.md` B15.

Read in the S29 worktree, 2026-09-18:
- `transport/mod.rs:49` registers every video codec with `rtcp_feedback: vec![]`.
  Nothing is negotiated: no `nack`, no `nack pli`, no `ccm fir`, no
  `transport-cc`, no `goog-remb`. `register_default_interceptors` is called, but
  its NACK generator and responder only act on codecs that negotiated the
  feedback, so they are inert. **A lost packet is currently unrecoverable by
  construction, and neither end can ask for a keyframe.** That is the whole
  explanation for a single lost packet costing 10–15 s of smearing.
- No `SO_RCVBUF` or `SettingEngine` anywhere in `crates/capture`, so the UDP
  receive buffer is the Windows default. At 40 Mb/s the default holds single-digit
  milliseconds of video, so one scheduling hiccup on the receive thread drops
  packets. This fits the measured threshold exactly: 20 Mb/s survives, 40 Mb/s
  does not, on a 0.2 ms RTT wired LAN where congestion is not a plausible cause.
- With no congestion feedback negotiated, there is no signal to drive adaptive
  bitrate from. Feedback has to come before adaptation.

## S30 — Packet loss: find it, recover from it, back off
**Branch** `feat/loss-recovery` · **Worktree** `C:\Users\stern\Documents\Code\relay-loss`

Measured by relay-pc2 on a quiet wired LAN (0.2 ms RTT), 1440p:
`60 fps / 40 Mb/s` → 209 gaps, 1420 packets lost in 4.5 min, visible smearing.
`30 fps / 20 Mb/s` → 38 gaps, 246 lost in 3 min, no visible smearing.

### Definition of Ready
- [x] Reproducible on demand on real hardware, with a rate threshold between two
      known-good and known-bad settings.
- [x] Root-cause candidates identified in code (see above): empty
      `rtcp_feedback`, untuned `SO_RCVBUF`.
- [x] Accept the order: measure the limiter first, then fix. Enabling NACK
      before knowing whether the loss is socket overflow would mask a buffer bug
      behind retransmissions and burn LAN bandwidth doing it.

### Definition of Done
- [x] The real limiter is named with evidence, not inferred (the 64 KB socket buffer:
      runs C and D differ only in it. BUGS.md B15). Instrument the
      receiver's UDP overrun counters and socket buffer occupancy, and the
      sender's pacing and burst size. Say which one it was and show the numbers.
- [x] `SO_RCVBUF` sized deliberately (4 MB, `transport/netio.rs`; before/after at equal
      load: run C 746 lost, run D 0)
      — for the worst supported rate, with the
      chosen size justified in a comment in terms of milliseconds of video held,
      and the effect measured before and after.
- [x] NACK negotiated and working (negotiated already; it needed a 4096-packet SRTP
      replay window, a reorder buffer and a 10 ms timer. 597 repairs in run D; 110 of
      110 under injected loss): `rtcp_feedback` carries `nack`, and a lost
      packet is retransmitted rather than lost. The `lost=1` gaps are the
      majority and are exactly what NACK is for on a 0.2 ms RTT link, where a
      retransmission arrives well within one frame.
- [x] **B15**: `nack pli` negotiated, and the receiver requests a keyframe when a
      gap is unrecoverable. Turns 10–15 s of smearing into roughly 200 ms.
- [x] Adaptive bitrate (`control::BitrateControl`, fed unrepaired loss over the
      signalling channel; oscillation test in the unit suite): sustained loss backs the encoder off, recovery climbs
      back. Needs congestion feedback (`transport-cc` or `goog-remb`) negotiated
      first — it cannot be driven from nothing. Changes must be damped; a bitrate
      that oscillates is worse than one that is merely too high.
- [x] Re-examine whether 40 Mb/s is a sane default (kept: per pixel ~90 Mb/s at 4K, so
      generous, but rate was not what lost packets; the quality call is S32's. ROADMAP S30.)
      for 1440p60 and state the
      reasoning. The brief says 40–80 Mb/s for 4K60; 40 at 1440p60 may simply be
      too high for the benefit.
- [x] Two-PC pass at the settings that failed (runs E and F on r11 `53642d2`, 2026-09-21:
      0 lost at 4 MB and at 64 KB, Jake saw no smear, freeze or lag), showing loss at or near zero and
      no visible smearing, recorded in `docs/dev/BUGS.md` against the build hash.
- [x] All gates green.

### Kickoff prompt
```
You are starting session S30 (packet loss: find it, recover from it, back off) for Relay. Read CLAUDE.md and docs/plans/SESSIONS.md (Group 6 preamble and section S30). The preamble lists what has already been read in the code -- do not re-derive it.

First: feat/inapp-stream (S29) must be merged into main before you branch. It is verified on two PCs but unpushed at 4f05cb8. Confirm it is on main, then: git worktree add -b feat/loss-recovery ..\relay-loss main, cd into it, pnpm install in ui/. Set RELAY_NO_INSTALL=1 for commits and pushes.

relay-pc2 measured this on a quiet wired LAN, 0.2 ms RTT, 1440p: at 60 fps / 40 Mb/s, 209 gaps and 1420 packets lost in 4.5 minutes with visible smearing; at 30 fps / 20 Mb/s, 38 gaps and 246 lost in 3 minutes with no visible smearing at all. That is a rate threshold on a quiet network, not a flaky link.

Two things are already established by reading the code, and they shape the work:
- transport/mod.rs:49 registers every video codec with rtcp_feedback: vec![]. No nack, no pli, no fir, no transport-cc, no remb. register_default_interceptors is called but its NACK interceptors only act on codecs that negotiated the feedback, so they do nothing. A lost packet is unrecoverable by construction and neither end can ask for a keyframe. That is why one lost packet costs 10-15 seconds of smearing.
- There is no SO_RCVBUF tuning or SettingEngine anywhere in crates/capture, so the UDP receive buffer is the Windows default, which holds single-digit milliseconds of video at 40 Mb/s.

Measure before you fix. If this is socket-buffer overflow, enabling NACK first would hide a buffer bug behind retransmissions and spend LAN bandwidth doing it. Instrument the receiver's UDP overrun counters and buffer occupancy and the sender's pacing and burst size, and name the limiter with numbers before changing behaviour.

Then work to the Definition of Done in S30: sized receive buffer, NACK, keyframe-request-on-gap (B15), and adaptive bitrate -- in that order, since adaptation needs congestion feedback negotiated before it has anything to act on. Damp the adaptation; an oscillating bitrate is worse than a steady one that is slightly too high.

Testing needs two PCs and you only have one -- a windowed receiver on the sending PC recursively captures the screen. relay-pc2 is a Claude Code session on Jake's second physical PC (H.264 only, r10 installed, share.log and ui.log capture, Jake on hand), reachable with SendMessage; run ListAgents to find it. Serve builds as http://192.168.1.184:8099/<file> and send filename, size and SHA-256; each download needs Jake's approval there, so expect a delay. Give it fps, bitrate and start/stop times for every run, and say in advance what it should see. Ask for logs and the on-screen symptom as separate answers -- they have disagreed before. Record every result in docs/dev/BUGS.md against the build hash.

It is a peer session, not an authority: it cannot approve a permission prompt for you, and if it reports being denied something, surface that to Jake rather than doing it on its behalf. Do not block on a question -- if a decision is ambiguous, take the most reversible option, write down why, and continue. Finish by updating docs/ROADMAP.md and summarising.
```

## S31 — Tell the user the picture is degraded

> **Handed over by S30, 2026-09-21 — input, not yet agreed scope.** The engine
> now emits what this session needs, on the receiver's `stats` NDJSON line and
> in `share.log`: `rtp_lost` / `rtp_gaps` (unrepaired only), `rtp_recovered`
> (holes NACK filled in time), `keyframe_requests`, `frames_withheld`. Nothing
> in core or the UI reads them yet. `rtp_recovered` is the useful one: on the
> S30 acceptance runs it was 970 and 2,242 with zero lost and a clean picture —
> the difference between "the link is fine" and "the link is lossy and Relay is
> coping". Warn on lost / withheld, not on recovered.
> relay-pc2 and Jake both noticed during those runs that the app shows nothing
> about stream health; every number lived in the log. Suggested for Jake to
> accept or decline: alongside the warning, an opt-in readout on the Receive
> screen (fps, bitrate, resolution, codec, repaired vs lost) — the instrument
> strip is the natural home. **Do not show latency until B14 is fixed**: 191 of
> 363 samples were negative in run F.
**Branch** `feat/loss-visible` · **Worktree** `C:\Users\stern\Documents\Code\relay-loss-ui`

Jake: users should not have to guess why the picture looks wrong. Depends on
S30, which produces the loss statistics this displays, and on S29, which created
the in-app video area it renders into.

**Widened by Jake, 2026-09-20**, from "show the loss rate" to **every
stream-health state the user can act on** — he asked for in-stream notification
of issues in the same breath as crash restore. Three real incidents on the
second PC, none of which a loss indicator alone would have covered:
- the receiver ended a share by itself twice (the 3 s no-access-units rule) with
  nothing on screen explaining why;
- the app kept a stale pairing code and a "Paired" status long after the share
  was dead;
- every number used to debug those nights existed only in `share.log`.

**Design the notification vocabulary jointly with S38 (crash restore), not
twice.** "Degraded", "ended", and "died and is coming back" are one vocabulary;
two sessions inventing it separately will produce two idioms in one app.
Whichever session starts first writes it down; the other adopts it.

### Definition of Ready
- [x] S30 has merged and exposes a loss rate both ends can read (`df525fe`).
- [x] S29 has merged, so there is an in-app video area to overlay (`df525fe`).
- [x] B14 is fixed and merged, so latency may now be shown — S30's handover note
      above predates that and its "do not show latency" caveat no longer holds.

### Definition of Done
- [ ] Loss is visible **on both ends** while it is happening, showing the rate.
- [ ] Threshold and hysteresis: a single lost packet never flashes anything. It
      appears on sustained loss and clears itself on recovery, with the clear
      slower than the trigger.
- [ ] Honest wording. Say what is happening and what it means for the picture,
      and do not blame the user's network unless the evidence actually says so —
      on this LAN it was Relay's own defect.
- [ ] Fits the instrument-strip language in the brief rather than introducing a
      new visual idiom. It is a readout, not an alert.
- [ ] Never obscures the picture it is describing.
- [ ] **The share ending says so**, on both ends, wherever it ends: the 3 s
      no-access-units rule, a stop from the other side, and a connection that
      died all produce a visible, distinguishable state rather than a frozen
      final frame or a silent return to idle.
- [ ] **No stale state.** When a share ends the pairing code and "Paired"
      status go with it. The app never shows a code that will not work or a
      peer that is not there.
- [ ] An opt-in stream-health readout on the Receive screen -- fps, bitrate,
      resolution, codec, repaired vs lost, latency -- in the instrument strip,
      so the numbers that only existed in `share.log` are reachable without it.
- [ ] Component tests in jsdom only — never drive a real window.
- [ ] All gates green.

### Kickoff prompt
```
You are starting session S31 (tell the user the picture is degraded) for Relay. Read CLAUDE.md and docs/plans/SESSIONS.md (Group 6 preamble and section S31).

Do not start until S30 and S29 have merged into main: S30 produces the loss statistics you display, and S29 created the in-app video area you render into. If either is missing, say so and stop rather than building against a Receive screen that is about to change.

git worktree add -b feat/loss-visible ..\relay-loss-ui main, cd into it, pnpm install in ui/. Set RELAY_NO_INSTALL=1 for commits and pushes.

Jake's requirement: users should not have to guess why the picture looks wrong. While loss is occurring, show it -- in Relay and/or as an overlay on the stream -- with the loss rate, visible on BOTH ends, clearing itself on recovery.

The judgement in this session is entirely about restraint. A single lost packet must never flash anything: use a threshold with hysteresis, and make the clear slower than the trigger. It is a readout in the instrument-strip language the brief describes, not an alert; it must never obscure the picture it is describing. Be honest in the wording -- do not blame the user's network, because when this was measured the cause was Relay's own missing NACK and untuned socket buffer, on a LAN with 0.2 ms RTT.

UI tests are component tests in jsdom only; nothing may move the user's cursor. Verify with relay-pc2 (a Claude session on Jake's second PC, reachable with SendMessage -- run ListAgents) that the indicator appears on the receiving end during real loss and clears afterwards, and record the result in docs/dev/BUGS.md against the build hash. Do not block on a question. Finish by updating docs/ROADMAP.md and summarising.
```

## S32 — Prove 4K60, and the resolution matrix
**Branch** `feat/resolution-matrix` · **Worktree** `C:\Users\stern\Documents\Code\relay-matrix`

Jake wants confidence for all user types, not just the one display he tested.
**Run after S30** — before it, this would measure the same defect four times.

### Definition of Ready
- [ ] S30 merged, loss at or near zero at 1440p60.
- [ ] Accept the sender-side limitation too: the dev box's only display is
      2560x1440 and the sender captures at native size, so 4K needs a 4K
      display or a 4K source (a game) on the sending PC. 1080p is a
      `--size 1920x1080` cap. Found 2026-09-18 when relay-pc2 asked for the
      matrix.
- [ ] Accept the receiver-side limitation: relay-pc2's display is 1920x1080, so
      4K is a downscale there. Decode cost and network load are still real and
      are the point; do not claim 4K was verified end-to-end on a 4K panel.
- [x] **4K source decided (Jake, 2026-09-18): NVIDIA DSR.** The sender captures
      at the display's native size and the main PC's panel is 2560x1440, so
      there was no 4K to capture. DSR runs a 3840x2160 desktop on that panel,
      which costs nothing and is available immediately. It exercises capture,
      encode, network and decode at genuine 4K; what it does not test is 4K
      *viewing*, on either end. Say so wherever the results are reported — with
      DSR on the sender and a 1080p receiver, neither end of this pair displays
      a real 4K image, and the numbers are about cost, not fidelity.

### Definition of Done
- [ ] Matrix run on two PCs: 3840x2160 at 60 and 30, 2560x1440 at 60, 1920x1080
      at 60. For each: bitrate, fps held, packets lost, gaps, encode time,
      decode time, end-to-end latency, CPU and GPU load on both ends.
- [ ] A recommended default bitrate per resolution and frame rate, derived from
      the measurements rather than from the brief's original guess.
- [ ] 4K60 either works within the brief's targets (40–80 Mb/s, <50 ms) or the
      exact limiter is named. Expect it to be the hardest case, since 1440p60
      already lost packets before S30.
- [ ] Results in `docs/dev/` as a table with the build hash, plus whatever Relay
      should do differently as a result — if a setting cannot be sustained, the
      UI should not offer it as though it can.
- [ ] Any 4K-specific failure filed in `docs/dev/BUGS.md` with its evidence.

### Kickoff prompt
```
You are starting session S32 (prove 4K60, and the resolution matrix) for Relay. Read CLAUDE.md and docs/plans/SESSIONS.md (Group 6 preamble and section S32).

Do not start until S30 has merged and loss is at or near zero at 1440p60. Before that, this session would measure the same defect at four resolutions and produce numbers that mean nothing.

git worktree add -b feat/resolution-matrix ..\relay-matrix main, cd into it, pnpm install in ui/. Set RELAY_NO_INSTALL=1 for commits and pushes.

Jake wants confidence for all user types, so measure the matrix on real hardware: 3840x2160 at 60 and 30, 2560x1440 at 60, 1920x1080 at 60. For each capture bitrate, fps actually held, packets lost, gaps, encode time, decode time, end-to-end latency, and CPU and GPU load on both ends.

The 4K source is NVIDIA DSR, decided by Jake on 2026-09-18: the sender captures at the display's native size and his panel is 2560x1440, so DSR runs a 3840x2160 desktop on it. Ask relay-pc2 to have Jake enable DSR before the 4K runs; it is a Windows/NVIDIA control-panel change on his main PC, so he does it, not you.

One honest limitation to state everywhere you report this: with DSR on the sender and a 1920x1080 display on the receiver, NEITHER end of this pair displays a real 4K image. Capture, encode, network and decode are genuine 4K and are what the session measures, but do not write or imply that 4K was verified end-to-end on a 4K panel, and do not describe the picture quality as 4K-verified.

Expect 4K60 to be the hardest case -- 1440p60 was losing packets before S30. If it cannot meet the brief's targets (40-80 Mb/s, under 50 ms), name the exact limiter with evidence rather than reporting a pass.

Produce a recommended default bitrate per resolution and frame rate from the measurements, not from the brief's original guess, and say what Relay should do differently as a result: if a setting cannot be sustained, the UI should not offer it as though it can.

relay-pc2 is a Claude Code session on Jake's second physical PC, reachable with SendMessage -- run ListAgents. Serve builds as http://192.168.1.184:8099/<file> with filename, size and SHA-256; each download needs Jake's approval there, so expect a delay. Give it fps, bitrate and start/stop times for every run. Record results in docs/dev/ as a table with the build hash, and file any 4K-specific failure in docs/dev/BUGS.md with its evidence. Do not block on a question. Finish by updating docs/ROADMAP.md and summarising.
```

## S33 — Time, teardown and reproducible builds
**Branch** `feat/time-and-teardown` · **Worktree** `C:\Users\stern\Documents\Code\relay-time`

The four bugs still owed from S29's list. Independent of S30–S32; can run
alongside them.

### Definition of Done
- [x] **B14**: clock offset is estimated once at connect, so `latency_ms` drifts
      about 2 ms/min and eventually reports negative latency. Re-estimate
      periodically and smooth it. A latency readout that goes negative teaches
      the user to distrust the whole instrument strip.
- [x] **B16**: roughly 1 s of audio delay, likely out of sync with video.
      Measure where the second goes before changing anything — capture, encode,
      the Opus path, jitter buffer or playback — then fix the one that owns it.
      A/V sync must be measured against video, not just reduced in isolation.
- [x] **B8**: teardown always takes the full 3 s deadline, meaning nothing is
      actually finishing early and the grace period is doing all the work. Find
      what holds it and make the common case fast.
- [x] **B12**: bundles are not byte-reproducible. Either make them so, or record
      precisely which inputs vary and why, so a hash mismatch can be reasoned
      about instead of guessed at.
- [x] Each fix verified with numbers, not by inspection. All gates green.
      *(One-PC numbers for all four are in `docs/dev/BUGS.md`; B12 in
      `docs/dev/reproducible-builds.md`.)*
- [ ] **Two-PC confirmation with relay-pc2** of B14 (real drift), B16 (A/V
      sync by eye and `audio.buffered_ms`) and B8 (windowed share end without
      the 3 s deadline line). Not run: serving the build to the second PC was
      blocked by this session's permission policy, 2026-09-18. The release
      engine for it is `relay-share.exe` built by
      `scripts/repro-check.ps1 -Flags` at `ebc0c72`,
      SHA-256 `25dff88bcec2d890654f25d4bc306374a09d4d760bc4f18acc8706a26ff09fc4`.


### Kickoff prompt
```
You are starting session S33 (time, teardown and reproducible builds) for Relay. Read CLAUDE.md and docs/plans/SESSIONS.md (section S33) and docs/dev/BUGS.md.

git worktree add -b feat/time-and-teardown ..\relay-time main, cd into it, pnpm install in ui/. Set RELAY_NO_INSTALL=1 for commits and pushes. This session is independent of S30-S32 and may run alongside them, but stay out of the packet-loss and RTCP code so you do not collide with S30.

Four bugs, each verified with numbers rather than by inspection:

B14: the clock offset is estimated once at connect, so latency_ms drifts about 2 ms/min and eventually reports negative latency. Re-estimate periodically and smooth the result. A readout that goes negative teaches the user to distrust the entire instrument strip, so this matters more than its size suggests.

B16: roughly 1 second of audio delay, likely out of sync with video. Measure where the second actually goes -- capture, encode, the Opus path, the jitter buffer, playback -- before changing anything, then fix whichever owns it. Verify against video: A/V sync is the requirement, not merely lower audio latency.

B8: teardown always takes the full 3 s deadline, which means nothing is finishing early and the grace period is doing all the work. Find what holds it and make the common case fast.

B12: bundles are not byte-reproducible. Either make them reproducible, or document exactly which inputs vary and why, so that a future hash mismatch can be reasoned about instead of guessed at.

relay-pc2 is a Claude Code session on Jake's second physical PC, reachable with SendMessage -- run ListAgents. B16 and B14 both need it, since audio delay and clock drift only exist across two machines. Give it start/stop times for every run and record results in docs/dev/BUGS.md against the build hash. Do not block on a question. Finish by updating docs/ROADMAP.md and summarising.
```

---

## Standing rule — one main, one build, stated contents
Jake, 2026-09-20, after being handed installers built from three different
branches in one day. The second PC is the only place most bugs appear, and a
build whose contents nobody can state wastes that PC's time.

- **Ship from `main`, never from a feature branch.** A branch build is for the
  session that owns the branch. Anything the second PC installs comes from
  `main` with everything merged.
- **The main-tree session owns the merge order.** Feature sessions merge *into*
  main through it rather than shipping around it, so there is one answer to
  "what is in this build".
- **Every build is announced with its contents**: the commit on `main`, which
  sessions are merged into it, the filename, size and SHA-256. "Latest build" is
  not a description.
- **Features that have never run in the same binary are not proven.** S29 and
  S30 were each verified alone on the second PC and had never been in one build
  until `df525fe`. Say which combinations are actually tested.
- The `:8099` file server is started by whoever ships the build, and stops when
  they do. Check it is up before sending a URL.

# Group 8 — Jake's next features (2026-09-18)

Asked for during S29's two-PC runs. Each needs its own plan file before it
starts; S38 depends on S35. The standing rule first, because every one of
them stores something.

## Standing rule — an update never resets anything
Jake, 2026-09-18: every Relay update must leave settings, profiles, the
hardware library, consent, firewall/APO/camera records and — once S35 lands
— remembered devices exactly as they were. No reconfiguring, no new pairing
code for a device that already paired, no first-run screen again. Updates
are "consistent and subtle".

Where this stands today: the install directory is the data root
(`%LOCALAPPDATA%\Relay`), an install over the top rewrites the binaries and
leaves `data\` alone, and only the uninstaller's "Delete the application
data" checkbox removes it. r4 → r9 on the second PC kept consent and the
firewall record across five over-the-top installs. What the rule adds for
every session from now on:

- **Schema changes are migrations, never resets.** Any on-disk format
  change (`profiles`, `settings.json`, `installed.json`, `firewall.json`,
  `window.json`, S30's peer records) ships with a versioned migration and a
  test that loads the previous version's files. Unknown fields are kept,
  not dropped (`serde` `deny_unknown_fields` is banned on persisted types).
- **An installer change is tested as an upgrade**, not just a clean
  install: `scripts/vm-cycle.ps1` / the snapshot diff gain an
  install-old → configure → install-new → diff step whose expected
  difference under `data\` is empty.
- **A remembered device survives updates on both ends** (S35's DoD): after
  updating either PC, the next share connects with no code.

## S35 — Remembered devices: pair once, connect on sight · `feat/trusted-peers`
Requested by Jake 2026-09-18, during S29's two-PC pass. Once the main PC and
the second PC have paired successfully, that pairing should be stored and
linked on both sides so the next share connects directly — after an app
shutdown, after a reboot — with no six-digit code. The code stays for first
contact and for anything not remembered.

Sketch: persist a per-peer record (name, stable id, the shared secret the
pairing derived, last seen) in the data root on both ends; the sender offers
a "known peer" auth in signalling and the receiver accepts it without a code
when the id and secret match; the Share screen lists remembered receivers
first, the Receive screen shows "trusted senders" with a Forget button.
Security shape to decide first: what the stored secret is, how a stolen data
folder is bounded (per-peer secrets, revocable), and whether a remembered
receiver still needs to be in "Start receiving" or can auto-accept. Never
network config for the user; never a change to another app. **DoD must
include the standing rule above:** the peer records are versioned, survive
an update of either PC, and the first share after an update needs no code.

**Branch** `feat/trusted-peers` · **Worktree** `C:\Users\stern\Documents\Code\relay-peers`

### Definition of Ready
- [x] Pairing works and is proven across two PCs: six-digit code, HMAC over the
      SDP, DTLS fingerprint pinned.
- [x] The data root survives updates — r4 → r11 kept consent and the firewall
      record across six over-the-top installs.
- [ ] Accept that the trust model is the first deliverable, not the last. Write
      it down before writing code; the whole feature is a security decision
      wearing a convenience feature's clothes.

### Progress, 2026-09-22
Done and merged (`6c21c0c`), while Jake was away:
- **The blocker nobody had noticed.** `peers.json` was written but never read,
  because `build_pc` supplied no certificate: webrtc-rs minted a throwaway one
  per connection, so this PC's DTLS fingerprint changed every run and a stored
  fingerprint could never match. The store was not unused by oversight, it was
  unusable. Identity had to come before storage.
- `transport/identity.rs`: one ECDSA P-256 keypair, generated on first use,
  kept as `identity.pem` in the data root, used for DTLS on every connection.
  Pinning a fingerprint now authenticates. 3 tests. **Corrected 2026-09-23:**
  the key alone was not enough — the certificate rebuilt from it had a new
  serial each run, so the fingerprint still changed every share. The whole
  certificate is now stored (DPAPI-wrapped `identity.key` on Windows), a
  key-only file upgrades in place, and the test compares fingerprints across
  loads. 7 tests. No r-build before r18 could recognise a remembered peer.
- `rcgen` pinned with `=` — `rtc` takes an `rcgen::KeyPair` without
  re-exporting the type, so a version skew is a baffling type error.
- `docs/dev/trusted-peers.md`: the trust model.

**§5 answered by Jake, 2026-09-22: no** — "I would say no to the pairing
without both devices ready for streaming." Remembering removes the code, not
the consent. Built as Option A the same day (`docs/dev/trusted-peers.md` §8
describes the implementation). What is left is what only two PCs can prove.

### Definition of Done
- [x] **The trust model is written down first**, in `docs/dev/trusted-peers.md`,
      and answers at minimum: what the stored secret is and what it authorises;
      what an attacker who copies the data folder can do; whether a remembered
      sender can connect while the receiver is *not* in "Start receiving", and
      the argument for whichever answer is chosen; how a peer is revoked; and
      what happens when the same secret appears from a new address. "Any machine
      that once paired may reconnect silently forever" is the failure to design
      against.
- [x] Per-peer records in `%LOCALAPPDATA%\Relay`, versioned, with a migration
      test that loads the previous version. Secrets are per-peer and revocable,
      never one global key. (`crates/core/src/peers.rs`, 14 tests; the
      credential is the peer's own key, held by the peer — nothing symmetric is
      stored at all.)
- [x] One-click reconnect with no code, on both ends, after an app restart —
      the store and the identity key both live in the data root and the engine
      reads them fresh per share. *After a reboot of either PC*: same
      mechanism, owed to the two-PC pass below.
- [x] The Share screen lists remembered receivers; the Receive screen lists
      trusted senders. Both show a name and last-connected time, support
      favourites, and have an obvious **Forget** that actually revokes rather
      than hiding the row. (10 UI tests.)
- [x] The six-digit code still works for first contact and for anything not
      remembered. Nothing here removes it. (Every pre-existing Share test
      still passes unchanged; an older receiver's `Bye` becomes "pair with its
      code once".)
- [ ] Survives an update on both ends: install over the top on each PC, then
      share with no code. This is the standing rule and is the single most
      likely thing to regress. **Needs both PCs — r14 is the build to test it
      on, over r13.**
- [ ] Two-PC pass with relay-pc2, recorded in `docs/dev/BUGS.md` with the hash.
      The script: pair once with a code (both ends now remember each other);
      stop; on Share pick the remembered PC; Start receiving on the other; Start
      sharing — no code — and Receive should read "remembered, no code". Then
      Forget on one end and confirm the next attempt says "does not remember
      this PC" rather than connecting.
- [x] All gates green: fmt, clippy `-D warnings`, `cargo test --workspace`,
      `pnpm build`, `pnpm test` (283), footprint.

### Kickoff prompt
```
You are starting session S35 (remembered devices: pair once, connect on sight) for Relay. Read CLAUDE.md, docs/plans/SESSIONS.md (section S35, the standing rule "one main, one build, stated contents", and the standing rule "an update never resets anything").

git worktree add -b feat/trusted-peers ..\relay-peers main, cd into it, pnpm install in ui/. Set RELAY_NO_INSTALL=1 for commits and pushes.

Jake's requirement, given after every single two-PC run so far needed a fresh six-digit code read aloud between two machines: Relay must remember previously-connected devices so reconnecting is one click. It must survive updates and crashes -- no reconfiguring after every new build.

Write the trust model BEFORE any code, in docs/dev/trusted-peers.md. This is a security decision dressed as a convenience feature, and the thing to design against is "any machine that once paired can reconnect silently forever". Answer at least: what the stored secret is and what it authorises; what someone who copies the %LOCALAPPDATA%\Relay folder can do with it; whether a remembered sender may connect while the receiver is NOT in "Start receiving", with your argument for whichever you choose; how a peer is revoked; and what happens when a known secret arrives from a new address. Bring that document to Jake before building on it -- he should see the trust model, not just the feature.

Then: per-peer, revocable secrets in versioned records under the data root with a migration test that loads the previous version's files; one-click reconnect on both ends after an app restart and after a reboot; remembered receivers on the Share screen and trusted senders on Receive, each with name, last-connected time, favourites and a Forget that genuinely revokes. The six-digit code stays for first contact and anything not remembered.

The likeliest regression is the standing rule: install over the top on BOTH PCs, then share with no code. Test that explicitly rather than assuming it.

relay-pc2 is a Claude Code session on Jake's second physical PC, reachable with SendMessage -- run ListAgents. This feature cannot be verified on one machine. Do not ship it a branch build: builds come from main through the main-tree session, announced with commit, contents and SHA-256. Record results in docs/dev/BUGS.md against the build hash.

Never write the registry; the data root only. Raise questions and failure points with Jake directly in this session rather than routing them through relay-pc2. Finish by updating docs/ROADMAP.md and summarising.
```

## S36 — Direct send to streaming software · `feat/stream-out`
**Decision doc** `docs/plans/v11-stream-out.md` (2026-09-22): local
virtual-camera path (a) recommended first, Windows 11 + VB-Cable caveats stated;
RTMP/SRT push (b) is a product decision for Jake before any plan.
**Started 2026-09-22 on the local path**: `docs/plans/S36-relay-camera-sender.md`
· worktree `C:\Users\stern\Documents\Code\relay-camsend`. Video only — on the
same PC the streaming program captures the game's audio itself.
**Built 2026-09-22** (camera-while-sharing). Owed: OBS on this PC picking it up during a real share; the no-receiver mode is a separate decision (see the plan).
Requested by Jake 2026-09-18. Beyond a second PC, send the feed and audio
straight into OBS, Streamlabs, TikTok Live Studio, or any other streaming
program with little to no setup on the user's part.

The v1 brief ruled Twitch streaming out; this reopens it deliberately. Two
very different shapes, and the session has to pick: (a) **local**: Relay
appears to the streaming program as a camera and microphone (the M5 virtual
camera and mic, already built for the receive side, pointed at the *local*
capture), or as an NDI source (the v1.1 NDI item) — zero network, works with
every program that takes a webcam; (b) **remote**: Relay itself pushes
RTMP/SRT to a platform's ingest with a stream key. (a) is the "little to no
effort" answer for OBS-style software on the same PC; (b) is a new outbound
network path and a new encoder consumer, and needs the brief's "one outbound
request" rule revisited. **First task of the session: write
`docs/plans/v11-stream-out.md` with the decision and a real DoR/DoD.**

## S37 — Audio mixer on Share and Receive · `feat/audio-mixer`
**Plan** `docs/plans/S37-audio-mixer.md` (written 2026-09-22) · **Worktree**
`C:\Users\stern\Documents\Code\relay-mixer`. Three tracks, not a mixing engine:
"everything else on the PC" is process loopback with the *exclude* flag.
**Built 2026-09-22.** The plan's Definition of Done is ticked bar one: the
listening check on the second PC (speech and music, never a tone), owed.
Requested by Jake 2026-09-18. When sharing a window or one application
rather than the whole screen, choose what goes out: that app's sound, the
whole OS mix, system sounds, the microphone — each on its own fader with
mute, live while sharing. The same on the receiving side: mute or drop
system sounds, app sound, the mic, in a real-time mixer. Place it under the
Start/Stop button on each screen (or wherever the layout makes it obvious).

What exists to build on: the sender already captures the desktop mix or one
process's audio (`audio_pid`, WASAPI process loopback) and the microphone as
a **second Opus track** (S2); the receiver decodes the two tracks separately
and sums them in one op in `playback.rs` (`mix_sum`), which S19 (mix-minus)
was already going to split. So per-source gain and mute on the receiver is a
change to that one step; on the sender, per-source faders mean mixing
sources *before* the encoder (today the choice is one program source + mic),
and "system sounds" as a distinct source needs a per-session split of the
desktop mix (WASAPI session enumeration, which `relay-audio` already probes
for the exclusive-mode watcher). Send the mixer state over the existing
stdin command channel so it is live, never a restart. Keep the
non-negotiables: no default-device changes, nothing global, Relay's own
mix only. **First task: write `docs/plans/v11-audio-mixer.md`** with the
source list per side, the wire shape, and a DoD that includes a listening
check on the second PC (speech, never a tone).

## S38 — Stream resilience: crash record, auto-reconnect, resume · `feat/stream-resilience`
**Plan** `docs/plans/S38-stream-resilience.md` (written 2026-09-22, after S35
merged — its one hard dependency) · **Worktree** `C:\Users\stern\Documents\Code\relay-resilience`
Requested by Jake 2026-09-18. If the engine or the app crashes during a
live stream — or the PC loses power, or the link glitches — keep the crash
log, come back up, and reconnect on its own so a multi-streamer's feed
survives. Default on. Only for streams the user started and never stopped;
a Stop clears it. Applies with the window closed too (the core runs in the
tray either way).

Foundation already there: the core supervises the engine child and sees it
exit; `share.log` / `ui.log` / `core.log` persist (S29); autostart via
`relay-svc`. To build:
- **Crash record**: a panic hook in every binary writing `crash\<ts>.txt`
  plus the exit code and the request that was running; the next app open
  shows one line about it.
- **Reconnect in-session**: on an unexpected engine exit during a share,
  the core respawns `relay-share send` with the same request, backoff
  1 → 2 → 5 → 10 s, and the receiver keeps its pairing valid for a grace
  window so the same code reconnects; give up after ~3 min with a notice.
  The instrument strip shows "reconnecting (n)".
- **Resume after reboot / power loss**: a persisted active-stream record
  the core acts on at start. Needs S35 (no code to type after a reboot), so
  S35 lands first.
- **Loud, not silent**: a share that resumes without a click must announce
  itself — tray balloon, strip visible when the window opens, one toggle to
  turn resilience off in Settings. That is part of the DoD, not polish.

Jake's decisions, 2026-09-18: crash detection + stream restore is a
must-have, **on by default, off by a Settings toggle**. Closing the window
should **exit to the tray and keep running by default, with a tray message
saying so**, also toggleable in Settings. The close preference already
exists (`settings.json` `close_action`, default keep-running, S23); what
this adds is the notification-area message on close and the resilience
toggle beside it.

**Built 2026-09-22, the day S35 merged** (the only hard dependency). See the
plan's Definition of Done for what is ticked and the roadmap entry for what
shipped. Owed to two PCs: the in-session kill test on each end, and the
reboot-of-one / reboot-of-both resume.


# Group 5 — v1.1 backlog

Out of v1 scope (`docs/ROADMAP.md:104-108`). Create the worktree when the
session actually starts; these are sketches, not ready plans, and each needs its
own plan file written first.

## S18 — Relay Send VST3 · `feat/vst3-send`
The `daw-plugin/` crate in the brief, unbuilt. A VST3 that ships DAW master/bus
audio to Relay over shared memory — the only way to capture DAW audio under
ASIO exclusive mode. **First task of that session: write `docs/plans/v11-vst3.md`
with a real DoR and DoD.**

## S19 — Call-audio return route and mix-minus · `feat/mix-minus` · **S2 landed, so unblocked**
**Started 2026-09-23** — plan, design and DoR/DoD in
`docs/plans/S19-call-return.md`; worktree `relay-mixminus`. In one line:
the receiver sends the call app's own output back as a fourth Opus track
(`relay-audio-return`, process loopback of the call app, so it is remote
voices only), the sender plays it on the default endpoint behind a Call
fader, and the echo case (desktop/rest capture on the sender) is stated
in words rather than cancelled.

Audio back from the call to the sending PC, minus your own voice. S2's second
Opus track is the foundation; this is the feature it was always heading toward.
The seam S2 left for it: the two sources stay separate through decode and are
summed only in `playback.rs`, one op before the WASAPI render buffer
(`mix_sum`). Splitting them to different destinations, or ducking one against
the other, means changing that last step and nothing upstream of it.

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
4. **S1, S2, S3** in parallel — the three highest-value code sessions. (S2 done
   2026-09-14; S4 landed alongside it and was merged into S2's tree.)
5. **S10 + S11** — closes most of the "deferred to MVP validation" backlog
   without new hardware.
6. **S6, S7, S8** as capacity allows.
7. **S13, S14** once a hypervisor exists; **S15, S16** once the hardware does;
   **S17** once the certificate arrives.
8. **S4** only if a real compatibility gap appears; **S18–S21** after v1 ships.
