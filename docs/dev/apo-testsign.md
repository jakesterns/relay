# APO development without the EV certificate — test-signing VM runbook

Windows only loads endpoint APOs whose DLLs it trusts. Until the EV
certificate + Hardware Dev Center attestation exist (tracked in
`docs/plans/M3b-apo.md`), all live registration work happens in a
**throwaway Windows VM with test signing enabled** — never on a dev machine.
The unsigned in-process coverage (`cargo test -p relay-apo`) already
exercises negotiation, RT processing and the property-store engine; this
runbook is only for the audiodg-hosted pass.

## One-time VM setup

1. Windows 11 VM (Hyper-V/VMware/VirtualBox), audio device enabled.
2. **Take a checkpoint now** — before any APO registration. This is the
   Definition-of-Ready item; every experiment ends with a revert or a
   verified clean uninstall diff.
3. Elevated prompt:
   ```
   bcdedit /set testsigning on
   shutdown /r /t 0
   ```
   ("Test Mode" watermark appears after reboot.)
4. Create a self-signed code-signing cert and trust it:
   ```powershell
   $c = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=Relay Dev" `
        -CertStoreLocation Cert:\CurrentUser\My
   Export-Certificate -Cert $c -FilePath relay-dev.cer
   Import-Certificate -FilePath relay-dev.cer -CertStoreLocation Cert:\LocalMachine\Root
   Import-Certificate -FilePath relay-dev.cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher
   ```
5. audiodg additionally verifies APO signatures unless told not to — for the
   test VM only:
   ```
   reg add "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio" `
       /v DisableProtectedAudioDG /t REG_DWORD /d 1
   ```
   (The production path is the EV-signed DLL; this key never ships.)

## Per-iteration loop

1. Build and sign:
   ```
   cargo build --release -p relay-apo
   signtool sign /fd SHA256 /n "Relay Dev" target\release\relay_apo.dll
   ```
2. Copy `relay_apo.dll` next to `relay-core.exe` in the VM.
3. Baseline export (the diff anchor):
   ```
   reg export "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\<endpoint-guid>\FxProperties" before.reg /y
   ```
4. Register (both gates deliberately explicit; elevated prompt):
   ```
   set RELAY_APO_ALLOW_LIVE_WRITE=1
   relay-core apo install     # or the InstallApo IPC from the Settings card
   ```
   The complete prior store is written to
   `%LOCALAPPDATA%\Relay\apo-backup\<endpoint>.json` *before* the registry
   changes.
5. Restart the audio engine so the endpoint re-enumerates without a reboot:
   ```
   net stop audiosrv && net start audiosrv
   ```
6. Play audio; drive params from the core (profile apply / bypass hotkey).
   Verify with the Games screen instrument readout and `relay-core status`.
7. Uninstall + proof:
   ```
   relay-core apo uninstall
   net stop audiosrv && net start audiosrv
   reg export "...\FxProperties" after.reg /y
   fc /b before.reg after.reg      # must report no differences
   ```
8. Revert the checkpoint before the next structural experiment.

## What to verify in the VM (the deferred M3b items)

- APO loads in audiodg (Event Viewer → MediaFoundation/Audio logs on failure).
- Format negotiation against the endpoint's real mix format; a 44.1 kHz and
  a 96 kHz endpoint if available.
- `before.reg` / `after.reg` byte-identical after uninstall (step 7).
- Endpoint re-enumeration picks the APO up after `audiosrv` restart with no
  reboot.
- WASAPI-exclusive game (or the `hold_exclusive_for_test` helper) still
  flips the UI banner while the APO is installed.
