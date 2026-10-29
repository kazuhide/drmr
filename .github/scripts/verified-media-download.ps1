$ErrorActionPreference = 'Stop'

function Get-VerifiedArtifact([string] $Url, [string] $Digest, [string] $Destination) {
    if (Test-Path -LiteralPath $Destination) {
        $entry = Get-Item -LiteralPath $Destination -Force
        if ($entry.PSIsContainer -or ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Media download cache entry is not a regular file'
        }
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $Destination).Hash.ToLowerInvariant() -eq $Digest) {
            return
        }
        Remove-Item -LiteralPath $Destination
    }
    $pending = $Destination + '.pending-' + [guid]::NewGuid().ToString('N')
    try {
        Invoke-WebRequest -Uri $Url -OutFile $pending
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $pending).Hash.ToLowerInvariant()
        if ($actual -ne $Digest) { throw 'Media tool artifact checksum mismatch' }
        Move-Item -LiteralPath $pending -Destination $Destination
    } finally {
        if (Test-Path -LiteralPath $pending) { Remove-Item -LiteralPath $pending }
    }
}
