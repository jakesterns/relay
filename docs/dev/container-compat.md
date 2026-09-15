# Recording container compatibility — measured 2026-09-14

Evidence gathered for session S4's Definition of Ready ("a named reason to do
it now: a player or editor that rejects the current Opus-in-fMP4 output").

**Verdict: a real rejection exists, but the cause is not Opus and not
truncation — it is the missing `mfra` index box.**

## Rig
Windows 11 Pro 26200. Installed media consumers: OBS, Windows Media Player,
Films & TV (`Microsoft.ZuneVideo`), Photos, Clipchamp, HEVC/AV1/VP9/MPEG2 video
extensions, `Microsoft.WebMediaExtensions`. No Premiere / Resolve / Vegas
installed, so those remain **untested**.

Probe: `scripts\mf-compat-probe.ps1 -Path <file>`. It exercises three Media
Foundation entry points:

| Row | API | Who uses it |
|---|---|---|
| `EDITOR` | `Windows.Media.Editing.MediaClip.CreateFromFileAsync` | the Windows video-editing API (Photos video editor, UWP/WinUI editors) |
| `PLAYER` | `MediaSource` + `MediaPlaybackItem` track resolution | Films & TV, `MediaPlayerElement` |
| `DECODE` | `MediaTranscoder.PrepareFileTranscodeAsync` | "can MF actually decode both tracks" |

Real Relay-muxer output was produced by
`cargo run -p relay-capture --example mux_from_annexb -- <in.h265> <out.mp4> <w> <h>`,
which drives the production `Mp4Muxer` over a real HEVC annex-B elementary
stream (300 frames, 640x360, keyint 60).

## Results

| File | Shape | EDITOR | PLAYER | DECODE |
|---|---|---|---|---|
| `relay_real.mp4` | **real `Mp4Muxer` output** — `ftyp moov moof×5 mdat×5`, no `mfra` | **REJECTED** "The parameter is incorrect." | opened, HEVC FullySupported | canTranscode=True |
| same + `mfra` appended (ffmpeg remux) | `… moof×5 mdat×5 mfra` | **accepted** | opened | True |
| same as non-fragmented MP4 | `ftyp free mdat moov` | **accepted** | opened | True |
| same as MKV | Matroska | **accepted** | opened | True |
| ffmpeg fMP4, HEVC `hvc1` + **Opus**, with `mfra` | intact | **accepted** | audio subtype=OPUS **FullySupported** | True |
| …same, `mfra` stripped | finalized, unindexed | **REJECTED** | opened | True |
| …same, truncated mid-fragment (crash sim) | partial trailing fragment | **REJECTED** | opened | True |
| …same, truncated at exact fragment boundary | clean fragments, no `mfra` | **REJECTED** | opened | True |
| ffmpeg fMP4, HEVC + **AAC**, with `mfra` (control) | intact | accepted | audio subtype=AAC FullySupported | True |
| MKV truncated mid-file | partial | **accepted**, full duration | opened | True |

## What this rules in and out

- **Opus-in-MP4 is a non-issue on this stack.** The hypothesis recorded in
  M6's Deferred note is dead: MF reports `OPUS … decoderStatus=FullySupported`
  and imports it into the editor happily, as long as `mfra` is present. The
  AAC control behaves identically.
- **Truncation is not the trigger either.** A cleanly *finalized* Relay file is
  rejected exactly like a truncated one. `Mp4Muxer::finalize` flushes the last
  fragment but writes no `mfra` (`crates/capture/src/record/mux.rs:173`), so
  **every** Relay recording — crashed or not — is un-importable into the
  Windows video-editing API today.
- **ffprobe / VLC-class tools were never the problem** and still are not; every
  variant above probes clean. M6's "every player tested" claim was true as far
  as it went — it just never tested an editor import path.
- A crash-truncated fMP4 still yields its completed fragments to ffmpeg (4.0 s
  of 4.03 s recovered, only the partial trailing fragment errors), so decision
  2's crash-safety claim holds for *playback*. It does not hold for *editing*.

## Consequence for S4

Two separable fixes, and they are not the same size:

1. **Write `mfra` on `finalize`.** Small, stays inside the existing muxer, and
   fixes the clean-stop case — which is the overwhelmingly common one. This
   alone makes normal Relay recordings importable.
2. **MKV as a per-preset container.** Covers the case (1) cannot: a crash means
   `finalize` never runs, so no `mfra` is ever written and the file stays
   un-importable. This is precisely the reason OBS defaults to MKV, and it is
   now demonstrated rather than assumed.

## Not yet verified
- Premiere Pro / DaVinci Resolve / Vegas — not installed on this rig.
- Clipchamp: it ships its own media stack rather than going through
  `MediaClip`, so the `EDITOR` row above does **not** describe it.
- Whether an `mfra`-bearing Relay file survives the *other* editors above.
