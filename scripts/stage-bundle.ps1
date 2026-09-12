<#
.SYNOPSIS
  Build every binary the installer ships and stage it where the Tauri NSIS
  bundler expects to find it.

.DESCRIPTION
  Relay installs seven files into one folder:

    relay-ui.exe        the Tauri shell (the bundle's main binary)
    relay-core.exe      the always-on service
    relay-share.exe     the share engine, spawned per share
    relay-preview.exe   the offline A/B renderer, spawned on demand
    relay_apo.dll       the endpoint APO (only registered if opted in)
    relay_vdevice.dll   the camera media source (only registered if opted in)
    uninstall.exe       written by NSIS

  The three helper exes ride along as Tauri "externalBin" sidecars, which is
  why they need the target triple in the staged filename -- the bundler strips
  it again on install. The two DLLs go through "resources", which drops them
  straight into the install directory; they are cdylibs, not sidecars, and
  the sidecar naming convention only covers executables.

  Both DLLs need their `com` feature (it is the default for those crates, but
  the workspace build takes them with default-features = false through the
  core, so they are built explicitly here).

  The payload is declared in ui/src-tauri/tauri.bundle.conf.json rather than
  the main tauri.conf.json, and that overlay has to be passed to `tauri build`
  explicitly. The reason is CI: tauri-build fails the crate's build script
  when an externalBin is missing, and the staged binaries are build artifacts
  that are not in the repo -- so a plain `cargo build`, `cargo clippy` or
  `cargo test` on a fresh checkout would fail before it compiled anything.
  Keeping packaging config out of the default config means only the packaging
  step needs the payload.

.PARAMETER SkipBuild
  Stage from whatever is already in target\release.
#>
[CmdletBinding()]
param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$release = Join-Path $repo 'target\release'
$staging = Join-Path $repo 'ui\src-tauri\binaries'

# Sidecars are matched by target triple, so read it from the toolchain rather
# than hard-coding x86_64 -- an arm64 build has to stage arm64 names.
$triple = (rustc -vV | Select-String '^host: ' | ForEach-Object { $_.Line -replace '^host: ', '' }).Trim()
if (-not $triple) { throw 'could not read the host target triple from rustc -vV' }
Write-Host "target triple: $triple"

# cargo writes progress to stderr, and Windows PowerShell turns native stderr
# into a terminating error while $ErrorActionPreference is 'Stop'. Exit codes
# are the only signal worth trusting here.
function Invoke-Cargo {
    param([string[]]$CargoArgs, [string]$What)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & cargo @CargoArgs 2>&1 | ForEach-Object { Write-Host "  $_" } }
    finally { $ErrorActionPreference = $prev }
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($What)" }
}

if (-not $SkipBuild) {
    Write-Host 'building relay-core and relay-share (release)'
    Invoke-Cargo @('build', '--release', '-p', 'relay-core', '-p', 'relay-capture', '--bins') 'core/capture'

    Write-Host 'building relay-preview (release, relay-audio dsp feature)'
    Invoke-Cargo @('build', '--release', '-p', 'relay-audio', '--bin', 'relay-preview') 'relay-preview'

    # The two COM cdylibs. `--features com` is explicit so this does not
    # depend on the workspace's default feature resolution.
    Write-Host 'building relay_apo.dll and relay_vdevice.dll (release, com)'
    Invoke-Cargo @('build', '--release', '-p', 'relay-apo', '--features', 'com', '--lib') 'relay-apo'
    Invoke-Cargo @('build', '--release', '-p', 'relay-vdevice', '--features', 'com', '--lib') 'relay-vdevice'
}

New-Item -ItemType Directory -Force -Path $staging | Out-Null

# name in target\release  ->  staged name
$sidecars = @{
    'relay-core.exe'    = "relay-core-$triple.exe"
    'relay-share.exe'   = "relay-share-$triple.exe"
    'relay-preview.exe' = "relay-preview-$triple.exe"
}
$resources = @('relay_apo.dll', 'relay_vdevice.dll')

$missing = @()
foreach ($src in ($sidecars.Keys + $resources)) {
    if (-not (Test-Path (Join-Path $release $src))) { $missing += $src }
}
if ($missing.Count -gt 0) {
    throw "not built: $($missing -join ', ') -- run without -SkipBuild"
}

foreach ($src in $sidecars.Keys) {
    Copy-Item (Join-Path $release $src) (Join-Path $staging $sidecars[$src]) -Force
    Write-Host "  sidecar  $src -> $($sidecars[$src])"
}
foreach ($src in $resources) {
    Copy-Item (Join-Path $release $src) (Join-Path $staging $src) -Force
    Write-Host "  resource $src"
}

Write-Host ''
Write-Host "staged into $staging"
Write-Host 'now run:  cd ui; pnpm tauri build --bundles nsis --config src-tauri/tauri.bundle.conf.json'
