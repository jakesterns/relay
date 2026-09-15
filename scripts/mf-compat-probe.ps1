param([Parameter(Mandatory=$true)][string]$Path)

$ErrorActionPreference = 'Continue'

Add-Type -AssemblyName System.Runtime.WindowsRuntime | Out-Null

$ms = [System.WindowsRuntimeSystemExtensions].GetMethods()
$asTaskOp = ($ms | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
$asTaskAct = ($ms | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncAction' })[0]

function Await($op, $resultType) {
  $t = $asTaskOp.MakeGenericMethod($resultType).Invoke($null, @($op))
  if (-not $t.Wait(30000)) { throw 'timed out' }
  $t.Result
}
function AwaitAction($act) {
  $t = $asTaskAct.Invoke($null, @($act))
  if (-not $t.Wait(30000)) { throw 'timed out' }
}

[Windows.Storage.StorageFile,Windows.Storage,ContentType=WindowsRuntime] | Out-Null
[Windows.Media.Editing.MediaClip,Windows.Media,ContentType=WindowsRuntime] | Out-Null
[Windows.Media.Core.MediaSource,Windows.Media,ContentType=WindowsRuntime] | Out-Null
[Windows.Media.Playback.MediaPlaybackItem,Windows.Media,ContentType=WindowsRuntime] | Out-Null
[Windows.Media.Transcoding.MediaTranscoder,Windows.Media,ContentType=WindowsRuntime] | Out-Null
[Windows.Media.MediaProperties.MediaEncodingProfile,Windows.Media,ContentType=WindowsRuntime] | Out-Null

$file = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync($Path)) ([Windows.Storage.StorageFile])
Write-Output ("FILE  : " + (Split-Path $Path -Leaf))

# --- 1. Editor import path (Windows Photos / Video Editor build on MediaClip) ---
try {
  $clip = Await ([Windows.Media.Editing.MediaClip]::CreateFromFileAsync($file)) ([Windows.Media.Editing.MediaClip])
  Write-Output ("EDITOR: accepted  duration=" + $clip.OriginalDuration)
} catch {
  Write-Output ("EDITOR: REJECTED  " + ($_.Exception.GetBaseException().Message -replace "`r?`n",' '))
}

# --- 2. Playback track resolution (Films and TV / MediaPlayerElement) ---
try {
  $src = [Windows.Media.Core.MediaSource]::CreateFromStorageFile($file)
  $item = New-Object Windows.Media.Playback.MediaPlaybackItem($src)
  AwaitAction ($src.OpenAsync())
  Write-Output ("PLAYER: opened  videoTracks=" + $item.VideoTracks.Count + " audioTracks=" + $item.AudioTracks.Count)
  foreach ($t in $item.AudioTracks) {
    $st = try { $t.GetEncodingProperties().Subtype } catch { '?' }
    Write-Output ("PLAYER:   audio subtype=" + $st + " decoderStatus=" + $t.SupportInfo.DecoderStatus + " mediaSourceStatus=" + $t.SupportInfo.MediaSourceStatus)
  }
  foreach ($t in $item.VideoTracks) {
    $st = try { $t.GetEncodingProperties().Subtype } catch { '?' }
    Write-Output ("PLAYER:   video subtype=" + $st + " decoderStatus=" + $t.SupportInfo.DecoderStatus + " mediaSourceStatus=" + $t.SupportInfo.MediaSourceStatus)
  }
} catch {
  Write-Output ("PLAYER: REJECTED  " + ($_.Exception.GetBaseException().Message -replace "`r?`n",' '))
}

# --- 3. Decode capability: can MF actually transcode it (i.e. decode both tracks)? ---
try {
  $tr = New-Object Windows.Media.Transcoding.MediaTranscoder
  $profile = [Windows.Media.MediaProperties.MediaEncodingProfile]::CreateMp4([Windows.Media.MediaProperties.VideoEncodingQuality]::Vga)
  $outName = [System.IO.Path]::GetFileNameWithoutExtension($Path) + '-mftest.mp4'
  $folder = Await ([Windows.Storage.StorageFolder]::GetFolderFromPathAsync((Split-Path $Path))) ([Windows.Storage.StorageFolder])
  $outFile = Await ($folder.CreateFileAsync($outName, [Windows.Storage.CreationCollisionOption]::ReplaceExisting)) ([Windows.Storage.StorageFile])
  $prep = Await ($tr.PrepareFileTranscodeAsync($file, $outFile, $profile)) ([Windows.Media.Transcoding.PrepareTranscodeResult])
  Write-Output ("DECODE: canTranscode=" + $prep.CanTranscode + " failureReason=" + $prep.FailureReason)
} catch {
  Write-Output ("DECODE: REJECTED  " + ($_.Exception.GetBaseException().Message -replace "`r?`n",' '))
}
