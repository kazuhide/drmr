$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'verified-media-download.ps1')
$directory = Join-Path $env:RUNNER_TEMP ('drmr-download-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
$script:downloads = 0
$script:payload = [Text.Encoding]::UTF8.GetBytes('public synthetic tool download')
function Invoke-WebRequest([string] $Uri, [string] $OutFile) {
    $script:downloads++
    [IO.File]::WriteAllBytes($OutFile, $script:payload)
}
try {
    $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($script:payload)).ToLowerInvariant()
    $destination = Join-Path $directory 'tool.download'
    Get-VerifiedArtifact 'https://example.invalid/tool' $digest $destination
    if ($script:downloads -ne 1) { throw 'Cold tool cache did not fetch once' }
    Get-VerifiedArtifact 'https://example.invalid/tool' $digest $destination
    if ($script:downloads -ne 1) { throw 'Valid cached tool was downloaded again' }
    [IO.File]::WriteAllText($destination, 'corrupted cached bytes')
    Get-VerifiedArtifact 'https://example.invalid/tool' $digest $destination
    if ($script:downloads -ne 2) { throw 'Corrupted tool cache was not refreshed' }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash.ToLowerInvariant() -ne $digest) {
        throw 'Refreshed tool cache retained incorrect bytes'
    }
    $bad = Join-Path $directory 'mismatch.download'
    $refused = $false
    try { Get-VerifiedArtifact 'https://example.invalid/tool' ('0' * 64) $bad } catch { $refused = $true }
    if (-not $refused -or (Test-Path -LiteralPath $bad) -or
        @(Get-ChildItem -LiteralPath $directory -Filter '*.pending-*').Count -ne 0) {
        throw 'Mismatched tool download was published or retained'
    }
    Write-Output 'Verified media cache cold, hit, corruption and mismatch controls passed'
} finally {
    Remove-Item -LiteralPath $directory -Recurse -Force
}
