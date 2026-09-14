<#
.SYNOPSIS
  Rebuild Relay and reinstall it on this machine, so the Start Menu copy
  always matches the working tree.

.DESCRIPTION
  Stage the payload -> build the NSIS bundle -> close the running copy ->
  silent install -> relaunch if it was open.

  Called by the post-commit and pre-push hooks in scripts/hooks (wired up with
  `git config core.hooksPath scripts/hooks`), and safe to run by hand.

  The install is per-user and the uninstaller is the same one shipped to
  users, so this exercises the real packaging path on every commit rather
  than only at release time.

.PARAMETER Force
  Reinstall even when the installed copy already matches HEAD.

.PARAMETER NoRelaunch
  Do not reopen the UI afterwards, even if it was running.
#>
[CmdletBinding()]
param([switch]$Force, [switch]$NoRelaunch)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

# State lives in the *common* git dir, not "$repo\.git". In a linked worktree
# .git is a file, so the old path did not exist -- and more importantly, every
# worktree installs over the same %LOCALAPPDATA%\Relay, so they must share one
# lock and one marker or two sessions race to install different builds.
Push-Location $repo
$gitDir = (& git rev-parse --path-format=absolute --git-common-dir).Trim()
Pop-Location
$marker = Join-Path $gitDir 'relay-installed-sha'
$log = Join-Path $gitDir 'relay-install.log'
$lock = Join-Path $gitDir 'relay-install.lock'

# One shared log across every worktree, so the tree has to be named or you
# cannot tell which session installed what.
$tree = Split-Path -Leaf $repo

function Say($m) {
    $line = "[{0}] ({1}) {2}" -f (Get-Date -Format 'HH:mm:ss'), $tree, $m
    Write-Host $line
    Add-Content -LiteralPath $log -Value $line -Encoding utf8
}

# A failed build leaves the previous version installed, which is easy to
# misread as "my change is live and broken". Say which commit is actually in
# the Start Menu, and name the usual cause: this builds the working tree, so
# editing files while a background install runs compiles a half-finished tree.
function Fail($why) {
    $have = if (Test-Path $marker) { (Get-Content $marker -Raw).Trim().Substring(0, 8) } else { 'nothing' }
    Say "FAILED: $why (see $log)"
    Say "still installed: $have -- rerun scripts\install-local.ps1 once the tree builds"
    exit 1
}

# One build at a time: a commit followed straight away by a push would
# otherwise have two builds writing the same staging directory.
$lockStream = $null
try {
    $lockStream = [System.IO.File]::Open($lock, 'OpenOrCreate', 'ReadWrite', 'None')
} catch {
    Write-Host 'relay: an install is already running; skipping'
    exit 0
}

try {
    Set-Location $repo
    $sha = (& git rev-parse HEAD).Trim()
    if (-not $Force -and (Test-Path $marker) -and (Get-Content $marker -Raw).Trim() -eq $sha) {
        Say "already installed at $($sha.Substring(0,8)); nothing to do"
        exit 0
    }
    Say "building $($sha.Substring(0,8))"

    # cargo and pnpm report progress on stderr; Windows PowerShell turns that
    # into a terminating error under ErrorActionPreference 'Stop'.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & (Join-Path $PSScriptRoot 'stage-bundle.ps1') 2>&1 | Out-File -Append -Encoding utf8 $log
    $staged = $LASTEXITCODE
    if ($staged -eq 0) {
        Push-Location (Join-Path $repo 'ui')
        & pnpm tauri build --bundles nsis --config src-tauri/tauri.bundle.conf.json 2>&1 |
            Out-File -Append -Encoding utf8 $log
        $built = $LASTEXITCODE
        Pop-Location
    }
    $ErrorActionPreference = $prev
    if ($staged -ne 0) { Fail "staging exited $staged" }
    if ($built -ne 0) { Fail "bundle exited $built" }

    $setup = Get-ChildItem (Join-Path $repo 'target\release\bundle\nsis') -Filter '*-setup.exe' |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $setup) { Fail 'no installer produced' }

    # The running UI holds relay-ui.exe open, which would block the upgrade.
    # The installer's own hook stops the core (and restores display/audio on
    # the way out), so only the window needs closing here.
    $wasRunning = [bool](Get-Process relay-ui -ErrorAction SilentlyContinue)
    if ($wasRunning) { Get-Process relay-ui | Stop-Process -Force; Start-Sleep -Seconds 1 }

    $p = Start-Process $setup.FullName -ArgumentList '/S' -PassThru
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { Fail "installer exited $($p.ExitCode)" }

    Set-Content -LiteralPath $marker -Value $sha -Encoding ascii
    Say "installed $($sha.Substring(0,8)) -> $env:LOCALAPPDATA\Relay"

    if ($wasRunning -and -not $NoRelaunch) {
        Start-Sleep -Seconds 2
        Start-Process "$env:LOCALAPPDATA\Relay\relay-ui.exe"
        Say 'reopened the window'
    }
} finally {
    if ($lockStream) { $lockStream.Dispose() }
    Remove-Item $lock -Force -ErrorAction SilentlyContinue
}
