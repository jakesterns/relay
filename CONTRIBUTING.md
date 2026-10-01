# Contributing to Relay

Thanks for your interest. Relay is maintained by one person, so please keep
changes focused.

## How changes land

- **Pull requests only.** Nobody pushes to `main` directly. Fork the repo,
  branch from `main`, and open a PR against `main`.
- **Every PR needs the owner's review.** `.github/CODEOWNERS` assigns the
  whole tree to the owner, and branch protection requires that approval
  before merging.
- For anything larger than a fix, open an issue first so we can agree on the
  approach.
- By contributing you agree that your contribution is licensed under the
  project's [MIT licence](LICENSE).

## Ground rules

The non-negotiables in `CLAUDE.md` apply to every change:

- Nothing that could trip anti-cheat: no injection, hooks, memory reads or
  kernel drivers (see `docs/dev/anti-cheat.md`).
- Never change global configuration (default devices, system-wide EQ, other
  apps). Back up original state before applying anything and restore it.
- LAN only; no new network requests.
- Keep the always-on core within its footprint budget (about 10 MB, ~0 % idle
  CPU).
- New dependencies must use a permissive licence; CI fails on GPL, AGPL and
  unknown licences (`deny.toml`).

## Running the tests

```
cd ui
pnpm install
pnpm build              # needed before any cargo step that builds relay-ui
pnpm test               # UI component tests (Vitest + jsdom)
cd ..
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pwsh scripts/footprint.ps1   # release footprint gate
```

Licence check (needs `cargo install --locked cargo-deny@0.20.2` and
`cargo install --locked cargo-about@0.9.2 --features cli`):

```
pwsh scripts/licenses.ps1 -Check
```

UI tests are jsdom component tests only; they must never drive a real window.
CI (`.github/workflows/ci.yml`) runs all of the above on every PR.

Set `RELAY_NO_INSTALL=1` if you have enabled the repo's post-commit hook and
do not want each commit installed over your local Relay.
