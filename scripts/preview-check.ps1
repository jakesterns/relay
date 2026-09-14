<#
.SYNOPSIS
  Run a loopback share with the preview tap on and decode a thumbnail.

.DESCRIPTION
  Proves the preview path end to end without a second PC: start a headless
  receiver, send to it with --preview-fps, collect the `preview` NDJSON
  events, and write the first frame out as a .jpg you can open.

  Reports the per-frame JPEG size, which is the number that decides whether
  this is cheap enough to leave on while sharing.

.PARAMETER Fps
  Preview rate to ask the engine for.

.PARAMETER Secs
  How long to share before stopping.

.PARAMETER Toggle
  Start with the preview off and turn it on mid-share with the `preview`
  stdin command, the way Ctrl+Alt+P does. Proves the toggle, not just the
  start-up flag.
#>
[CmdletBinding()]
param(
    [int]$Fps = 2,
    [int]$Secs = 12,
    [switch]$Toggle,
    [string]$OutDir = "$env:TEMP\relay-preview-check"
)

$ErrorActionPreference = 'Stop'
$exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\relay-share.exe'
if (-not (Test-Path $exe)) { throw "build first: cargo build --release -p relay-capture --bins" }
New-Item -ItemType Directory -Force $OutDir | Out-Null

# Headless receiver on a port it picks for itself.
$recvPsi = New-Object System.Diagnostics.ProcessStartInfo
$recvPsi.FileName = $exe
$recvPsi.Arguments = 'recv --headless --code 424242 --name previewcheck'
$recvPsi.RedirectStandardOutput = $true
$recvPsi.UseShellExecute = $false
$recv = [System.Diagnostics.Process]::Start($recvPsi)
$waiting = $recv.StandardOutput.ReadLine()
$m = [regex]::Match($waiting, '"port":(?<p>\d+)')
if (-not $m.Success) { throw "receiver did not report a port: $waiting" }
$port = $m.Groups['p'].Value
$null = $recv.StandardOutput.ReadToEndAsync()
Start-Sleep -Seconds 1

$sendPsi = New-Object System.Diagnostics.ProcessStartInfo
$sendPsi.FileName = $exe
$startFps = if ($Toggle) { 0 } else { $Fps }
$sendPsi.Arguments = "send --code 424242 --peer 127.0.0.1:$port --preview-fps $startFps"
$sendPsi.RedirectStandardOutput = $true
$sendPsi.RedirectStandardInput = $true
$sendPsi.UseShellExecute = $false
$send = [System.Diagnostics.Process]::Start($sendPsi)

Write-Host "sharing for $Secs s with --preview-fps $startFps ..."
$lines = New-Object System.Collections.Generic.List[string]
$before = 0
$deadline = (Get-Date).AddSeconds($Secs)
$flip = if ($Toggle) { (Get-Date).AddSeconds([int]($Secs / 3)) } else { $null }
while ((Get-Date) -lt $deadline) {
    if ($flip -and (Get-Date) -ge $flip) {
        # Count what arrived while the preview was meant to be off, then turn
        # it on the way the Ctrl+Alt+P hotkey does.
        $before = ($lines | Where-Object { $_ -match '"event":"preview"' }).Count
        $send.StandardInput.WriteLine("{""cmd"":""preview"",""fps"":$Fps}")
        Write-Host "  -> preview turned on mid-share (fps $Fps)"
        $flip = $null
    }
    $l = $send.StandardOutput.ReadLine()
    if ($null -eq $l) { break }
    $lines.Add($l)
}
$send.StandardInput.WriteLine('stop')
Start-Sleep -Seconds 2
if (-not $send.HasExited) { $send.Kill() }
if (-not $recv.HasExited) { $recv.Kill() }

$previews = $lines | Where-Object { $_ -match '"event":"preview"' }
Write-Host ""
Write-Host ("preview events : {0} in {1} s" -f $previews.Count, $Secs)
if ($Toggle) {
    Write-Host ("before toggle  : {0} (must be 0)" -f $before)
    if ($before -ne 0) { throw "preview emitted frames while it was switched off" }
}
if ($previews.Count -eq 0) {
    Write-Host "no preview frames - check that the source produced any frames at all:"
    $lines | Where-Object { $_ -match '"event":"stats"' } | Select-Object -Last 1
    exit 1
}

$sizes = @()
foreach ($p in $previews) { $sizes += ([Convert]::FromBase64String(($p | ConvertFrom-Json).jpeg)).Length }
$first = $previews[0] | ConvertFrom-Json
$bytes = [Convert]::FromBase64String($first.jpeg)
$path = Join-Path $OutDir 'frame.jpg'
[IO.File]::WriteAllBytes($path, $bytes)

$soi = '{0:X2}{1:X2}' -f $bytes[0], $bytes[1]
$eoi = '{0:X2}{1:X2}' -f $bytes[$bytes.Length - 2], $bytes[$bytes.Length - 1]
Write-Host ("thumbnail      : {0}x{1}" -f $first.width, $first.height)
Write-Host ("jpeg markers   : SOI {0} / EOI {1}" -f $soi, $eoi)
Write-Host ("jpeg size      : min {0:N0} / avg {1:N0} / max {2:N0} bytes" -f `
    ($sizes | Measure-Object -Minimum).Minimum,
    ($sizes | Measure-Object -Average).Average,
    ($sizes | Measure-Object -Maximum).Maximum)
Write-Host ("bandwidth      : ~{0:N0} KB/s at {1} fps" -f `
    ((($sizes | Measure-Object -Average).Average * $Fps) / 1KB), $Fps)
Write-Host ("saved          : {0}" -f $path)
if ($soi -ne 'FFD8' -or $eoi -ne 'FFD9') { throw "not a well-formed JPEG" }
