$ErrorActionPreference = 'Stop'
$root = Join-Path $env:RUNNER_TEMP 'drmr-media-tools'
New-Item -ItemType Directory -Path $root | Out-Null

function Get-VerifiedArtifact([string] $Url, [string] $Digest, [string] $Destination) {
    Invoke-WebRequest -Uri $Url -OutFile $Destination
    $actual = (Get-FileHash -Algorithm SHA256 $Destination).Hash.ToLowerInvariant()
    if ($actual -ne $Digest) { throw 'Media tool artifact checksum mismatch' }
}

# Immutable upstream release artifacts. FFmpeg's digest is also published in the
# release checksums.sha256; Bento4's official SDK archive was verified separately.
$ffmpeg = 'ffmpeg-n9.0.2-17-g2a571b6068-win64-gpl-9.0'
$archive = Join-Path $root 'ffmpeg.zip'
Get-VerifiedArtifact "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/$ffmpeg.zip" 'a0e45723c72141975f51d8666302e614711745f3102b704ca3f82c897a58d278' $archive
Expand-Archive $archive -DestinationPath $root
Copy-Item (Join-Path $root "$ffmpeg/bin/ffmpeg.exe") $root
Copy-Item (Join-Path $root "$ffmpeg/bin/ffprobe.exe") $root

$bento = 'Bento4-SDK-1-6-0-641.x86_64-microsoft-win32'
$archive = Join-Path $root 'bento4.zip'
Get-VerifiedArtifact "https://www.bok.net/Bento4/binaries/$bento.zip" '6916a390f75878872594be74554b8b54ab220bb29812424441a8e1ecc9a6ac5e' $archive
Expand-Archive $archive -DestinationPath $root
Copy-Item (Join-Path $root "$bento/bin/*") $root

Get-VerifiedArtifact 'https://github.com/shaka-project/shaka-packager/releases/download/v3.8.0/packager-win-x64.exe' 'e9b18cbb9d2c3c1290340b727fa3c5d72d3ab58fa822007767b2d4c4040d0e3a' (Join-Path $root 'packager.exe')
$root | Out-File -FilePath $env:GITHUB_PATH -Encoding utf8 -Append
"DRMR_WINDOWS_MEDIA_TOOLS=$root" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
