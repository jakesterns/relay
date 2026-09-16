<#
.SYNOPSIS
  Exercise the core-owned notification-area icon without touching the mouse.

.DESCRIPTION
  The tray lives on the winloop's hidden window (class `RelayCoreHardware`),
  and Windows delivers clicks to it as `WM_APP+1` with the mouse message in
  the low word of lParam. Posting that message by hand drives exactly the same
  path a real click does -- window procedure, `Tray::on_message`,
  `CoreEvent::Tray`, `Service::on_tray` -- so the plumbing can be proved in a
  headless session.

  It stops short of the right-click menu on purpose: `TrackPopupMenu` is
  modal and needs real input to resolve, so a posted right-click would leave
  the core's winloop blocked on a menu nobody can dismiss. Only the left-click
  ("Open Relay") path is driven here.

.PARAMETER Action
  `find`  report whether the window and therefore the icon host exist.
  `open`  post a left-click, i.e. the tray's "Open Relay".

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\tray-probe.ps1 -Action find
  powershell -ExecutionPolicy Bypass -File scripts\tray-probe.ps1 -Action open
#>
[CmdletBinding()]
param([ValidateSet('find', 'open')][string]$Action = 'find')

$ErrorActionPreference = 'Stop'

Add-Type -Namespace RelayProbe -Name Win -MemberDefinition @'
public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);

[DllImport("user32.dll")]
public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);

[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder s, int n);

[DllImport("user32.dll")]
public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);

[DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
'@

# Must match crates/core/src/winloop.rs and crates/core/src/tray.rs.
$CLASS = 'RelayCoreHardware'
$WM_TRAY = 0x8000 + 1   # WM_APP + 1
$WM_LBUTTONUP = 0x0202

# Found by enumeration rather than FindWindowW, so the window is tied to a
# *running relay-core process* -- a stale window of the right class belonging
# to something else would otherwise pass.
$pids = @((Get-Process -Name relay-core -ErrorAction SilentlyContinue).Id)
if (-not $pids) {
    Write-Output 'core          not running'
    exit 1
}
Write-Output "core          pid $($pids -join ', ')"

$script:hwnd = [IntPtr]::Zero
$cb = [RelayProbe.Win+EnumWindowsProc] {
    param($h, $l)
    $owner = 0
    [void][RelayProbe.Win]::GetWindowThreadProcessId($h, [ref]$owner)
    if ($pids -contains $owner) {
        $sb = New-Object System.Text.StringBuilder 256
        [void][RelayProbe.Win]::GetClassNameW($h, $sb, 256)
        if ($sb.ToString() -eq $CLASS) { $script:hwnd = $h; return $false }
    }
    return $true
}
[void][RelayProbe.Win]::EnumWindows($cb, [IntPtr]::Zero)

if ($script:hwnd -eq [IntPtr]::Zero) {
    Write-Output "window        not found (class $CLASS) -- the tray has no host"
    exit 1
}
$hwnd = $script:hwnd
Write-Output ("window        0x{0:X} (class {1})" -f $hwnd.ToInt64(), $CLASS)

if ($Action -eq 'find') { exit 0 }

$before = [bool](Get-Process -Name relay-ui -ErrorAction SilentlyContinue)
Write-Output "ui before     $before"

# lParam's low word carries the mouse message; wParam is the icon id.
$ok = [RelayProbe.Win]::PostMessageW($hwnd, $WM_TRAY, [IntPtr]1, [IntPtr]$WM_LBUTTONUP)
Write-Output "posted click  $ok"

$sw = [System.Diagnostics.Stopwatch]::StartNew()
while ($sw.Elapsed.TotalSeconds -lt 20) {
    if (Get-Process -Name relay-ui -ErrorAction SilentlyContinue) { break }
    Start-Sleep -Milliseconds 250
}
$sw.Stop()

$after = [bool](Get-Process -Name relay-ui -ErrorAction SilentlyContinue)
Write-Output "ui after      $after"
Write-Output ("seconds       {0}" -f [math]::Round($sw.Elapsed.TotalSeconds, 2))
if ($after) { Write-Output 'PASS: the tray opened the Relay window.' }
else { Write-Output 'FAIL: no window appeared.'; exit 1 }
