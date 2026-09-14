<#
.SYNOPSIS
  Rebuild the bundled headphone catalogue from AutoEQ's master index.

.DESCRIPTION
  Relay ships a searchable list of headphone models so "Add headset" can be a
  search box instead of a hunt through a GitHub repo. What it ships is the
  *index* -- model names and the path each one lives at -- not the
  measurements. That distinction matters: the measurements are licensed
  CC BY-NC-SA by oratory1990 and others, so Relay never redistributes them.
  The curve for a chosen model is fetched from the source at pick time,
  cached locally, and credited in the UI.

  Output is one tab-separated line per entry, which is a tenth the size of
  JSON and can be searched by scanning without parsing:

      name <TAB> source <TAB> rig <TAB> path

  `path` is relative to the repo's results/ directory and already
  percent-encoded, so the fetch URL is just a concatenation.

  Run this when the upstream index has moved on; the result is committed, so
  a normal build never touches the network.
#>
[CmdletBinding()]
param(
    [string]$IndexUrl = 'https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/INDEX.md',
    [string]$Out
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Out) { $Out = Join-Path $repo 'crates\core\catalog\autoeq-index.tsv' }

Write-Host "fetching $IndexUrl"
$md = (Invoke-WebRequest -Uri $IndexUrl -UseBasicParsing -TimeoutSec 120).Content
Write-Host ("  {0:N0} KB" -f ($md.Length / 1KB))

# - [Name](./source/rig/Name) by SOURCE on RIG
#                                    ^^^^^^ optional
#
# The path is matched greedily rather than as "anything but a bracket": a
# quarter of the models have parentheses in their name -- "1MORE Aero (ANC
# Off)" -- which appear in the link target too, so a lazy match stops at the
# wrong bracket and silently drops them. Greedy plus the required ") by "
# anchors on the real closing bracket.
$line = [regex]'^\s*-\s*\[(?<name>.+?)\]\(\./(?<path>.+)\)\s+by\s+(?<source>.+?)(?:\s+on\s+(?<rig>.+?))?\s*$'

$rows = New-Object System.Collections.Generic.List[string]
$seen = New-Object System.Collections.Generic.HashSet[string]
$skipped = 0
foreach ($l in ($md -split "`n")) {
    $m = $line.Match($l)
    if (-not $m.Success) {
        if ($l.TrimStart().StartsWith('- [')) { $skipped++ }
        continue
    }
    $name = $m.Groups['name'].Value.Trim()
    $path = $m.Groups['path'].Value.Trim()
    $source = $m.Groups['source'].Value.Trim()
    $rig = $m.Groups['rig'].Value.Trim()
    # A tab in a field would corrupt the format; none exist today, but a
    # silent corruption later would be hard to trace.
    if (($name + $source + $rig + $path) -match "`t") { continue }
    $key = "$name|$source|$rig"
    if (-not $seen.Add($key)) { continue }
    $rows.Add(($name, $source, $rig, $path) -join "`t")
}

if ($rows.Count -lt 1000) { throw "only $($rows.Count) entries parsed - the index format probably changed" }
if ($skipped -gt 0) { Write-Host "  warning: $skipped bullet lines did not match the expected shape" }

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
# Sorted so the file diffs cleanly when it is regenerated.
$sorted = $rows | Sort-Object
[IO.File]::WriteAllLines($Out, $sorted, (New-Object Text.UTF8Encoding($false)))

Write-Host ""
Write-Host ("{0:N0} models -> {1}" -f $sorted.Count, $Out)
Write-Host ("  {0:N0} KB" -f ((Get-Item $Out).Length / 1KB))
Write-Host ("  sources: {0}" -f (($sorted | ForEach-Object { ($_ -split "`t")[1] } | Sort-Object -Unique) -join ', '))
