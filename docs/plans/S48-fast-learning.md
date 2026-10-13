# S48 — Fast learning (live and from gameplay video)

Branch `feat/s48-fast-learning`, cut from `origin/feat/s46-learned-game-eq`
(PR #8, 50167bb). PR goes into that branch.

**Owner's priority.** Per-game EQ and display learning is core; it must take
much less time — from live background play and from gameplay footage —
without weakening any safety rule.

## 1. Faster live learning

### Audio (`crates/audio/src/learn/state.rs`)

| Constant | S46 | S48 |
|---|---|---|
| `CHECKPOINT_SECS` | 60 | **30** |
| `CONVERGE_CHECKPOINTS` (within `CONVERGE_DB` 0.5 dB, every band) | 3 | **2** |
| `MIN_ACTIVE_SECS` | 600 | **300** |
| Target events needed | 120 (raw count) | **40–120, by per-band standard error** |
| Masker events needed | 60 (raw count) | **20–60, by per-band standard error** |

The evidence minimum is no longer a raw count. For each goal group (targets,
maskers) and each class that is at least `CLASS_SHARE_MIN` = 10 % of the
group, every band the class lives in (within `SE_BAND_RANGE_DB` = 12 dB of its
loudest band) must have its median level known within `MEDIAN_SE_DB` = 2 dB
(one histogram bin): `SE = MEDIAN_SE_FACTOR (1.2533) × sd / √n`, with `sd`
from the band's interquartile range (`IQR_PER_SD` 1.349). The group needs
`n / share` events at the mix heard so far. Stationary classes (voice, music,
vehicle, ambience) keep a histogram entry per frame, so their independent
samples are `STATIONARY_SAMPLE_FRAMES` = 10 frames (100 ms) apart. The
result is clamped to `MIN_CUES_FLOOR` 40 … `MIN_CUES` 120 (targets) and
`MIN_MASKERS_FLOOR` 20 … `MIN_MASKERS` 60 (maskers): never more than S46
asked for, never fewer than a third of it. With no events at all the ceiling
applies. Classes are never pooled (two classes at different levels are not
"spread").

**ETA** (`LearnRecord::eta_secs`): the slowest of the remaining active time,
the remaining target and masker events at the observed rate, and the
agreeing checkpoints still due. Unknown (`None`) under
`ETA_MIN_ACTIVE_SECS` = 60 s of play or while a group has no events; the UI
says "time left not known yet" then, "about N min left" (rounded up)
otherwise. Status now reports the *scaled* requirement as `min_targets` /
`min_maskers`.

### Look (`crates/display/src/learn/converge.rs`)

| Constant | S47 | S48 |
|---|---|---|
| `SAMPLE_FPS` (live and file) | 1 | **2** |
| `MIN_GAMEPLAY_FRAMES` | 600 (10 min) | 600 (**5 min**) |
| `CHECKPOINT_FRAMES` | 120 (2 min) | **60 (30 s)** |
| `CONVERGE_CHECKPOINTS` | 3 | **2** |
| `MIN_FRAMES_ONE_SCENE` | 900 (15 min) | 900 (7.5 min) |
| `MIN_FRAMES_ONE_SCENE_CONFIDENT` | — | **480 (4 min)** |
| `ROLLING_WINDOW_FRAMES` | 3600 (1 h) | 7200 (1 h) |

The dark-only (one-scene) path takes the short budget only when its
statistics are tight: the standard error of every look axis (shadow,
saturation, highlight, in look units) is under `CONFIDENT_SE` = 0.0125, a
quarter of the 0.05 rounding step, with samples `DECORRELATION_FRAMES` = 4
(2 s) apart counted as one. The aggregate now keeps squared sums for that
(`crush_sq`, `sat_sq`, `clip_sq`; absent in older records, which are then
never "confident"). Agreement tolerances, scenes, the freeze rule and
`MEANINGFUL_CHANGE` are unchanged. Readiness reports `eta_secs` (`None` while
a kind of scene is missing — no amount of the same scene supplies it) and
`confident`.

### Results (synthetic replays, `cargo test --release`)

| Replay | S46/S47 constants | S48 |
|---|---|---|
| r51-like audio (Warzone rates: ~10 steps/min, callouts, gunfire) | 10.0 min | **5.0 min** |
| r53-like audio (streamer commentary half of every minute) | 10.0 min | **5.0 min** |
| Typical audio (a step every 1.5 s, a boom every 8 s) | 9 min | **5 min** |
| Steady varied look (three scenes) | ≥ 600 s | **300 s** |
| Tight night-only look | ≥ 900 s | **240 s** |
| Noisy night-only look | ≥ 900 s | 450 s (long budget, not the confident one) |

The r51 replay's ETA at 2 minutes in was within 90 s of the real time to
ready (test). The hostile cases still hold: a mix that never converges never
offers however much evidence it has; a wide level spread needs the full S46
count; malformed aggregates are never "confident"; every S46 safety guard
(caps, speech guard, hearing rule, freeze, `UPDATE_DB`) is untouched.

**Sampler cost at 2 fps.** Analysis of one 480×270 sample: 3.06 ms → 0.61 %
of one core at 2 fps (release, this PC; asserted < 1 %). The GPU blt and
~190 KB readback are not CPU work. A live run on a static desktop produced
no samples (Desktop Duplication only hands out changed frames), so the
in-game number is for the live pass below.

## 2. Learn from a gameplay video

Games ▸ Audio and Games ▸ Display carry **"Learn faster: use a recording"**:
"Learn from a video file…" lists Relay's own recordings (the recording
folder) first, then the user's Videos folder (top level), and "Choose
another file…" opens the Windows Open dialog (mp4 / mkv / mov / webm). The
card explains what is analysed, says **local files only — no downloading from
YouTube or other sites**, and that the file is never copied, uploaded or
changed.

```
UI ── LearnFromFile{id, path} ──▶ core: validate (local, existing, video ext; no URL / UNC)
                                   stop this game's live learner; spawn ↓ (BELOW_NORMAL)
relay-share learn-file --file F --exe E --record game-eq\E.json --goal G
   holds the record lock; MF source reader (read-only)
   ├─ audio: Float at the file's rate (never resampled) → S46 Analyzer;
   │         1 s of content → absorb + checkpoint, as live play
   ├─ video: NV12, DXVA when available, in up to 4 parallel stretches
   │         (seekable sources); a frame every 0.5 s of content → 480×270
   │         → S47 Analyser; reports replayed in time order into a Learner
   └─ stdout: progress (pos, duration, speed) … note … look_result … done
core file_tick: merge the look into every connected monitor's record
   (Learner::merge), learning left on for both halves, auto-apply honoured,
   re-apply if a profile is active
```

No input-idle signal exists for a file; the content classifiers still apply
(letterbox → cutscene, loading, static menus, outliers; music; overlay voice
/ commentary; player chat). Cancel (`stop` on stdin, or stdin closing) leaves
**without saving anything**; the record is only written after a finished
run. A file whose picture cannot be decoded (an HEVC file without the HEVC
Video Extensions) still teaches the sound, and says so; one whose sound
cannot be decoded still teaches the look.

### Combining

Audio: the file's seconds are absorbed into the same per-exe record and
rolling window as live play, so file and live evidence add, weighted by how
much of each there is, and checkpoints/convergence run on the combined
window under the same rules. Look: `Learner::merge` adds the aggregates
(weighted by frames, scaled into the rolling window); an empty record takes
the file's checkpoints (a file that settled is settled), otherwise one
checkpoint of the merged aggregate is taken and must agree with the live
ones before it — a file never overrides live evidence without agreeing.
Nothing is applied by a merge; the usual offer / auto-apply flow decides.

### Speed and CPU on this PC (main Win11 PC, release build)

Synthetic clips written by `learn_file::testclip` (Windows' software H.264
encoder MFT + Opus, through Relay's own fMP4 and MKV muxers):

| Clip | Decode | Speed | Helper CPU |
|---|---|---|---|
| 1080p60, 120 s, fMP4 (seekable: 4 stretches) | GPU (DXVA) | **13.2× real time** | 6.2 s total (68 % of one core avg) |
| 1080p60, 120 s, fMP4 | CPU (forced) | 22.5× | 106 s (~20 cores busy) |
| 1080p60, 120 s, MKV (no cue index: one stretch) | GPU | 6.9× | 5.3 s (30 %) |
| 1080p60, 120 s, MKV | CPU (forced) | 10.5× | 81 s (~7 cores) |
| 720p60, 90 s, fMP4, `relay-share learn-file` end to end | GPU | 19.7× | 4.1 s |

One DXVA session is bound by its per-frame round trip (~400 fps at 1080p),
so seekable files are decoded in up to `VIDEO_SEGMENTS` = 4 stretches of at
least `MIN_SEGMENT_SECS` = 30 s side by side. Relay's MKV recordings carry
no cue index, so Media Foundation cannot seek them and they decode in one
stretch (~7× at 1080p60); adding cues to the MKV muxer would bring them to
the fMP4 number (follow-up). The GPU path is preferred over the CPU even
where the CPU is faster: the CPU path occupies most of the cores, which is
wrong for a background job on a gaming PC.

### Footprint

The core links only the pure merge/derivation code and a reader thread
while a job runs; the helper exists only during a job and is killed by its
own handle on cancel, exit or core shutdown. Release footprint gate on this
branch (`scripts/footprint.ps1`): relay-core.exe 2.56 MB, idle RSS 8.54 MB,
private working set 1.05 MB, idle CPU 0 % over 31 s — **PASS**.

## Where the code lives

| Piece | File |
|---|---|
| Audio thresholds, SE-scaled evidence, ETA | `crates/audio/src/learn/state.rs` |
| Look constants, confidence, ETA, merge | `crates/display/src/learn/{converge,derive}.rs` |
| File helper, decode, sampling, test clips | `crates/capture/src/learn_file.rs`, `learn_file/testclip.rs` |
| Core job, path checks, video list, look merge | `crates/core/src/learn_file.rs`, `service.rs` (`file_tick`, `LearnFromFile` …) |
| IPC | `ipc.rs` `ListLearnVideos` / `LearnFromFile` / `LearnFileStatus` / `LearnFileCancel`; `ui/src/lib/ipc.ts` |
| Shell | `ui/src-tauri/src/lib.rs` (`learn_*`, `pick_video_file`: IFileOpenDialog) |
| UI | `ui/src/components/LearnFromVideoCard.tsx`, ETA in `Games.tsx` and `LearnLookCard.tsx`, `ui/src/lib/eta.ts` |

## Tests

- relay-audio: named constants; r51-like and r53-like replays reach ready in
  ≤ 0.65 × the S46 time; ETA accuracy and countdown; ETA waits for the
  minimum and for settling; the SE rule (tight → floor, wide → ceiling, no
  events → ceiling, malformed → none); an unsettled mix never offers;
  file + live evidence add in one record.
- relay-display: constants; steady varied ≤ 55 % of the S47 minimum; tight vs
  noisy dark-only budgets; confidence needs squared sums (old records are
  never confident); ETA countdown and `None` without the scenes; merge
  weighted by frames, merge into an empty record keeps the file's
  convergence, merge into live evidence needs agreement; merge stays inside
  the rolling window; analysis cost at 2 fps < 1 % of a core.
- relay-capture (`learn_file`): a generated fMP4 clip and MKV clip decode
  (audio seconds, frames every 0.5 s, file unchanged), faster than real
  time; a long clip split into stretches is sampled whole in both
  containers; cancel keeps nothing; a non-video file fails with a sentence;
  argument parsing; the half-second sampling grid; NV12 shrink keeps levels;
  ignored 1080p60 speed bench.
- relay-core: path validation (URL, YouTube, UNC, relative, wrong
  extension, empty, missing); Relay recordings listed first; helper wire
  lines → status (incl. unknown duration → no percentage); look merge into
  every listed monitor (offered, not applied); helper arguments; IPC wire
  shape; game EQ status ETA.
- UI: `LearnFromVideoCard.test.tsx` (what is analysed, local only, list
  order, pick, native dialog, closed dialog, progress + speed, done summary,
  cancel, refusal, no profile, ETA text); Games S46 ETA (known and unknown)
  and the video card beside it; LearnLookCard ETA.

## Two-PC test plan (owner present)

Main Win11 PC (learner) and the Win10 second PC (PC2). No reboots on the
main PC; UAC prompts wait for the owner.

**A. Learn from a recording made on PC2.**
1. On PC2, share a game to the main PC with recording on (fMP4, the
   default), 15 min of real play with menus and a cutscene. Copy the file
   to the main PC's `Videos\Relay` folder.
2. Main PC: Games ▸ Audio for that game's profile, goal Awareness.
   "Learn from a video file…": the copied recording is listed first, tagged
   Relay recording. Pick it.
3. Record: speed shown on the card, wall time, `relay-share.exe` CPU in Task
   Manager (target ≥ 10× for fMP4, ≤ 1 core average). Cancel once at ~30 %:
   "Nothing from the file was kept"; `game-eq\<exe>.json` unchanged
   (timestamp). Start again and let it finish.
4. Done line: sound minutes and frames analysed; the Audio card shows Ready
   (or Applied with auto-apply), the Display card shows a settled look or
   progress with a believable ETA. Nothing on screen or in the audio
   changed until Apply.
5. Repeat with an MKV recording (expect ~7× at 1080p60) and with an HEVC
   recording on a PC without HEVC Video Extensions (expect "only its sound
   was learned").
6. Then play the game live 5 min: progress continues from the file-learned
   start (no reset), and the offer does not jump (file and live agree) or,
   if it does, only after two agreeing live checkpoints.

**B. Live learning time on YouTube footage.** Play a gameplay video from
YouTube *in a browser* on the main PC as the "game" (profile for the
browser exe, learning on; Relay never downloads it — it only hears and
sees what is on screen through the usual live path). Keep a hand on the
mouse now and then so input-idle does not gate it.
1. Note the ETA at 1, 2 and 3 minutes; note the minute the Audio card says
   Ready and the minute the look settles.
2. Targets: audio ready in ~5–8 min of active footage (S46: 10–20), look
   settled in ~5 min for varied footage, ~4 min for a steady night scene.
3. Compare the ETA at 2 min with the real time to ready (expect within
   ~1.5 min).
4. Look sampler cost at 2 fps: `relay-share.exe` CPU with the footage
   playing (target < 1 % of a core; analysis alone measured 0.61 %).

Record results here under "Live results".

## Live results

Pending (owner's two-PC pass).
