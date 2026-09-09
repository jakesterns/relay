# Milestone plans

One file per milestone, one chat session per milestone. Each plan is
self-contained: a session that reads only `CLAUDE.md` and its plan file has
enough context to start.

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
