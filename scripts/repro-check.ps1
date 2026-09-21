<#
.SYNOPSIS
  B12: build the same commit from two different folders and compare the
  shipped binaries byte for byte.

.DESCRIPTION
  Creates a throwaway detached worktree of HEAD next to the repo, builds the
  chosen packages in release in both trees (each into its own target dir,
  so nothing is shared or cached between them), hashes the outputs and
  removes the worktree again. Pass -Flags to apply the reproducibility
  RUSTFLAGS from scripts/repro-flags.ps1; without it this measures what a
  plain `cargo build --release` does.

  Needs cmake on PATH for relay-capture (see docs/dev/reproducible-builds.md).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\repro-check.ps1 -Packages relay-core -Flags
#>
[CmdletBinding()]
param(
    [string[]]$Packages = @('relay-core'),
    [switch]$Flags
)
$ErrorActionPreference = 'Stop'
# `powershell -File` hands an array over as one comma-joined string.
$Packages = @($Packages | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$repo = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$other = Join-Path (Split-Path -Parent $repo) 'relay-repro-tmp'
$env:RELAY_NO_INSTALL = '1'

function Invoke-Native {
    param([scriptblock]$Block, [string]$What)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $lines = @()
    try { & $Block 2>&1 | ForEach-Object { $lines += "$_"; Write-Verbose "$_" } }
    finally { $ErrorActionPreference = $prev }
    if ($LASTEXITCODE -ne 0) {
        $lines | Select-Object -Last 15 | ForEach-Object { Write-Host "  $_" }
        throw "$What failed ($LASTEXITCODE)"
    }
}

function Build-Tree {
    param([string]$Tree, [string]$Label)
    $target = Join-Path $Tree "target\repro-$Label"
    $env:CARGO_TARGET_DIR = $target
    if ($Flags) {
        . (Join-Path $repo 'scripts\repro-flags.ps1')
        Add-CMakeToPath | Out-Null
        $env:CARGO_ENCODED_RUSTFLAGS = Get-ReproRustFlags -Repo $Tree
    } else {
        Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    }
    $cargoArgs = @('build', '--release', '--bins', '--manifest-path', (Join-Path $Tree 'Cargo.toml'))
    foreach ($p in $Packages) { $cargoArgs += @('-p', $p) }
    Push-Location $Tree
    try { Invoke-Native { & cargo @cargoArgs } "cargo build in $Tree" }
    finally { Pop-Location }
    $out = @{}
    Get-ChildItem (Join-Path $target 'release') -File |
        Where-Object { $_.Extension -in '.exe', '.dll' } |
        ForEach-Object { $out[$_.Name] = (Get-FileHash $_.FullName -Algorithm SHA256).Hash }
    return $out
}

if (Test-Path $other) { Invoke-Native { git -C $repo worktree remove --force $other } 'worktree remove' }
Invoke-Native { git -C $repo worktree add --detach $other HEAD } 'worktree add'
try {
    $a = Build-Tree -Tree $repo -Label 'a'
    $b = Build-Tree -Tree $other -Label 'b'
    $same = 0; $diff = 0
    foreach ($name in ($a.Keys | Sort-Object)) {
        if ($a[$name] -eq $b[$name]) {
            $same++; Write-Host ("  same   {0}  {1}" -f $name, $a[$name].Substring(0, 16))
        } else {
            $diff++; Write-Host ("  DIFFER {0}  {1} vs {2}" -f $name, $a[$name].Substring(0, 16), $b[$name].Substring(0, 16))
        }
    }
    Write-Host ("flags={0}: {1} identical, {2} differ" -f [bool]$Flags, $same, $diff)
}
finally {
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    Invoke-Native { git -C $repo worktree remove --force $other } 'worktree remove'
}
if ($diff -gt 0) { exit 1 }
