<#
.SYNOPSIS
  Check that Relay compiles for macOS, from this Windows machine.

.DESCRIPTION
  There is no Mac on this network, so "compiles for the Apple targets" is the
  bar, not "runs on macOS". This script runs cargo clippy (-D warnings) for
  aarch64-apple-darwin and x86_64-apple-darwin over the whole workspace.

  Two build scripts compile C for those targets (ring, objc2-exception-helper),
  which needs a C compiler that knows the macOS headers. zig does, and installs
  without admin rights: the script pip-installs it under target\apple-cc once,
  builds the scripts\apple-cc shim with rustc, and points cc-rs at the shim.
  Nothing is linked, so no Apple SDK is needed.

  Needs Python (for pip), rustup, and ui\dist (pnpm build) because
  tauri::generate_context! embeds it.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\check-macos.ps1
  powershell -ExecutionPolicy Bypass -File scripts\check-macos.ps1 -Targets aarch64-apple-darwin
#>
[CmdletBinding()]
param(
  [string[]]$Targets = @("aarch64-apple-darwin", "x86_64-apple-darwin"),
  [string]$ZigVersion = "0.16.0"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path $root "target\apple-cc"
$zigDir = Join-Path $work "zig-$ZigVersion"
$zig = Join-Path $zigDir "ziglang\zig.exe"
$shim = Join-Path $work "apple-cc.exe"
$shimAr = Join-Path $work "apple-ar.exe"

# Native tools write progress to stderr, which Windows PowerShell 5.1 turns
# into a terminating error under 'Stop'. The exit code is the signal.
function Invoke-Native([string]$exe, [string[]]$argv) {
  $prev = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try { & $exe @argv 2>&1 | ForEach-Object { Write-Host "  $_" } }
  finally { $ErrorActionPreference = $prev }
  if ($LASTEXITCODE -ne 0) { throw "$exe failed ($LASTEXITCODE)" }
}

New-Item -ItemType Directory -Force -Path $work | Out-Null

if (-not (Test-Path $zig)) {
  Write-Host "installing zig $ZigVersion (pip, no admin)..."
  Invoke-Native "python" @("-m", "pip", "install", "--quiet", "--target", $zigDir, "ziglang==$ZigVersion")
}

$shimSrc = Join-Path $root "scripts\apple-cc\main.rs"
if (-not (Test-Path $shim) -or (Get-Item $shimSrc).LastWriteTime -gt (Get-Item $shim).LastWriteTime) {
  Write-Host "building the apple-cc shim..."
  Invoke-Native "rustc" @("-O", "--edition", "2021", $shimSrc, "-o", $shim)
  Copy-Item -Force $shim $shimAr
}

if (-not (Test-Path (Join-Path $root "ui\dist\index.html"))) {
  throw "ui\dist is missing: run 'pnpm build' in ui\ first (tauri embeds it at compile time)"
}

$env:ZIG = $zig
foreach ($t in $Targets) {
  Invoke-Native "rustup" @("target", "add", $t)
  $key = $t.Replace("-", "_")
  Set-Item -Path "env:CC_$key" -Value $shim
  Set-Item -Path "env:AR_$key" -Value $shimAr
}

foreach ($t in $Targets) {
  Write-Host "cargo clippy --workspace --all-targets --target $t"
  Invoke-Native "cargo" @("clippy", "--workspace", "--all-targets", "--target", $t, "--", "-D", "warnings")
}
Write-Host "PASS: the workspace compiles for $($Targets -join ', ') (compile only; nothing was run on macOS)"
