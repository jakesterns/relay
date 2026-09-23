<#
.SYNOPSIS
  The M7 acceptance cycle: install -> opt in to both components -> share ->
  uninstall -> diff against the pre-install snapshot.

.DESCRIPTION
  Run this from a clean Windows VM sitting on a checkpoint. It drives the
  whole cycle and ends with a PASS/FAIL verdict plus a Markdown summary to
  paste into docs/plans/M7-installer.md.

  Phases run in order and each one is idempotent enough to re-run on its own
  (-Phase), which matters because the interesting failures happen in the
  middle and re-snapshotting from scratch means reverting the checkpoint.

    before     snapshot the untouched machine
    install    run the NSIS installer silently, check what it created
    optin      opt in to the endpoint APO and the virtual camera
    share      start a loopback share so the capture/encode path has run
    uninstall  run the uninstaller silently
    after      snapshot again and diff

  The opt-in phase needs elevation (both components write HKLM) and sets the
  two live-write gates the component installers demand. Everything else runs
  as a normal user, which is the point: a per-user install must not need
  admin for anything the user did not explicitly opt into.

.PARAMETER Installer
  Path to Relay_<version>_x64-setup.exe. Defaults to the newest one under
  target\release\bundle\nsis.

.PARAMETER WorkDir
  Where snapshots and the report go. Defaults to C:\relay-m7.

.PARAMETER Phase
  One phase, or 'all' (default).

.PARAMETER KeepData
  Uninstall with the data folder kept. The diff then allows entries under
  %LOCALAPPDATA%\Relay and nothing else. Without this the diff must be
  completely empty.

.PARAMETER SkipOptIn
  Run the cycle without the two opt-in components (for a non-elevated pass,
  or before the EV certificate exists). Recorded as such in the report.

.EXAMPLE
  # Clean VM, full cycle:
  .\vm-cycle.ps1
  # Then revert the checkpoint before the next run.
#>
[CmdletBinding()]
param(
    [string]$Installer,
    [string]$WorkDir = 'C:\relay-m7',
    [ValidateSet('all', 'before', 'install', 'optin', 'share', 'uninstall', 'after')]
    [string]$Phase = 'all',
    [switch]$KeepData,
    [switch]$SkipOptIn
)

$ErrorActionPreference = 'Stop'
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $scriptDir

$before = Join-Path $WorkDir 'before.json'
$after = Join-Path $WorkDir 'after.json'
$report = Join-Path $WorkDir 'diff.md'
$log = Join-Path $WorkDir 'cycle.log'

New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

function Say {
    param([string]$Text)
    $line = "[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $Text
    Write-Host $line
    Add-Content -LiteralPath $log -Value $line -Encoding utf8
}

function Fail {
    param([string]$Text)
    Say "FAIL: $Text"
    throw $Text
}

function Resolve-Installer {
    if ($Installer) {
        if (-not (Test-Path -LiteralPath $Installer)) { Fail "installer not found: $Installer" }
        return (Resolve-Path -LiteralPath $Installer).Path
    }
    $dir = Join-Path $repo 'target\release\bundle\nsis'
    if (-not (Test-Path $dir)) { Fail "no bundle in $dir -- run: cd ui; pnpm tauri build --bundles nsis" }
    $exe = Get-ChildItem $dir -Filter '*-setup.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $exe) { Fail "no *-setup.exe in $dir" }
    $exe.FullName
}

function Get-InstallDir {
    $arp = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Relay' -ErrorAction SilentlyContinue
    # NSIS writes this value with the quotes included.
    if ($arp -and $arp.InstallLocation) { return $arp.InstallLocation.Trim('"') }
    $null
}

function Relay-Core {
    param([string[]]$CoreArgs, [switch]$AllowFail)
    $dir = Get-InstallDir
    if (-not $dir) { Fail 'Relay does not look installed (no InstallLocation in Add/Remove Programs)' }
    $exe = Join-Path $dir 'relay-core.exe'
    if (-not (Test-Path $exe)) { Fail "relay-core.exe missing from $dir" }
    Say "relay-core $($CoreArgs -join ' ')"
    # relay-core logs to stderr when it has a console, and Windows PowerShell
    # turns native stderr into a terminating error while ErrorActionPreference
    # is 'Stop'. The exit code is the signal.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { $out = & $exe @CoreArgs 2>&1 } finally { $ErrorActionPreference = $prev }
    $out | ForEach-Object { Add-Content -LiteralPath $log -Value "    $_" -Encoding utf8 }
    if ($LASTEXITCODE -ne 0 -and -not $AllowFail) { Fail "relay-core $($CoreArgs -join ' ') exited $LASTEXITCODE" }
    # Only the lines that are output, not the log noise, so callers can match
    # on them.
    $out | Where-Object { $_ -notmatch '^\d{4}-\d{2}-\d{2}T' }
}

function Is-Elevated {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

# ------------------------------------------------------------------ phases ---

function Phase-Before {
    Say '=== before: snapshotting the untouched machine ==='
    if (Get-InstallDir) {
        Fail 'Relay is already installed -- revert the VM checkpoint first, or the diff is meaningless'
    }
    & (Join-Path $scriptDir 'machine-snapshot.ps1') -Out $before -Broad
    Say "baseline written to $before"
}

function Phase-Install {
    Say '=== install: running the NSIS installer silently ==='
    $exe = Resolve-Installer
    Say "installer: $exe"
    # /S is NSIS silent; the Tauri template accepts it and installs per-user
    # without prompting.
    #
    # WaitForExit() rather than -Wait: the post-install hook starts the core,
    # and -Wait blocks until the whole spawned tree exits, which for an
    # always-on service is never.
    $p = Start-Process -FilePath $exe -ArgumentList '/S' -PassThru
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { Fail "installer exited $($p.ExitCode)" }
    # The hook's RunAsUser call is asynchronous; give the core a moment to
    # come up before anything asks it questions.
    Start-Sleep -Seconds 2

    $dir = Get-InstallDir
    if (-not $dir) { Fail 'installer left no InstallLocation in Add/Remove Programs' }
    Say "installed to $dir"

    # Everything the core spawns has to be beside it, or the on-demand
    # children (share engine, preview renderer, elevated install helper) and
    # the two opt-in DLLs are unreachable at runtime.
    $needed = @('relay-ui.exe', 'relay-core.exe', 'relay-svc.exe', 'relay-elevate.exe',
                'relay-share.exe', 'relay-preview.exe', 'relay_apo.dll',
                'relay_vdevice.dll', 'uninstall.exe')
    $missing = @()
    foreach ($f in $needed) { if (-not (Test-Path (Join-Path $dir $f))) { $missing += $f } }
    if ($missing.Count -gt 0) { Fail "missing from the install: $($missing -join ', ')" }
    Say "all $($needed.Count) payload files present"

    $lnk = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Relay.lnk"
    if (-not (Test-Path $lnk)) {
        $lnk = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Relay\Relay.lnk"
    }
    if (-not (Test-Path $lnk)) { Fail 'no Start Menu shortcut' }
    Say "start menu shortcut: $lnk"

    # A fresh install must not have touched the machine yet: no autostart, no
    # consent recorded, nothing registered.
    $auto = Relay-Core @('autostart')
    if ("$auto".Trim() -ne 'off') { Fail "autostart should default to off, got '$auto'" }
    $plan = Relay-Core @('uninstall', '--dry-run')
    Say 'post-install uninstall plan:'
    $plan | ForEach-Object { Say "    $_" }
    foreach ($line in $plan) {
        if ($line -match '^\[x\].*(endpoint audio chain|virtual camera|start-at-login)') {
            Fail "installer registered something it should not have: $line"
        }
    }
    Say 'install touched nothing beyond its own files -- as promised'

    Relay-Core @('autostart', 'on') | Out-Null
    if ("$(Relay-Core @('autostart'))".Trim() -ne 'on') { Fail 'autostart on did not stick' }

    # The Run value must launch the windowless launcher, not the core
    # directly: relay-core.exe is a console binary, so pointing the Run key at
    # it flashes a black window at every login. It must also be an absolute
    # path inside the install directory.
    $runValue = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run').Relay
    Say "run value: $runValue"
    if ($runValue -notmatch [regex]::Escape('relay-svc.exe')) {
        Fail "autostart points at something other than relay-svc.exe: $runValue"
    }
    if ($runValue -notmatch [regex]::Escape($dir)) {
        Fail "autostart points outside the install directory: $runValue"
    }
    Say 'autostart opt-in exercised and points at the windowless launcher'

    Say 'starting the core'
    Start-Process -FilePath (Join-Path $dir 'relay-core.exe') -ArgumentList 'run' | Out-Null
    Start-Sleep -Seconds 3
    Relay-Core @('status') | ForEach-Object { Say "    $_" }
}

function Phase-OptIn {
    if ($SkipOptIn) { Say '=== optin: SKIPPED (-SkipOptIn) ==='; return }
    Say '=== optin: endpoint APO + virtual camera ==='
    if (-not (Is-Elevated)) {
        Fail 'the opt-in phase needs an elevated prompt (both components write HKLM). Re-run this phase as administrator, or pass -SkipOptIn.'
    }
    $env:RELAY_APO_ALLOW_LIVE_WRITE = '1'
    $env:RELAY_VDEVICE_ALLOW_LIVE_WRITE = '1'

    Relay-Core @('apo', 'install') | ForEach-Object { Say "    $_" }
    $apo = Relay-Core @('apo', 'status')
    if ("$apo" -notmatch 'installed: true') { Fail "APO did not register: $apo" }
    # The install backup is what makes the restore exact; no backup, no promise.
    $backups = Get-ChildItem "$env:LOCALAPPDATA\Relay\apo-backup" -Filter '*.json' -ErrorAction SilentlyContinue
    if (-not $backups) { Fail 'APO installed without writing a pre-install backup' }
    Say "apo backup: $($backups[0].FullName)"

    Relay-Core @('vdevice', 'consent-camera') | ForEach-Object { Say "    $_" }
    Relay-Core @('vdevice', 'install') | ForEach-Object { Say "    $_" }
    $vd = Relay-Core @('vdevice', 'status')
    if ("$vd" -notmatch 'camera registered: true') { Fail "camera did not register: $vd" }
    Say 'both components registered; installed.json records them'

    # The third machine-wide change: the inbound rule for relay-share.exe.
    # Added here rather than by the installer because the installer is
    # per-user and unelevated and never grabs a token on its own.
    $env:RELAY_FIREWALL_ALLOW_LIVE_WRITE = '1'
    Relay-Core @('firewall', 'allow') | ForEach-Object { Say "    $_" }
    $fw = Relay-Core @('firewall', 'status')
    if ("$fw" -notmatch 'our rule: present') { Fail "firewall rule did not go in: $fw" }
    # Backup-then-apply applies here too: the record is what the uninstaller
    # plans the removal from.
    if (-not (Test-Path "$env:LOCALAPPDATA\Relay\firewall.json")) {
        Fail 'firewall rule added without writing firewall.json'
    }
    Say 'firewall rule added and recorded in firewall.json'

    Say 'restarting audiosrv so the APO is picked up by new streams'
    Restart-Service audiosrv -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 3
}

function Phase-Share {
    Say '=== share: loopback share so the capture/encode path has run ==='
    $dir = Get-InstallDir
    $code = '424242'
    $env:RELAY_PEER = '127.0.0.1'
    # Headless, always: a windowed receiver on the PC being captured shows the
    # capture of itself and smears the whole screen (docs/dev/BUGS.md B9).
    $recv = Start-Process -FilePath (Join-Path $dir 'relay-share.exe') `
        -ArgumentList @('recv', '--headless', '--code', $code) -PassThru
    Start-Sleep -Seconds 2
    Relay-Core @('share-start', $code) -AllowFail | ForEach-Object { Say "    $_" }
    Start-Sleep -Seconds 8
    $status = Relay-Core @('status')
    $status | ForEach-Object { Say "    $_" }
    Relay-Core @('share-stop') -AllowFail | Out-Null
    if ($recv -and -not $recv.HasExited) { $recv | Stop-Process -Force -ErrorAction SilentlyContinue }

    # What this phase is for is that the capture, encode and transport code
    # paths ran at least once before the uninstall, so the uninstall is
    # tested against a machine that has actually done work. Whether the
    # loopback peer completed its handshake is an M4 concern and does not
    # gate M7: a warning, not a failure.
    if (($status -join ' ') -notmatch 'share\s+sharing') {
        Say 'NOTE: the loopback peer did not complete pairing; the share engine still span up and tore down.'
    }
    Say 'share stopped; the engine is a child process and goes with it'
    $global:LASTEXITCODE = 0
}

function Phase-Uninstall {
    Say '=== uninstall ==='
    $dir = Get-InstallDir
    Say 'plan the uninstaller will execute:'
    Relay-Core @('uninstall', '--dry-run') | ForEach-Object { Say "    $_" }

    $arp = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Relay'
    $un = $arp.UninstallString.Trim('"')
    Say "uninstaller: $un"
    $dataArg = if ($KeepData) { '/KEEPDATA' } else { '/DELETEDATA' }
    # Same reason as the installer: wait on this process, not its tree. NSIS
    # uninstallers copy themselves to %TEMP% and relaunch, so the first
    # process exits quickly and the copy does the work.
    $p = Start-Process -FilePath $un -ArgumentList @('/S', $dataArg) -PassThru
    $p.WaitForExit()
    Say "uninstaller exited $($p.ExitCode)"

    # NSIS detaches itself to delete its own directory; give it a moment.
    for ($i = 0; $i -lt 30 -and (Test-Path $dir); $i++) { Start-Sleep -Seconds 1 }
    if (Test-Path $dir) {
        # With the data kept this is the expected outcome: the per-user
        # installer shares its folder with the data root, so the folder
        # survives holding nothing but the user's profiles. The diff decides
        # whether that is all it holds.
        $leftover = @(Get-ChildItem $dir -Force -ErrorAction SilentlyContinue | Where-Object {
            $_.Name -notin @('data', 'logs', 'previews', 'apo-backup', 'installed.json')
        })
        if ($KeepData -and $leftover.Count -eq 0) {
            Say "install dir kept for the data folder, program files gone: $dir"
        } else {
            Say "WARNING: install dir still holds program files after 30 s: $dir"
            $leftover | ForEach-Object { Say "    leftover: $($_.Name)" }
        }
    }
    if (Get-Process relay-core, relay-ui, relay-share -ErrorAction SilentlyContinue) {
        Fail 'a Relay process survived the uninstall'
    }
    Say 'no Relay process left running'
}

function Phase-After {
    Say '=== after: snapshot and diff ==='
    & (Join-Path $scriptDir 'machine-snapshot.ps1') -Out $after -Broad
    # A hashtable, not an array: splatting an array passes its elements
    # positionally, so "-Before" arrives as a value rather than a parameter
    # name. Also not named $args -- that is an automatic variable.
    $diffArgs = @{ Before = $before; After = $after; Out = $report }
    if ($KeepData) { $diffArgs['KeepData'] = $true }
    & (Join-Path $scriptDir 'snapshot-diff.ps1') @diffArgs
    $code = $LASTEXITCODE
    Say "diff report -> $report"
    if ($code -ne 0) { Fail 'the uninstall left differences behind -- see the report' }
    Say 'PASS: the machine is back to its pre-install state'
}

# -------------------------------------------------------------------- drive ---

Say "M7 cycle: phase=$Phase keepData=$KeepData skipOptIn=$SkipOptIn workdir=$WorkDir"
switch ($Phase) {
    'before'    { Phase-Before }
    'install'   { Phase-Install }
    'optin'     { Phase-OptIn }
    'share'     { Phase-Share }
    'uninstall' { Phase-Uninstall }
    'after'     { Phase-After }
    'all' {
        Phase-Before
        Phase-Install
        Phase-OptIn
        Phase-Share
        Phase-Uninstall
        Phase-After
    }
}
Say 'done'
