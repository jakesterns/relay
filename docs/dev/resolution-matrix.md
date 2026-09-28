# S32 — resolution matrix, two PCs

Measured 2026-09-28 with sender build `0b37c23` (branch `fix/core-version`) and receiver r30 (`053bcfe`).
The sender is the main PC: Windows 11, RTX-class NVIDIA, 2560x1440 desktop, NVENC H.264. The receiver
is PC 2: Windows 10 19045, RTX 2080, 1920x1080 at 240 Hz panel, H.264 only (DXVA Microsoft H264 decoder).
Both PCs are wired to the same router. Content: `scripts/motion-test.html` full screen, which keeps the
encoder at its target. Each run lasted 45 s and was started with `relay-core share-start`, using `RELAY_SIZE`,
`RELAY_FPS` and `RELAY_BITRATE_MBPS` for the size, frame rate and bitrate.

| Encode | Target | Sent (mean / max) | Received | fps presented | Lost | Gaps | Keyframe req | Latency p95 | Receiver GPU decode (avg / peak) | CPU (either end) |
|---|---|---|---|---|---|---|---|---|---|---|
| 3840x2160 @ 60 | 60 Mb/s | 61.7 / 68.8 | 61.7 | 60.3 | 0 | 0 | 0 | 10.1 ms | 33 / 36 % | < 8 % total |
| 3840x2160 @ 30 | 40 Mb/s | 41.4 / 44.3 | 41.4 | 30.0 | 0 | 0 | 0 | 11.4 ms | 18 / 19 % | < 8 % total |
| 2560x1440 @ 60 | 40 Mb/s | 41.1 / 45.3 | 41.1 | 60.0 | 0 | 0 | 0 | 4.1 ms | 17 / 18 % | < 8 % total |
| 1920x1080 @ 60 | 25 Mb/s | 25.7 / 28.5 | 25.7 | 60.1 | 0 | 0 | 0 | 3.0 ms | 10 / 11 % | < 8 % total |

A static desktop at the same settings sent only 3–5 Mb/s. It held every frame rate with no loss, and its
latency p95 ranged from 6 to 10 ms.

## What it says

- **4K60 meets the brief on a wired LAN.** 60 Mb/s sustained, with no loss, no keyframe requests and
  10 ms p95 latency. The brief asks for 40–80 Mb/s and < 50 ms. The receive queue peaked at 127 KB against
  the 4 MB socket buffer from S30.
- **Decode cost depends on pixel rate, not content.** 4K60 used a third of the RTX 2080's decode engine.
  An older or integrated GPU is where 4K60 would fail first, so the decode load should be checked before
  4K60 is offered on such a receiver.
- **Latency roughly triples at 4K** (3–4 ms to 10–11 ms). Decode, plus downscaling to a 1080p panel,
  accounts for it.
- **Recommended defaults**, based on these measurements, not the brief's guess:
  1080p60 at 25 Mb/s, 1440p60 at 40 Mb/s, 4K30 at 40 Mb/s, and 4K60 at 60 Mb/s. Each held its target with
  headroom. None of them needed more.

## Limits of this run

- The 4K runs are encoded from a 1440p desktop, scaled up, and shown scaled down on a 1080p panel.
  Capture size aside, the encode, the network and the decode really are 4K. Picture fidelity at 4K was not
  checked.
- Wired LAN only. Wi-Fi was not tested.
- The encoder's GPU load on the sender went unmeasured, because the typeperf counter read 0. The sender's
  CPU stayed under 8 %.
- The first latency sample after a connect reads 100–290 ms. That is measurement warm-up, and it is
  excluded from the p95. The health card should skip it too; this is filed in BUGS.
