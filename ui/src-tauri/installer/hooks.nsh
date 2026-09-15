; Relay NSIS installer hooks.
;
; Tauri's NSIS template provides four insertion points. Relay uses three of
; them, and the rule for all of them is the same as everywhere else in this
; codebase: the installer itself changes nothing on the machine beyond its own
; files, and the uninstaller removes exactly what was recorded -- by asking
; relay-core, not by re-deriving it here.
;
; Command-line switches (silent installs, and the VM acceptance cycle):
;   installer   /AUTOSTART    also enable start-at-login
;   installer   /FIREWALL     also add the inbound firewall rule for
;                             relay-share.exe (raises one UAC prompt unless
;                             the installer is already elevated)
;   uninstaller /KEEPDATA     keep the profiles and hardware library (default)
;   uninstaller /DELETEDATA   delete them too
;
; Interactively the uninstaller's own "Delete the application data" checkbox
; decides, so the user is asked about their data exactly once.
;
; ASCII only: makensis reads this file as ANSI, so a stray em dash would reach
; the progress log as mojibake.

!macro NSIS_HOOK_PREINSTALL
  ; A running core holds relay-core.exe and the share engine open, so an
  ; upgrade over a live install would fail to replace them. Shutting it down
  ; also restores the user's audio and display settings, which is what we
  ; want before swapping binaries underneath them.
  ${If} ${FileExists} "$INSTDIR\relay-core.exe"
    DetailPrint "Stopping the running Relay core..."
    nsExec::ExecToLog '"$INSTDIR\relay-core.exe" shutdown'
    Pop $0
    Sleep 1500
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Autostart is opt-in and off by default: this is the one Run-key value
  ; Relay is allowed to create, and only when asked for. The interactive
  ; opt-in lives on the app's first-run screen; /AUTOSTART is here for
  ; silent and managed installs.
  ${GetOptions} $CMDLINE "/AUTOSTART" $0
  ${IfNot} ${Errors}
    DetailPrint "Enabling start at login..."
    nsExec::ExecToLog '"$INSTDIR\relay-core.exe" autostart on'
    Pop $0
  ${EndIf}

  ; The inbound firewall rule for relay-share.exe. Opt-in and off by default,
  ; for the same reason autostart is: this installer runs per-user and
  ; unelevated, and reaching for an administrator token the user did not offer
  ; would be exactly the behaviour Relay promises not to have.
  ;
  ; So there is no silent grab here. /FIREWALL is for silent and managed
  ; installs, where the deploying admin has decided; it goes through
  ; relay-elevate.exe, which raises a normal UAC prompt when the installer is
  ; not already elevated, and declining it changes nothing. Interactively the
  ; app asks instead, at the moment it matters: the Share and Receive screens
  ; detect the blocked state and offer the same one-click fix.
  ;
  ; Either way the rule is recorded in firewall.json and removed by the
  ; uninstaller, so it cannot survive a clean-VM diff.
  ${GetOptions} $CMDLINE "/FIREWALL" $0
  ${IfNot} ${Errors}
    DetailPrint "Allowing Relay through Windows Firewall..."
    nsExec::ExecToLog '"$INSTDIR\relay-core.exe" elevate run allow-firewall'
    Pop $0
    ${If} $0 != 0
      DetailPrint "Firewall rule not added (exit $0). Relay still runs; the app will offer to add it."
    ${EndIf}
  ${EndIf}

  ; Start the always-on core so the UI has live data the moment it opens.
  ; Through relay-svc.exe, the GUI-subsystem launcher, so the user does not
  ; see a console window flash at the end of the install -- it spawns
  ; relay-core with CREATE_NO_WINDOW and exits. RunAsUser because the core
  ; must run as the user whose profiles it manages, whatever token the
  ; installer happens to be holding.
  DetailPrint "Starting the Relay core..."
  nsis_tauri_utils::RunAsUser "$INSTDIR\relay-svc.exe" "run"
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Everything that went onto the machine comes off here, while
  ; relay-core.exe still exists -- NSIS deletes the files after this macro
  ; returns.
  ;
  ; Order, gating and the record of what was registered all live in
  ; crates/core/src/uninstall.rs. Duplicating any of it in NSIS would mean
  ; two places to keep in step, and the installer would be the one that is
  ; wrong.

  ; Keeping the user's tuning work is the default in every path: a profile is
  ; hours of listening tests and a few hundred kilobytes on disk.
  StrCpy $R1 "--keep-data"

  ${GetOptions} $CMDLINE "/DELETEDATA" $0
  ${IfNot} ${Errors}
    StrCpy $R1 "--delete-data"
  ${Else}
    ${GetOptions} $CMDLINE "/KEEPDATA" $0
    ${If} ${Errors}
      ; No switch given, so follow the uninstaller's own "Delete the
      ; application data" checkbox. A MessageBox here would be a second
      ; question about the thing the user just answered on the page behind
      ; this one.
      ${If} $DeleteAppDataCheckboxState = 1
        StrCpy $R1 "--delete-data"
      ${EndIf}
    ${EndIf}
  ${EndIf}

  ${If} ${FileExists} "$INSTDIR\relay-core.exe"
    DetailPrint "Restoring Windows settings and removing Relay's components..."
    ; Windows may prompt for permission here, and only here: the endpoint APO,
    ; the virtual camera and the firewall rule are the three things registered
    ; machine-wide, and only if the user opted in to them.
    nsExec::ExecToLog '"$INSTDIR\relay-core.exe" uninstall --silent $R1'
    Pop $0
    ${If} $0 != 0
      DetailPrint "relay-core uninstall reported errors (exit $0); continuing with file removal."
    ${EndIf}
    Sleep 1000
  ${Else}
    DetailPrint "relay-core.exe is already gone; removing files only."
  ${EndIf}

  ; The share engine and the preview renderer are children of the core and go
  ; with it, but a crashed core can orphan one and a held file blocks the
  ; uninstall. Belt and braces.
  nsExec::ExecToLog 'taskkill /F /IM relay-share.exe /T'
  Pop $0
  nsExec::ExecToLog 'taskkill /F /IM relay-preview.exe /T'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Tauri's template records the install location under
  ; HKCU\Software\relay\Relay so a reinstall can offer the same folder, and
  ; only deletes it when "Delete the application data" is ticked. That is
  ; install bookkeeping, not the user's profiles, and Relay's promise is that
  ; an uninstall leaves nothing behind except the data folder they chose to
  ; keep -- so it goes on every uninstall.
  ;
  ; Caught by scripts/snapshot-diff.ps1 on the first full cycle, which is
  ; what that harness is for.
  DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
  DeleteRegKey HKCU "${MANUPRODUCTKEY}"
  DeleteRegKey /ifempty HKCU "${MANUKEY}"
  DeleteRegKey SHCTX "${MANUPRODUCTKEY}"
  DeleteRegKey /ifempty SHCTX "${MANUKEY}"
!macroend
