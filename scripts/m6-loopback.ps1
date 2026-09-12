# M6 loopback measurement: run a full send->recv share on this PC and record
# sender/receiver stats, optionally with recording + replay buffer on.
# Usage: powershell -File scripts\m6-loopback.ps1 [-Record] [-Secs 60] [-OutDir path]
param(
    [switch]$Record,
    [switch]$SwitchTest,
    [int]$Secs = 60,
    [string]$OutDir = "$env:TEMP\relay-m6-loopback"
)

$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot '..\target\release\relay-share.exe'
if (-not (Test-Path $exe)) { throw "build first: cargo build --release -p relay-capture" }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$tag = if ($Record) { 'rec-on' } else { 'rec-off' }
$recvLog = Join-Path $OutDir "recv-$tag.ndjson"
$sendLog = Join-Path $OutDir "send-$tag.ndjson"
$recDir = Join-Path $OutDir "recordings-$tag"

# Receiver, headless, fixed code.
$recvPsi = New-Object System.Diagnostics.ProcessStartInfo
$recvPsi.FileName = $exe
$recvPsi.Arguments = 'recv --headless --code 424242 --name m6loop'
$recvPsi.RedirectStandardOutput = $true
$recvPsi.RedirectStandardError = $true
$recvPsi.UseShellExecute = $false
$recv = [System.Diagnostics.Process]::Start($recvPsi)
# First line is the `waiting` event with the signalling port; use the
# explicit address so a stale mDNS advertisement can never misroute us.
$waiting = $recv.StandardOutput.ReadLine()
if ($waiting -notmatch '"port":(\d+)') { throw "receiver did not report a port: $waiting" }
$port = $Matches[1]
$recvOut = $recv.StandardOutput.ReadToEndAsync()
$recvErr = $recv.StandardError.ReadToEndAsync()
Start-Sleep -Seconds 1

# Sender with stdin under our control.
$args = "send --code 424242 --peer 127.0.0.1:$port"
if ($Record) { $args += " --record-dir `"$recDir`" --record --replay-secs 60" }
$sendPsi = New-Object System.Diagnostics.ProcessStartInfo
$sendPsi.FileName = $exe
$sendPsi.Arguments = $args
$sendPsi.RedirectStandardInput = $true
$sendPsi.RedirectStandardOutput = $true
$sendPsi.RedirectStandardError = $true
$sendPsi.UseShellExecute = $false
# PowerShell's default stdin writer emits a BOM on the first line; the engine
# strips it (command::parse_line), so no encoding override is needed here.
if ($env:RELAY_LOG) { $sendPsi.EnvironmentVariables['RELAY_LOG'] = $env:RELAY_LOG }
$send = [System.Diagnostics.Process]::Start($sendPsi)
$sendOut = $send.StandardOutput.ReadToEndAsync()
$sendErr = $send.StandardError.ReadToEndAsync()

Write-Host "sharing for $Secs s ($tag)..."
if ($SwitchTest) {
    Start-Sleep -Seconds ([math]::Max(5, [int]($Secs / 3)))
    Write-Host 'switching to a region...'
    $send.StandardInput.WriteLine('{"cmd":"switch","target":{"kind":"region","display":0,"x":100,"y":100,"w":1280,"h":720}}')
    $send.StandardInput.Flush()
    Start-Sleep -Seconds ([math]::Max(5, [int]($Secs / 3)))
    Write-Host 'switching back to display 0...'
    $send.StandardInput.WriteLine('{"cmd":"switch","target":{"kind":"display","index":0}}')
    $send.StandardInput.Flush()
    Start-Sleep -Seconds ([math]::Max(5, $Secs - 2 * [int]($Secs / 3)))
} else {
    Start-Sleep -Seconds $Secs
}

if ($Record) {
    Write-Host 'saving replay...'
    $send.StandardInput.WriteLine('{"cmd":"replay_save"}')
    $send.StandardInput.Flush()
    Start-Sleep -Seconds 5
}

$send.StandardInput.WriteLine('stop')
$send.StandardInput.Flush()
if (-not $send.WaitForExit(10000)) { $send.Kill() }
if (-not $recv.WaitForExit(10000)) { $recv.Kill() }

$sendOut.Result | Out-File -Encoding utf8 $sendLog
@($waiting) + ($recvOut.Result -split "`n") | Out-File -Encoding utf8 $recvLog
$sendErr.Result | Out-File -Encoding utf8 (Join-Path $OutDir "send-$tag.stderr.log")
$recvErr.Result | Out-File -Encoding utf8 (Join-Path $OutDir "recv-$tag.stderr.log")

Write-Host "--- receiver summary ---"
Get-Content $recvLog | Select-String '"summary"'
Write-Host "--- last sender stats ---"
(Get-Content $sendLog | Select-String '"stats"' | Select-Object -Last 3).Line
Write-Host "--- recording / replay events ---"
Get-Content $sendLog | Select-String 'recording|replay_saved|"error"' | ForEach-Object { $_.Line }
if ($Record -and (Test-Path $recDir)) {
    Get-ChildItem $recDir | Format-Table Name, @{n='MB';e={[math]::Round($_.Length/1MB,1)}}
}
Write-Host "logs in $OutDir"
