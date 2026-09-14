# Parallel feature streams (post-M7)

Four independent pieces of deferred work, each with its own worktree and
branch cut from `main`. They touch nearly disjoint files, so they can run at
the same time and merge back without fighting. Tree layout and merge-back
steps: `docs/dev/parallel-sessions.md`.

Every stream keeps the non-negotiables in `CLAUDE.md` — no anti-cheat
surface, no global config, backup-then-apply, the footprint gate — and ends
with `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D
warnings`, `cargo test --workspace`, `pnpm build`, and `scripts/footprint.ps1`
green. Set `RELAY_NO_INSTALL=1` when committing from a feature tree unless you
want the installed app to follow it.

---

## A. ADLX display backend — `feat/adlx-display`, tree `relay-adlx`

**Kickoff prompt:**
> Read CLAUDE.md, docs/plans/parallel-streams.md and docs/plans/M2-display.md. Work in the `relay-adlx` worktree on branch `feat/adlx-display`. The display path is NVIDIA-only today; add the AMD equivalent behind the existing `DisplayIo` seam so an AMD PC gets the same per-game colour that an NVIDIA one does. Keep backup-then-apply and restore-on-blur identical between vendors. Finish by updating docs/plans/M2-display.md and docs/ROADMAP.md.

The seam already exists (`crates/display`, `DisplayIo`), and NvAPI is the
reference implementation. The dev machine reports both an NVIDIA and an AMD
encoder, so some of this is testable here. Expect the interesting work to be
ADLX's initialisation and the gamma/vibrance mapping, which is not
one-to-one with NvAPI's.

## B. Mic plus desktop audio — `feat/dual-audio`, tree `relay-dual-audio`

**Kickoff prompt:**
> Read CLAUDE.md, docs/plans/parallel-streams.md and the Deferred section of docs/plans/M4-share.md. Work in the `relay-dual-audio` worktree on branch `feat/dual-audio`. The sender ships one audio track, so choosing Microphone drops the desktop mix; add a second Opus track so both travel together, with the receiver mixing or routing them. Remove the warning note under the Audio chips in ui/src/screens/Share.tsx once it is no longer true. Finish by updating docs/plans/M4-share.md and docs/ROADMAP.md.

The mic path is built and benchmarked (`AudioSource::Microphone`); what is
missing is a second track on the peer connection and the receiver side of it.
Watch the latency budget — this is on the hot path.

## C. MKV recording container — `feat/mkv-container`, tree `relay-mkv`

**Kickoff prompt:**
> Read CLAUDE.md, docs/plans/parallel-streams.md and docs/plans/M6-recording-presets.md. Work in the `relay-mkv` worktree on branch `feat/mkv-container`. Recording writes fragmented MP4; add MKV as an option, for the same reason OBS defaults to it — an MKV survives a crash mid-file. Match the existing golden-fixture test approach. Recording must still add no measurable latency or CPU to the live share. Finish by updating docs/plans/M6-recording-presets.md and docs/ROADMAP.md.

The recorder tees the share's existing bitstream, so this is a muxer
addition, not a new encode path. Keep that property.

## D. Vendor VCP opcodes — `feat/monitor-vcp`, tree `relay-monitor-vcp`

**Kickoff prompt:**
> Read CLAUDE.md, docs/plans/parallel-streams.md and docs/plans/M2-display.md. Work in the `relay-monitor-vcp` worktree on branch `feat/monitor-vcp`. Black equalizer and Response are greyed out on every panel because `quirks_for` in crates/display/src/vcp.rs has no verified vendor opcode for any PNP prefix. Research and add opcodes for the common gaming panels, keyed by PNP ID and model, with a source recorded for each. Never guess an opcode — writing the wrong VCP code changes a setting the user did not ask for. Finish by updating docs/plans/M2-display.md and docs/ROADMAP.md.

This is the monitor-side answer to the headphone catalogue: per-model data
rather than a category-wide guess. Verify against the dev machine's own panel
where possible and mark everything else as unverified until someone with that
hardware confirms it. The UI note in `ui/src/screens/Games.tsx` explaining why
these are off should shrink as the table grows.
