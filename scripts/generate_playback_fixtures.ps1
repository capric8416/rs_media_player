param(
    [string]$Ffmpeg = "ffmpeg",
    [string]$OutputDirectory = (Join-Path $PSScriptRoot "..\fixtures\generated")
)

$ErrorActionPreference = "Stop"
$fixtureRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\fixtures"))
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$fixturePrefix = $fixtureRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
if (-not $outputRoot.Equals($fixtureRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
    -not $outputRoot.StartsWith($fixturePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Output directory must remain under $fixtureRoot"
}

$encoderList = & $Ffmpeg -hide_banner -encoders 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "Could not query encoders from '$Ffmpeg'"
}

New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null

function Invoke-Fixture([string]$Encoder, [string]$Name, [string[]]$CodecArguments) {
    if (-not ($encoderList -match "\b$([regex]::Escape($Encoder))\b")) {
        Write-Warning "Skipping $Name; encoder $Encoder is unavailable"
        return
    }
    $destination = Join-Path $outputRoot $Name
    $arguments = @(
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
        "-frames:v", "48", "-an"
    ) + $CodecArguments + @("-movflags", "+faststart", $destination)
    & $Ffmpeg @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "FFmpeg failed generating $destination (exit $LASTEXITCODE)"
    }
    Write-Host "Created $destination"
}

Invoke-Fixture "libx264" "avc_high_bframes.mp4" @(
    "-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p",
    "-bf", "2", "-g", "24", "-threads", "1"
)
Invoke-Fixture "libx265" "hevc_main_bframes.mp4" @(
    "-c:v", "libx265", "-profile:v", "main", "-pix_fmt", "yuv420p",
    "-bf", "2", "-g", "24", "-x265-params", "pools=1:frame-threads=1:log-level=error"
)
Invoke-Fixture "libx265" "hevc_main10.mp4" @(
    "-c:v", "libx265", "-profile:v", "main10", "-pix_fmt", "yuv420p10le",
    "-bf", "2", "-g", "24", "-x265-params", "pools=1:frame-threads=1:log-level=error"
)
Invoke-Fixture "libaom-av1" "av1_main_bframes.mp4" @(
    "-c:v", "libaom-av1", "-profile:v", "0", "-pix_fmt", "yuv420p",
    "-bf", "2", "-g", "24", "-cpu-used", "8", "-crf", "35", "-b:v", "0", "-threads", "1"
)
Invoke-Fixture "libaom-av1" "av1_main10.mp4" @(
    "-c:v", "libaom-av1", "-profile:v", "0", "-pix_fmt", "yuv420p10le",
    "-bf", "2", "-g", "24", "-cpu-used", "8", "-crf", "35", "-b:v", "0", "-threads", "1"
)

Write-Host "Fixture generation complete. Files are local-only under $outputRoot"
