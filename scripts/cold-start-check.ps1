<#
.SYNOPSIS
  Prove that opening Relay from the Start Menu reaches live state with no core
  running and no terminal involved (S23).

.DESCRIPTION
  The regression this guards: autostart is off by default and the UI never
  started the core, so install -> reboot -> open Relay left every screen
  reporting a dead service with a terminal command as the only remedy.

  Run this straight after a reboot, before opening anything. It checks the
  preconditions (autostart off, nothing of Relay running), launches the real
  Start Menu shortcut -- the same .lnk a user clicks -- and then waits for the
  core to appear and answer on its pipe, timing both.

  It deliberately does NOT start the core itself. If the core comes up, the
  app did it.

.PARAMETER TimeoutSeconds
  How long to wait for the core to answer after the window is launched.

.PARAMETER Json
  Emit the result as one JSON object instead of prose.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\cold-start-check.ps1
#>
[CmdletBinding()]
param(
    [int]$TimeoutSeconds = 40,
    [switch]$Json
)

$ErrorActionPreference = 'Stop'

$install = Join-Path $env:LOCALAPPDATA 'Relay'
$shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Relay.lnk'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

function Running($name) {
    [bool](Get-Process -Name $name -ErrorAction SilentlyContinue)
}

# The core's pipe. Its presence is the real "is it live" signal -- a running
# process that has not finished starting is not yet serving the window.
function PipeUp {
    [bool]((Get-ChildItem '\\.\pipe\' -ErrorAction SilentlyContinue).Name -contains 'relay-core')
}

$result = [ordered]@{
    shortcut          = $shortcut
    shortcut_present  = Test-Path $shortcut
    autostart_present = $false
    core_before       = Running 'relay-core'
    ui_before         = Running 'relay-ui'
    pipe_before       = PipeUp
    launched          = $false
    core_after        = $false
    pipe_after        = $false
    seconds_to_core   = $null
    seconds_to_pipe   = $null
    verdict           = 'not run'
}

try {
    $run = Get-ItemProperty -Path $runKey -ErrorAction Stop
    $result.autostart_present = $null -ne $run.Relay
} catch {
    $result.autostart_present = $false
}

if (-not $result.shortcut_present) {
    $result.verdict = "FAIL: no Start Menu shortcut at $shortcut -- is Relay installed?"
} elseif ($result.autostart_present) {
    $result.verdict = 'FAIL: autostart is ON, so this proves nothing. Turn it off and reboot.'
} elseif ($result.core_before -or $result.pipe_before) {
    $result.verdict = 'FAIL: a core was already running before the window opened.'
} else {
    # Exactly what clicking the Start Menu entry does.
    Start-Process -FilePath $shortcut
    $result.launched = $true

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        if ($null -eq $result.seconds_to_core -and (Running 'relay-core')) {
            $result.seconds_to_core = [math]::Round($sw.Elapsed.TotalSeconds, 2)
        }
        if (PipeUp) {
            $result.seconds_to_pipe = [math]::Round($sw.Elapsed.TotalSeconds, 2)
            break
        }
        Start-Sleep -Milliseconds 250
    }
    $sw.Stop()

    $result.core_after = Running 'relay-core'
    $result.pipe_after = PipeUp
    $result.verdict = if ($result.core_after -and $result.pipe_after) {
        "PASS: the app started the core by itself in $($result.seconds_to_pipe)s. No terminal involved."
    } else {
        'FAIL: the window opened but no core came up within the timeout.'
    }
}

if ($Json) {
    $result | ConvertTo-Json -Compress
} else {
    foreach ($k in $result.Keys) { '{0,-18} {1}' -f $k, $result[$k] }
}

if ($result.verdict -like 'FAIL*') { exit 1 }
