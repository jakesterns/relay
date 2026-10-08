<#
.SYNOPSIS
  Fetch the NDI(R) 6 runtime that Relay's installer bundles, from NDI's own
  download, and verify it against the pins in scripts/ndi-runtime.psd1.

.DESCRIPTION
  NDI(R) is a registered trademark of Vizrt NDI AB. https://ndi.video/

  Relay never commits an NDI file to its repository (NDI SDK licence s2d;
  docs/dev/ndi-licensing.md). The release build gets the runtime here:

    1. download the NDI 6 Runtime installer from NDI (the target of
       http://ndi.link/NDIRedistV6), check its pinned SHA-256 and its
       Authenticode signature (Vizrt AG);
    2. download innoextract (pinned SHA-256) and unpack the installer with it
       -- the installer is never run, so the build machine is not changed;
    3. check each shipped file's pinned SHA-256, and the DLL's signature;
    4. copy them to -Out.

  Any mismatch throws. When -Out already holds files that match every pin,
  nothing is downloaded.

  stage-bundle.ps1 calls this for every build (local, test and release)
  unless it is given -NoNdi.

.PARAMETER Out
  Folder for the verified files. Default: the shared build cache,
  %LOCALAPPDATA%\RelayBuildCache\ndi-runtime\<version> (or under
  $env:RELAY_BUILD_CACHE), so every worktree reuses one download. Never
  %LOCALAPPDATA%\Relay: that is the install folder.

.PARAMETER PrintCacheDir
  Print the default -Out folder and exit.
#>
[CmdletBinding()]
param([string]$Out, [switch]$PrintCacheDir)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$pins = Import-PowerShellDataFile (Join-Path $PSScriptRoot 'ndi-runtime.psd1')
$cacheRoot = if ($env:RELAY_BUILD_CACHE) { $env:RELAY_BUILD_CACHE }
    elseif ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA 'RelayBuildCache' }
    else { Join-Path $repo 'target\build-cache' }
$defaultOut = Join-Path $cacheRoot ('ndi-runtime\' + $pins.Version)
if ($PrintCacheDir) { Write-Output $defaultOut; return }
if (-not $Out) { $Out = $defaultOut }

function Get-Sha256([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-Signed([string]$Path) {
    $sig = Get-AuthenticodeSignature -LiteralPath $Path
    if ($sig.Status -ne 'Valid') { throw "$(Split-Path -Leaf $Path): Authenticode signature is $($sig.Status)" }
    $subject = $sig.SignerCertificate.Subject
    if (-not $subject.StartsWith($pins.Signer)) {
        throw "$(Split-Path -Leaf $Path): signed by '$subject', expected '$($pins.Signer)'"
    }
}

# $true when $Dir holds every pinned file with the pinned hash.
function Test-Pinned([string]$Dir) {
    foreach ($f in $pins.Files) {
        $p = Join-Path $Dir $f.Name
        if (-not (Test-Path -LiteralPath $p)) { return $false }
        if ((Get-Sha256 $p) -ne $f.Sha256) { return $false }
    }
    return $true
}

function Get-File([string]$Url, [string]$Dest, [string]$Sha256, [string]$What) {
    Write-Host "  downloading $What"
    Write-Host "    $Url"
    $prev = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'   # 5.1's progress bar makes this 10x slower
    try { Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing }
    finally { $ProgressPreference = $prev }
    $got = Get-Sha256 $Dest
    if ($got -ne $Sha256) {
        throw ("$What SHA-256 mismatch: got $got, pinned $Sha256. " +
            'If NDI has published a new runtime, update scripts/ndi-runtime.psd1 ' +
            '(docs/dev/ndi-licensing.md, "Updating the bundled runtime"); never skip this check.')
    }
}

if (Test-Pinned $Out) {
    foreach ($f in $pins.Files) { if ($f.Signed) { Assert-Signed (Join-Path $Out $f.Name) } }
    Write-Host "NDI runtime $($pins.Version): verified copy already in $Out"
    return
}

# Windows PowerShell 5.1 may default to TLS 1.0; both hosts need 1.2.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$work = Join-Path ([IO.Path]::GetTempPath()) ("relay-ndi-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $work | Out-Null
try {
    Write-Host "NDI runtime $($pins.Version)"
    $setup = Join-Path $work 'ndi-runtime-setup.exe'
    Get-File $pins.InstallerUrl $setup $pins.InstallerSha256 'NDI 6 Runtime installer'
    Assert-Signed $setup

    $zip = Join-Path $work 'innoextract.zip'
    Get-File $pins.InnoextractUrl $zip $pins.InnoextractSha256 'innoextract'
    $ie = Join-Path $work 'innoextract'
    Expand-Archive -LiteralPath $zip -DestinationPath $ie -Force
    $ieExe = Join-Path $ie 'innoextract.exe'
    if (-not (Test-Path $ieExe)) { throw 'innoextract.exe not found in its release zip' }

    $unpacked = Join-Path $work 'unpacked'
    Write-Host '  unpacking (the installer is not run)'
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $ieExe --silent --output-dir $unpacked $setup 2>&1 | ForEach-Object { Write-Host "    $_" } }
    finally { $ErrorActionPreference = $prev }
    if ($LASTEXITCODE -ne 0) { throw "innoextract failed (exit $LASTEXITCODE)" }

    $app = Join-Path $unpacked 'app'
    foreach ($f in $pins.Files) {
        $p = Join-Path $app $f.Name
        if (-not (Test-Path -LiteralPath $p)) { throw "$($f.Name) is not in the NDI runtime installer" }
        $got = Get-Sha256 $p
        if ($got -ne $f.Sha256) { throw "$($f.Name) SHA-256 mismatch: got $got, pinned $($f.Sha256)" }
        if ($f.Signed) { Assert-Signed $p }
        $ver = (Get-Item -LiteralPath $p).VersionInfo.FileVersion
        if ($f.Signed -and $ver -and ($ver.Trim() -ne $pins.Version)) {
            throw "$($f.Name) is version $ver, pinned $($pins.Version)"
        }
    }

    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    foreach ($f in $pins.Files) {
        Copy-Item -LiteralPath (Join-Path $app $f.Name) -Destination (Join-Path $Out $f.Name) -Force
        Write-Host "  verified $($f.Name)"
    }
    if (-not (Test-Pinned $Out)) { throw "copy to $Out did not verify" }
    Write-Host "NDI runtime $($pins.Version) -> $Out"
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
