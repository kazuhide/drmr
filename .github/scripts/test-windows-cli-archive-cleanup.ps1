$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-cli-archive-cleanup.ps1')
$checkout = (Get-Location).Path
$python = (Get-Command python).Source
$shell = (Get-Command pwsh).Source
$parent = $env:RUNNER_TEMP

function Assert-ArchiveCleanupTest {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

foreach ($mode in @('success', 'qualification-failure', 'public-copy-failure', 'setup-failure', 'reparse-refusal', 'report-type-refusal', 'replacement-refusal')) {
    $username = 'drmr_arc_' + [guid]::NewGuid().ToString('N').Substring(0, 10)
    $directory = Join-Path $parent ('drmr-archive-check-' + [guid]::NewGuid().ToString('N'))
    $external = Join-Path $parent ('drmr-archive-sentinel-' + [guid]::NewGuid().ToString('N'))
    $retained = $null
    $directoryCreated = $false
    $directoryIdentity = $null
    $user = $null
    $userSid = $null
    $credential = $null
    $process = $null
    $executionAttempted = $false
    $executionExited = $false
    $cleanupAttempted = $false
    try {
        New-Item -ItemType Directory -Path $directory | Out-Null
        $directoryCreated = $true
        $directoryIdentity = Get-DrmrArchiveDirectoryIdentity $directory $python
        New-Item -ItemType Directory -Path $external | Out-Null
        [IO.File]::WriteAllText((Join-Path $external 'keep.txt'), 'unrelated sentinel')
        $password = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(36)) + '!aA1'
        Write-Output "::add-mask::$password"
        $securePassword = ConvertTo-SecureString $password -AsPlainText -Force
        $user = New-LocalUser -Name $username -Password $securePassword -Description 'Ephemeral archive cleanup regression user'
        $userSid = $user.SID
        $users = Get-LocalGroup -SID 'S-1-5-32-545'
        if (-not (Get-LocalGroupMember -Group $users | Where-Object SID -eq $userSid)) {
            Add-LocalGroupMember -Group $users -Member $user
        }
        $credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $securePassword)
        if ($mode -notin @('setup-failure', 'replacement-refusal')) {
            # Only the helper scripts and Python modules are needed; no builds or media tools.
            foreach ($path in @($checkout, (Join-Path $checkout '.github'))) {
                & icacls $path /grant "*$($userSid.Value):RX" /Q
                if ($LASTEXITCODE -ne 0) { throw 'Could not grant cleanup test directory traversal' }
            }
            foreach ($path in @((Join-Path $checkout 'tools'), (Join-Path $checkout '.github/scripts'))) {
                & icacls $path /grant "*$($user.SID.Value):(OI)(CI)RX" /T /Q
                if ($LASTEXITCODE -ne 0) { throw 'Could not grant read-only cleanup test helpers' }
            }
            & icacls $directory /grant "*$($user.SID.Value):(OI)(CI)M" /T /Q
            if ($LASTEXITCODE -ne 0) { throw 'Could not grant the cleanup test directory' }
            & icacls $external /grant "*$($user.SID.Value):(OI)(CI)RX" /T /Q
            if ($LASTEXITCODE -ne 0) { throw 'Could not grant read-only sentinel access' }
            $context = @{ python = $python; helper = (Join-Path $checkout 'tools/qualify_cli_archive.py'); mode = $mode; external = $external }
            $context | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $directory 'context.json')
            if ($mode -eq 'public-copy-failure') {
                New-Item -ItemType Directory -Path (Join-Path $directory 'public-report.json') | Out-Null
            }
            $fixture = Join-Path $directory 'fixture.ps1'
            @'
$ErrorActionPreference = 'Stop'
$context = Get-Content -Raw (Join-Path $PSScriptRoot 'context.json') | ConvertFrom-Json
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'The ACL fixture must be ordinary' }
$code = @"
import importlib.util, pathlib, sys
sys.dont_write_bytecode = True
root, helper, mode = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
spec = importlib.util.spec_from_file_location('qualify_cli_archive', helper)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
if mode == 'report-type-refusal':
    (root / 'qualification.json').mkdir()
else:
    module.private_write(root / 'qualification.json', b'{"success":false}')
temporary = root / 'temp'
temporary.mkdir()
if mode != 'reparse-refusal':
    private = temporary / 'private'
    private.mkdir()
    module.secure_windows(private, True)
    module.private_write(private / 'leftover.bin', b'private test payload')
"@
& $context.python -c $code $PSScriptRoot $context.helper $context.mode
if ($LASTEXITCODE -ne 0) { throw 'Production private-file creation failed' }
if ($context.mode -ne 'report-type-refusal') {
    $acl = Get-Acl -LiteralPath (Join-Path $PSScriptRoot 'qualification.json')
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    if (-not $acl.AreAccessRulesProtected -or $acl.GetOwner([Security.Principal.SecurityIdentifier]) -ne $identity.User -or
        $rules.Count -ne 1 -or $rules[0].IdentityReference -ne $identity.User -or $rules[0].IsInherited -or
        $rules[0].FileSystemRights -ne [Security.AccessControl.FileSystemRights]::FullControl) {
        throw 'The protected report ACL was not preserved during execution'
    }
}
if ($context.mode -eq 'reparse-refusal') {
    New-Item -ItemType Junction -Path (Join-Path $PSScriptRoot 'temp/escape') -Target $context.external | Out-Null
}
@{ ordinary_sid = $identity.User.Value; private_acl_checked = ($context.mode -ne 'report-type-refusal') } |
    ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'public-check.json')
# These failures occur after genuine production ACL creation, before owner cleanup.
if ($context.mode -eq 'qualification-failure') { exit 7 }
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'public-report.json'), '{"success":true}', [Text.UTF8Encoding]::new($false))
'@ | Set-Content -Encoding utf8 $fixture
            $executionAttempted = $true
            $process = Start-Process -FilePath $shell -Credential $credential -LoadUserProfile -WorkingDirectory $checkout -ArgumentList @('-NoLogo', '-NoProfile', '-NonInteractive', '-File', "`"$fixture`"") -Wait -PassThru
            $executionExited = $true
            $proof = Get-Content -Raw -LiteralPath (Join-Path $directory 'public-check.json') | ConvertFrom-Json
            Assert-ArchiveCleanupTest ($proof.ordinary_sid -eq $user.SID.Value) 'The fixture ran as the wrong account'
            $expectedFailure = $mode -in @('qualification-failure', 'public-copy-failure')
            Assert-ArchiveCleanupTest (($process.ExitCode -ne 0) -eq $expectedFailure) 'The fixture failed at the wrong stage'
            if ($mode -ne 'report-type-refusal') {
                Assert-ArchiveCleanupTest $proof.private_acl_checked 'The private report ACL was not checked'
                $denied = $false
                try {
                    $read = [IO.File]::OpenRead((Join-Path $directory 'qualification.json'))
                    $read.Dispose()
                } catch [UnauthorizedAccessException] { $denied = $true }
                Assert-ArchiveCleanupTest $denied 'The elevated collector read the protected report before cleanup'
            }
            if ($mode -ne 'reparse-refusal') {
                $denied = $false
                try {
                    $read = [IO.File]::OpenRead((Join-Path $directory 'temp/private/leftover.bin'))
                    $read.Dispose()
                } catch [UnauthorizedAccessException] { $denied = $true }
                Assert-ArchiveCleanupTest $denied 'The elevated collector read protected TEMP data before cleanup'
            }
        }
        if ($mode -eq 'replacement-refusal') {
            $retained = $directory + '-retained'
            Move-Item -LiteralPath $directory -Destination $retained
            New-Item -ItemType Directory -Path $directory | Out-Null
            [IO.File]::WriteAllText((Join-Path $directory 'keep.txt'), 'replacement sentinel')
        }
        $cleanupFailure = $null
        $cleanupAttempted = $true
        try {
            Complete-DrmrArchiveOwner -Directory $directory -ExpectedParent $parent -ExpectedIdentity $directoryIdentity -Python $python -PowerShell $shell -WorkingDirectory $checkout -Credential $credential -UserSid $userSid -Process $process -DirectoryCreated $directoryCreated -ExecutionAttempted $executionAttempted -ExecutionExited $executionExited
        } catch { $cleanupFailure = $_.Exception }
        $refusal = $mode -in @('reparse-refusal', 'report-type-refusal', 'replacement-refusal')
        Assert-ArchiveCleanupTest (($null -ne $cleanupFailure) -eq $refusal) 'Cleanup accepted a refusal or rejected an ordinary failure path'
        Assert-ArchiveCleanupTest (@(Get-LocalUser | Where-Object SID -eq $user.SID).Count -eq 0) 'The cleanup account remains'
        Assert-ArchiveCleanupTest ([IO.File]::ReadAllText((Join-Path $external 'keep.txt')) -eq 'unrelated sentinel') 'Cleanup changed an unrelated target'
        if (-not $refusal) {
            Assert-ArchiveCleanupTest (-not (Test-Path -LiteralPath $directory)) 'The owned directory remains after successful cleanup'
        } else {
            Assert-ArchiveCleanupTest (Test-Path -LiteralPath $directory) 'A refused cleanup claimed outer deletion'
            if ($mode -eq 'report-type-refusal') {
                Assert-ArchiveCleanupTest (-not (Test-Path -LiteralPath (Join-Path $directory 'temp'))) 'A report refusal prevented the independent TEMP cleanup'
            } elseif ($mode -eq 'reparse-refusal') {
                Assert-ArchiveCleanupTest (-not (Test-Path -LiteralPath (Join-Path $directory 'qualification.json'))) 'A TEMP refusal prevented report cleanup'
                # Remove only this fixture's junction entry; never traverse its target.
                Remove-Item -LiteralPath (Join-Path $directory 'temp/escape') -Force
            } else {
                Assert-ArchiveCleanupTest ([IO.File]::ReadAllText((Join-Path $directory 'keep.txt')) -eq 'replacement sentinel') 'Cleanup removed a replacement directory'
            }
        }
        Write-Output "Archive cleanup regression passed: $mode"
    } finally {
        if (-not $cleanupAttempted) {
            Complete-DrmrArchiveOwner -Directory $directory -ExpectedParent $parent -ExpectedIdentity $directoryIdentity -Python $python -PowerShell $shell -WorkingDirectory $checkout -Credential $credential -UserSid $userSid -Process $process -DirectoryCreated $directoryCreated -ExecutionAttempted $executionAttempted -ExecutionExited $executionExited
        }
        # Refusal fixtures retain only inherited-ACL entries after their assertions.
        foreach ($path in @($directory, $retained, $external)) {
            if ($path -and (Test-Path -LiteralPath $path)) {
                Assert-NoDrmrArchiveReparsePoint $path
                Remove-Item -LiteralPath $path -Recurse -Force
                Assert-ArchiveCleanupTest (-not (Test-Path -LiteralPath $path)) 'A regression fixture directory remains'
            }
        }
    }
}
