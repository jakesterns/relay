# S2 measurement: what does a second Opus track cost?
#
# Runs the same loopback share with one audio track and with the microphone
# alongside it, alternating and repeating, then prints the receiver's
# capture->arrival percentiles and the sender's encode/CPU figures for each so
# the pair can be read against M4's baseline table. Also runs the audio-only
# packetization bench for each source alone and for both together.
#
# A tone plays through the default endpoint throughout: WASAPI loopback of a
# silent endpoint delivers no packets at all, so a quiet desktop makes the
# program track read zero and measures nothing. Each run asserts it actually
# carried the packets it should have.
#
# -Compare codec (S27) runs the same loopback with HEVC and with H.264
# instead, alternating: the H.264 leg restricts the *receiver* to H.264 via
# RELAY_VIDEO_CODECS, so the sender negotiates down exactly as it would to a
# PC with no HEVC decoder. Each run asserts the codec it actually negotiated.
# The audio benches are skipped in that mode; the tone still plays.
#
# Usage: powershell -File scripts\dual-audio-check.ps1 [-Secs 30] [-Reps 2] [-Compare mic|codec]
param(
    [int]$Secs = 30,
    [int]$Reps = 2,
    [switch]$NoTone,
    [ValidateSet('mic', 'codec')][string]$Compare = 'mic',
    [string]$OutDir = "$env:TEMP\relay-s2-dual-audio"
)

$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot '..\target\release\relay-share.exe'
if (-not (Test-Path $exe)) { throw "build first: cargo build --release -p relay-capture" }
New-Item -ItemType Directory -Force $OutDir | Out-Null

# One second of a quiet 440 Hz sine, then repeated by block copy: a PowerShell
# loop over every sample of a multi-minute file would take longer than the
# measurement it is for.
function New-ToneFile {
    param([string]$Path, [int]$Seconds)
    $rate = 48000
    $one = New-Object byte[] ($rate * 4)  # 16-bit stereo
    for ($i = 0; $i -lt $rate; $i++) {
        $b = [BitConverter]::GetBytes([int16]([math]::Sin(2 * [math]::PI * 440 * $i / $rate) * 3000))
        $one[$i * 4] = $b[0]; $one[$i * 4 + 1] = $b[1]
        $one[$i * 4 + 2] = $b[0]; $one[$i * 4 + 3] = $b[1]
    }
    $data = New-Object byte[] ($one.Length * $Seconds)
    for ($s = 0; $s -lt $Seconds; $s++) {
        [Buffer]::BlockCopy($one, 0, $data, $s * $one.Length, $one.Length)
    }
    $ms = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($ms)
    $w.Write([char[]]'RIFF'); $w.Write([int](36 + $data.Length))
    $w.Write([char[]]'WAVE'); $w.Write([char[]]'fmt '); $w.Write([int]16)
    $w.Write([int16]1); $w.Write([int16]2); $w.Write([int]$rate)
    $w.Write([int]($rate * 4)); $w.Write([int16]4); $w.Write([int16]16)
    $w.Write([char[]]'data'); $w.Write([int]$data.Length); $w.Write($data)
    $w.Flush()
    [System.IO.File]::WriteAllBytes($Path, $ms.ToArray())
    $ms.Dispose()
}

function Invoke-Loopback {
    param([string]$Tag, [bool]$Mic, [string]$ReceiverCodecs = '', [string]$ExpectCodec = '')

    $recvPsi = New-Object System.Diagnostics.ProcessStartInfo
    $recvPsi.FileName = $exe
    $recvPsi.Arguments = 'recv --headless --code 424242 --name s2loop'
    $recvPsi.RedirectStandardOutput = $true
    $recvPsi.RedirectStandardError = $true
    $recvPsi.UseShellExecute = $false
    if ($ReceiverCodecs) { $recvPsi.EnvironmentVariables['RELAY_VIDEO_CODECS'] = $ReceiverCodecs }
    $recv = [System.Diagnostics.Process]::Start($recvPsi)
    $waiting = $recv.StandardOutput.ReadLine()
    if ($waiting -notmatch '"port":(\d+)') { throw "receiver did not report a port: $waiting" }
    $port = $Matches[1]
    $recvOut = $recv.StandardOutput.ReadToEndAsync()
    $recvErr = $recv.StandardError.ReadToEndAsync()
    Start-Sleep -Seconds 1

    $sendArgs = "send --code 424242 --peer 127.0.0.1:$port"
    if ($Mic) { $sendArgs += ' --audio-mic' }
    $sendPsi = New-Object System.Diagnostics.ProcessStartInfo
    $sendPsi.FileName = $exe
    $sendPsi.Arguments = $sendArgs
    $sendPsi.RedirectStandardInput = $true
    $sendPsi.RedirectStandardOutput = $true
    $sendPsi.RedirectStandardError = $true
    $sendPsi.UseShellExecute = $false
    $send = [System.Diagnostics.Process]::Start($sendPsi)
    $sendOut = $send.StandardOutput.ReadToEndAsync()
    $sendErr = $send.StandardError.ReadToEndAsync()

    Write-Host "  sharing for $Secs s ($Tag)..."
    Start-Sleep -Seconds $Secs
    $send.StandardInput.WriteLine('stop')
    $send.StandardInput.Flush()
    if (-not $send.WaitForExit(10000)) { $send.Kill() }
    if (-not $recv.WaitForExit(10000)) { $recv.Kill() }

    $sendLog = Join-Path $OutDir "send-$Tag.ndjson"
    $recvLog = Join-Path $OutDir "recv-$Tag.ndjson"
    $sendOut.Result | Out-File -Encoding utf8 $sendLog
    @($waiting) + ($recvOut.Result -split "`n") | Out-File -Encoding utf8 $recvLog
    $sendErr.Result | Out-File -Encoding utf8 (Join-Path $OutDir "send-$Tag.stderr.log")
    $recvErr.Result | Out-File -Encoding utf8 (Join-Path $OutDir "recv-$Tag.stderr.log")

    $summary = (Get-Content $recvLog | Select-String '"summary"' | Select-Object -Last 1).Line
    if (-not $summary) { throw "no receiver summary for $Tag; see $recvLog" }
    $s = $summary | ConvertFrom-Json
    # Average the steady-state sender stats, skipping the first two ticks
    # (encoder warm-up) so a start-up outlier does not swamp the run.
    $stats = Get-Content $sendLog | Select-String '"event":"stats"' | ForEach-Object { $_.Line | ConvertFrom-Json }
    $steady = $stats | Select-Object -Skip 2
    $last = $steady | Select-Object -Last 1
    $codecLine = (Get-Content $sendLog | Select-String '"event":"codec"' | Select-Object -Last 1).Line
    $codec = if ($codecLine) { ($codecLine | ConvertFrom-Json).codec } else { '' }
    if ($ExpectCodec -and $codec -ne $ExpectCodec) {
        throw "$Tag negotiated '$codec', expected '$ExpectCodec'; see $sendLog"
    }
    [pscustomobject]@{
        tag           = $Tag
        codec         = $codec
        aus           = $s.aus
        arr_p50       = [math]::Round($s.capture_to_arrival_ms.p50, 2)
        arr_p99       = [math]::Round($s.capture_to_arrival_ms.p99, 2)
        enc_mean      = [math]::Round((($steady | Measure-Object encode_ms -Average).Average), 2)
        enc_max       = [math]::Round((($steady | Measure-Object encode_ms -Maximum).Maximum), 2)
        cts_mean      = [math]::Round((($steady | Measure-Object capture_to_send_ms -Average).Average), 2)
        cts_max       = [math]::Round((($steady | Measure-Object capture_to_send_ms -Maximum).Maximum), 2)
        fps           = [math]::Round((($steady | Measure-Object fps -Average).Average), 2)
        cpu           = [math]::Round((($steady | Measure-Object cpu_percent -Average).Average), 2)
        mbps          = [math]::Round((($steady | Measure-Object bitrate_mbps -Average).Average), 2)
        dropped       = $last.dropped
        audio_packets = $last.audio_packets
        mic_packets   = $last.mic_packets
    }
}

$player = $null
if (-not $NoTone) {
    # Long enough for every rep plus setup, so it never runs out mid-run and
    # leaves a silent endpoint (which would measure nothing).
    $toneSecs = ($Secs + 20) * $Reps * 2 + 180
    $tone = Join-Path $OutDir 'tone.wav'
    Write-Host "generating a $toneSecs s tone..."
    New-ToneFile -Path $tone -Seconds $toneSecs
    $player = New-Object System.Media.SoundPlayer $tone
    $player.Load()
    $player.Play()
    Write-Host 'playing a 440 Hz tone through the default endpoint'
    Start-Sleep -Seconds 2
}

if ($Compare -eq 'mic') { Write-Host '=== audio packetization benches ===' }
foreach ($mode in @(if ($Compare -eq 'mic') { '', 'mic', 'dual' })) {
    $label = if ($mode) { $mode } else { 'desktop' }
    # An empty third argument would reach the exe as a source name, so only
    # pass one when there is one. And Windows PowerShell 5.1 turns a native
    # command's stderr into an ErrorRecord under -ErrorAction Stop, so drop
    # to Continue around the call: the bench logs progress on stderr.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    if ($mode) { $out = & $exe bench-audio 10 $mode } else { $out = & $exe bench-audio 10 }
    $ErrorActionPreference = $prev
    $json = $out | Where-Object { $_ -like '{*' }
    $json | Out-File -Encoding utf8 (Join-Path $OutDir "bench-$label.json")
    Write-Host "-- $label"
    Write-Host $json
}

Write-Host ''
$rows = @()
if ($Compare -eq 'codec') {
    Write-Host '=== loopback share, HEVC vs H.264 ==='
    $A = 'hevc'; $B = 'h264'
    for ($r = 1; $r -le $Reps; $r++) {
        Write-Host "rep $r of $Reps"
        $rows += Invoke-Loopback -Tag "hevc-$r" -Mic $false -ExpectCodec 'hevc'
        Start-Sleep -Seconds 2
        $rows += Invoke-Loopback -Tag "h264-$r" -Mic $false -ReceiverCodecs 'h264' -ExpectCodec 'h264'
        Start-Sleep -Seconds 2
    }
} else {
    Write-Host '=== loopback share, one track vs two ==='
    $A = 'single'; $B = 'dual'
    for ($r = 1; $r -le $Reps; $r++) {
        Write-Host "rep $r of $Reps"
        $rows += Invoke-Loopback -Tag "single-$r" -Mic $false
        Start-Sleep -Seconds 2
        $rows += Invoke-Loopback -Tag "dual-$r" -Mic $true
        Start-Sleep -Seconds 2
    }
}
if ($player) { $player.Stop() }

$rows | Format-Table -AutoSize

# Every run must have carried what it claimed, or the comparison is void.
$expected = [int]($Secs * 100 * 0.9)
foreach ($row in $rows) {
    if ($row.audio_packets -lt $expected) {
        Write-Warning "$($row.tag): program track carried $($row.audio_packets) packets, expected ~$($Secs * 100) - was the endpoint silent?"
    }
    if ($row.tag -like 'dual*' -and $row.mic_packets -lt $expected) {
        Write-Warning "$($row.tag): mic track carried $($row.mic_packets) packets, expected ~$($Secs * 100)"
    }
    if ($Compare -eq 'mic' -and $row.tag -like 'single*' -and $row.mic_packets -ne 0) {
        Write-Warning "$($row.tag): single-track run reported $($row.mic_packets) mic packets"
    }
}

function Show-Median {
    param([string]$Field)
    $s = @($rows | Where-Object { $_.tag -like "$A*" } | ForEach-Object { $_.$Field } | Sort-Object)
    $d = @($rows | Where-Object { $_.tag -like "$B*" } | ForEach-Object { $_.$Field } | Sort-Object)
    $sm = $s[[int]([math]::Floor($s.Count / 2))]
    $dm = $d[[int]([math]::Floor($d.Count / 2))]
    Write-Host ("  {0,-9} {1} {2,8}   {3} {4,8}   delta {5,8}" -f $Field, $A, $sm, $B, $dm, [math]::Round($dm - $sm, 2))
}
Write-Host 'medians across reps:'
foreach ($f in @('arr_p50', 'arr_p99', 'enc_mean', 'enc_max', 'cts_mean', 'cts_max', 'cpu', 'fps', 'mbps')) {
    Show-Median -Field $f
}
Write-Host "logs in $OutDir"
