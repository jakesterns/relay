<#
.SYNOPSIS
  Prove both close behaviours: the window always frees its own process, and
  the core stops only when the user asked for that (S23).

.DESCRIPTION
  Two runs of the same sequence -- open from the Start Menu, close the window,
  look at what is left:

    keep_running (default)  relay-ui gone, relay-core still up.
    quit_relay              relay-ui gone, relay-core gone too.

  "relay-ui gone" is half the point and the half that is easy to get wrong: a
  window that disappears while its process stays resident is worse than no
  window at all, because the memory it was supposed to give back while you
  game is still held.

  The close is a posted WM_CLOSE to the real top-level window, which is the
  same message the title bar's X produces.

  The preference is set over IPC exactly as the Settings screen sets it, and
  restored to whatever it was when the script started.
#>
[CmdletBinding()]
param([int]$SettleSeconds = 8)

$ErrorActionPreference = 'Stop'
$shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Relay.lnk'
$settings = Join-Path $env:LOCALAPPDATA 'Relay\data\settings.json'

Add-Type -Namespace CloseCheck -Name Win -MemberDefinition @'
public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder s, int n);
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
'@

# The class tao gives a real Tauri window. Matching on it matters: the process
# also owns a top-level, visible, unowned window of class "Tao Thread Event
# Target" -- tao's internal event sink. Closing that is not something the
# title bar's X can do, and doing it wedges the event loop, which looks
# exactly like the app refusing to exit.
$WINDOW_CLASS = 'Tauri Window'

function Up($name) { [bool](Get-Process -Name $name -ErrorAction SilentlyContinue) }

function Stop-Everything {
    Get-Process relay-ui, relay-core -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 2
}

# WM_CLOSE to relay-ui's actual window, and nothing else -- the same message
# the title bar's X produces.
function Close-RelayWindow {
    $pids = @((Get-Process relay-ui -ErrorAction SilentlyContinue).Id)
    if (-not $pids) { return 0 }
    $script:sent = 0
    $cb = [CloseCheck.Win+EnumWindowsProc] {
        param($h, $l)
        $owner = 0
        [void][CloseCheck.Win]::GetWindowThreadProcessId($h, [ref]$owner)
        if ($pids -contains $owner) {
            $cls = New-Object System.Text.StringBuilder 256
            [void][CloseCheck.Win]::GetClassNameW($h, $cls, 256)
            if ($cls.ToString() -eq $WINDOW_CLASS) {
                [void][CloseCheck.Win]::PostMessageW($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
                $script:sent++
            }
        }
        return $true
    }
    [void][CloseCheck.Win]::EnumWindows($cb, [IntPtr]::Zero)
    return $script:sent
}

# One request over the core's pipe. Used to set the preference the same way
# the Settings screen does, rather than by writing the file behind its back.
function Invoke-Core($json) {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'relay-core', [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect(5000)
    try {
        $reader = New-Object System.IO.StreamReader($pipe)
        $writer = New-Object System.IO.StreamWriter($pipe)
        $writer.AutoFlush = $true
        $writer.WriteLine($json)
        return $reader.ReadLine()
    } finally { $pipe.Dispose() }
}

function Test-Close($closeAction, $expectCoreAlive) {
    Write-Host ""
    Write-Host "=== close_action = $closeAction ===" -ForegroundColor Cyan
    Stop-Everything

    Start-Process -FilePath $shortcut
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.Elapsed.TotalSeconds -lt 30 -and -not (Up 'relay-core')) { Start-Sleep -Milliseconds 250 }
    Start-Sleep -Seconds 4   # let the window finish coming up and read its prefs

    if (-not (Up 'relay-core')) { Write-Host 'FAIL: no core came up' -ForegroundColor Red; return $false }

    $reply = Invoke-Core ('{{"id":1,"method":"set_ui_prefs","params":{{"prefs":{{"close_action":"{0}"}}}}}}' -f $closeAction)
    if ($reply -notlike "*$closeAction*") { Write-Host "FAIL: core did not accept the preference: $reply" -ForegroundColor Red; return $false }

    # The window mirrors the preference at startup, so it has to be restarted
    # to pick up one set behind its back. A user changing it in Settings goes
    # through the command, which updates the mirror in place.
    Get-Process relay-ui -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 2
    Start-Process -FilePath $shortcut
    Start-Sleep -Seconds $SettleSeconds

    $uiMem = 0
    $p = Get-Process relay-ui -ErrorAction SilentlyContinue
    if ($p) { $uiMem = [math]::Round(($p | Measure-Object WorkingSet64 -Sum).Sum / 1MB) }
    Write-Host ("before close   relay-ui {0} ({1} MB)   relay-core {2}" -f (Up 'relay-ui'), $uiMem, (Up 'relay-core'))

    $n = Close-RelayWindow
    Write-Host "WM_CLOSE sent to $n window(s)"
    Start-Sleep -Seconds $SettleSeconds

    $uiAlive = Up 'relay-ui'
    $coreAlive = Up 'relay-core'
    Write-Host ("after close    relay-ui {0}   relay-core {1}" -f $uiAlive, $coreAlive)

    $ok = (-not $uiAlive) -and ($coreAlive -eq $expectCoreAlive)
    if ($ok) {
        $tail = if ($expectCoreAlive) { 'and Relay kept running' } else { 'and Relay stopped too' }
        Write-Host "PASS: the window freed its process $tail." -ForegroundColor Green
    } else {
        if ($uiAlive) { Write-Host 'FAIL: relay-ui is still resident after the window closed.' -ForegroundColor Red }
        if ($coreAlive -ne $expectCoreAlive) { Write-Host "FAIL: expected core alive=$expectCoreAlive, got $coreAlive." -ForegroundColor Red }
    }
    return $ok
}

$had = if (Test-Path $settings) { Get-Content $settings -Raw } else { $null }

$a = Test-Close 'keep_running' $true
$b = Test-Close 'quit_relay'  $false

Stop-Everything
if ($null -ne $had) { Set-Content -LiteralPath $settings -Value $had -Encoding utf8 }
else { Remove-Item $settings -Force -ErrorAction SilentlyContinue }
Write-Host "`npreference restored to what it was before this run."

if ($a -and $b) { Write-Host 'BOTH PASS' -ForegroundColor Green; exit 0 }
Write-Host 'FAILED' -ForegroundColor Red
exit 1
