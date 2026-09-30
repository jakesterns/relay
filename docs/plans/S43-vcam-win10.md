# S43 — Relay Camera on Windows 10

Branch `feat/s43-vcam-win10` (cut from `origin/fix/r34`). Goal: "Relay Camera"
works on the Win10 19045 test PC, which today says "Needs Windows 11 22H2+".

## Design

Two camera paths, one frame ring, one DLL (`relay_vdevice.dll`).

| | Windows 11 22H2+ (unchanged) | Windows 10 / anything without the API (new) |
|---|---|---|
| Picked by | `detect::camera_path()` — `MFCreateVirtualCamera` export present | export missing |
| What apps load | Frame Server hosts the media source (CLSID `{9B7E62D4-…}`) | The app loads a DirectShow source filter (CLSID `{5E0B7C1F-8A34-4D62-9B1E-C47A2F90D835}`) in its own process |
| Registration | 2 keys under HKLM, via the elevated helper | 3 keys under **HKCU**\Software\Classes, no elevation |
| Ring | `Global\Relay.Cam` | `Local\Relay.Cam` (same session as the app; no privilege) |
| `installed.json` component | `camera-media-source` (`hklm_keys`) | `camera-dshow-filter` (`hkcu_keys`) |

The approach is the OBS VirtualCam one: a user-mode COM DLL, no driver, no
driver signing. It works only in apps that enumerate webcams through
DirectShow (`ICreateDevEnum` / `CLSID_VideoInputDeviceCategory`), which is
Zoom, Discord, Teams and Chromium (Chrome/Edge merge DirectShow-only devices
into their Media Foundation list). **The Windows Camera app is Media Foundation
only and will not list it.** That limit is built into this approach.

### The filter (`crates/vdevice/src/camera/dshow.rs`)
- `Filter`: `IBaseFilter` (+`IMediaFilter`, `IPersist`), `IAMFilterMiscFlags`
  (`AM_FILTER_MISC_FLAGS_IS_SOURCE`).
- `Pin` (one output pin, id "Capture"): `IPin`, `IAMStreamConfig`,
  `IKsPropertySet` (`AMPROPERTY_PIN_CATEGORY` → `PIN_CATEGORY_CAPTURE`).
- Formats: NV12, YUY2, RGB24 (bottom-up BGR, BT.709 limited) at the
  producer's size (announced in the ring header), plus 1080p/720p/360p.
  `SetFormat` sizes are honoured; a stream of another size is scaled with
  nearest-neighbour sampling (`camera/picture.rs`).
- Delivery: one worker thread per run: `GetBuffer` → newest ring frame →
  convert → `Receive`. Paced to the negotiated fps. Timestamps are stream time
  from the run start and always increase. The app's threads never touch the
  ring. Paused state delivers one preroll frame and then holds.
- No producer, or a producer silent for 2 s: the "RELAY CAMERA / WAITING FOR A
  STREAM" still, drawn at the negotiated size. An app never gets an error or a
  frozen frame.
- Lifetime: the filter holds its pin, and the pin holds only a `Weak` pointer
  back, so there is no reference cycle.
- Allocator: the downstream pin's own, else `CLSID_MemoryAllocator` (quartz).

### Frame ring
The header's three padding words became `hint_width/height/fps`. Old writers
leave them zero, so the layout version stays at 1. `VcamSink::start_dshow`
(capture) writes the `Local\` ring and announces the size. The receiver and
the S36 sender ("Relay Camera here") both go through `VcamSink::start`, which
picks the path. `MFCreateVirtualCamera` is never called where the export is
missing.

### Registration (`reg.rs`, `livereg.rs`)
`plan_dshow_install` lists the keys in HKCU:
1. `Software\Classes\CLSID\{filter}` (default = class name)
2. `…\InprocServer32` (default = DLL path, `ThreadingModel=Both`)
3. `Software\Classes\CLSID\{860BB310-…}\Instance\{filter}` (`FriendlyName=Relay Camera`, `CLSID`)

`vet_dshow_keys` refuses any key that is not one of those three,
case-insensitively. It runs before every install and uninstall, so a tampered
`installed.json` cannot delete the category or another filter.
`livereg::apply_user` / `remove_user` use the same
`RELAY_VDEVICE_ALLOW_LIVE_WRITE=1` gate as the HKLM path. No test runs them.

No `FilterData` value is written. It needs `IFilterMapper2` (HKLM/admin) or a
hand-serialised REGFILTER2 blob, and `ICreateDevEnum` lists the category from
`FriendlyName` + `CLSID`. If the live pass shows an app that needs it, add it
then.

HKLM was not needed, so the elevated-helper allow-list was **not** extended.
The helper refuses `install_camera` on the DirectShow path instead. If some
app turns out to read only HKLM, the planner already has a pure key list:
add an `InstallCameraFilterMachine` op beside `InstallCamera`, with a vetting
test.

### Core and UI
- `VdeviceStatus.camera_path` (`frame_server` | `direct_show`).
  `camera_supported` is now true on any Windows. `camera_registered` refers to
  the component for this PC's path.
- `vdevice::install_vcam` / `uninstall_vcam` pick the path. The existing
  non-elevated `InstallVcam` / `UninstallVcam` IPC and the CLI use them.
  Uninstall removes every recorded component, including a leftover filter
  record on a PC upgraded from 10 to 11.
- Uninstaller: new `StepKind::RemoveVcamFilter` (HKCU, no elevation). There is
  one step per recorded key.
- UI: Settings shows "installs for your account only — no administrator
  prompt" and a `PerUserCameraPanel` (plan → Install now → consent → install;
  remove → consent withdrawn afterwards). Receive "In calls" no longer says
  "Needs Windows 11" and, once installed, names the apps that list the camera.
  The Share preset toggle "Relay Camera here" is offered on Windows 10 too.

## What was done (tests)
- `camera/picture.rs`: sizes, YUY2 interleave, bottom-up RGB24, BT.709 red,
  scaling, and the waiting still.
- `camera/dshow.rs` unit: format offers, media-type round trip, and refusal
  of top-down RGB and I420.
- `tests/dshow_inproc.rs` (in-process, no registration): the filter is built
  through the DLL's own `DllGetClassObject` → `IClassFactory`, then
  interface probes (class id, misc flags, the one pin, id, `QueryPinInfo`
  owner, `PIN_CATEGORY_CAPTURE`, stream caps NV12/YUY2/RGB24). A test sink pin
  plays the app. It covers the waiting still with no producer, NV12 byte-exact
  pass-through, strictly increasing timestamps, pacing, YUY2 and RGB24
  conversion, `SetFormat` 640×360 with scaling, the still returning after the
  producer stops, and no filter↔pin cycle.
- `reg.rs`: plan contents, deepest-first uninstall, refusal of foreign or
  tampered keys. `livereg`: the HKCU paths are gated.
- Core: HKCU-only dry run, component per path, install refused without
  consent (nothing recorded), a tampered record refused before the registry,
  a good record stopped at the gate with the record kept, and the uninstaller
  steps without elevation.
- UI: Receive (Win10 available, app note), Settings (per-user install and
  remove, no `run_elevated`, HKCU plan), Share (toggle offered on Win10).

## Cannot be proven without a real Windows 10 box
- Whether `ICreateDevEnum` lists a filter registered **only in HKCU** in each
  app. COM resolves `HKCR` from HKCU first. The device enumerator's category
  cache is the unknown.
- Each app's own negotiation: which subtype and size it picks, and whether
  it needs `FilterData`.
- **32-bit apps:** only a 64-bit DLL is built and registered. A 32-bit
  caller needs an i686 build registered under `Software\Classes\WOW6432Node`.
  Current Zoom, Discord, Teams and Chrome are 64-bit.
- Apps that run elevated or sandboxed (AppContainer) may not see HKCU classes,
  or may not be able to open the `Local\` section.

## Live test plan (Win10 19045 PC)
Needs a build with the post-commit installer, or a manual copy of
`relay_vdevice.dll` next to `relay-core.exe`.
1. `relay-core vdevice status` → `camera path: DirectShow filter, per user`.
2. `relay-core vdevice dry-run` → exactly the 3 HKCU keys + the DLL.
3. Register: Settings → Virtual camera → Install… → Install now. Or run
   `set RELAY_VDEVICE_ALLOW_LIVE_WRITE=1` and then
   `relay-core vdevice consent-camera && relay-core vdevice install`. That is
   the gate. The core started from the UI needs the same env var until S43b
   decides the production gate.
   Check with `reg query "HKCU\Software\Classes\CLSID\{860BB310-5D01-11D0-BD3B-00A0C911CE86}\Instance\{5E0B7C1F-8A34-4D62-9B1E-C47A2F90D835}"`.
   `installed.json` should list `camera-dshow-filter` with those 3
   `hkcu_keys`.
4. With no share running, open each app. Relay Camera should appear with
   the waiting still:
   - Zoom → Settings → Video → Camera.
   - Discord → Settings → Voice & Video → Camera → Test Video.
   - Teams → Settings → Devices → Camera.
   - Chrome → https://webcamtests.com (or `chrome://media-internals`), then
     pick "Relay Camera".
   - Windows Camera app: expected **not** to list it. Record this, it is not a
     failure.
5. Start a receive from the Win11 PC (and separately, a share from this PC
   with "Relay Camera here" on). Each app shows the live stream within about
   1 s at the stream size, with no tearing or colour cast (skin tones and red
   UI elements check the BT.709 path). In Chrome, pick 1280×720 to exercise
   scaling.
6. Stop the share. The still returns in about 2 s and the app keeps running.
7. Close and reopen the app while a share runs. Frames resume.
8. Uninstall: Settings → Remove… → Remove now (or `relay-core vdevice
   uninstall` with the gate). The `reg query` returns "unable to find". Then
   `reg query HKCU\Software\Classes\CLSID\{5E0B7C1F-8A34-4D62-9B1E-C47A2F90D835}`
   is gone, `installed.json` has no components, and apps no longer list the
   camera after a restart. Nothing under HKLM changed: compare
   `reg export HKLM\SOFTWARE\Classes\CLSID` before and after.
9. `relay-core uninstall --dry-run` on a registered PC lists the three
   "Unregister the virtual camera (per-user filter)" steps and does **not**
   ask for elevation.
