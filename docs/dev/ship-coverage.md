# Pre-ship two-PC coverage queue

Drawn up 2026-09-29 from BUGS.md (Open + build log to r33), ROADMAP.md and
SESSIONS.md. Sender = main PC (Win11), receiver = PC2 (Win10 19045, no HEVC,
no frame-server camera). Both share one Rodecaster: audio rows are tone runs
measured by meter, never listened to. Tick a row with its build and date.

| # | Test | Audio | Closes | Result |
|---|------|-------|--------|--------|
| 1 | Install over the top on both PCs, then a no-code share: identity loaded, trusted, embedded, aus == presented | no | release gate | |
| 2 | Lossy link: receiver `RELAY_TEST_LOSS`, 1440p60 -- keyframe requests sent and honoured, 1 s withholding fires, picture recovers | no | B15 | **Pass r36** 2026-09-29: 2a (every=200) 19 repairs, 0 lost, clean picture; 2b (every=5000,retx, 120 s) 54 losses = 54 requests, recovery median 18 ms, max 91 ms (double losses 82-91 ms). Found and fixed on the way: still-screen repeat ignored keyframe requests (r35), damaged answer waited 500 ms (r36) |
| 3 | Forced receiver decode/render failure -- sender reports `error where=receiver` within 1 s | no | B3 | |
| 4 | 15 min soak at 30 fps -- receiver reads 30.0, latency never negative, no teardown-deadline line | no | B1, B14, B8 | r33 2026-09-29: **B14 pass** (1,789 samples, p50 3.2 ms, max 27, 0 negative, PC2 W32Time stopped); **B8 pass** (teardown 145 ms); B1 open -- capture is change-driven, needs the motion page |
| 5 | Receive banner on the no-HEVC PC (screenshot) | no | B6 | |
| 6 | Multi-source switch mid-share (display, region, window) -- no renegotiation, fps holds | no | multi-source | |
| 7 | Both PCs rebooted mid-share -- each side resumes with no press | no | S38 | |
| 8 | Relay Camera on the sender (S36) read locally while sharing | no | S36 | |
| 9 | Mic track: tone into the mic, meter on the Mic fader, isolation from Game/Rest | tone | mic | |
| 10 | Recording during a two-PC share + replay save; ffprobe both Opus tracks | tone | M6 | |
| 11 | Wi-Fi run: loss, bitrate controller, wired/6E notice | no | risk 5 | |
| 12 | Win10 receiver camera: greyed state explained (no frame-server API) | no | M5 on Win10 | |

Not two-PC: B4 (firewall on a disabled Private profile) is a decision; B12
(reproducible installer) is one-PC; B16's in-engine A/V meter is S39.
