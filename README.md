# Relay

Software capture card + per-game audio/display profiles for Windows. See `CLAUDE.md` for the full brief, non-negotiables and repo map.

## Prerequisites
- Rust stable (via rustup), MSVC Build Tools 2022, Windows 10/11 SDK
- Node 24 + pnpm 9
- WebView2 runtime (ships with Windows 11)

## Quick start
```
cargo build
cargo run -p relay-core -- run        # always-on service (headless; logs to %LOCALAPPDATA%\Relay\logs)
cargo run -p relay-core -- status     # what it is doing right now
cd ui && pnpm install && pnpm tauri dev
```

## Checks (what CI runs)
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                # includes the crash-restore integration test
cd ui && pnpm build
pwsh scripts/footprint.ps1            # release relay-core must idle at <=10 MB working set, ~0 % CPU
```
