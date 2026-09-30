# Anti-cheat audit

Audit date 2026-09-29, branch `fix/r34`. Read-only code audit; nothing was run
against a game. Scope: every place Relay touches another process or
system-wide state while a game may be running.

## Principles Relay follows

1. **Nothing enters the game process.** No DLL injection, no AppInit, no
   in-context hooks, no remote threads, no memory reads or writes, no handles
   with `PROCESS_VM_*`, `PROCESS_DUP_HANDLE` or `PROCESS_ALL_ACCESS`.
2. **The only process access right Relay requests on a game is
   `PROCESS_QUERY_LIMITED_INFORMATION`**, which every anti-cheat whitelists
   (Task Manager, Discord and OBS open it constantly). It is opened, used for
   one image-path query and closed straight away.
3. **Everything else goes through OS brokers**: user32 window queries, the
   audio engine (audiodg) for sessions, loopback and the APO, DWM/WGC for
   capture, the display driver for colour, DDC/CI over the monitor bus.
4. **No input synthesis.** No `SendInput`, `keybd_event`, `mouse_event`,
   `SetCursorPos`. Hotkeys use `RegisterHotKey`, not a low-level hook.
5. **No polling of the game.** Per-game work is event-driven (foreground
   change); the only timers touch window handles or the audio endpoint,
   never the game process.
6. **Kernel: nothing.** The only planned driver is the signed audio-class
   virtual device, which never interacts with games.

## Findings

Risk scale: **none** = no interaction with the game; **low** = the same
access Discord/OBS/Task Manager perform, with no known anti-cheat reacting;
**medium** = known to have triggered a flag or block in at least one
anti-cheat; **high** = would be detected or banned.

| # | Location | What it does | Touches game? | Risk | Reasoning / mitigation |
|---|---|---|---|---|---|
| 1 | `crates/core/src/winloop.rs:237-259` | `SetWinEventHook(EVENT_SYSTEM_FOREGROUND` and `EVENT_SYSTEM_MOVESIZEEND)` with `WINEVENT_OUTOFCONTEXT \| WINEVENT_SKIPOWNPROCESS` | No. Events are posted to Relay's thread; no DLL is mapped into the game | none | Out-of-context hooks load nothing into other processes. Accessibility tools, Discord and screen readers use the same hook. Keep `OUTOFCONTEXT`; a test could assert the flag. |
| 2 | `crates/core/src/winloop.rs:503-521` `describe` | `GetWindowThreadProcessId`, `GetWindowTextW`, `MonitorFromWindow` on the new foreground window | Window only (user32 / win32k) | none | No process handle. `GetWindowTextW` on another process's window reads the cached title from win32k and does not send `WM_GETTEXT` into a hung or protected window. |
| 3 | `crates/core/src/winloop.rs:525-533` `process_image` | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW`, closed at once; runs once per foreground change | Yes: one handle open | low | Kernel anti-cheats (EAC, BattlEye, Vanguard, FACEIT) strip rights from handles opened on the game via `ObRegisterCallbacks`, but they leave `QUERY_LIMITED` alone because the shell, Task Manager and every overlay needs it. Frequency is human-paced (alt-tab), not a loop. With protected games the call may fail; the code already falls back to an empty exe name. Mitigation (optional): if a game is ever reported, take the exe name from the Toolhelp snapshot (no handle) instead. |
| 4 | `crates/core/src/service.rs:756-787` `recheck_focus`, `winloop.rs:92-104` `window_alive` | Runs on the exit-watch tick only while a profile is active; calls `IsWindow` on the stored game HWND | Window handle only | none | `IsWindow` is a win32k handle-table lookup; nothing is opened in the game and it cannot be observed from inside the game. This replaced a per-tick process open, which was the right call. When the window is gone it calls `current_foreground()` once, the same path as #3. |
| 5 | `crates/core/src/winloop.rs:264` | `RegisterHotKey` global hotkeys | No | none | Not a keyboard hook; the OS delivers `WM_HOTKEY` to Relay only. No `WH_KEYBOARD_LL`. Side effect only: a chosen combo is swallowed from the game; keep defaults off common game binds. |
| 6 | whole repo | `SendInput`, `keybd_event`, `mouse_event`, `SetWindowsHookEx`, `ReadProcessMemory`, `WriteProcessMemory`, `CreateRemoteThread`, AppInit | n/a | none | Grep finds none of these in `crates/` or `ui/src-tauri/`. |
| 7 | `crates/core/src/processes.rs:8-56` `list_windowed` | `EnumWindows`, then #3's query-limited open for each windowed process | Yes when the game is running, one open each | low | Only on a user action (exe picker `Method::ListProcesses`), not periodic. Same access as #3. |
| 8 | `crates/core/src/processes.rs:69-90` `list_windowed_and_audible` | Adds audio-session PIDs, then query-limited open | Same as #7 | low | User action only. |
| 9 | `crates/core/src/processes.rs:171-200` `pids_by_name` / `list_all` | `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` | No handle to any process | none | Snapshot is kernel-side; only used by the uninstaller. |
| 10 | `crates/core/src/processes.rs:107-133` `terminate_by_name` | `PostMessage(WM_CLOSE)` + `OpenProcess(PROCESS_TERMINATE)` + `TerminateProcess` | Only `relay-ui.exe` (sole caller `uninstall.rs:511`) | none | Hard-coded to Relay's own UI. Mitigation: keep it private to the uninstaller; never expose an exe name through IPC. A future caller passing a game exe would be high risk (terminate rights are stripped and logged by EAC/BattlEye). |
| 11 | `crates/audio/src/sessions.rs:126-175` `probe_default_render` | `IAudioSessionManager2` enumeration + `GetProcessId`, throwaway shared-mode `IAudioClient::Initialize` | No: audio engine metadata | none | Talks to audiosrv/audiodg, never the game. Runs at 1 Hz only while a profile with audio processing is active (`service.rs:363`, `:791-815`). Volume mixers and Discord do the same. |
| 12 | `crates/capture/src/audio.rs:140-185` | `ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK)` with a target PID (include/exclude tree) | No: audiodg taps the game's mixed stream | low | Documented Windows 10 2004+ API used by OBS 28+ "Application Audio Capture"; no known anti-cheat reacts. Only during a share. |
| 13 | `crates/capture/src/source/wgc.rs:55` | `IGraphicsCaptureItemInterop::CreateForWindow(hwnd)` on the game window | No: DWM composes the frame | low | OBS/Discord use the same path; no injection (unlike OBS "game capture", which injects and is what anti-cheats whitelist by signature). Games that set `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` (some Vanguard / anti-screenshot titles) show black. That is a correct outcome, not a detection; surface it in the UI rather than working around it. Never add a hooking "game capture" fallback. |
| 14 | `crates/capture` DXGI Desktop Duplication | Duplicates the whole output | No | none | Output-level, used by Windows itself. Exclusive-fullscreen games can make duplication lose access; handled by re-create. |
| 15 | `crates/capture/src/render/host.rs:123,416,447,490,578` | `IsWindow` on the receiver's own owner window | No (Relay's UI) | none | |
| 16 | `crates/audio/apo` (relay-apo) | Endpoint APO loaded by audiodg | No; the game never loads it | low | Audiodg is a protected-process-light system process; anti-cheats don't scan it. The DLL must be signed (EV/attestation blocker in ROADMAP) or audiodg will reject it; an unsigned APO is a Windows problem, not an anti-cheat one. Some anti-cheats (Vanguard, FACEIT) have flagged unsigned *drivers*, not user-mode APOs. |
| 17 | `crates/display/src/nvapi.rs:100`, `amd.rs:213` | `LoadLibrary` of nvapi64/atiadlxx into Relay, set vibrance/hue | No | low | Same calls as NVIDIA Control Panel / VibranceGUI. VibranceGUI is widely run with Valorant, CS2 and FACEIT; no bans reported. Relay only calls the public set functions and never "NVAPI injection"/driver-level shader tweaks. |
| 18 | `crates/display/src/gamma.rs:135` | `SetDeviceGammaRamp` | No | low | System-wide ramp, same as f.lux / night light. Some competitive anti-cheats (historically ESEA, CS "mat_monitorgamma" era) considered gamma boosts a visibility edge but none flag the API. Restored on blur. |
| 19 | `crates/display/src/ddc.rs:78` | DDC/CI `SetVCPFeature` over dxva2 | No | none | Goes to the monitor's I2C bus; invisible to software on the PC. Black-equalizer style settings are a monitor feature. |
| 20 | `crates/vdevice` virtual camera | `MFCreateVirtualCamera`, frame-server media source in the Frame Server service | No | none | Only matters on the receiver PC; loaded by FrameServer, not games. |
| 21 | `crates/core/src/firewall.rs` | Reads firewall policy store; one inbound rule for `relay-share.exe` via the elevated helper | No | none | |
| 22 | `crates/core/src/elevate.rs`, `relay-elevate` | Elevated helper writing HKLM (APO FX store, vcam, firewall) | No | low | Runs only on install/uninstall at user request. Vanguard/FACEIT do not react to admin processes; they react to drivers and handles. Mitigation: never run it while a game is up (it doesn't need to), keep the op allow-list. |
| 23 | `crates/core/src/ipc.rs:551`, `processes.rs:218` | `OpenProcessToken(GetCurrentProcess())` | Relay itself | none | |

## Open risks

No medium or high findings. Residual items to watch:

1. **Handle opens on protected games (#3, #7).** Low, but it is the one place
   Relay holds a handle to a game. If any anti-cheat is ever seen logging it,
   switch exe resolution to the Toolhelp snapshot (no handle at all).
2. **Capture-blocking games (#13).** Black frames from `WDA_EXCLUDEFROMCAPTURE`
   must be shown as "this game blocks capture", never worked around.
3. **Signed APO / virtual audio driver (#16).** A test-signed or
   attestation-less driver requires test-signing mode, which Vanguard,
   FACEIT and ESEA refuse to run under. Production users must never be asked
   to enable test signing. The owner has ruled out an EV cert, so the APO and
   driver may never ship to game-playing users; document that.
4. **Gamma / vibrance fairness (#17, #18).** Not detection, but tournament
   rules (FACEIT/ESL) sometimes ban "visibility" tools. Neutral note in UI.
5. **Future code.** Any new `OpenProcess` with more than
   `PROCESS_QUERY_LIMITED_INFORMATION`, any `SetWindowsHookEx`, any
   `SendInput`, or any in-process capture is high risk. Suggested CI grep
   gate over `crates/` and `ui/src-tauri/` for those symbols.

## Test plan

Run on the main Win11 PC with the second PC as receiver. Use accounts the
owner accepts risk on (ideally alts); never use main ranked accounts for the
first pass.

| Game | Anti-cheat |
|---|---|
| Valorant | Vanguard (kernel, boot-start) |
| Fortnite or Apex Legends | Easy Anti-Cheat |
| Rainbow Six Siege or PUBG | BattlEye |
| CS2 (Valve MM) and CS2 via FACEIT client | VAC, FACEIT AC |
| Call of Duty (Warzone/MW) | Ricochet |
| One nProtect GameGuard title (e.g. Blade & Soul NEO) | GameGuard |

For each game, with Relay core running from boot:

1. **Launch with Relay idle.** Anti-cheat starts, game reaches menu, no
   "untrusted software" / "vulnerable driver" prompt.
2. **Profile apply.** A profile keyed to the game exe (EQ + HRTF, GPU
   vibrance, gamma, one DDC setting). Alt-tab in and out 10 times: applies on
   focus, restores on blur, `core.log` shows the exe name resolved (or empty
   if the handle was refused; note which).
3. **Exit watch.** Quit the game while it has focus; profile restores within
   the tick.
4. **Share.** Start a Game preset share (window capture of the game via WGC,
   process-loopback audio of the game) to the second PC for 30 min of play,
   including one match/round online. Receiver sees video and hears audio,
   or black frames with a clear message if the game blocks capture.
5. **Desktop capture.** Same with DXGI whole-output capture.
6. **Hotkeys.** Use each Relay hotkey in game.
7. **Afterwards.** Anti-cheat log/clients show no warnings; no kick during the
   session; account in good standing after 7 days (delayed bans, especially
   VAC/Ricochet).

**Pass:** no launch refusal, no kick or disconnect attributable to Relay, no
anti-cheat warning, no account action within 7 days, and profiles/share behave
as described. Any failure: record the game, anti-cheat version, step, and
`core.log`, and stop testing that title.
