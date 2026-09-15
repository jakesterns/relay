<#
.SYNOPSIS
  Inspect, clean and create Windows Firewall rules for relay-share.exe.

.DESCRIPTION
  relay-share.exe is the only Relay binary that touches the network (WebRTC on
  the LAN plus mDNS discovery); relay-core talks over a named pipe and never
  opens a socket. Windows prompts the first time a given *path* listens, and
  dismissing that prompt writes a Block rule. Because every worktree and every
  build profile is a different path, the prompt comes back, and the Block rules
  accumulate -- which looks like "Relay keeps getting blocked".

  -List   show current state (no elevation needed)
  -Clean  delete every Block rule for relay-share.exe
  -Allow  add inbound Allow rules for the installed app and every worktree

  -Clean and -Allow change firewall policy and need an elevated shell. This
  script refuses to guess: it tells you what it would change and stops if it is
  not elevated.

.PARAMETER Profiles
  Which firewall profiles the Allow rules apply to. Default Private, because
  Relay is LAN-only by design and has no business being reachable on a public
  network.

.EXAMPLE
  pwsh scripts/firewall-rules.ps1 -List
  # then, from an elevated terminal:
  pwsh scripts/firewall-rules.ps1 -Clean -Allow
#>
[CmdletBinding()]
param(
    [switch]$List,
    [switch]$Clean,
    [switch]$Allow,
    [string]$Profiles = 'Private'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$TAG = 'Relay (relay-share)'

function Test-Elevated {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Every relay-share.exe that could ever listen: the installed copy, this repo,
# and each linked worktree, in both build profiles.
function Get-RelayBinaries {
    $paths = New-Object System.Collections.Generic.List[string]
    $installed = Join-Path $env:LOCALAPPDATA 'Relay\relay-share.exe'
    $paths.Add($installed)

    $trees = @($repo)
    Push-Location $repo
    try {
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        $wt = & git worktree list --porcelain 2>$null
        $ErrorActionPreference = $prev
    } finally { Pop-Location }
    foreach ($line in $wt) {
        if ($line -match '^worktree\s+(.+)$') { $trees += $Matches[1] }
    }

    foreach ($t in ($trees | Sort-Object -Unique)) {
        $t = $t -replace '/', '\'
        foreach ($cfg in 'release', 'debug') {
            $paths.Add((Join-Path $t "target\$cfg\relay-share.exe"))
        }
    }
    $paths | Sort-Object -Unique
}

function Get-RelayRules {
    Get-NetFirewallRule -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -like '*relay-share*' -or $_.DisplayName -eq $TAG }
}

if (-not ($List -or $Clean -or $Allow)) { $List = $true }

if ($List) {
    $rules = @(Get-RelayRules)
    Write-Host ("existing relay rules: {0}" -f $rules.Count)
    $rules | Group-Object Action | ForEach-Object {
        Write-Host ("  {0,-6} x{1}" -f $_.Name, $_.Count)
    }
    Write-Host ''
    Write-Host 'binaries that may listen (exists / missing):'
    foreach ($b in Get-RelayBinaries) {
        Write-Host ("  [{0}] {1}" -f $(if (Test-Path $b) { 'x' } else { ' ' }), $b)
    }
    Write-Host ''
    $prof = Get-NetFirewallProfile -PolicyStore ActiveStore |
        Select-Object Name, Enabled, DefaultInboundAction
    $prof | ForEach-Object {
        Write-Host ("  profile {0,-8} enabled={1,-6} inbound={2}" -f $_.Name, $_.Enabled, $_.DefaultInboundAction)
    }
    if (-not ($Clean -or $Allow)) { exit 0 }
}

if (($Clean -or $Allow) -and -not (Test-Elevated)) {
    Write-Host ''
    Write-Host 'This needs an elevated terminal. Nothing was changed.'
    Write-Host 'Open PowerShell as Administrator, then run:'
    Write-Host ("  cd `"{0}`"" -f $repo)
    Write-Host ('  powershell -NoProfile -File scripts\firewall-rules.ps1 -Clean -Allow')
    exit 1
}

if ($Clean) {
    $blocks = @(Get-RelayRules | Where-Object { $_.Action -eq 'Block' })
    if ($blocks.Count -eq 0) {
        Write-Host 'no Block rules to remove'
    } else {
        foreach ($b in $blocks) { Remove-NetFirewallRule -Name $b.Name }
        Write-Host ("removed {0} Block rule(s)" -f $blocks.Count)
    }
}

if ($Allow) {
    $profileList = $Profiles -split ',' | ForEach-Object { $_.Trim() }
    $added = 0
    foreach ($bin in Get-RelayBinaries) {
        if (-not (Test-Path $bin)) { continue }
        $existing = Get-NetFirewallRule -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -eq $TAG } |
            Where-Object {
                ($_ | Get-NetFirewallApplicationFilter).Program -ieq $bin -and $_.Action -eq 'Allow'
            }
        if ($existing) { continue }
        foreach ($proto in 'TCP', 'UDP') {
            New-NetFirewallRule -DisplayName $TAG -Direction Inbound -Action Allow `
                -Program $bin -Protocol $proto -Profile $profileList `
                -Description 'Relay LAN share (WebRTC + mDNS). Added by scripts/firewall-rules.ps1.' | Out-Null
        }
        $added++
        Write-Host ("allowed {0}" -f $bin)
    }
    if ($added -eq 0) { Write-Host 'nothing to add (all present binaries already allowed)' }
}
