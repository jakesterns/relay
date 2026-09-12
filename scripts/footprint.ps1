<#
.SYNOPSIS
  Release-footprint gate for relay-core.

.DESCRIPTION
  Builds relay-core in release, runs it headless with a temporary data root for
  -Seconds, samples working set and CPU time via Get-Process, and fails if the
  peak working set exceeds -MaxRssMb or average CPU exceeds -MaxCpuPercent
  (percent of one core). Prints the binary size, RSS and CPU so the numbers can
  be pasted into docs/plans/M0-foundation.md.

.EXAMPLE
  pwsh scripts/footprint.ps1
  pwsh scripts/footprint.ps1 -NoBuild -Seconds 10
#>
[CmdletBinding()]
param(
  [double]$MaxRssMb = 10,
  [double]$MaxCpuPercent = 0.5,
  [int]$Seconds = 30,
  [int]$WarmupSeconds = 3,
  [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\release\relay-core.exe"

if (-not $NoBuild) {
  Write-Host "building relay-core (release)..."
  # cargo reports progress on stderr, which Windows PowerShell turns into a
  # terminating error while ErrorActionPreference is 'Stop'. CI runs pwsh,
  # where this does not bite; the exit code is the signal either way.
  $prev = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try { & cargo build --release -p relay-core 2>&1 | ForEach-Object { Write-Host "  $_" } }
  finally { $ErrorActionPreference = $prev }
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
}
if (-not (Test-Path $exe)) { throw "missing $exe" }

$sizeBytes = (Get-Item $exe).Length
$dataDir = Join-Path ([System.IO.Path]::GetTempPath()) ("relay-footprint-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $dataDir | Out-Null
$instance = "footprint-" + $PID
$env:RELAY_INSTANCE = $instance

$stderr = Join-Path $dataDir "stderr.txt"
$p = Start-Process -FilePath $exe -ArgumentList @("run", "--data-dir", "`"$dataDir`"") `
  -PassThru -NoNewWindow -RedirectStandardError $stderr

try {
  Start-Sleep -Seconds $WarmupSeconds
  if ($p.HasExited) {
    Get-Content $stderr -ErrorAction SilentlyContinue | Write-Host
    throw "relay-core exited during warm-up (code $($p.ExitCode))"
  }

  $proc = Get-Process -Id $p.Id
  $cpu0 = $proc.TotalProcessorTime
  $t0 = Get-Date
  $peakRss = 0L
  $samples = 0
  while (((Get-Date) - $t0).TotalSeconds -lt $Seconds) {
    Start-Sleep -Seconds 1
    $proc = Get-Process -Id $p.Id -ErrorAction Stop
    $proc.Refresh()
    if ($proc.WorkingSet64 -gt $peakRss) { $peakRss = $proc.WorkingSet64 }
    $samples++
  }
  $proc.Refresh()
  $privateWs = 0L
  try {
    $perf = Get-CimInstance Win32_PerfRawData_PerfProc_Process -Filter "IDProcess=$($p.Id)" -ErrorAction Stop
    if ($perf) { $privateWs = [long]$perf.WorkingSetPrivate }
  } catch { }
  $cpu1 = $proc.TotalProcessorTime
  $elapsed = ((Get-Date) - $t0).TotalSeconds
  $cpuPercent = (($cpu1 - $cpu0).TotalSeconds / $elapsed) * 100.0
  $finalRss = $proc.WorkingSet64
}
finally {
  if (-not $p.HasExited) {
    & $exe shutdown 2>$null | Out-Null
    if (-not $p.WaitForExit(5000)) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
  }
  Remove-Item $env:RELAY_INSTANCE -ErrorAction SilentlyContinue
  Remove-Item -Recurse -Force $dataDir -ErrorAction SilentlyContinue
}

$sizeMb = [math]::Round($sizeBytes / 1MB, 2)
$peakMb = [math]::Round($peakRss / 1MB, 2)
$finalMb = [math]::Round($finalRss / 1MB, 2)
$cpuRounded = [math]::Round($cpuPercent, 3)

Write-Host ""
Write-Host ("relay-core.exe size : {0} MB ({1} bytes)" -f $sizeMb, $sizeBytes)
Write-Host ("idle RSS (peak/end) : {0} MB / {1} MB over {2} samples" -f $peakMb, $finalMb, $samples)
Write-Host ("private working set : {0} MB (what Task Manager shows)" -f ([math]::Round($privateWs / 1MB, 2)))
Write-Host ("idle CPU            : {0} % of one core over {1:N1} s" -f $cpuRounded, $elapsed)
Write-Host ("FOOTPRINT size_bytes={0} peak_rss_bytes={1} private_ws_bytes={2} cpu_percent={3}" -f $sizeBytes, $peakRss, $privateWs, $cpuRounded)

$failed = $false
if ($peakMb -gt $MaxRssMb) { Write-Host "FAIL: RSS $peakMb MB > $MaxRssMb MB"; $failed = $true }
if ($cpuRounded -gt $MaxCpuPercent) { Write-Host "FAIL: CPU $cpuRounded % > $MaxCpuPercent %"; $failed = $true }
if ($failed) { exit 1 }
Write-Host "PASS: footprint within budget"
exit 0
