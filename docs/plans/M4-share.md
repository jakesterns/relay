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
- [x] `relay-capture::source::dxgi`: Desktop Duplication fallback when WGC is unavailable; same trait. (`RELAY_CAPTURE=dxgi` forces it for testing; verified: present→receive p50 0.10 ms.)
- [x] Frame timing: capture at display refresh, drop to target fps without CPU copies (bounded channel; a busy consumer closes the frame, no copy); measure capture→encoder-input latency.

### Encode
- [x] `relay-capture::encode::mf`: Media Foundation HEVC hardware MFT (NVENC / QSV / AMF via vendor MFTs) fed D3D11 textures (`MFCreateDXGISurfaceBuffer`), low-latency mode, CBR, B-frames off, keyframe on request. Software MFTs are never enumerated (`MFT_ENUM_FLAG_HARDWARE` only, bound to the capture adapter's LUID), so a software fallback is impossible by construction.
- [x] Encoder benchmark: 4K60 sustained for 60 s (`relay-share bench-encode 60 4k`, GPU upscale 1440p→2160p because the sender monitor is 1440p): p50 10.2 ms, p99 10.8 ms, max 12.0 ms, 3601 frames at 60.0 fps, 0 drops, process CPU 2.1 %.
- [x] Decision gate: capture + encode p99 ≈ 10.8 ms « 20 ms → **Media Foundation holds the budget; direct NVENC not needed.** (Sender: NVIDIA HEVC Encoder MFT on the RTX 3090.)

### Audio
- [x] WASAPI loopback of the default render endpoint; process-loopback (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`) for game-only capture; Opus 48 kHz stereo 160 kb/s default, 10 ms frames. All three paths (desktop / process / mic) verified with `relay-share bench-audio`.
- [x] Optional microphone track. **Simultaneous since S2 (2026-09-14):** the sender carries the program mix *and* the microphone as two Opus tracks; `--audio-mic` adds the second track rather than replacing the first.

### Transport
- [x] webrtc-rs sender: one video track (HEVC RTP), one or two audio tracks, DTLS-SRTP, ICE host candidates only (LAN), STUN off by default. (webrtc-rs 0.20; `build_pc` registers HEVC pt98 + Opus pt120, no ICE servers.)
- [x] Discovery: mDNS `_relay._udp.local` with instance name = hostname; pairing by six-digit code. The code seeds an HMAC over each side's SDP; the SDP carries the DTLS fingerprint and DTLS verifies the cert against it, so a good MAC transitively pins the peer. Paired peers persist in `%LOCALAPPDATA%\Relay\data\peers.json`.
- [x] Wi-Fi detection (adapter type of the route to the peer, `GetAdaptersAddresses` → `IF_TYPE_IEEE80211`) → `link` event with a "wired or 6 GHz" recommendation; AIMD bitrate step-down driven by receiver RTP-sequence loss feedback (`SigMsg::Loss`), applied live via `ICodecAPI` mean-bitrate. Verified: link=wired on the test LAN.

### Receiver mode
- [x] "Receive" screen: shows this PC's pairing code, Start/Stop receiving, and pair status; the stream renders in a native D3D11 swapchain window (not the webview) via MF DXVA HEVC decode. The core spawns/kills `relay-share recv` (StartReceive/StopReceive) and relays its code/pair events as `core://receive-status`. `recv --headless` remains the transport benchmark.
- [x] Receiver latency measurement: sender stamps frames with an in-band SEI (unix ns), receiver rebases via NTP clock sync and reports capture→arrival and capture→present. Loopback: capture→arrival p50 4.6 ms / p99 7.4 ms.

### Process model and UI
- [x] `relay-share` spawned by the core on `StartShare`, killed on `StopShare`; crash/unexpected exit → `ShareStatus { message }` so the UI can report and offer restart. Core RSS unchanged while sharing (measured 9.96 → 10.16 MB; the core never loads capture/encode).
- [x] Stats events every 500 ms: bitrate, latency, dropped/sent, encoder load, CPU, audio peak → `Event::ShareStats` → live instrument strip.
- [x] Share screen: Start/Stop, preset chips, source toggles, bitrate slider, receiver scan + pairing-code entry, live strip. Preview toggle (P) relayed as a notice (webview preview surface deferred — see Deferred).
- [x] Hotkeys Ctrl+Alt+S (toggle share, re-runs the last request) and Ctrl+Alt+P (preview) wired in the core.

## Definition of Done
> These are the milestone's acceptance criteria. Two of the four are **still
> aspirational** — they name a two-PC run that has never happened (decision
> 2026-09-10; see Deferred). They are kept unchecked rather than reworded,
> because the target is right even though it is unmet.

- [x] Every checklist item checked or moved to Deferred with a reason; Measurements table filled.
- [ ] **Aspirational — not yet run.** 4K60 HEVC wired LAN share to a second PC, glass-to-glass < 50 ms measured, sender CPU ≤ 2 %, zero dropped frames over 10 minutes. What *is* measured is the loopback equivalent (capture→present p50 5.6 ms, 4K60 MF HEVC encode p99 10.8 ms, zero AU loss) plus thorough unit coverage of the transport logic; no glass-to-glass number exists. Deferred to the MVP validation pass with a runbook.
- [x] Stopping the share leaves no `relay-share` process and no change in core RSS. ✔ on loopback.
- [ ] **Aspirational — not yet run.** Works with no network configuration on either PC. Code pairing, DTLS and host-only candidates do run end to end on this machine — `scripts/m6-loopback.ps1` pairs a real sender and receiver over `127.0.0.1` with a fixed code. But it passes `--peer` explicitly, so **mDNS discovery is bypassed**, and that is precisely the part this criterion is about: two machines on a real LAN finding each other with nothing configured has never been run. Same deferred run.

## Measurements
`relay-share probe` (MFTEnumEx, video encoder category, HEVC, `MFT_ENUM_FLAG_HARDWARE`), 2026-09-09:
hardware HEVC encoder MFTs = "NVIDIA HEVC Encoder MFT", "AMDh265Encoder" (×2, iGPU);
Windows.Graphics.Capture supported = true. No software MFT is ever requested.

| Stage | p50 | p99 | Notes |
|---|---|---|---|
| capture → encoder input | −3.28 ms | −2.80 ms | WGC stamps the DWM present slot, so frames reach the encoder ~3 ms *before* they hit glass; max 75 ms is the one first-frame warm-up outlier. 1440p60, 0 drops. |
| encode | 10.2 ms | 10.8 ms | MF (NVIDIA HEVC Encoder MFT, RTX 3090), 4K60 CBR 60 Mb/s, 60 s, 3601 frames, 0 drops, max 12.0 ms. 1440p60 native: p50 4.9 / p99 5.1 ms. |
| network + decode + present | ~1 ms | ~3 ms | Loopback delta: capture→present p50 5.6 ms minus capture→arrival 4.6 ms ≈ decode + video-processor + swapchain present. DXVA decode via HEVCVideoExtension MFT, D3D11 flip-discard swapchain, RTX 3090. |
| glass-to-glass (loopback) | 5.6 ms | 7–9 ms | Full pipeline capture→present, single machine (no LAN transit, no monitor scan-out). 1200+ AUs, 0 decode errors, 1440p60. Real two-PC wired glass-to-glass (camera+stopwatch) is the user's final DoD run — see Deferred. |

### Second audio track (session S2, 2026-09-14)

`scripts/dual-audio-check.ps1 -Secs 25 -Reps 4`: the same loopback share run
alternately with one audio track and with the microphone alongside it, four
times each, 1440p60. A generated 440 Hz tone plays through the default
endpoint for the whole run and every run asserts it carried ~2 500 program
packets — **WASAPI loopback of a silent endpoint delivers no packets at all**,
so without the tone the program track reads zero and the comparison is void
(two earlier passes were thrown away for exactly this).

| Stage | one track | + mic track | delta |
|---|---|---|---|
| capture → arrival p50 | 2.35 ms | 2.40 ms | **+0.05 ms** |
| capture → arrival p99 | 3.52 ms | 4.09 ms | **+0.57 ms** |
| encode mean | 4.99 ms | 5.00 ms | +0.01 ms |
| encode max | 5.11 ms | 5.15 ms | +0.04 ms |
| capture → send mean | 2.22 ms | 2.31 ms | +0.09 ms |
| capture → send max | 2.91 ms | 3.37 ms | +0.46 ms |
| sender CPU (median of 4) | 5.8 % | 9.8 % | +4.0 pt, but the per-rep ranges overlap (3.5–6.5 % vs 5.1–11.1 %) — too noisy at this granularity to quote as a figure |
| fps / dropped | 60.0 / 0 | 60.0 / 0 | — |

Medians of four reps. Every delta except CPU is inside run-to-run noise, and
glass-to-glass stays far inside the 50 ms budget (loopback baseline 5.6 ms p50
plus ~0.6 ms of tail). Loopback runs sender *and* receiver on one machine, so
the tail figures are pessimistic relative to a real two-PC share.

Opus packetization latency (WASAPI block arrival → packet encoded,
`relay-share bench-audio 10 [mic|dual]`), which is the only latency the audio
path itself adds:

| Track | alone | both running | bitrate |
|---|---|---|---|
| program mix | p50 0.229 ms / p99 0.344 ms | p50 0.222 ms / p99 0.347 ms | 161 kb/s |
| microphone | p50 0.235 ms / p99 0.420 ms | p50 0.243 ms / p99 0.422 ms | 50 kb/s |

**The mic encoder profile is load-bearing, not a nicety.** The first
implementation gave the mic the same music-grade encoder as the program mix
(160 kb/s stereo, libopus default complexity). Across three reps that cost the
*video* path a clean, repeatable regression — encode mean 5.12 → 5.86 ms and
capture→arrival p99 3.3 → 7.6 ms — with no separation in the audio numbers at
all: the cost was CPU contention, not the audio pipeline. Giving the mic a
speech profile (`OpusProfile::voice()`: 64 kb/s target, `Application::Voip`,
complexity 5) removed it entirely — encode mean became identical to the
single-track case. The program mix keeps libopus's default complexity
explicitly (`complexity: None`), so the single-track path is byte-for-byte
what M4 measured.

## Out of scope (this milestone)
Virtual camera/mic on the receiver (M5), recording and replay (M6), DAW/Desktop presets (M6), WAN.

## Unit coverage (decision 2026-09-10)
The dual-PC run cannot be executed now, so the pipeline's logic is unit-tested
thoroughly instead and **live integration testing moves to the future MVP
pass**. 101 tests across the workspace (63 in `relay-capture`, 36+2 in
`relay-core`); everything that runs without a second machine or GPU session is
covered:
- H265 RTP depacketization (single NAL / AP / FU, orphan and stale fragments,
  truncated payloads, PACI), SEI timestamp round-trip incl. emulation
  prevention, 3-byte start codes, garbage input.
- Pairing/signalling: HMAC verify (case, truncation, tamper), SDP fingerprint
  extraction, `SigMsg` wire format, `SigStream` over real localhost TCP
  (round-trip, close, garbage, 256 KB cap), NTP-style `clock_sync` recovering
  a simulated 250 ms skew end-to-end, peer store upsert/corrupt-file recovery.
- Feedback control (`transport/control.rs`, extracted pure): AIMD
  increase/decrease/clamp — this extraction also fixed a panic when the
  requested bitrate is below the 8 Mb/s floor (`clamp` with floor > ceiling) —
  and RTP-sequence loss windows incl. u16 wraparound; reordered/duplicate
  packets no longer count as ~65 k losses.
- Peer pick (name match, case-insensitivity, empty LAN), mDNS instance-name
  parsing, `Discovered` JSON shape, Wi-Fi recommendation gating + `link_kind`
  wire format, loopback adapter never classed as Wi-Fi.
- QPC time scale/monotonicity, 90 kHz→100 ns PTS math, `InflightClock`
  encode-latency bookkeeping, `Percentiles` edge cases.
- CLI parsing of `relay-share send|recv` flags; core↔engine contract:
  `send_args`/`recv_args` command-line mapping (extracted pure) and NDJSON
  `decode_line` for every event incl. junk tolerance, plus `ShareRequest`
  serde defaults and `ShareEvent` UI tag format.

Still hardware/two-machine-bound (not unit-testable): WGC/DXGI capture, MF
encode/decode sessions, WASAPI capture, D3D11 present, webrtc DTLS/ICE
end-to-end — exercised by the bench commands and the loopback run instead.

## Deferred
- **Live integration testing: two-PC wired glass-to-glass camera+stopwatch run and the 10-minute 4K60 zero-drop DoD run.** Moved to the future MVP validation pass (decision 2026-09-10: dual-PC testing not currently possible; unit coverage above stands in). Everything it needs is built and green on loopback (full capture→encode→transport→DXVA-decode→present pipeline, p50 5.6 ms capture→present, zero AU loss). Runbook: on PC-B `relay-share recv` (or the UI Receive screen) → note the code; on PC-A `RELAY_PEER=<PC-B> relay-core share-start <code>` (or the UI Share screen) → let it run 10 min at 4K60 and read the receiver's `capture_to_present_ms` p50/p99 plus a camera+stopwatch check for the absolute number.
- ~~**Simultaneous microphone track.**~~ **Done 2026-09-14 (session S2).** The sender opens up to two Opus tracks — `relay-audio` (desktop endpoint loopback or one process tree) and `relay-audio-mic` — and the receiver decodes both and sums them one op before the WASAPI render buffer, so a plain call still hears a single stream while the two sources stay separable for the virtual mic and for S19's mix-minus. Decision record: `docs/dev/dual-audio-decision.md`. Presets carry an audio *source set* (`{desktop, mic}`), the pre-S2 four-way string still deserializes to the same meaning, and a peer sending one unnamed audio track still lands on the program mix (`transport::audio_role`, arrival-order fallback). Measured cost in Measurements below: +0.05 ms p50 / +0.6 ms p99 on capture→arrival, no change to encode. Harness: `scripts/dual-audio-check.ps1`.
- ~~**In-webview preview surface.**~~ **Done 2026-09-14.** No second capture path: the video pipeline taps the frame it already has, the existing NV12 video processor scales it to 480×270 on the GPU (~200 KB readback), and WIC encodes a 24bppBGR JPEG that rides the engine's NDJSON to the Share screen. Ctrl+Alt+P sends a `preview` command all the way into the engine, so switching it off stops the readback rather than just hiding the picture. Measured on loopback (`scripts/preview-check.ps1 -Toggle`): 0 frames while off, 2 fps once on, ~21 KB per frame, ~41 KB/s.
- ~~**HEVC Video Extension dependency on the receiver.**~~ **Closed 2026-09-14 (S7) — decision: keep detect-and-warn; do not bundle, do not build a DXVA-direct decoder.** Three options were on the table and only one survives.
  - *Bundle the extension.* Not available to us. The free "HEVC Video Extensions from Device Manufacturer" is licensed by Microsoft per-device to PC makers; the generally available "HEVC Video Extensions" is a paid Store item. Neither may be redistributed inside a third-party installer, so this is a licensing wall, not an engineering one.
  - *Write a DXVA-direct decoder.* Rejected on cost against benefit. Skipping the MFT means owning HEVC bring-up: VPS/SPS/PPS and slice-header parsing, reference-picture-set management, and hand-filled DXVA2 picture/slice/quantisation buffers, with per-vendor quirks to chase on hardware we do not have. Weeks of work, and the failure mode of getting it subtly wrong is corrupt video rather than a clean error — strictly worse than the message we can print today. It buys nothing for a receiver that is one free Store install away from working.
  - *Detect and warn.* Kept and sharpened. `Method::ShareCapabilities` runs `relay-share probe` when the Receive or Share screen opens; the banner now links straight into the Store (`ms-windows-store://search/?query=HEVC Video Extensions` — the search rather than a product deep link, because the free package's product ID is not something we can verify from here) and says plainly why Relay cannot ship it for you. The decoder's own `bail!` carries the same explanation for anyone running the engine directly.
  - Verified on the dev machine: `{"adapters":["NVIDIA GeForce RTX 3090"],"can_share":true,"can_receive":true,"encoders":["AMDh265Encoder","NVIDIA HEVC Encoder MFT"],"decoders":["HEVCVideoExtension"]}`.

## S7 — codec and capture robustness (2026-09-14)

Three documented edges that all failed the same way: a machine unlike this dev PC
hit a `bail!` instead of a graceful path. The question S7 had to answer first was
which of them deserve code. Two did; one is an honest permanent limit.

- **MFT allocator path (`encode/mf.rs`) — fixed with code.** The old branch bailed
  with "allocator path not implemented" on the *first encoded frame*, i.e. after
  the share had already started. It is now implemented: `alloc_output_sample`
  hands `ProcessOutput` an aligned memory buffer sized from
  `MFT_OUTPUT_STREAM_INFO::cbSize`. The MFT contract has three states, and the
  code now distinguishes them — `PROVIDES_SAMPLES` (the MFT insists),
  `CAN_PROVIDE_SAMPLES` (either), neither (the caller must allocate). Both
  encoders on this PC set `PROVIDES`, so the new branch **cannot be exercised
  end-to-end here**; `RELAY_FORCE_MFT_ALLOCATOR=1` takes it on an MFT that reports
  `CAN_PROVIDE` only, and logs that it was ignored otherwise (confirmed against
  NVENC). What *is* covered here is the part most likely to be wrong: a unit test
  allocates at four alignments, writes into the buffer as an MFT would, and reads
  it back through exactly the `take_output` sequence. A failed `ProcessOutput` no
  longer leaks the sample we allocated.
- **No software HEVC encode (`encode/mf.rs`) — a real limit, not a gap.** Left
  unimplemented deliberately: CPU HEVC cannot hold 4K60 inside the <50 ms budget,
  and CLAUDE.md pins the share to NVENC / Quick Sync / AMF. What changed is the
  message. It now names the adapter it actually looked at, and — the case that
  matters most, a laptop whose display hangs off the iGPU while the encoder lives
  on the dGPU — enumerates HEVC encoders on *every* adapter and tells the user to
  move the capture to that GPU or set Relay to "High performance". With no encoder
  anywhere it says so and points at the graphics driver. The Share screen's banner
  (added S1) covers the same ground before anything is attempted, and now names
  the GPU: `ProbeReport` gained `adapters`, plumbed through `Capabilities` →
  `Reply::Capabilities` → `ipc.ts`.
- **48 kHz stereo only (`audio.rs`) — fixed with code.** This was the wrong thing
  to call a limit. A 44.1 kHz USB interface and a mono headset microphone are the
  common case, not the exotic one, and the mic path in particular would have
  failed outright on most machines. New pure module `capture/src/resample.rs`
  folds any channel count to stereo (mono duplicated; >2 downmixed per ITU-R
  BS.775 over the WASAPI channel order, LFE dropped, clamped so Opus never sees
  a value above 1.0) and converts any rate to 48 kHz (Catmull-Rom over a
  fractional read position, with three cascaded low-pass sections at 20 kHz ahead
  of any downsample). State carries across blocks, so 10 ms WASAPI packets stitch
  without a click. This is the network path, not the APO path — CLAUDE.md's "never
  resample" rule is about matching the endpoint chain and is untouched.
  - Covered by tests: output length tracks the rate ratio within 8 frames over a
    second at seven rates (this is what stops audio drifting from video on a long
    share); a 1 kHz sine survives 44.1→48 kHz with its peak intact and no
    block-boundary discontinuity; a 40 kHz tone at 192 kHz is attenuated below
    0.05 instead of aliasing into the passband; and — closest to the real thing
    available here — a simulated 44.1 kHz *mono* endpoint drives the real Opus
    encoder and decoder for a second, 480 frames per packet.
  - **Not exercisable live on this PC:** every endpoint here reports 48 kHz
    stereo, and changing a Windows endpoint's default format to test it is exactly
    the global config Relay is not allowed to touch. `relay-share bench-audio` now
    prints `endpoint_rate` / `endpoint_channels` / `conversion`, and the sender
    logs `audio pipeline up rate=… channels=… conversion=…`, so the first run on
    an unusual device says what happened without a debugger. Measured here:
    desktop and microphone both `48000 / 2 / conversion: none` (passthrough).
- **Two smaller edges found and closed while in there.**
  - The capture loop reads the WASAPI shared buffer as `f32` with no check.
    Shared-mode always mixes in 32-bit float, so this is a guard rather than an
    expected path — but a mismatch would have reinterpreted integers as floats.
    It now verifies the mix format (including the `WAVEFORMATEXTENSIBLE` subtype)
    and names the bit depth and format tag it found.
  - The decoder assumed MFT-provided samples, so a decoder wanting
    caller-allocated output would have surfaced as "decoder gave no sample" on
    frame one. Because the receive path is DXVA-only by construction
    (`frame_from_sample` casts to `IMFDXGIBuffer` and presents the texture with no
    system-memory copy), that condition means "this decoder is running on the
    CPU" — so it is now checked at construction and says that.

### Merging with S2 (dual audio): the A/V sync unit trap

S2 landed per-block capture stamps (`VecDeque<(qpc, sample_count)>`) so the
receiver can rebase A/V sync, and S7 put a resampler in front of the buffer those
counts describe. The two are only compatible if the stamp counts **post-conversion
samples**, because that is the unit the drain spends. Counting the WASAPI block's
own length instead leaves the deque permanently starved at 441 fed against 960
spent, so instead of naming the block the oldest queued sample came from it names
whichever block arrived most recently — capture time biased new, latency
under-reported, audio that will not line up with video. It compiles, it is silent,
and it only misbehaves on endpoints that are not already 48 kHz stereo.

Resolved by stamping `converted.len()`. The arithmetic is now a pure
`take_capture_stamp()` so it can be tested without WASAPI, and
`stamps_counted_in_source_samples_lose_the_capture_time` drives the real converter
through ten seconds of a 44.1 kHz mono mic and asserts both halves: converted
counts keep the deque exactly in step with the buffer, source counts disagree on
>90 % of packets and skew new. (Honest limit: that test locks the contract and
proves the two unit systems diverge, but it does not execute `OpusStream::next`,
which needs WASAPI — the line itself is kept correct structurally, by binding
`converted` once and feeding both the stamp and the buffer from it.)

This is per track by construction: each `OpusStream` owns its own converter and
its own stamps, so a 44.1 kHz mono mic alongside a 48 kHz desktop endpoint
converts at two different ratios without them interacting. The bench and the
sender log report `endpoint_rate` / `endpoint_channels` / `conversion` per track
for the same reason. The muxers' hardcoded 48 kHz stereo `AudioConfig` is also now
true by construction rather than by a `bail!`.

Loopback regression after the changes (`scripts/m6-loopback.ps1`, 25 s): 1446 AUs,
zero drops, capture→arrival p50 3.34 ms / p99 7.51 ms — unchanged. Footprint gate
still green (6.82 MB / 0 %). After merging S1–S6 and S8, re-measured with
`scripts/dual-audio-check.ps1` (12 s, tone playing): single-track arrival p50
3.87 ms / p99 6.01 ms against dual-track 3.85 / 6.02, and 424 workspace tests
green.

Note, not a defect: WASAPI desktop loopback delivers no packets at all while
nothing is rendering, so `audio_packets` can sit at 0 or freeze on an idle
machine. That is documented Windows behaviour and the receiver simply has no
audio to play.
