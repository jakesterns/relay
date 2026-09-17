# Porting Relay: the platform seam, and what macOS needs

Written 2026-09-16 (S28). **Relay compiles for macOS. It has never run on
macOS.** There is no Mac on this network, so everything below about macOS is
from the documented APIs, not from running code. The difficulty ratings are
estimates to plan by, not measurements.

## Where things stand

| | Windows | macOS (aarch64 + x86_64) |
|---|---|---|
| `cargo clippy --workspace --all-targets -D warnings` | green | green, cross-checked from Windows and in CI |
| `cargo test --workspace` | green | **never run** |
| Links / produces a binary | yes | **not attempted** (needs the Apple SDK) |
| Does anything | yes | every OS-facing capability reports "not supported on this platform" |

Every crate in the workspace builds for both Apple targets, so no crate is
blocked on a missing API at compile time. What is missing is behaviour: on
macOS each capability below is a stub.

### How to check it

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check-macos.ps1
```

This runs clippy for both Apple targets from Windows. Only two build scripts
compile C for those targets: `ring` (webrtc-rs's DTLS) and
`objc2-exception-helper` (Tauri). They need a C compiler that knows the macOS
headers. The script installs zig with pip (no admin rights) and points `cc-rs`
at `scripts/apple-cc`, a small shim that turns clang's target flags into
zig's. Nothing is linked, so no Xcode or SDK is needed.

In CI:

- **`macos-cross`** runs on every push and PR. It does the same cross-check from
  a Linux runner, then runs the platform seam test and the stub tests on Linux,
  the first non-Windows OS any Relay code has actually run on.
- **`macos-native`** runs only when started by hand (`workflow_dispatch`). It
  builds and tests on a real `macos-latest` runner. It is manual because
  macOS minutes on a private repo cost several times as much as Linux minutes.
  Check the current rate on GitHub's billing page before putting it on every
  push. Its full `cargo test --workspace` step reports without failing the job:
  some tests use Windows path literals (`C:\…`) and are expected to fail until
  they are made path-neutral.

## The seam

Two rules, both enforced:

1. **Every `windows` crate call is inside a `#[cfg(windows)]` module.** That
   means a file gated at its `mod` line, or an inline `#[cfg(windows)] mod imp
   { … }`. A `#[cfg(windows)]` on a single function or statement does not
   count. `crates/core/tests/platform_seam.rs` scans the workspace and fails
   on any use outside such a module. It runs on every OS.
2. **Every platform module has a `#[cfg(not(windows))]` twin with the same
   signatures.** An *action* (install, apply, spawn, fetch, execute) returns
   `relay_core::platform::unsupported(Capability::…)`, whose message is
   "<capability> is not supported on this platform (macos) yet". A *read* may
   give the honest empty answer ("no processes", "autostart off",
   "link kind unknown"). A stub never reports success for an action it did
   not do.

`relay_core::platform::Capability` is the list of units a port lands in, and
`platform::supported(cap)` is what a status screen would ask. Callers use the
same path on every OS (`crate::audio_apo::install_live`,
`crate::ipc::client::Client::connect`). The `cfg` choice is made once, inside
the module.

The four trait seams that already existed are unchanged: `AudioControl`,
`DisplayControl`, `HardwareProbe` and `FrameSource`. On macOS the service wires
`ApoAudioControl` (a stub), `display_backend::UnsupportedDisplay` and
`NoopHardwareProbe`.

What is portable today and runs anywhere: profile model and selection,
backup/restore bookkeeping, the `Applier`, DSP (`relay-audio::dsp`), headset
curve fitting, the AutoEQ index, presets, the uninstall *planner*, the APO FX
store model, the camera registration planner, vendor colour mappings
(`relay_display::{nvapi, amd}` unit curves), gamma-ramp maths, VCP quirks,
the WebRTC transport (pairing, mDNS discovery, DTLS-SRTP, depacketizing, SEI,
loss window), recording muxers (MP4/MKV) and the replay ring, audio
resampling, and the UI.

## Capability by capability

Difficulty is the effort to reach Windows parity on macOS:
**Easy** means a known API and a direct translation. **Medium** means a known API
with real design or permission work. **Hard** means new architecture, signing
entitlements, or private APIs. **No clean answer** means we cannot promise parity.

### Core service

| Capability | Windows today | macOS equivalent | Difficulty |
|---|---|---|---|
| `Ipc` | Named pipe `\\.\pipe\relay-core`, DACL = current user, remote clients rejected (`ipc.rs`) | Unix domain socket in `~/Library/Application Support/Relay`, mode 0600, peer uid checked with `getpeereid`. Same newline-delimited JSON. | Easy |
| single instance | Named mutex `Local\RelayCore` (`instance.rs`) | `flock` on a lock file in the data root. The stub currently always acquires. | Easy |
| `FocusWatch` | `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`, `RegisterHotKey`, `WTSRegisterSessionNotification` on one message-loop thread (`winloop.rs`) | `NSWorkspace.didActivateApplicationNotification`, pid → path via `NSRunningApplication.executableURL`. Hotkeys: Carbon `RegisterEventHotKey`, which needs no permission; a `CGEventTap` would need Input Monitoring. Lock: `com.apple.screenIsLocked` distributed notification. Needs a CFRunLoop thread. Which display a window is on: `CGWindowListCopyWindowInfo` bounds. Window *titles* need Screen Recording permission. | Medium |
| `Tray` | `Shell_NotifyIconW` on the winloop's hidden window (`tray.rs`) | `NSStatusItem`. Must live on the main thread's run loop in the core process, which also decides how `FocusWatch` is threaded. | Medium |
| `Autostart` | `HKCU\…\Run\Relay` (`autostart.rs`) | A LaunchAgent plist in `~/Library/LaunchAgents`, or `SMAppService.mainApp` (macOS 13+, needs a signed app bundle). | Easy |
| `Launcher` | `relay-svc.exe` (GUI subsystem) spawns the core with `CREATE_NO_WINDOW` (`launcher.rs`) | No console flash to avoid: launchd starts the core. Opening the UI is `NSWorkspace.openApplication`. | Easy |
| `Processes` | `EnumWindows`, Toolhelp snapshot, `TerminateProcess`, token elevation (`processes.rs`) | `NSWorkspace.runningApplications` (apps with UI) or `proc_listpids` + `proc_pidpath`; `kill`. "Elevated" has no meaning for a per-user app. | Easy |
| footprint readout | `K32GetProcessMemoryInfo`, `GetProcessTimes`, `K32EmptyWorkingSet` (`footprint.rs`) | `task_info(MACH_TASK_BASIC_INFO)` / `TASK_VM_INFO.phys_footprint`, `getrusage`. There is no working-set trim; macOS reclaims pages itself. **Re-baseline the 10 MB budget:** RSS and phys_footprint are not Windows' working set. | Easy (the budget: Medium) |
| `CatalogFetch` | WinHTTP (`hardware/catalog.rs`), chosen for the 10 MB budget | `NSURLSession` through objc2-foundation, which keeps the same no-TLS-stack-in-binary property. | Easy |
| `Elevation` | `relay-elevate.exe` via `ShellExecute runas`, the only HKLM writer (`elevate.rs`) | No UAC. The only privileged install on macOS would be an audio plug-in in `/Library/Audio/Plug-Ins/HAL`. That belongs in a signed `.pkg` installer, not a runtime helper (`SMJobBless` is deprecated). | Medium |
| `Firewall` | One inbound rule for `relay-share.exe` through `INetFwPolicy2` (`firewall.rs`) | Nothing to write. The Application Firewall asks once per *signed* app and remembers. An unsigned binary is asked again on every change. So the fix is code signing and notarization, not code. | Easy (in code); requires paid Apple Developer Program |
| `Uninstall` | NSIS uninstaller from Add/Remove Programs + the planner (`uninstall.rs`) | The planner carries over. Execution: remove the LaunchAgent, any installed plug-in or extension, and the data folder. The app bundle goes to the Trash. | Medium |

### `HardwareProbe`

| Windows today | macOS equivalent | Difficulty |
|---|---|---|
| Audio endpoints: `IMMDeviceEnumerator` + property store; change notifications via `IMMNotificationClient` (`hardware/probe_win.rs`, `watch_win.rs`) | CoreAudio `AudioObjectGetPropertyData(kAudioHardwarePropertyDevices)`, transport type and name per device; `AudioObjectAddPropertyListener` for changes | Easy |
| Monitors: `QueryDisplayConfig`, `DisplayConfigGetDeviceInfo`, `EnumDisplayMonitors`, EDID from the registry/SetupAPI | `CGGetOnlineDisplayList`, `CGDisplayVendorNumber`/`ModelNumber`/`SerialNumber`; `CGDisplayRegisterReconfigurationCallback`. Raw EDID bytes: in the IORegistry, but on Apple Silicon the location is not a documented API. | Medium (EDID on Apple Silicon: Hard) |
| DDC/CI capability string: `GetCapabilitiesStringLength` (dxva2) | See DDC/CI below | No clean answer |

### `DisplayControl`

Three parts that port very differently.

| Part | Windows today | macOS equivalent | Difficulty |
|---|---|---|---|
| Gamma ramp (gamma, contrast, shadow lift) | `SetDeviceGammaRamp` on the monitor's DC (`relay_display::gamma::io`) | `CGSetDisplayTransferByTable` / `CGGetDisplayTransferByTable` per display. The ramp maths is already portable. macOS resets a table when the process that set it exits. That makes crash restore free, but it means the always-on core, not a helper, must own the apply. | Easy |
| GPU vibrance and hue | NvAPI `SetDVCLevel`/`SetHUEAngle`, AMD ADL `ADL2_Display_Color_Set` (`nvapi/ffi.rs`, `amd/ffi.rs`) | **None.** Apple GPUs expose no saturation control, and current macOS has no NVIDIA driver. A per-channel transfer table cannot express saturation. The profile fields have to report "unavailable on this Mac", as they already do on a GPU without a vendor driver. | No clean answer |
| DDC/CI monitor controls | `GetPhysicalMonitorsFromHMONITOR` + `SetVCPFeature` over dxva2, with retries and per-model write delays (`relay_display::ddc`) | Intel Macs: IOKit `IOFBCopyI2CInterfaceForBus` + `IOI2CSendRequest`. Documented but deprecated. **Apple Silicon: no public API.** Tools like MonitorControl and m1ddc drive I2C through private `IOAVService*` calls. Those are undocumented and have changed between releases, and some ports (the built-in HDMI on several M1 models) do not pass DDC at all. The VCP planner, quirks table and `VerifiedCode` gate carry over unchanged; the transport does not. | No clean answer |

The honest plan for DDC/CI on macOS is to decide whether shipping on a private
API is acceptable. If it is not, monitor controls stay "unavailable on this
Mac" and gamma-ramp shaping is the whole display story.

### `AudioProcessing` — a different architecture, not a port

On Windows, Relay's EQ/HRTF/limiter runs *inside the audio engine* as an
endpoint APO (`relay-apo`, a COM DLL that `audiodg.exe` loads, registered in the
endpoint's FX property store). It processes one endpoint without changing the
default device, which is what lets Relay "never touch global config".

**macOS has no equivalent insertion point.** Nothing third-party can insert an
effect into another device's signal path. The options are new designs:

1. **AudioServerPlugIn** (a HAL plug-in in `/Library/Audio/Plug-Ins/HAL`,
   loaded by `coreaudiod`). It creates a *virtual output device* that runs the
   DSP and forwards to the real headset. That is how every macOS system-wide EQ
   works. Cost: admin-rights install, a `coreaudiod` restart, and a
   forwarding path whose clock drift and latency we own. It also only processes
   audio sent to *that* device, so applying a profile means routing the game to
   it. Per-app routing of that kind is not something macOS lets a third party
   do for another app, and switching the system default device breaks the
   "never touch global config" rule. That tension has to be designed around,
   not ported.
2. **Core Audio process taps** (`AudioHardwareCreateProcessTap`, macOS 14.2+).
   These capture one process's output, can mute the original, and let Relay
   process it and play it to the headset. This is per-app and does not change
   the default device, which fits the brief better. Cost: the "System Audio
   Recording" permission prompt, an extra hop of latency, and a user-space
   real-time path we have to keep glitch-free.

The DSP itself (`relay_audio::dsp`, parameters in `relay_audio::params`) is
portable and is what either design would host. The shared-memory parameter
block (`relay_audio::shm`) would become a POSIX shared-memory object, or an
XPC property for a plug-in. Rating: **Hard**, with a design decision before any
code.

`ExclusiveModeDetect` has a simple counterpart: macOS "hog mode"
(`kAudioDevicePropertyHogMode`, which names the owning pid). **Easy.**

### `VirtualCamera` and the virtual microphone

| Windows today | macOS equivalent | Difficulty |
|---|---|---|
| Frame Server media source DLL registered in HKLM, started with `MFCreateVirtualCamera`; frames through a named shared-memory NV12 ring (`relay-vdevice`) | **CoreMediaIO Camera Extension** (macOS 12.3+). A system extension inside the app bundle. The user approves it in System Settings. It needs a Developer ID signature with the system-extension entitlement. Frames: the receiver feeds the extension's sink stream, or an IOSurface-backed shared buffer. The ring's seqlock layout carries over as a design, not as code. | Hard (entitlement and signing, not the API) |
| Interim mic: render into an installed VB-Cable / VoiceMeeter input | Render into an installed BlackHole device. Same "install nothing ourselves" model; detection is a CoreAudio device-name match, the same idea as `relay_vdevice::detect::mic_kind`. | Easy |
| Signed virtual mic driver (deferred, EV cert) | An AudioServerPlugIn virtual input device, the same plug-in machinery as option 1 above | Hard |

### `Share` — capture, encode, transport, present

| Stage | Windows today | macOS equivalent | Difficulty |
|---|---|---|---|
| Screen capture (`FrameSource`) | Windows.Graphics.Capture, DXGI Desktop Duplication fallback (`source/`) | **ScreenCaptureKit** `SCStream` (macOS 12.3+), IOSurface-backed `CVPixelBuffer`s, cursor on/off, per-display or per-window. Needs Screen Recording permission. | Medium |
| Desktop / per-process audio | WASAPI loopback and process loopback (`audio.rs`) | ScreenCaptureKit audio (system mix and per-app, macOS 13+), or Core Audio process taps | Medium |
| Microphone | WASAPI capture | `AVCaptureDevice` / CoreAudio input; Microphone permission | Easy |
| Hardware encode | Media Foundation hardware MFT, D3D11 NV12 surfaces (`encode/`) | **VideoToolbox** `VTCompressionSession`, hardware HEVC and H.264 on Apple Silicon, fed IOSurface buffers with no copy. Low-latency rate control has to be verified per codec on real hardware before the <50 ms budget can be claimed. | Medium |
| Transport | webrtc-rs, mDNS, pairing, DTLS-SRTP (`transport/`) | **Already portable** and compiles for macOS. `netcheck::link_kind_for` reports `Unknown` until it reads `getifaddrs` + `SCNetworkInterfaceGetInterfaceType`. | Easy |
| Decode + present | MF hardware decoder + D3D11 swapchain window (`decode/`, `render.rs`) | `VTDecompressionSession` + `CAMetalLayer`, or `AVSampleBufferDisplayLayer`, which decodes and presents in one step | Medium |
| Audio playback | WASAPI shared mode (`playback.rs`) | CoreAudio `AudioUnit` (`kAudioUnitSubType_HALOutput`) | Easy |
| Opus | `opus` crate, a Windows-only dependency today | Same crate; builds with CMake on macOS. Move the dependency out of `[target.'cfg(windows)']`. | Easy |
| Recording | Muxers portable; `GetLocalTime`, `GetDiskFreeSpaceExW` (`record/mod.rs`) | `localtime_r`, `statvfs`. The stub names files in UTC and skips the free-space floor. | Easy |
| Preview tap | WIC JPEG encode from a staging texture (`preview.rs`) | ImageIO `CGImageDestination` from the pixel buffer | Easy |
| Capability probe | `MFTEnumEx`, WGC `IsSupported` (`probe.rs`) | `VTCopyVideoEncoderList`, `VTIsHardwareDecodeSupported`, `SCShareableContent` | Easy |

### UI shell (`ui/src-tauri`)

Tauri 2 supports macOS, and the React UI carries over unchanged. On macOS today
the IPC client stub makes every command fail as "core not running". The
window-state stub opens the window centred every time; the port reads
`outer_position` / `outer_size`. The custom title bar needs the traffic-light
inset. Packaging changes from NSIS to `.app` + `.dmg` (or `.pkg` if a
plug-in is installed).

## Not code, and not free

- **Apple Developer Program** (paid, yearly) is needed to sign and notarize.
  Without it, Gatekeeper blocks the app. System extensions (the camera) cannot
  load, and the firewall prompt repeats. "Free for every user" is still true
  for users; shipping costs this.
- **Permissions the user will be asked for**: Screen Recording (capture, and
  window titles for focus matching), Microphone, System Audio Recording (if
  process taps are used), and approval of the camera system extension. Each
  needs a plain-English "why" on the first-run screen, the same way the two
  Windows opt-ins do today.
- **Anti-cheat constraint**: unchanged. Everything above is OS-layer. No
  injection, and nothing reads another process's memory.
