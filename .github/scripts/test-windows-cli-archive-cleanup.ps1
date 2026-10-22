param([ValidateRange(1, 5)][int]$DiagnosticIteration = 1)

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

function Write-ArchiveCleanupDiagnostic {
    param([string]$Directory, [string]$ExpectedIdentity, [string]$Mode,
        [Nullable[int]]$ChildExitCode, [long]$ChildElapsedMs, [string]$LaunchStatus = 'exited')
    $result = [ordered]@{
        event = 'archive-cleanup-diagnostic'; schema_version = 1; iteration = $DiagnosticIteration
        mode = $Mode; launch_status = $LaunchStatus; child_exit_code = $ChildExitCode
        child_elapsed_ms = $ChildElapsedMs; diagnostic_status = 'missing'; stage = 'unavailable'
        python_exit_code = $null; acl_calls = @()
    }
    try {
        Assert-DrmrArchiveDirectory $Directory $parent $ExpectedIdentity $python
        $path = Join-Path $Directory 'diagnostic.json'
        if (Test-Path -LiteralPath $path) {
            $entry = Get-Item -LiteralPath $path -Force
            if ($entry.PSIsContainer -or ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Invalid diagnostic entry' }
            # The whole child tree has exited; deny sharing while reading this owned file.
            $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
            try {
                if ($stream.Length -lt 1 -or $stream.Length -gt 8192) { throw 'Invalid diagnostic size' }
                $buffer = [byte[]]::new(8193)
                $count = 0
                while ($count -lt $buffer.Length) {
                    $read = $stream.Read($buffer, $count, $buffer.Length - $count)
                    if ($read -eq 0) { break }
                    $count += $read
                }
                if ($count -gt 8192) { throw 'Invalid diagnostic size' }
                $record = [Text.UTF8Encoding]::new($false, $true).GetString($buffer, 0, $count) | ConvertFrom-Json -AsHashtable -Depth 8
            } finally { $stream.Dispose() }
            $keys = @('schema_version', 'mode', 'stage', 'python_exit_code', 'acl_calls')
            if ($record -isnot [System.Collections.IDictionary] -or
                @(Compare-Object @($record.Keys | Sort-Object) @($keys | Sort-Object)).Count -ne 0 -or
                ($record.schema_version -isnot [int] -and $record.schema_version -isnot [long]) -or
                $record.schema_version -ne 1 -or $record.mode -cnotin @($Mode, 'unavailable') -or
                $record.stage -cnotin @('fixture-entered', 'context-loaded', 'ordinary-checked', 'python-started',
                    'python-import', 'report-file', 'temp-directory', 'private-payload', 'python-exited',
                    'report-checked', 'proof-written', 'complete') -or
                ($null -ne $record.python_exit_code -and
                    (($record.python_exit_code -isnot [int] -and $record.python_exit_code -isnot [long]) -or
                     $record.python_exit_code -lt -2147483648 -or $record.python_exit_code -gt 2147483647)) -or
                $record.acl_calls -isnot [array] -or $record.acl_calls.Count -gt 3) { throw 'Invalid diagnostic schema' }
            $calls = @()
            foreach ($call in $record.acl_calls) {
                if ($call -isnot [System.Collections.IDictionary] -or
                    @(Compare-Object @($call.Keys | Sort-Object) @('elapsed_ms', 'exit_code', 'stage', 'status')).Count -ne 0 -or
                    $call.stage -cnotin @('report-file', 'temp-directory', 'private-payload') -or
                    $call.status -cnotin @('running', 'ok', 'timeout', 'nonzero', 'os-error') -or
                    ($call.elapsed_ms -isnot [int] -and $call.elapsed_ms -isnot [long]) -or
                    $call.elapsed_ms -lt 0 -or $call.elapsed_ms -gt 120000 -or
                    ($null -ne $call.exit_code -and
                        (($call.exit_code -isnot [int] -and $call.exit_code -isnot [long]) -or
                         $call.exit_code -lt -2147483648 -or $call.exit_code -gt 2147483647))) { throw 'Invalid ACL diagnostic schema' }
                $calls += [ordered]@{ stage = $call.stage; status = $call.status; elapsed_ms = $call.elapsed_ms; exit_code = $call.exit_code }
            }
            $result.diagnostic_status = 'validated'
            $result.stage = $record.stage
            $result.python_exit_code = $record.python_exit_code
            $result.acl_calls = $calls
        }
    } catch { $result.diagnostic_status = 'invalid-or-unreadable' }
    Write-Output ('DRMR_ARCHIVE_DIAGNOSTIC ' + ($result | ConvertTo-Json -Depth 8 -Compress))
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
$diagnosticPath = Join-Path $PSScriptRoot 'diagnostic.json'
$diagnostic = [ordered]@{ schema_version = 1; mode = 'unavailable'; stage = 'fixture-entered'; python_exit_code = $null; acl_calls = @() }
function Publish-FixtureStage {
    param([string]$Stage)
    $diagnostic.stage = $Stage
    try { [IO.File]::WriteAllText($diagnosticPath, ($diagnostic | ConvertTo-Json -Depth 8 -Compress), [Text.UTF8Encoding]::new($false)) } catch { }
}
Publish-FixtureStage 'fixture-entered'
$context = Get-Content -Raw (Join-Path $PSScriptRoot 'context.json') | ConvertFrom-Json
$diagnostic.mode = $context.mode
Publish-FixtureStage 'context-loaded'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'The ACL fixture must be ordinary' }
Publish-FixtureStage 'ordinary-checked'
$code = @"
import importlib.util, json, pathlib, subprocess, sys, time
sys.dont_write_bytecode = True
root, helper, mode = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
diagnostic_path = root / 'diagnostic.json'
diagnostic = dict(schema_version=1, mode=mode, stage='python-import', python_exit_code=None, acl_calls=[])

def publish_diagnostic():
    try:
        data = json.dumps(diagnostic, separators=(',', ':'))
        if len(data.encode('utf-8')) <= 8192:
            diagnostic_path.write_text(data, encoding='utf-8')
    except (OSError, ValueError):
        pass

def observe_acl_run(command, *args, **kwargs):
    if not isinstance(command, (list, tuple)) or not command or command[0] != 'powershell.exe':
        return original_run(command, *args, **kwargs)
    call = dict(stage=diagnostic['stage'], status='running', elapsed_ms=0, exit_code=None)
    diagnostic['acl_calls'].append(call)
    publish_diagnostic()
    started = time.monotonic()
    try:
        result = original_run(command, *args, **kwargs)
    except subprocess.TimeoutExpired:
        call['status'] = 'timeout'
        raise
    except OSError:
        call['status'] = 'os-error'
        raise
    else:
        call['status'] = 'ok' if result.returncode == 0 else 'nonzero'
        call['exit_code'] = result.returncode
        return result
    finally:
        call['elapsed_ms'] = int((time.monotonic() - started) * 1000)
        publish_diagnostic()

publish_diagnostic()
spec = importlib.util.spec_from_file_location('qualify_cli_archive', helper)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
original_run = module.subprocess.run
module.subprocess.run = observe_acl_run
if mode == 'report-type-refusal':
    (root / 'qualification.json').mkdir()
else:
    diagnostic['stage'] = 'report-file'
    module.private_write(root / 'qualification.json', b'{"success":false}')
temporary = root / 'temp'
temporary.mkdir()
if mode != 'reparse-refusal':
    private = temporary / 'private'
    private.mkdir()
    diagnostic['stage'] = 'temp-directory'
    module.secure_windows(private, True)
    diagnostic['stage'] = 'private-payload'
    module.private_write(private / 'leftover.bin', b'private test payload')
"@
Publish-FixtureStage 'python-started'
& $context.python -c $code $PSScriptRoot $context.helper $context.mode
$pythonExit = $LASTEXITCODE
try { $diagnostic = Get-Content -Raw -LiteralPath $diagnosticPath | ConvertFrom-Json -AsHashtable } catch { }
$diagnostic.python_exit_code = $pythonExit
Publish-FixtureStage 'python-exited'
if ($pythonExit -ne 0) { throw 'Production private-file creation failed' }
if ($context.mode -ne 'report-type-refusal') {
    $acl = Get-Acl -LiteralPath (Join-Path $PSScriptRoot 'qualification.json')
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    if (-not $acl.AreAccessRulesProtected -or $acl.GetOwner([Security.Principal.SecurityIdentifier]) -ne $identity.User -or
        $rules.Count -ne 1 -or $rules[0].IdentityReference -ne $identity.User -or $rules[0].IsInherited -or
        $rules[0].FileSystemRights -ne [Security.AccessControl.FileSystemRights]::FullControl) {
        throw 'The protected report ACL was not preserved during execution'
    }
}
Publish-FixtureStage 'report-checked'
if ($context.mode -eq 'reparse-refusal') {
    New-Item -ItemType Junction -Path (Join-Path $PSScriptRoot 'temp/escape') -Target $context.external | Out-Null
}
@{ ordinary_sid = $identity.User.Value; private_acl_checked = ($context.mode -ne 'report-type-refusal') } |
    ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'public-check.json')
Publish-FixtureStage 'proof-written'
# These failures occur after genuine production ACL creation, before owner cleanup.
if ($context.mode -eq 'qualification-failure') { exit 7 }
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'public-report.json'), '{"success":true}', [Text.UTF8Encoding]::new($false))
Publish-FixtureStage 'complete'
'@ | Set-Content -Encoding utf8 $fixture
            $executionAttempted = $true
            $watch = [Diagnostics.Stopwatch]::StartNew()
            try {
                $process = Start-Process -FilePath $shell -Credential $credential -LoadUserProfile -WorkingDirectory $checkout -ArgumentList @('-NoLogo', '-NoProfile', '-NonInteractive', '-File', "`"$fixture`"") -Wait -PassThru
            } catch {
                $watch.Stop()
                Write-ArchiveCleanupDiagnostic $directory $directoryIdentity $mode $null $watch.ElapsedMilliseconds 'launch-failed'
                throw 'Archive cleanup fixture launch failed'
            }
            $watch.Stop()
            $executionExited = $true
            Write-ArchiveCleanupDiagnostic $directory $directoryIdentity $mode $process.ExitCode $watch.ElapsedMilliseconds
            if (-not (Test-Path -LiteralPath (Join-Path $directory 'public-check.json') -PathType Leaf)) {
                throw 'Archive cleanup fixture public proof unavailable'
            }
            try { $proof = Get-Content -Raw -LiteralPath (Join-Path $directory 'public-check.json') | ConvertFrom-Json }
            catch { throw 'Archive cleanup fixture public proof unavailable' }
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
