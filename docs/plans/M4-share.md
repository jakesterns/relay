# M4 — Share MVP

**Kickoff prompt:**
> Read CLAUDE.md and docs/plans/M4-share.md. Work on branch `m4-share`. Latency and resource efficiency are the top priority: measure them at every step and record numbers in the plan's Measurements section. Work through the checklist in order, check items off as you go, and update docs/ROADMAP.md when done.

## Goal
Share one PC's display and audio to another PC on the LAN at up to 4K60 HEVC,
hardware-encoded, under 50 ms glass-to-glass, with the receiver rendering to a
window. The share engine is a separate process that exists only while sharing.

## Priorities (decision 2)
1. Glass-to-glass latency < 50 ms wired.
2. Sender CPU ≈ 1 %, GPU encoder load only; no CPU encode path at all.
3. Zero copies where the APIs allow: capture texture → encoder input on the GPU.
4. Media Foundation first because it is vendor-neutral; if it cannot hold the
   budget after tuning (low-latency mode, B-frames off, GOP/rate control), move to
   the NVENC SDK directly within this milestone rather than deferring.

## Depends on
M0 (process model, logging, CI). Does **not** depend on M1–M3.

## Architecture
```
relay-core ──spawn──► relay-share (new bin in crates/capture)
                        capture (WGC) ─► encoder (MF HEVC HW) ─► webrtc-rs ─► LAN
                        WASAPI loopback ─► Opus ───────────────┘
                        stats ──IPC events──► core ──► UI instrument strip
Receiver: same app, "Receive" screen; webrtc-rs ─► MF HW decode ─► D3D11 swapchain window
```

## Definition of Ready
- [x] M0 complete: initial commit, CI, footprint gate, logging, child-process model available. (ROADMAP: done 2026-09-09, CI green.)
- [x] Second PC on the same wired LAN available for receiver testing (confirmed 2026-09-09); hostname to be filled in from mDNS discovery when transport testing starts: ____
- [x] Sender GPU and driver noted here (NVENC/QSV/AMF availability): NVIDIA GeForce RTX 3090, driver 32.0.16.1664 (NVENC, HEVC + B-frames, Ampere gen-7 NVENC); AMD Raphael iGPU (0x164E, VCN — AMF available but unused). Sender hostname `Jake`.
- [x] Media Foundation HEVC hardware encoder MFT confirmed present: registry `HKLM\SOFTWARE\Classes\MediaFoundation\Transforms` lists "NVIDIA HEVC Encoder MFT" ({966F107C-8EA2-425D-B822-E4A71BEF01D7}) and "AMDh265Encoder" ({5fd65104-a924-4835-ab71-09a223e3e37b}). A live `MFTEnumEx` listing to be recorded in Measurements by the encode probe.
- [x] Latency measurement method agreed: frame timestamps embedded by the sender, receiver reports glass-to-glass estimate; a camera-and-stopwatch check for the final number.

## Checklist
### Capture
- [x] `relay-capture::source::wgc`: Windows.Graphics.Capture of a monitor (window later), `Direct3D11CaptureFramePool` with 2 buffers, cursor toggle via `IsCursorCaptureEnabled`, border suppression on Win11.
- [ ] `relay-capture::source::dxgi`: Desktop Duplication fallback when WGC is unavailable; same trait.
- [x] Frame timing: capture at display refresh, drop to target fps without CPU copies (bounded channel; a busy consumer closes the frame, no copy); measure capture→encoder-input latency.

### Encode
- [x] `relay-capture::encode::mf`: Media Foundation HEVC hardware MFT (NVENC / QSV / AMF via vendor MFTs) fed D3D11 textures (`MFCreateDXGISurfaceBuffer`), low-latency mode, CBR, B-frames off, keyframe on request. Software MFTs are never enumerated (`MFT_ENUM_FLAG_HARDWARE` only, bound to the capture adapter's LUID), so a software fallback is impossible by construction.
- [x] Encoder benchmark: 4K60 sustained for 60 s (`relay-share bench-encode 60 4k`, GPU upscale 1440p→2160p because the sender monitor is 1440p): p50 10.2 ms, p99 10.8 ms, max 12.0 ms, 3601 frames at 60.0 fps, 0 drops, process CPU 2.1 %.
- [x] Decision gate: capture + encode p99 ≈ 10.8 ms « 20 ms → **Media Foundation holds the budget; direct NVENC not needed.** (Sender: NVIDIA HEVC Encoder MFT on the RTX 3090.)

### Audio
- [ ] WASAPI loopback of the default render endpoint; process-loopback (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`) for game-only capture; Opus 48 kHz stereo 128–256 kb/s, 10 ms frames.
- [ ] Optional microphone track.

### Transport
- [ ] webrtc-rs sender: one video track (HEVC RTP), one or two audio tracks, DTLS-SRTP, ICE host candidates only (LAN), STUN off by default.
- [ ] Discovery: mDNS `_relay._udp.local` with instance name = hostname; pairing by six-digit code that seeds the DTLS fingerprint check. Paired peers persist in `%LOCALAPPDATA%\Relay\peers.json`.
- [ ] Wi-Fi detection (adapter type of the route to the peer) → UI recommendation "wired or 6 GHz"; bitrate step-down on sustained loss.

### Receiver mode
- [ ] "Receive" screen in the UI: list discovered senders, enter code, show stream in a D3D11-backed window (native, not in the webview) with MF hardware decode.
- [ ] Receiver latency measurement: sender stamps frames; receiver reports glass-to-glass estimate.

### Process model and UI
- [ ] `relay-share` binary spawned by the core on `Method::StartShare`, killed on `StopShare`, crash → core reports and UI offers restart. Core RSS unchanged while sharing.
- [ ] Stats events every 500 ms: bitrate, latency, dropped/sent, encoder load, CPU, audio peak → `Event::ShareStats` → instrument strip.
- [ ] Share screen: Start/Stop, preset chips (Game only for now), source toggles, preview toggle (P), receiver card.
- [ ] Hotkeys Ctrl+Alt+S (toggle share) and Ctrl+Alt+P (preview) wired.

## Definition of Done
- Every checklist item checked or moved to Deferred with a reason; Measurements table filled.
- 4K60 HEVC wired LAN share to a second PC, glass-to-glass < 50 ms measured, sender CPU ≤ 2 %, zero dropped frames over 10 minutes.
- Stopping the share leaves no `relay-share` process and no change in core RSS.
- Works with no network configuration on either PC.

## Measurements
`relay-share probe` (MFTEnumEx, video encoder category, HEVC, `MFT_ENUM_FLAG_HARDWARE`), 2026-09-09:
hardware HEVC encoder MFTs = "NVIDIA HEVC Encoder MFT", "AMDh265Encoder" (×2, iGPU);
Windows.Graphics.Capture supported = true. No software MFT is ever requested.

| Stage | p50 | p99 | Notes |
|---|---|---|---|
| capture → encoder input | −3.28 ms | −2.80 ms | WGC stamps the DWM present slot, so frames reach the encoder ~3 ms *before* they hit glass; max 75 ms is the one first-frame warm-up outlier. 1440p60, 0 drops. |
| encode | 10.2 ms | 10.8 ms | MF (NVIDIA HEVC Encoder MFT, RTX 3090), 4K60 CBR 60 Mb/s, 60 s, 3601 frames, 0 drops, max 12.0 ms. 1440p60 native: p50 4.9 / p99 5.1 ms. |
| network + decode + present | | | |
| glass-to-glass | | | |

## Out of scope (this milestone)
Virtual camera/mic on the receiver (M5), recording and replay (M6), DAW/Desktop presets (M6), WAN.

## Deferred
_(none yet)_
