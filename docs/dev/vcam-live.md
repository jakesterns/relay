# Virtual camera — live verification runbook (M5)

Everything below is what could not run in the M5 session: the dev session
had no elevation (UAC declined / no admin shell) and registry writes are
required once, so the frame-server hosting and the in-app checks are
unverified. All other links in the chain are proven by tests:

- ring seqlock + cross-mapping: `relay-vdevice` unit tests
- media source serves ring frames byte-for-byte: `crates/vdevice/tests/source_inproc.rs`
- GPU NV12 texture → staging → ring byte-for-byte: `crates/capture/tests/vcam_ring.rs`

What is *not* proven yet: the Frame Server actually loading the DLL,
attribute marshaling from `IMFVirtualCamera` to the activate, the `Global\`
section ACL, and the apps themselves.

The elevated step is unavoidable: per-user (HKCU) registration was tested
and does not work — see the last section for the evidence.

## One-time setup (elevated prompt)

```powershell
cargo build --release            # relay-core.exe, relay-share.exe, relay_vdevice.dll

# consent (or click through the app's first-run screen instead)
target\release\relay-core.exe vdevice status
target\release\relay-core.exe vdevice consent-camera

# register — the only step that needs elevation; both gates on purpose
$env:RELAY_VDEVICE_ALLOW_LIVE_WRITE = '1'
target\release\relay-core.exe vdevice install
target\release\relay-core.exe vdevice status    # camera registered: true
```

## Quick probe, no share needed

Creates the camera, feeds one test frame, enumerates capture devices the
way a conferencing app does, opens "Relay Camera" with an
`IMFSourceReader`, and asserts the served bytes are the ring frame:

```powershell
$env:RELAY_VCAM_LIVE = '1'
cargo test -p relay-vdevice --release --test camera_live -- --nocapture
```

Expected: `LIVE PASS: Relay Camera enumerated and served frames`.
If `MFCreateVirtualCamera` fails with REGDB_E_CLASSNOTREG, the install step
did not run; with E_ACCESSDENIED, check the Windows Camera privacy toggle
(Settings → Privacy → Camera).

## Loopback share into the apps

Two terminals on the receiver PC (sender loopback, like the M4 measurements):

```powershell
target\release\relay-share.exe recv --vcam          # prints the pairing code
target\release\relay-share.exe send --code <code>   # 1080p60 default
```

Watch for the `{"event":"vcam_up"}` NDJSON line from recv. Then:

| App | Where | Check |
|---|---|---|
| Discord | Settings → Voice & Video → Camera | "Relay Camera" listed; preview shows the shared desktop; appears within 5 s of `vcam_up` |
| Zoom | Settings → Video → Camera | same |
| Meet | Chrome, meet.new → camera picker | same |

Repeat at 4K30: `send --fps 30` with a 4K source monitor. Record results in
`docs/plans/M5-vdevices.md`.

Mic route (needs VB-Cable installed): `recv --vcam --mic-route <endpoint-id>`
(ids from `relay-core vdevice status --json`), pick "CABLE Output" as the
mic in Discord, clap on the sender and listen for offset (plan's alignment
item).

## Remove

```powershell
$env:RELAY_VDEVICE_ALLOW_LIVE_WRITE = '1'
target\release\relay-core.exe vdevice uninstall     # elevated
target\release\relay-core.exe vdevice status        # camera registered: false
Get-Content $env:LOCALAPPDATA\Relay\installed.json  # components: []
```


## Per-user (HKCU) registration: settled, it does not work

**Do not try this again.** Session S5 (2026-09-14) ran the experiment on
Windows 11 Pro 26200 with an RTX 3090. Registering the media source under
`HKCU\Software\Classes\CLSID` instead of HKLM is *not* a way to drop the
elevated install step. The elevated write stays required.

### What happens

The per-user registration gets further than you would expect, which is why
this needs writing down: it passes every check made in the *calling* process
and fails only inside the Frame Server.

| Step | Result with an HKCU-only registration |
|---|---|
| `CoGetClassObject(CLSCTX_INPROC_SERVER)` in our process | **OK** — HKCR merges `HKCU\Software\Classes` over `HKLM\SOFTWARE\Classes`, so our own process resolves the class and loads the DLL |
| `CoGetClassObject(CLSCTX_LOCAL_SERVER)` | `0x80040154` REGDB_E_CLASSNOTREG |
| `MFCreateVirtualCamera(..., MFVirtualCameraAccess_CurrentUser, CLSID)` | **OK** — returns an `IMFVirtualCamera` |
| `IMFVirtualCamera::Start` | **`0x80070003` ERROR_PATH_NOT_FOUND** |

`Start` is where the request leaves our process. The Frame Server service
(`FrameServer`, `svchost -k Camera`, running as **NT AUTHORITY\LocalService**)
starts on demand — `sc query FrameServer` goes STOPPED → RUNNING on the call —
creates the virtual-camera device node, and then fails to bring the source up.
From `Microsoft-Windows-MF-FrameServer/Camera_FrameServer`:

```
FsProxy Initialization Start, SymbolicLink: \\?\SWD#VCAMDEVAPI#…, Devices: 1, StreamType: SHARED.
FsProxy Initialization Stop,  SymbolicLink: \\?\SWD#VCAMDEVAPI#…, Devices: 1, Streams: 0,
                              StreamType: SHARED, hr: 0x80070003.
```

The reason is the hive, not the DLL, the path, or the ACL:

- **The service never loads the DLL.** A temporary build that appended a line
  to `C:\Windows\Temp\…` on entry to `DllGetClassObject` logged two entries,
  both from the probe process, none from `svchost`. The class is never
  resolved in the service's context, so nothing about our media source code
  is even reached.
- **It is not file permissions.** The DLL was moved from the user profile to
  `C:\ProgramData\…` (BUILTIN\Users: ReadAndExecute), which LocalService can
  read. Identical `0x80070003`.
- **LocalService's HKCU is `HKU\S-1-5-19`, not ours.** A class written to the
  interactive user's `Software\Classes` is simply not in the service's view
  of HKCR. That is the whole story.

### Control arms (same CLSID, same probe)

| Arm | `Start` HRESULT | Frame-server event |
|---|---|---|
| A — HKCU registered, DLL present | `0x80070003` PATH_NOT_FOUND | yes, init fails 0x80070003 |
| B — HKCU registered, `InprocServer32` points at a missing file | `0x8007007E` MOD_NOT_FOUND | none |
| C — not registered in any hive | `0x80040154` CLASSNOTREG | none |

B and C never reach the service at all: the client-side lookup (which honours
HKCU) short-circuits first. Only a *well-formed* per-user registration gets as
far as the Frame Server, and that is exactly where it dies. If you are ever
staring at `0x80070003` from `IMFVirtualCamera::Start`, this is what it means:
the Frame Server cannot resolve your CLSID.

### Reproducing it

The probe is checked in and writes nothing:

```powershell
cargo run -p relay-vdevice --release --example vcam_reg_probe -- '{CLSID}'
```

It prints both `CoGetClassObject` results, the `MFCreateVirtualCamera` and
`Start` HRESULTs, and the Frame Server service state either side of the call.
Add the HKCU key by hand if you want to re-run arm A (no elevation needed) and
delete it afterwards:

```powershell
$c = '{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}'
New-Item -Path "HKCU:\Software\Classes\CLSID\$c\InprocServer32" -Force
Set-ItemProperty "HKCU:\Software\Classes\CLSID\$c\InprocServer32" '(default)' <path-to-relay_vdevice.dll>
Set-ItemProperty "HKCU:\Software\Classes\CLSID\$c\InprocServer32" 'ThreadingModel' 'Both'
# … run the probe …
Remove-Item "HKCU:\Software\Classes\CLSID\$c" -Recurse -Force
```

Nothing was left behind by S5: no HKLM key was ever written, the HKCU key and
the `C:\ProgramData` copy were removed, and `installed.json` was untouched.

### Two minutes to confirm it when you do the elevated pass

The one thing S5 could not do is the positive control — register in HKLM and
watch the same probe succeed. When you run the elevated pass (S12), run the
probe before the rest of the runbook:

```powershell
target\release\examples\vcam_reg_probe.exe '{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}'
```

`Start` returning `S_OK` with the key in HKLM, against `0x80070003` with the
identical key in HKCU, closes the loop.

### What is left, if the elevated step ever has to go

Not HKCU. The only remaining candidate is **packaged (MSIX) COM registration** —
an MSIX `com:ComServer` extension is written to the package catalogue at
install time rather than to the user's `Software\Classes`, and per-user MSIX
installs do not need admin. Whether the Frame Server resolves a package-scoped
class from LocalService is **unverified**, and it is an installer-shaped change
(M7), not a registry-shaped one. Do not start it without testing that one
question first, the same way this page tests HKCU.
