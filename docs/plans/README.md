# Milestone plans

**M0–M7 are done and merged into `main`.** Everything that remains — the work
deferred out of those milestones plus the v1.1 backlog — is broken into
sessions in `SESSIONS.md`, each with its branch, worktree, Definition of Ready,
Definition of Done and kickoff prompt. Start there; the files below are the
history that explains why each remaining item was deferred.

One file per milestone, one chat session per milestone. Each plan is
self-contained: a session that reads only `CLAUDE.md` and its plan file has
enough context to start.

## Where the truth lives
Three files, in this order:

1. **`docs/ROADMAP.md`** — the status catalogue. Its "Plan files" table has one
   row per milestone summarising what is genuinely done and what is deferred,
   and each milestone section carries the same story at length. Read a row
   *with* the plan's own **Deferred** section; the row is the index, the
   Deferred section is the detail, and they are kept in sync deliberately
   (session S9, 2026-09-15).
2. **`SESSIONS.md`** — the work queue. Every remaining item from those Deferred
   sections, as a session with a branch, worktree, DoR, DoD and kickoff prompt.
3. **The milestone plans below** — the history that explains *why* something was
   deferred, plus the runbook for doing it.

Rule these files are held to: **no claim in a plan may assert something the
code does not do.** A claim that is a target rather than a measurement is
marked as such (aspirational, extrapolated, fixture-proven vs live-proven).
When a later session closes a deferred item, strike it through in place and
date it rather than deleting it — the deferral is the record of the decision.

## Starting a milestone session
Open a new Claude Code chat in the Stream Share project and paste the block for
the milestone from `KICKOFF-PROMPTS.md`. The prompt names the plan file, so the
session reads it, verifies the Definition of Ready, checks boxes as it goes,
and updates the status column in `docs/ROADMAP.md` when done.

## Definition of Ready / Definition of Done
Every plan has both sections. DoR lists what must be true before a session
starts (dependencies, hardware, decisions); a session that finds an unmet DoR
item stops and asks. DoD is the checklist fully checked or deferred with a
reason, plus the milestone's measurable acceptance criteria and the standard
verification commands below.

## Conventions
- Work on a branch named after the milestone (`m4-share`). M0 creates the
  initial commit on `main` first.
- A plan's checklist is the definition of done. Add items if scope is
  discovered; do not silently drop items — move them to "Deferred" with a reason.
- Every milestone ends with: `cargo clippy --workspace --all-targets`,
  `cargo test --workspace`, `pnpm build` in `ui/`, and the footprint gate once
  M0 provides it.
- Non-negotiables from `CLAUDE.md` apply to every plan: no hooks/injection, no
  global config changes, backup before any change, low footprint.
