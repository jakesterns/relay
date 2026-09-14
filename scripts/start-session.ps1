<#
.SYNOPSIS
  Launch one or more sessions from docs/plans/SESSIONS.md as background
  Claude Code sessions, each in its own worktree.

.DESCRIPTION
  Reads the kickoff prompt straight out of SESSIONS.md so there is exactly one
  copy of it -- edit the catalogue, not this script. Starts each session with
  `claude --bg`, which returns immediately and prints a short id; `claude
  attach <id>` opens it in a terminal, `claude agents` lists them, `claude stop
  <id>` ends one.

  Feature trees get RELAY_NO_INSTALL=1 so eight sessions do not fight over the
  one installed app in %LOCALAPPDATA%\Relay. The main tree is left alone, so
  whatever you run there still updates the installed copy.

  Sessions are staggered: a Rust release build is not something to start eight
  of at the same instant.

.PARAMETER Session
  Session ids to start, e.g. S1 S5 S9. Case-insensitive.

.PARAMETER All
  Start every session that has a worktree and a kickoff prompt (S1-S8).
  Validation and blocked sessions are deliberately excluded -- they need you at
  the keyboard or an external unblock, so launching them unattended wastes
  tokens. Name them explicitly if you want them anyway.

.PARAMETER DryRun
  Print what would be started, including the resolved prompt, and exit.

.PARAMETER PermissionMode
  Passed to `claude --permission-mode`. Default 'acceptEdits' lets a session
  edit files in its own worktree without prompting, but still asks before
  running commands. Use 'manual' to approve everything, 'auto' to let it run
  commands too.

.PARAMETER Model
  Passed to `claude --model`. Defaults to whatever your config uses.

.PARAMETER StaggerSeconds
  Delay between launches. Default 20.

.EXAMPLE
  pwsh scripts/start-session.ps1 -Session S5 -DryRun
  pwsh scripts/start-session.ps1 -Session S1 S2 S3
  pwsh scripts/start-session.ps1 -All
#>
[CmdletBinding()]
param(
    # Comma-separated: -Session S1,S2,S3. Space separation does not work
    # through `powershell -File` -- the second value binds positionally to the
    # next parameter instead, which surfaces as a confusing ValidateSet error
    # about PermissionMode. Elements are split on commas below so both
    # "S1,S2" and @('S1','S2') behave the same.
    [string[]]$Session,
    [switch]$All,
    [switch]$DryRun,
    [ValidateSet('manual', 'acceptEdits', 'auto', 'plan', 'dontAsk', 'bypassPermissions')]
    [string]$PermissionMode = 'acceptEdits',
    [string]$Model,
    [int]$StaggerSeconds = 20
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$catalogue = Join-Path $repo 'docs\plans\SESSIONS.md'
if (-not (Test-Path $catalogue)) { throw "catalogue not found: $catalogue" }

# Sessions that are safe to start unattended: they have a worktree and no
# external blocker. Everything else you start by name, on purpose.
$autoStartable = @('S1', 'S2', 'S3', 'S4', 'S5', 'S6', 'S7', 'S8')

<#
  Parse the catalogue. Each session is a "## S<n> ..." heading followed by a
  "**Branch** `x` ... **Worktree** `y`" line and, later, a fenced block under
  "### Kickoff prompt". Sections without a fenced prompt (the v1.1 sketches)
  are returned with an empty prompt so the caller can say why they are skipped.
#>
function Read-Catalogue($path) {
    $lines = [System.IO.File]::ReadAllLines($path, [System.Text.Encoding]::UTF8)
    $found = [ordered]@{}
    $id = $null
    $inPrompt = $false
    $inFence = $false

    foreach ($line in $lines) {
        if ($line -match '^##\s+(S\d+)\b\s*(.*)$') {
            $id = $Matches[1]
            # Trim the separator and any trailing metadata from the title.
            $title = ($Matches[2] -replace '^[^A-Za-z0-9]+', '').Trim()
            $found[$id] = [pscustomobject]@{
                Id = $id; Title = $title; Branch = $null; Tree = $null; Prompt = @()
            }
            $inPrompt = $false
            $inFence = $false
            continue
        }
        if (-not $id) { continue }

        if ($line -match '\*\*Branch\*\*\s+`([^`]+)`') { $found[$id].Branch = $Matches[1] }
        if ($line -match '\*\*Worktree\*\*\s+`([^`]+)`') {
            $found[$id].Tree = $Matches[1]
        } elseif ($line -match '\*\*Worktree\*\*\s+main tree') {
            $found[$id].Tree = 'MAIN'
        }

        if ($line -match '^###\s+Kickoff prompt') { $inPrompt = $true; continue }
        if ($inPrompt) {
            if ($line -match '^```') {
                if ($inFence) { $inPrompt = $false; $inFence = $false } else { $inFence = $true }
                continue
            }
            if ($inFence) { $found[$id].Prompt += $line }
        }
    }
    return $found
}

$catalog = Read-Catalogue $catalogue

if ($All) {
    $Session = $autoStartable
} elseif (-not $Session) {
    Write-Host 'Sessions in docs/plans/SESSIONS.md:'
    Write-Host ''
    foreach ($s in $catalog.Values) {
        $tree = if ($s.Tree -eq 'MAIN') { '(main tree)' } elseif ($s.Tree) { Split-Path -Leaf $s.Tree } else { '-' }
        $ready = if ($s.Prompt.Count -gt 0) { '' } else { '   no prompt yet' }
        $auto = if ($autoStartable -contains $s.Id) { '*' } else { ' ' }
        '{0} {1,-4} {2,-38} {3,-22}{4}' -f $auto, $s.Id, $s.Title, $tree, $ready
    }
    Write-Host ''
    Write-Host '* = safe to start unattended (-All starts these)'
    Write-Host 'Start one with:  scripts\start-session.ps1 -Session S5'
    exit 0
}

$Session = @($Session | ForEach-Object { $_ -split ',' } | Where-Object { $_ -ne '' })

$started = @()
$first = $true
foreach ($name in $Session) {
    $key = $name.ToUpperInvariant()
    if (-not $catalog.Contains($key)) {
        Write-Warning "$key is not in the catalogue; skipping"
        continue
    }
    $s = $catalog[$key]

    if ($s.Prompt.Count -eq 0) {
        Write-Warning "$key ($($s.Title)) has no kickoff prompt yet -- write its plan file first. Skipping."
        continue
    }
    $tree = if ($s.Tree -eq 'MAIN') { $repo } else { $s.Tree }
    if (-not $tree -or -not (Test-Path $tree)) {
        Write-Warning "$key worktree missing: $tree. Create it with: git worktree add -b $($s.Branch) $tree main"
        continue
    }

    $prompt = ($s.Prompt -join "`n").Trim()

    Write-Host ''
    Write-Host ("=== {0} -- {1}" -f $s.Id, $s.Title)
    Write-Host ("    branch {0}" -f $s.Branch)
    Write-Host ("    tree   {0}" -f $tree)

    if ($DryRun) {
        Write-Host '    --- prompt ---'
        $prompt -split "`n" | ForEach-Object { '    ' + $_ }
        continue
    }

    if (-not $first) { Start-Sleep -Seconds $StaggerSeconds }
    $first = $false

    # Feature trees must not fight over the one installed app.
    $prevNoInstall = $env:RELAY_NO_INSTALL
    if ($s.Tree -ne 'MAIN') { $env:RELAY_NO_INSTALL = '1' }

    $claudeArgs = @('--bg', '--permission-mode', $PermissionMode)
    if ($Model) { $claudeArgs += @('--model', $Model) }
    $claudeArgs += $prompt

    Push-Location $tree
    try {
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        $out = & claude @claudeArgs 2>&1 | Out-String
        $code = $LASTEXITCODE
        $ErrorActionPreference = $prev
    } finally {
        Pop-Location
        $env:RELAY_NO_INSTALL = $prevNoInstall
    }

    Write-Host ($out.Trim())
    if ($code -ne 0) {
        Write-Warning "$key failed to start (exit $code)"
        continue
    }
    $started += $s.Id
}

if ($started.Count -gt 0) {
    Write-Host ''
    Write-Host ("Started: {0}" -f ($started -join ', '))
    Write-Host 'claude agents        list them'
    Write-Host 'claude attach <id>   open one in this terminal'
    Write-Host 'claude stop <id>     end one'
}
