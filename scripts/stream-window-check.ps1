# Enumerate top-level windows of relay-ui / relay-share and describe them.
# Read-only, except -Close (posts WM_CLOSE to RelayReceiver windows) and
# -MoveShell x y w h (SetWindowPos on the shell). Window messages only, never input.
param([switch]$Close, [int[]]$MoveShell)
$ErrorActionPreference = 'Stop'
$src = @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class WE {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint cmd);
  [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtrW(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  public static List<IntPtr> All() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => { list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }
  public static string Cls(IntPtr h) { var sb = new StringBuilder(256); GetClassNameW(h, sb, 256); return sb.ToString(); }
  public static string Title(IntPtr h) { var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256); return sb.ToString(); }
}
'@
Add-Type -TypeDefinition $src
$pids = @{}
Get-Process relay-ui, relay-share -ErrorAction SilentlyContinue | ForEach-Object { $pids[[uint32]$_.Id] = $_.ProcessName }
$out = @()
$shell = [IntPtr]::Zero
foreach ($h in [WE]::All()) {
  $wpid = [uint32]0; [void][WE]::GetWindowThreadProcessId($h, [ref]$wpid)
  if (-not $pids.ContainsKey($wpid)) { continue }
  $cls = [WE]::Cls($h)
  $isRecv = ($cls -eq 'RelayReceiver')
  $isShell = ($pids[$wpid] -eq 'relay-ui' -and [WE]::Title($h) -eq 'Relay')
  if (-not ($isRecv -or $isShell)) { continue }
  $r = New-Object WE+RECT; [void][WE]::GetWindowRect($h, [ref]$r)
  $style = [int64][WE]::GetWindowLongPtrW($h, -16); $ex = [int64][WE]::GetWindowLongPtrW($h, -20)
  $owner = [WE]::GetWindow($h, 4)
  $o = [ordered]@{
    what = $(if ($isRecv) { 'receiver' } else { 'shell' }); hwnd = [int64]$h; pid = $wpid
    title = [WE]::Title($h); owner = [int64]$owner; visible = [WE]::IsWindowVisible($h)
    popup = (($style -band 0x80000000) -ne 0); caption = (($style -band 0x00C00000) -eq 0x00C00000)
    noactivate = (($ex -band 0x08000000) -ne 0); toolwindow = (($ex -band 0x80) -ne 0)
    rect = "$($r.L),$($r.T) $($r.R - $r.L)x$($r.B - $r.T)"
  }
  if ($isShell) {
    $shell = $h
    $p = New-Object WE+POINT; [void][WE]::ClientToScreen($h, [ref]$p)
    $c = New-Object WE+RECT; [void][WE]::GetClientRect($h, [ref]$c)
    $o.client = "$($p.X),$($p.Y) $($c.R)x$($c.B)"
  }
  if ($isRecv -and $Close) { [void][WE]::PostMessageW($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); $o.closed = $true }
  $out += [pscustomobject]$o
}
if ($MoveShell -and $shell -ne [IntPtr]::Zero) {
  [void][WE]::SetWindowPos($shell, [IntPtr]::Zero, $MoveShell[0], $MoveShell[1], $MoveShell[2], $MoveShell[3], 0x0014)
}
Write-Output ("windows: " + $out.Count)
$out | ForEach-Object { Write-Output ($_ | ConvertTo-Json -Compress) }
