# B12: the RUSTFLAGS that make a release build independent of where the repo
# and the cargo home live and of when it was linked. Dot-source, then set
# $env:CARGO_ENCODED_RUSTFLAGS = Get-ReproRustFlags -Repo <tree>. Used by stage-bundle.ps1 and repro-check.ps1; the why
# of each flag is in docs/dev/reproducible-builds.md.

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
