# Relay

Relay is a Windows desktop app that does two things:

1. **Shares one PC's screen and audio to another PC on the same network**, as a
   software replacement for a capture card. On the receiving PC the stream
   shows inside Relay and can be exposed as a virtual camera, so you can use it
   in Discord, Zoom or Meet.
2. **Applies per-game profiles**: audio EQ and spatial sound, plus display
   colour and monitor settings, matched to the headset and monitor you have
   connected. A profile applies only while that game has focus and is undone
   when you leave it.

One installer, one uninstaller, one UI.

## Status

Relay is pre-release and developed by one person. Be aware of what works today:

- **Screen and audio sharing** between two Windows PCs on a LAN works and is
  tested on real hardware (Windows 11 and Windows 10). Hardware encoding only
  (NVIDIA, Intel or AMD): HEVC when both ends can decode it, H.264 otherwise.
- **Recording and replay buffer**, multi-source switching and the instrument
  strip (bitrate, latency, drops, load) work.
- **Display profiles** (GPU colour through NVIDIA/AMD APIs, monitor settings
  over DDC/CI) work, and are restored on blur, exit, crash or reboot.
- **Per-game audio EQ and spatial sound are optional and off by default.**
  They run inside a Windows audio component (an endpoint APO). Windows only
  loads audio components signed by Microsoft, and Relay's is not, so per-game
  EQ needs one Windows audio protection (`DisableProtectedAudioDG`) turned off
  for the whole PC — the same switch Equalizer APO uses. Relay never changes
  it unless you turn it on in Settings, after a confirmation that lists the
  exact change; turning it off again, or uninstalling Relay, puts back exactly
  what was there before. Everything else in Relay works without it.
- The **virtual camera** works on Windows 11; Windows 10 support is limited.

Only Windows 10 and 11 (x64) are supported.

## Install

Download the installer from the [Releases](../../releases) page and run it.
It installs per user into `%LOCALAPPDATA%\Relay` and needs no admin rights for
sharing. Two optional components (the audio component and the virtual
camera/microphone) are offered at first run, each with a plain description of
what it installs and how to remove it. Uninstall from Windows Settings > Apps.

## Privacy and what Relay changes

- **LAN only.** Sharing uses WebRTC between PCs on the same network, with
  mDNS discovery, a six-digit pairing code and DTLS-SRTP encryption. Nothing
  goes through a server. There are no accounts, no telemetry and no analytics.
- Relay makes two other network requests, both to GitHub:
  - when you ask, fetching a headphone's measured frequency response from the
    AutoEQ project (or you can paste a curve instead);
  - an update check: at most once a day, never during a share or a game, Relay
    asks GitHub's Releases API whether a newer Relay exists. Nothing about you
    or your PC is sent. Turn it off in Settings > Updates ("Check for updates
    automatically"). An update is downloaded and installed only when you choose
    Install now (or turn on "Install updates automatically", off by default),
    and only after its SHA-256 and signature check out.
- **Nothing global is changed.** Relay does not change your default audio
  devices, apply system-wide EQ or edit other apps' settings. Before applying
  any profile it writes the original state to disk, and it restores that state
  when the game loses focus, exits or crashes, or after a reboot. Every screen
  says plainly whether anything on your PC was changed.
- The only system-level items Relay installs are the two opt-in components
  above and one Windows Firewall rule (private and domain networks only) for
  the sharing process. The uninstaller removes all of them.

## Anti-cheat

Relay works only at the OS and hardware layer: Windows screen-capture APIs,
WASAPI audio, the GPU vendors' colour APIs and DDC/CI. It never injects into,
hooks or reads the memory of a game, and ships no kernel driver. See
[docs/dev/anti-cheat.md](docs/dev/anti-cheat.md) for the details.

## Build from source

Requirements: Windows 10/11 x64, Rust stable via rustup, MSVC Build Tools 2022
with the Windows 10/11 SDK, Node.js 24, pnpm 9 and the WebView2 runtime (ships
with Windows 11).

```
cd ui
pnpm install
pnpm build          # the Tauri shell embeds ui/dist at compile time
cd ..
cargo build --workspace
cargo test --workspace
cd ui && pnpm test  # UI component tests (jsdom)
```

Run the service with `cargo run -p relay-core -- run`, then the UI with
`cd ui && pnpm tauri dev`. To build the installer, run
`scripts/stage-bundle.ps1` and then the `pnpm tauri build` command it prints.
Staging also generates `licenses.html` and needs `cargo-about` (see
`scripts/licenses.ps1`).

`CLAUDE.md` has a map of the crates; `docs/` has the roadmap, plans and
runbooks.

## Licence

Relay is released under the [MIT licence](LICENSE). Bundled third-party
assets (SADIE II HRIRs, the Geist fonts, the AutoEQ index) are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). See
[CONTRIBUTING.md](CONTRIBUTING.md) to contribute. Release signing is described
in the [code signing policy](docs/CODE_SIGNING_POLICY.md).
