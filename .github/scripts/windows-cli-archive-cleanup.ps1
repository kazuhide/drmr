param(
    [string]$Directory,
    [string]$ExpectedParent,
    [string]$ExpectedIdentity,
    [string]$Python,
    [string]$ExpectedSid
)

$ErrorActionPreference = 'Stop'
$script:DrmrArchiveCleanupScript = $PSCommandPath

function Get-DrmrArchiveDirectoryIdentity {
    param([string]$Directory, [string]$Python)
    $code = @'
import os, stat, sys
try:
    value = os.lstat(sys.argv[1])
    if not stat.S_ISDIR(value.st_mode) or getattr(value, "st_file_attributes", 0) & 0x400:
        sys.exit(1)
    print(str(value.st_dev) + ":" + str(value.st_ino))
except OSError:
    sys.exit(1)
'@
    $identity = & $Python -c $code $Directory
    if ($LASTEXITCODE -ne 0 -or $identity -isnot [string] -or $identity -notmatch '^[0-9]+:[0-9]+$') {
        throw 'The owned archive directory identity is unavailable'
    }
    return $identity
}

function Assert-DrmrArchiveDirectory {
    param([string]$Directory, [string]$ExpectedParent, [string]$ExpectedIdentity, [string]$Python)
    $full = [IO.Path]::GetFullPath($Directory).TrimEnd([char[]]'\/')
    $parent = [IO.Path]::GetFullPath($ExpectedParent).TrimEnd([char[]]'\/')
    if (-not [string]::Equals([IO.Path]::GetDirectoryName($full), $parent, [StringComparison]::OrdinalIgnoreCase) -or
        -not [IO.Path]::GetFileName($full).StartsWith('drmr-archive-', [StringComparison]::Ordinal) -or
        -not $ExpectedIdentity -or (Get-DrmrArchiveDirectoryIdentity $full $Python) -ne $ExpectedIdentity) {
        throw 'The owned archive directory changed'
    }
}

function Assert-NoDrmrArchiveReparsePoint {
    param([string]$Path)
    # Without FollowSymlink, enumeration lists links without descending into them.
    foreach ($entry in @((Get-Item -LiteralPath $Path -Force)) + @(Get-ChildItem -LiteralPath $Path -Force -Recurse)) {
        if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw 'Archive cleanup refuses reparse points'
        }
    }
}

function Remove-DrmrPrivateArchiveFiles {
    param([string]$Directory, [string]$ExpectedParent, [string]$ExpectedIdentity, [string]$Python, [string]$ExpectedSid)
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if ($identity.User.Value -ne $ExpectedSid -or $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Private archive cleanup requires its ordinary owner'
    }
    Assert-DrmrArchiveDirectory $Directory $ExpectedParent $ExpectedIdentity $Python
    $failed = $false
    foreach ($name in @('qualification.json', 'temp')) {
        try {
            Assert-DrmrArchiveDirectory $Directory $ExpectedParent $ExpectedIdentity $Python
            $path = Join-Path $Directory $name
            if (Test-Path -LiteralPath $path) {
                $entry = Get-Item -LiteralPath $path -Force
                if ($entry.PSIsContainer -ne ($name -eq 'temp')) {
                    throw 'The private cleanup entry has an unexpected type'
                }
                Assert-NoDrmrArchiveReparsePoint $path
                Remove-Item -LiteralPath $path -Recurse -Force
                if (Test-Path -LiteralPath $path) { throw 'The private cleanup entry remains' }
            }
        } catch {
            # Attempt both known private locations, but never turn a refusal into success.
            $failed = $true
        }
    }
    if ($failed) { throw 'Private archive cleanup was not confirmed' }
}

function Complete-DrmrArchiveOwner {
    param(
        [string]$Directory, [string]$ExpectedParent, [string]$ExpectedIdentity,
        [string]$Python, [string]$PowerShell, [string]$WorkingDirectory,
        [PSCredential]$Credential, [Security.Principal.SecurityIdentifier]$UserSid,
        [Diagnostics.Process]$Process, [bool]$DirectoryCreated,
        [bool]$ExecutionAttempted, [bool]$ExecutionExited
    )
    try {
        if (-not $DirectoryCreated) { return }
        Assert-DrmrArchiveDirectory $Directory $ExpectedParent $ExpectedIdentity $Python
        if ($ExecutionAttempted) {
            # Start-Process -Wait must have returned for the whole qualification tree.
            if (-not $ExecutionExited -or $null -eq $Process -or -not $Process.HasExited -or
                $null -eq $Credential -or $null -eq $UserSid) {
                throw 'The qualification process tree exit is unconfirmed'
            }
            $Process.Dispose()
            $Process = $null
            $arguments = @('-NoLogo', '-NoProfile', '-NonInteractive', '-File', "`"$script:DrmrArchiveCleanupScript`"",
                '-Directory', "`"$Directory`"", '-ExpectedParent', "`"$ExpectedParent`"",
                '-ExpectedIdentity', $ExpectedIdentity, '-Python', "`"$Python`"", '-ExpectedSid', $UserSid.Value)
            $cleanup = Start-Process -FilePath $PowerShell -Credential $Credential -LoadUserProfile -WorkingDirectory $WorkingDirectory -ArgumentList $arguments -Wait -PassThru
            try {
                if ($cleanup.ExitCode -ne 0) { throw 'The ordinary owner could not clean its private archive files' }
            } finally {
                $cleanup.Dispose()
            }
            foreach ($name in @('qualification.json', 'temp')) {
                if (Test-Path -LiteralPath (Join-Path $Directory $name)) { throw 'A private archive cleanup entry remains' }
            }
        }
        Assert-DrmrArchiveDirectory $Directory $ExpectedParent $ExpectedIdentity $Python
        Assert-NoDrmrArchiveReparsePoint $Directory
        Remove-Item -LiteralPath $Directory -Recurse -Force
        if (Test-Path -LiteralPath $Directory) { throw 'The owned archive directory remains' }
    } finally {
        try {
            if ($null -ne $Process -and $ExecutionExited) { $Process.Dispose() }
        } finally {
            # A failed file cleanup must not skip account teardown. The captured SID
            # cannot select an unrelated replacement account with the same name.
            if ($null -ne $UserSid) {
                if (@(Get-LocalUser | Where-Object SID -eq $UserSid).Count -ne 0) {
                    Remove-LocalUser -SID $UserSid
                }
                if (@(Get-LocalUser | Where-Object SID -eq $UserSid).Count -ne 0) {
                    throw 'The archive qualification account remains'
                }
            }
        }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    Remove-DrmrPrivateArchiveFiles $Directory $ExpectedParent $ExpectedIdentity $Python $ExpectedSid
}
