# S27 measurement: what does H.264 cost against HEVC, per bit and per frame?
#
# Encodes the same raw 60 fps clips (1440p by default) with the share's own hardware encoders
# (relay-share bench-codec: identical MFT, low-latency CBR, no B-frames, paced
# at 60 fps), at a sweep of target bitrates, for both codecs. Every encode runs
# before any scoring, so the encode-latency figures are taken on a quiet
# machine. Then each bitstream is decoded by ffmpeg and scored against the
# source with PSNR (luma), SSIM and VMAF, and the H.264 bitrate that matches
# HEVC's quality is interpolated in log-bitrate.
#
# Quality is set against the bitrate the encoder actually PRODUCED, not the
# target: CBR undershoots badly on easy content (scrolling text asked for
# 8 Mb/s produced ~1 Mb/s), so the target is not the variable being compared.
#
# Clips (generated with ffmpeg if absent, never committed):
#   scroll   - source code scrolling at 420 px/s: screen-share content
#   fractal  - Mandelbrot zoom: dense detail and continuous motion, a
#              worst case that behaves like fast game footage for an encoder
#
# Usage: powershell -File scripts\codec-quality.ps1 [-Rates 1,2,4,8,16,32] [-OutDir ...]
#        powershell -File scripts\codec-quality.ps1 -Width 3840 -Height 2160 -Clips fractal -Rates 10,20,40,60,80
#
# -Rates and -Clips are comma-separated strings: under `powershell -File` an
# [int[]] parameter receives "10,20,40" as one token and silently becomes the
# single number 102040.
param(
    [string]$Rates = '1,2,4,8,16,32',
    [string]$Clips = 'scroll,fractal',
    [int]$Width = 2560,
    [int]$Height = 1440,
    [string]$OutDir = "$env:TEMP\relay-s27-codec-quality",
    [int]$Frames = 360
)
$RateList = @($Rates -split ',' | ForEach-Object { [int]$_.Trim() })
$ClipList = @($Clips -split ',' | ForEach-Object { $_.Trim() })

$ErrorActionPreference = 'Stop'
$repo = Join-Path $PSScriptRoot '..'
$exe = Join-Path $repo 'target\release\relay-share.exe'
if (-not (Test-Path $exe)) { throw "build first: cargo build --release -p relay-capture" }
New-Item -ItemType Directory -Force $OutDir | Out-Null

function Find-Tool([string]$name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $hit = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\*FFmpeg*" -Recurse -Filter "$name.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($hit) { return $hit.FullName }
    throw "$name not found (install ffmpeg: winget install Gyan.FFmpeg)"
}
$ffmpeg = Find-Tool 'ffmpeg'

# Run a native tool without PowerShell 5.1 turning its stderr into errors.
function Invoke-Native([string]$file, [string[]]$argv) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $out = & $file @argv 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    return [pscustomobject]@{ code = $code; lines = @($out) }
}

$W = $Width; $H = $Height
Push-Location $OutDir
try {
    # ---- clips
    foreach ($clip in $ClipList) {
        $raw = "$clip-${W}x${H}.nv12"
        if (Test-Path $raw) { continue }
        Write-Host "generating $raw ..."
        if ($clip -eq 'scroll') {
            Copy-Item "$env:WINDIR\Fonts\consola.ttf" 'consola.ttf' -Force
            $src = @(Get-Content (Join-Path $repo 'crates\capture\src\transport\sender.rs')) +
                   @(Get-Content (Join-Path $repo 'crates\capture\src\record\mux.rs'))
            $src | Select-Object -First 400 | Set-Content -Encoding ascii 'code.txt'
            $r = Invoke-Native $ffmpeg @('-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', "color=c=0x1e1e1e:s=${W}x9000",
                '-vf', 'drawtext=fontfile=consola.ttf:textfile=code.txt:expansion=none:fontsize=21:fontcolor=0xd4d4d4:x=40:y=20',
                '-frames:v', '1', 'tall.png')
            if ($r.code -ne 0) { throw "drawtext failed: $($r.lines -join ' ')" }
            $r = Invoke-Native $ffmpeg @('-hide_banner', '-loglevel', 'error', '-y', '-loop', '1', '-framerate', '60', '-i', 'tall.png',
                '-vf', "crop=${W}:${H}:0:'t*420',format=nv12", '-frames:v', "$Frames", '-f', 'rawvideo', $raw)
        } elseif ($clip -eq 'fractal') {
            $r = Invoke-Native $ffmpeg @('-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi',
                '-i', "mandelbrot=s=${W}x${H}:r=60:start_scale=3:end_scale=0.3,format=nv12",
                '-frames:v', "$Frames", '-f', 'rawvideo', $raw)
        } else { throw "unknown clip $clip" }
        if ($r.code -ne 0) { throw "clip $clip failed: $($r.lines -join ' ')" }
    }

    # ---- encode everything first
    $runs = @()
    foreach ($clip in $ClipList) {
        foreach ($codec in @('hevc', 'h264')) {
            foreach ($rate in $RateList) {
                $bits = "$clip-${W}x${H}-$codec-$rate.$codec"
                Write-Host "encode $clip $codec $rate Mb/s"
                $r = Invoke-Native $exe @('bench-codec', "$clip-${W}x${H}.nv12", "$W", "$H", $codec, "$rate", $bits)
                $json = $r.lines | Where-Object { $_ -like '{*' } | Select-Object -Last 1
                if (-not $json) { throw "bench-codec produced no result: $($r.lines -join ' ')" }
                $b = $json | ConvertFrom-Json
                if ($b.frames_out -ne $Frames) { throw "$bits encoded $($b.frames_out) of $Frames frames" }
                $runs += [pscustomobject]@{
                    clip = $clip; codec = $codec; target = $rate; bits = $bits; encoder = $b.encoder
                    mbps = [math]::Round($b.produced_mbps, 3)
                    enc_p50 = [math]::Round($b.encode_ms.p50, 2); enc_p99 = [math]::Round($b.encode_ms.p99, 2)
                }
            }
        }
    }

    # ---- score
    foreach ($run in $runs) {
        Write-Host "score $($run.bits)"
        $vmafLog = "$($run.bits).vmaf.json"
        # Both inputs pinned to 60 fps and frame-index timestamps: the raw
        # H.264/HEVC demuxers default to 25 fps, which pairs the wrong frames.
        $graph = "[0:v]settb=1/60,setpts=N,split=3[d1][d2][d3];" +
                 "[1:v]settb=1/60,setpts=N,split=3[r1][r2][r3];" +
                 "[d1][r1]psnr;[d2][r2]ssim;[d3][r3]libvmaf=n_threads=16:log_fmt=json:log_path=$vmafLog"
        $r = Invoke-Native $ffmpeg @('-hide_banner', '-nostats', '-framerate', '60', '-f', $run.codec, '-i', $run.bits,
            '-f', 'rawvideo', '-pix_fmt', 'nv12', '-s', "${W}x${H}", '-framerate', '60', '-i', "$($run.clip)-${W}x${H}.nv12",
            '-lavfi', $graph, '-f', 'null', '-')
        $psnr = ($r.lines | Select-String 'PSNR y:([0-9.]+)').Matches | Select-Object -Last 1
        $ssim = ($r.lines | Select-String 'SSIM Y:([0-9.]+)').Matches | Select-Object -Last 1
        if (-not $psnr -or -not $ssim) { throw "no metrics for $($run.bits): $($r.lines -join ' ')" }
        $vmaf = (Get-Content $vmafLog -Raw | ConvertFrom-Json).pooled_metrics.vmaf.mean
        $run | Add-Member psnr_y ([math]::Round([double]$psnr.Groups[1].Value, 2))
        $run | Add-Member ssim_y ([math]::Round([double]$ssim.Groups[1].Value, 4))
        $run | Add-Member vmaf ([math]::Round([double]$vmaf, 2))
    }
} finally {
    Pop-Location
}

$runs | Format-Table clip, codec, target, mbps, psnr_y, ssim_y, vmaf, enc_p50, enc_p99 -AutoSize

# Bitrate a codec needs for quality q, by linear interpolation of the metric
# over log(bitrate) between the two produced points that bracket q. $null
# outside the measured range: never extrapolated.
function Get-RateFor($points, [string]$metric, [double]$q) {
    $p = @($points | Sort-Object mbps)
    for ($i = 0; $i -lt $p.Count - 1; $i++) {
        $a = [double]$p[$i].$metric; $b = [double]$p[$i + 1].$metric
        $lo = [math]::Min($a, $b); $hi = [math]::Max($a, $b)
        if ($q -ge $lo -and $q -le $hi -and $a -ne $b) {
            $t = ($q - $a) / ($b - $a)
            $la = [math]::Log($p[$i].mbps); $lb = [math]::Log($p[$i + 1].mbps)
            return [math]::Exp($la + $t * ($lb - $la))
        }
    }
    return $null
}

$equal = @()
foreach ($clip in $ClipList) {
    $hevc = $runs | Where-Object { $_.clip -eq $clip -and $_.codec -eq 'hevc' }
    $h264 = $runs | Where-Object { $_.clip -eq $clip -and $_.codec -eq 'h264' }
    foreach ($metric in @('vmaf', 'psnr_y')) {
        foreach ($pt in $hevc) {
            $need = Get-RateFor $h264 $metric ([double]$pt.$metric)
            $equal += [pscustomobject]@{
                clip = $clip; metric = $metric; hevc_mbps = $pt.mbps; quality = $pt.$metric
                h264_mbps = if ($need) { [math]::Round($need, 3) } else { $null }
                extra_pct = if ($need) { [math]::Round(100 * ($need / $pt.mbps - 1), 1) } else { $null }
            }
        }
    }
}
Write-Host ''
Write-Host 'H.264 bitrate for the same quality as each HEVC point (blank = outside the measured H.264 range):'
$equal | Format-Table -AutoSize

$runs | ConvertTo-Json | Set-Content -Encoding ascii (Join-Path $OutDir 'runs.json')
$equal | ConvertTo-Json | Set-Content -Encoding ascii (Join-Path $OutDir 'equal-quality.json')
Write-Host "results in $OutDir"
