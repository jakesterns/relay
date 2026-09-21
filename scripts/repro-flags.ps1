# B12: the RUSTFLAGS that make a release build independent of where the repo
# and the cargo home live and of when it was linked. Dot-source, then set
# $env:CARGO_ENCODED_RUSTFLAGS = Get-ReproRustFlags -Repo <tree>. Used by stage-bundle.ps1 and repro-check.ps1; the why
# of each flag is in docs/dev/reproducible-builds.md.

# Put cmake on PATH if it is installed but not on it.
#
# Setting these flags changes the fingerprint of every crate, which forces a
# rebuild of `opusic-sys` -- and that one builds libopus with cmake. On a
# machine where cmake is installed in its default location but never added to
# PATH, an ordinary `cargo build` succeeds from cache while the *release
# bundle* fails, with an error naming cmake rather than the flags that
# triggered the rebuild. It cost a build cycle to track down here, so find it
# rather than telling the next person to.
function Add-CMakeToPath {
    if (Get-Command cmake -ErrorAction SilentlyContinue) { return $true }
    $candidates = @(
        "$env:ProgramFiles\CMake\bin",
        "${env:ProgramFiles(x86)}\CMake\bin",
        "$env:LOCALAPPDATA\Programs\CMake\bin"
    )
    foreach ($dir in $candidates) {
        if ($dir -and (Test-Path (Join-Path $dir 'cmake.exe'))) {
            $env:PATH = "$dir;$env:PATH"
            Write-Host "  cmake found off PATH, using $dir"
            return $true
        }
    }
    Write-Warning 'cmake not found. opusic-sys builds libopus with it, and the repro flags force that rebuild. Install CMake or add it to PATH.'
    return $false
}

function Get-ReproRustFlags {
    param([Parameter(Mandatory)][string]$Repo)
    $cargoHome = $env:CARGO_HOME
    if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
    $rustup = $env:RUSTUP_HOME
    if (-not $rustup) { $rustup = Join-Path $env:USERPROFILE '.rustup' }
    $repoFull = (Resolve-Path $Repo).Path.TrimEnd('\')
    # Joined with 0x1f for CARGO_ENCODED_RUSTFLAGS, not spaces for RUSTFLAGS:
    # the main checkout lives in a folder with a space in its name.
    return @(
        "--remap-path-prefix=$repoFull=relay",
        "--remap-path-prefix=$cargoHome=cargo",
        "--remap-path-prefix=$rustup=rustup",
        '-Clink-arg=/Brepro'
    ) -join [string][char]0x1f
}
