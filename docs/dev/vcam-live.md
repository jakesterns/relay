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

## Worth trying while in there

`MFVirtualCameraAccess_CurrentUser` may accept a CLSID registered under
`HKCU\Software\Classes\CLSID` (per-user frame server hosting). If the quick
probe passes with an HKCU-only registration, the install path can drop the
elevation requirement entirely — file that as a decision in the M5 plan.
