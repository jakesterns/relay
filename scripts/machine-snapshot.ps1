<#
.SYNOPSIS
  Snapshot every place on Windows that Relay could possibly write, so an
  install/uninstall cycle can be diffed against it.

.DESCRIPTION
  M7's Definition of Done is "the clean-VM diff after uninstall is empty".
  That claim needs a snapshot with two properties: it has to cover everything
  Relay touches, and it has to be stable across runs so the diff is signal
  rather than noise.

  Two tiers:

  * Targeted (default) -- exactly the keys, values, files and folders Relay's
    own uninstall plan names, plus the audio endpoint FX property stores and
    the COM/frame-server roots its two opt-in components register into. This
    is the authoritative check for the promise and is stable even on a
    working dev machine.

  * Broad (-Broad) -- full exports of the registry hives Relay can reach and
    a file index of the locations it can write. Only meaningful from a clean
    VM checkpoint, where nothing else is changing; on a working machine it
    picks up every other app's churn.

  Output is one JSON file of sorted, normalised entries. Volatile fields
  (file write times, log contents, sizes of files Relay appends to) are
  deliberately excluded: an uninstall cannot be blamed for a log line.

.PARAMETER Out
  Path of the JSON snapshot to write.

.PARAMETER Broad
  Also capture the whole-hive exports and file index (clean VM only).

.PARAMETER DataRoot
  Relay's data root. Defaults to %LOCALAPPDATA%\Relay.

.PARAMETER InstallDir
  Relay's program directory, when it is known. Defaults to the path recorded
  in Add/Remove Programs, else %LOCALAPPDATA%\Relay's sibling.

.EXAMPLE
  # On the clean VM, before installing:
  .\machine-snapshot.ps1 -Out C:\relay-test\before.json -Broad
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [switch]$Broad,
    [string]$DataRoot = "$env:LOCALAPPDATA\Relay",
    [string]$InstallDir
)

$ErrorActionPreference = 'Stop'

# Relay's virtual-camera CLSID (crates/vdevice/src/reg.rs::VCAM_CLSID) and the
# APO's (crates/audio/apo/src/ids.rs). Hard-coded here on purpose: the point of
# the diff is to catch a stray key, so the checker must not ask the code under
# test where to look.
$VCAM_CLSID = '{9B7E62D4-2A31-4C8E-8F5A-D0C4B6E91A27}'

function Get-RegValues {
    param([string]$Path)
    $out = @()
    if (-not (Test-Path -LiteralPath $Path)) { return $out }
    $item = Get-Item -LiteralPath $Path -ErrorAction SilentlyContinue
    if (-not $item) { return $out }
    foreach ($name in $item.GetValueNames()) {
        $raw = $item.GetValue($name, $null, 'DoNotExpandEnvironmentNames')
        $text = if ($raw -is [byte[]]) { [System.BitConverter]::ToString($raw) }
                elseif ($raw -is [array]) { ($raw -join '|') }
                else { [string]$raw }
        $out += [pscustomobject]@{
            key   = $Path
            name  = if ($name -eq '') { '(default)' } else { $name }
            kind  = [string]$item.GetValueKind($name)
            value = $text
        }
    }
    $out
}

function Get-RegTree {
    param([string]$Path)
    $out = @()
    if (-not (Test-Path -LiteralPath $Path)) { return $out }
    $out += Get-RegValues -Path $Path
    foreach ($sub in (Get-ChildItem -LiteralPath $Path -Recurse -ErrorAction SilentlyContinue)) {
        $out += Get-RegValues -Path $sub.PSPath
    }
    $out
}

# ---------------------------------------------------------------- registry ---

$reg = @()

# The one autostart value Relay is allowed to create.
$reg += Get-RegValues 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

# Add/Remove Programs (per-user; a per-user install must never appear in HKLM).
$reg += Get-RegTree 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Relay'
$reg += Get-RegTree 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Relay'

# The virtual camera's COM registration, both views.
foreach ($root in 'HKLM:\SOFTWARE\Classes\CLSID', 'HKLM:\SOFTWARE\WOW6432Node\Classes\CLSID',
                  'HKCU:\SOFTWARE\Classes\CLSID') {
    $reg += Get-RegTree "$root\$VCAM_CLSID"
}

# Anything at all registered under a Relay-shaped name.
foreach ($root in 'HKCU:\Software\Relay', 'HKLM:\SOFTWARE\Relay',
                  'HKLM:\SOFTWARE\Classes\Relay.Camera') {
    $reg += Get-RegTree $root
}

# Every render endpoint's FX property store. This is brief risk #2 -- the one
# place a bad uninstall breaks the user's audio -- so all of it is captured,
# not just the endpoint Relay chose.
$mm = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render'
if (Test-Path $mm) {
    foreach ($ep in (Get-ChildItem $mm -ErrorAction SilentlyContinue)) {
        $reg += Get-RegTree "$($ep.PSPath)\FxProperties"
        # Properties carries the endpoint's friendly name and, on some
        # machines, effect flags an APO install could disturb.
        $reg += Get-RegValues "$($ep.PSPath)\Properties"
    }
}

# The frame-server camera allow-list the virtual camera registers into.
$reg += Get-RegTree 'HKLM:\SOFTWARE\Microsoft\Windows Media Foundation\Platform\VirtualCamera'

# ------------------------------------------------------------------- files ---

if (-not $InstallDir) {
    $arp = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Relay' -ErrorAction SilentlyContinue
    # NSIS writes this value with the quotes included.
    if ($arp -and $arp.InstallLocation) { $InstallDir = $arp.InstallLocation.Trim('"') }
}

$fileRoots = @(
    $DataRoot,
    $InstallDir,
    "$env:APPDATA\Relay",
    "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Relay",
    "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Relay.lnk",
    "$env:PROGRAMDATA\Relay",
    "${env:ProgramFiles}\Relay",
    "$env:USERPROFILE\Desktop\Relay.lnk",
    "$env:SystemRoot\System32\relay_apo.dll",
    "$env:SystemRoot\System32\relay_vdevice.dll"
) | Where-Object { $_ } | Select-Object -Unique

$files = @()
foreach ($root in $fileRoots) {
    if (-not (Test-Path -LiteralPath $root)) { continue }
    $item = Get-Item -LiteralPath $root -Force
    if ($item.PSIsContainer) {
        # Relative paths so a snapshot taken before the install dir exists
        # still compares cleanly, and so the diff reads as "what is here".
        foreach ($f in (Get-ChildItem -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue)) {
            $rel = $f.FullName.Substring($root.Length).TrimStart('\')
            # Logs and rendered previews grow while Relay runs; their presence
            # matters, their size does not.
            $volatile = $rel -match '^(logs|previews)\\' -or $rel -match '\.log(\.\d+)?$'
            $files += [pscustomobject]@{
                root  = $root
                path  = $rel
                dir   = [bool]$f.PSIsContainer
                bytes = if ($f.PSIsContainer -or $volatile) { $null } else { $f.Length }
            }
        }
        $files += [pscustomobject]@{ root = $root; path = '.'; dir = $true; bytes = $null }
    } else {
        $files += [pscustomobject]@{ root = $root; path = '.'; dir = $false; bytes = $item.Length }
    }
}

# ---------------------------------------------------------------- services ---

# Relay installs no service and (until the signed mic driver ships) no driver.
# Capturing the audio-class driver list proves it.
$drivers = @()
foreach ($d in (Get-CimInstance Win32_SystemDriver -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -match 'relay' -or $_.PathName -match 'relay' })) {
    $drivers += [pscustomobject]@{ name = $d.Name; path = $d.PathName; state = $d.State }
}
$services = @()
foreach ($s in (Get-Service -ErrorAction SilentlyContinue | Where-Object { $_.Name -match 'relay' })) {
    $services += [pscustomobject]@{ name = $s.Name; status = [string]$s.Status }
}

# Audio endpoints, by name: an uninstall must not change the default device.
$endpoints = @()
$defaultRender = $null
try {
    $defaultRender = (Get-CimInstance -Namespace root\cimv2 -ClassName Win32_SoundDevice -ErrorAction Stop |
        Select-Object -First 1 -ExpandProperty Name)
} catch { $defaultRender = $null }

# ------------------------------------------------------------------- broad ---

$broad = $null
if ($Broad) {
    $dir = Split-Path -Parent (Resolve-Path -LiteralPath (Split-Path -Parent $Out) -ErrorAction SilentlyContinue)
    $exportDir = Join-Path (Split-Path -Parent $Out) ((Split-Path -Leaf $Out) -replace '\.json$', '')
    New-Item -ItemType Directory -Force -Path $exportDir | Out-Null
    $hives = @{
        'HKCU'                = 'HKCU'
        'HKLM-Classes-CLSID'  = 'HKLM\SOFTWARE\Classes\CLSID'
        'HKLM-MMDevices'      = 'HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices'
        'HKLM-MediaFoundation'= 'HKLM\SOFTWARE\Microsoft\Windows Media Foundation'
        'HKLM-Run'            = 'HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run'
    }
    $exports = @()
    foreach ($name in $hives.Keys) {
        $file = Join-Path $exportDir "$name.reg"
        # reg.exe, not Export-Registry: it is the same format the M3b
        # before/after endpoint diff already uses. Native stderr would be a
        # terminating error under ErrorActionPreference 'Stop'.
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try { & reg.exe export $hives[$name] $file /y 2>&1 | Out-Null }
        finally { $ErrorActionPreference = $prev }
        if (Test-Path $file) {
            $exports += [pscustomobject]@{ hive = $hives[$name]; file = $file }
        }
    }
    $index = @()
    foreach ($root in @($env:LOCALAPPDATA, $env:APPDATA, ${env:ProgramFiles}, "${env:ProgramFiles(x86)}",
                        $env:PROGRAMDATA, "$env:SystemRoot\System32\drivers")) {
        if (-not $root -or -not (Test-Path -LiteralPath $root)) { continue }
        foreach ($f in (Get-ChildItem -LiteralPath $root -Recurse -Force -Depth 3 -ErrorAction SilentlyContinue)) {
            $index += $f.FullName
        }
    }
    $broad = [pscustomobject]@{ exports = $exports; file_index = ($index | Sort-Object) }
}

# ------------------------------------------------------------------- write ---

$snapshot = [pscustomobject]@{
    taken_at   = (Get-Date).ToUniversalTime().ToString('o')
    machine    = $env:COMPUTERNAME
    user       = $env:USERNAME
    data_root  = $DataRoot
    install_dir= $InstallDir
    registry   = ($reg | Sort-Object key, name)
    files      = ($files | Sort-Object root, path)
    drivers    = ($drivers | Sort-Object name)
    services   = ($services | Sort-Object name)
    sound_device_first = $defaultRender
    broad      = $broad
}

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
$snapshot | ConvertTo-Json -Depth 8 | Out-File -FilePath $Out -Encoding utf8

Write-Host ("snapshot -> {0}" -f $Out)
Write-Host ("  registry values : {0}" -f @($reg).Count)
Write-Host ("  file entries    : {0}" -f @($files).Count)
Write-Host ("  relay services  : {0}  drivers: {1}" -f @($services).Count, @($drivers).Count)
if ($Broad) { Write-Host ("  broad exports   : {0}" -f @($broad.exports).Count) }
