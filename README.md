# Relay

Software capture card + per-game audio/display profiles for Windows. See `CLAUDE.md` for the full brief, non-negotiables and repo map.

## Prerequisites
- Rust stable (via rustup), MSVC Build Tools 2022, Windows 10/11 SDK
- Node 24 + pnpm 9
- WebView2 runtime (ships with Windows 11)

## Quick start
```
cargo build
cargo run -p relay-core -- run        # always-on service
cd ui && pnpm install && pnpm tauri dev
```
