<#
.SYNOPSIS
  Generates licenses.html: Relay's own third-party notices, every shipped Rust
  crate's licence (cargo-about) and every production npm package's licence
  (pnpm licenses). stage-bundle.ps1 calls this and the installer ships it.

.PARAMETER Out
  Output file. Default: target\licenses\licenses.html.

.PARAMETER Check
  Also run `cargo deny check licenses`, which fails on GPL/AGPL/unknown.

  Needs cargo-about 0.9.2 (and cargo-deny 0.20.2 for -Check) on PATH:
    cargo install --locked cargo-deny@0.20.2; cargo install --locked cargo-about@0.9.2 --features cli
#>
param(
    [string]$Out,
    [switch]$Check
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Out) { $Out = Join-Path $repo 'target\licenses\licenses.html' }
New-Item -ItemType Directory -Force (Split-Path -Parent $Out) | Out-Null

# Windows PowerShell 5.1 turns any native stderr line into a terminating error
# under Stop, and cargo writes progress there; judge native tools by exit code.
function Invoke-Native([scriptblock]$cmd) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $cmd 2>&1 | ForEach-Object { "$_" } } finally { $ErrorActionPreference = $prev }
}

function Esc([string]$s) {
    if ($null -eq $s) { return '' }
    return $s.Replace('&', '&amp;').Replace('<', '&lt;').Replace('>', '&gt;')
}

Push-Location $repo
try {
    if ($Check) {
        Invoke-Native { cargo deny --all-features check licenses } | Write-Host
        if ($LASTEXITCODE -ne 0) { throw 'cargo deny check licenses failed' }
    }

    $rustHtml = Join-Path (Split-Path -Parent $Out) 'rust.html'
    Invoke-Native { cargo about generate --all-features --fail -o $rustHtml about.hbs } | Write-Host
    if ($LASTEXITCODE -ne 0) { throw 'cargo about generate failed' }
    $html = [System.IO.File]::ReadAllText($rustHtml)

    # Relay's own notices (SADIE II, Geist, AutoEQ), verbatim.
    $notices = [System.IO.File]::ReadAllText((Join-Path $repo 'THIRD_PARTY_NOTICES.md'))
    $html = $html.Replace('<!-- NOTICES -->', "<h2>Bundled assets</h2>`n<pre>" + (Esc $notices) + "</pre>")

    # npm: production dependencies only (what the UI bundle contains).
    Push-Location (Join-Path $repo 'ui')
    try {
        $prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
        $json = (& pnpm licenses list --prod --json 2>$null) -join "`n"
        $ErrorActionPreference = $prev
        if ($LASTEXITCODE -ne 0) { throw 'pnpm licenses list failed' }
    } finally { Pop-Location }
    $byLicence = $json | ConvertFrom-Json
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append("<h2>npm packages</h2>`n")
    foreach ($lic in $byLicence.PSObject.Properties) {
        foreach ($pkg in @($lic.Value)) {
            $ver = (@($pkg.versions) -join ', ')
            [void]$sb.Append("<h3>" + (Esc $pkg.name) + " " + (Esc $ver) + " (" + (Esc $lic.Name) + ")</h3>`n")
            if ($pkg.homepage) { [void]$sb.Append("<p><a href=`"" + (Esc $pkg.homepage) + "`">" + (Esc $pkg.homepage) + "</a></p>`n") }
            $text = $null
            foreach ($p in @($pkg.paths)) {
                if (-not $p) { continue }
                $f = Get-ChildItem -LiteralPath $p -File -ErrorAction SilentlyContinue |
                    Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING)' } | Select-Object -First 1
                if ($f) { $text = [System.IO.File]::ReadAllText($f.FullName); break }
            }
            if (-not $text) { throw "no licence text found for npm package $($pkg.name)" }
            [void]$sb.Append("<pre>" + (Esc $text) + "</pre>`n")
        }
    }
    $html = $html.Replace('<!-- NPM -->', $sb.ToString())

    [System.IO.File]::WriteAllText($Out, $html, (New-Object System.Text.UTF8Encoding $false))
    Remove-Item $rustHtml
    Write-Host ("licenses.html -> {0} ({1:N0} KB)" -f $Out, ((Get-Item $Out).Length / 1KB))
} finally { Pop-Location }
