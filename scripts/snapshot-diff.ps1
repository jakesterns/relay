<#
.SYNOPSIS
  Diff two machine snapshots and decide whether the uninstall kept Relay's
  promise.

.DESCRIPTION
  Compares registry values, files, services and drivers between a "before"
  and an "after" snapshot from machine-snapshot.ps1.

  The verdict is PASS only when the difference is empty, or empty except for
  entries under the data root when the uninstall was run with the data folder
  kept (-KeepData). Anything else -- a leftover registry value, a changed FX
  property store, a stray file -- is a FAIL, because that is precisely the
  thing the product promises does not happen.

  Exits 0 on PASS, 1 on FAIL, so it can gate CI or a VM script.

.PARAMETER Before
  Snapshot taken before the install.

.PARAMETER After
  Snapshot taken after the uninstall.

.PARAMETER KeepData
  The uninstall was told to keep %LOCALAPPDATA%\Relay, so entries under the
  data root are expected and are reported as "allowed" rather than failures.

.PARAMETER Out
  Optional path for a Markdown summary to paste into the plan file.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Before,
    [Parameter(Mandatory = $true)][string]$After,
    [switch]$KeepData,
    [string]$Out
)

$ErrorActionPreference = 'Stop'

$b = Get-Content -LiteralPath $Before -Raw | ConvertFrom-Json
$a = Get-Content -LiteralPath $After  -Raw | ConvertFrom-Json

function Key-Registry { param($e) "{0}||{1}" -f $e.key, $e.name }
function Key-File     { param($e) "{0}||{1}" -f $e.root, $e.path }

function Compare-Set {
    param($BeforeItems, $AfterItems, [scriptblock]$KeyOf, [string[]]$Compare)

    # An empty section round-trips through Windows PowerShell 5.1's JSON as a
    # single property-less placeholder rather than an empty array. Left in, it
    # keys the map on "||" and reports a phantom entry on both sides; keyed on
    # $null it throws "array index evaluated to null". Drop anything with no
    # properties, and anything that yields no key.
    function Index-By {
        param($Items, [scriptblock]$Of)
        $map = @{}
        foreach ($e in @($Items)) {
            if ($null -eq $e -or $e -is [string]) { continue }
            if (@($e.PSObject.Properties).Count -eq 0) { continue }
            $k = & $Of $e
            if ([string]::IsNullOrEmpty($k)) { continue }
            $map[$k] = $e
        }
        $map
    }

    $bi = Index-By $BeforeItems $KeyOf
    $ai = Index-By $AfterItems $KeyOf
    $script:LastBeforeCount = $bi.Count
    $script:LastAfterCount = $ai.Count
    $diffs = @()
    foreach ($k in $ai.Keys) {
        if (-not $bi.ContainsKey($k)) {
            $diffs += [pscustomobject]@{ change = 'added'; key = $k; detail = ($ai[$k] | ConvertTo-Json -Compress) }
        } else {
            foreach ($prop in $Compare) {
                if ("$($bi[$k].$prop)" -ne "$($ai[$k].$prop)") {
                    $diffs += [pscustomobject]@{
                        change = 'changed'; key = $k
                        detail = "$prop : '$($bi[$k].$prop)' -> '$($ai[$k].$prop)'"
                    }
                    break
                }
            }
        }
    }
    foreach ($k in $bi.Keys) {
        if (-not $ai.ContainsKey($k)) {
            $diffs += [pscustomobject]@{ change = 'removed'; key = $k; detail = ($bi[$k] | ConvertTo-Json -Compress) }
        }
    }
    $diffs | Sort-Object change, key
}

# Counts come back from Compare-Set rather than being recounted here, so the
# summary table can never disagree with the comparison it is summarising.
$counts = @{}
function Compare-Section {
    param([string]$Name, $BeforeItems, $AfterItems, [scriptblock]$KeyOf, [string[]]$Compare)
    $d = Compare-Set $BeforeItems $AfterItems $KeyOf $Compare
    $counts[$Name] = @{ before = $script:LastBeforeCount; after = $script:LastAfterCount }
    $d
}

$regDiff = Compare-Section 'registry' $b.registry $a.registry { param($e) Key-Registry $e } @('value', 'kind')
$fileDiff = Compare-Section 'files'   $b.files    $a.files    { param($e) Key-File $e }     @('bytes', 'dir')
$svcDiff = Compare-Section 'services' $b.services $a.services { param($e) $e.name }         @('status')
$drvDiff = Compare-Section 'drivers'  $b.drivers  $a.drivers  { param($e) $e.name }         @('path', 'state')

# The data root is the one thing the user is offered a choice about.
$dataRoot = if ($a.data_root) { $a.data_root } else { $b.data_root }
function Is-DataRoot { param($key) $dataRoot -and $key -like ("{0}*" -f $dataRoot) }

$allowed = @()
$failures = @()
foreach ($d in @($regDiff) + @($fileDiff) + @($svcDiff) + @($drvDiff)) {
    if ($KeepData -and (Is-DataRoot $d.key)) { $allowed += $d } else { $failures += $d }
}

$pass = @($failures).Count -eq 0

# ------------------------------------------------------------------ report ---

$lines = @()
$lines += "### Clean-VM uninstall diff"
$lines += ""
$lines += "| | before | after | diff |"
$lines += "|---|---|---|---|"
$lines += "| registry values | $($counts['registry'].before) | $($counts['registry'].after) | $(@($regDiff).Count) |"
$lines += "| file entries | $($counts['files'].before) | $($counts['files'].after) | $(@($fileDiff).Count) |"
$lines += "| relay services | $($counts['services'].before) | $($counts['services'].after) | $(@($svcDiff).Count) |"
$lines += "| relay drivers | $($counts['drivers'].before) | $($counts['drivers'].after) | $(@($drvDiff).Count) |"
$lines += ""
if ($pass) {
    $lines += if (@($allowed).Count -eq 0) {
        "**PASS -- the diff is empty.** Nothing Relay installed is left on the machine."
    } else {
        "**PASS -- the diff is empty except the data folder the user chose to keep** ($(@($allowed).Count) entries under ``$dataRoot``)."
    }
} else {
    $lines += "**FAIL -- $(@($failures).Count) leftover difference(s):**"
    $lines += ""
    foreach ($f in ($failures | Select-Object -First 40)) {
        $lines += "- ``$($f.change)`` $($f.key) -- $($f.detail)"
    }
    if (@($failures).Count -gt 40) { $lines += "- ...and $(@($failures).Count - 40) more" }
}
if (@($allowed).Count -gt 0) {
    $lines += ""
    $lines += "<details><summary>Kept by choice ($(@($allowed).Count) entries under the data root)</summary>"
    $lines += ""
    foreach ($x in ($allowed | Select-Object -First 20)) { $lines += "- ``$($x.change)`` $($x.key)" }
    $lines += ""
    $lines += "</details>"
}

$report = $lines -join "`n"
Write-Host $report
if ($Out) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
    $report | Out-File -FilePath $Out -Encoding utf8
    Write-Host ""
    Write-Host "report -> $Out"
}

# Explicit both ways: without an exit on the success path, $LASTEXITCODE keeps
# whatever the last native command left behind and the caller reads a stale
# failure.
if ($pass) { exit 0 } else { exit 1 }
