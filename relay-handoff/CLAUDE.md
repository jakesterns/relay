# Relay — project brief for Claude Code

Working name: **Relay**. Single Windows desktop app that (1) shares one PC's screen + audio to another PC for Discord/Zoom/Meet as a software capture-card replacement, and (2) applies per-game audio EQ / spatial and display color profiles, keyed to the user's headset and monitor. One installer, one uninstaller, one UI.

## Non-negotiables
- **Never trip anti-cheat.** OS/hardware-layer only: DXGI Desktop Duplication / Windows.Graphics.Capture for video, WASAPI process loopback + endpoint APO for audio, NvAPI/ADLX + DDC/CI for color. No DLL injection, no game hooks, no memory reads, no kernel driver except the signed audio-class virtual device.
- **Never touch global config.** No default-device changes, no global EQ, no edits to other apps. Profiles apply only while the target app has focus and restore on blur, exit, crash, or reboot. Original state is written to disk before any change.
- **Zero network config for the user.** WebRTC (ICE/STUN, mDNS discovery, DTLS-SRTP). LAN-first; pairing by code.
- **Low footprint.** Always-on core ≤ ~10 MB RAM, ~0% idle CPU. Audio engine, capture/encode, and UI load only on demand and tear down fully. Hardware encode only (NVENC/QSV/AMF). Target 4K60 HEVC at 40–80 Mb/s on LAN, <50 ms latency.
- **Two explicit opt-ins** at first run: the endpoint APO and the virtual camera/mic. Clear "what we install / how to remove" screen.

## Architecture
- `core/` Rust service: focus watcher, profile apply/restore, hotkeys, state backup. Tiny, always resident.
- `audio/` Rust DSP + signed APO DLL: biquad EQ cascade, partitioned-convolution HRTF, soft limiter. Bypass = pass-through, zero allocations. Match endpoint rate; never resample.
- `capture/` DXGI/WGC capture → HW encoder → webrtc-rs. Spun up per share only.
- `vdevice/` virtual camera (prefer Win11 MediaFrameSource; fallback OBS VirtualCam detection) + signed virtual mic/audio driver for the receiver side.
- `display/` NvAPI / ADLX color (vibrance, gamma, contrast, hue, LUT) + DDC/CI monitor controls (brightness, contrast, black equalizer, etc.).
- `daw-plugin/` "Relay Send" VST3 (later AU/CLAP) that ships DAW master/bus audio to the app over shared memory. This is how DAW audio is captured under ASIO exclusive mode.
- `ui/` Tauri. Separate process; closing the window costs nothing.
- `ai/` optional: user-supplied API key. Takes headset measured curve + measured footstep/explosion bands from live game audio + user goal → starting profile → short A/B listening/screenshot tests → refine. Not on the hot path.

## Data model
- **Hardware library**: headsets/IEMs (measured curves from AutoEQ / oratory1990 / crinacle), monitors (panel type, DDC/CI capability), audio interfaces.
- **Profile** = game × headset × monitor → {EQ bands, HRTF, limiter, GPU color, monitor settings, share preset}. Multiple rows per game allowed; auto-select by currently connected hardware.
- **Share presets**: Game / DAW / Desktop (encoder + audio sources + cursor).

## Feature scope
v1: share (LAN, 4K60, virtual cam/mic on receiver), presets, instrument strip (bitrate/latency/drops/load/audio), local high-bitrate recording + replay buffer, multi-source switching, per-game audio EQ + HRTF, per-game GPU/monitor color, profiles + hardware library, first-run consent.
v1.1: Relay Send DAW plugin, call-audio return route, mix-minus, Stream Deck, NDI-compatible output, AI tuning loop.
Out of scope: WAN sharing, Twitch streaming, overlays, in-game shaders (ReShade-style).

## UI direction
Dark, restrained, hardware-inspired. Warm black `#0E0D0C`, surfaces `#151312`/`#1C1917`, ivory text `#ECE6DC`, one champagne accent `#C9A96A`. Geist for UI, Geist Mono for readouts (tabular figures). Hairline dividers, slow damped motion, no glow/gradient/gamer palette. Signature element: the instrument strip. Mocks in `mocks/` (open the HTML files; PNGs are 2× renders). Every screen carries a plain-English "nothing on your PC was changed" line.

## Key risks to solve early
1. Signed APO + audio-class driver (EV cert, Hardware Dev Center attestation).
2. Rock-solid uninstall that restores the endpoint APO chain.
3. WASAPI-exclusive games bypass the APO — detect and tell the user.
4. Restore-on-crash for display settings; multi-monitor handling.
5. Wi-Fi at 4K60 is unreliable — surface a wired/6E recommendation, degrade gracefully.
