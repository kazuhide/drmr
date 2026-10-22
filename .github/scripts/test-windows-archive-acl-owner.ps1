$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-cli-archive-cleanup.ps1')
$checkout = (Get-Location).Path
$python = (Get-Command python).Source
$shell = (Get-Command pwsh).Source
$parent = $env:RUNNER_TEMP
$directory = Join-Path $parent ('drmr-archive-owner-research-' + [guid]::NewGuid().ToString('N'))
$directoryCreated = $false
$directoryIdentity = $null
$userSid = $null
$credential = $null
$process = $null
$executionAttempted = $false
$executionExited = $false
$classifiedFailure = $false
$researchStage = 'create-root'

function Assert-OwnerResearch {
    param([bool]$Condition)
    if (-not $Condition) { throw 'The controlled owner diagnostic assertion failed' }
}

function Read-OwnedResearchJson {
    param([string]$Name)
    Assert-DrmrArchiveDirectory $directory $parent $directoryIdentity $python
    $path = Join-Path $directory $Name
    $entry = Get-Item -LiteralPath $path -Force
    Assert-OwnerResearch (-not $entry.PSIsContainer -and -not ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint))
    $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
    try {
        Assert-OwnerResearch ($stream.Length -ge 1 -and $stream.Length -le 8192)
        $buffer = [byte[]]::new(8193)
        $count = 0
        while ($count -lt $buffer.Length) {
            $read = $stream.Read($buffer, $count, $buffer.Length - $count)
            if ($read -eq 0) { break }
            $count += $read
        }
        Assert-OwnerResearch ($count -le 8192)
        return [Text.UTF8Encoding]::new($false, $true).GetString($buffer, 0, $count) | ConvertFrom-Json -AsHashtable -Depth 8
    } finally { $stream.Dispose() }
}

function Assert-ExactKeys {
    param($Record, [string[]]$Keys)
    Assert-OwnerResearch ($Record -is [System.Collections.IDictionary])
    Assert-OwnerResearch (@(Compare-Object @($Record.Keys | Sort-Object) @($Keys | Sort-Object)).Count -eq 0)
}

function Assert-ResearchInteger {
    param($Value, [long]$Minimum, [long]$Maximum)
    Assert-OwnerResearch (($Value -is [int] -or $Value -is [long]) -and $Value -ge $Minimum -and $Value -le $Maximum)
}

try {
    New-Item -ItemType Directory -Path $directory | Out-Null
    $directoryCreated = $true
    $directoryIdentity = Get-DrmrArchiveDirectoryIdentity $directory $python
    $researchStage = 'create-user'
    $username = 'drmr_own_' + [guid]::NewGuid().ToString('N').Substring(0, 10)
    $password = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(36)) + '!aA1'
    Write-Output "::add-mask::$password"
    $securePassword = ConvertTo-SecureString $password -AsPlainText -Force
    $user = New-LocalUser -Name $username -Password $securePassword -Description 'Ephemeral owner ACL diagnostic user'
    $userSid = $user.SID
    $researchStage = 'group-membership'
    $users = Get-LocalGroup -SID 'S-1-5-32-545'
    if (-not (Get-LocalGroupMember -Group $users | Where-Object SID -eq $userSid)) {
        Add-LocalGroupMember -Group $users -Member $user
    }
    $credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $securePassword)
    $researchStage = 'grant-helper-read'
    foreach ($path in @($checkout, (Join-Path $checkout '.github'))) {
        $null = & icacls $path /grant "*$($userSid.Value):RX" /Q 2>&1
        Assert-OwnerResearch ($LASTEXITCODE -eq 0)
    }
    foreach ($path in @((Join-Path $checkout 'tools'), (Join-Path $checkout '.github/scripts'))) {
        $null = & icacls $path /grant "*$($userSid.Value):(OI)(CI)RX" /T /Q 2>&1
        Assert-OwnerResearch ($LASTEXITCODE -eq 0)
    }
    foreach ($path in @('docs', 'site-content', 'site-content/guide', 'docs/windows.md', 'site-content/guide/en.md', 'site-content/guide/ja.md')) {
        $null = & icacls (Join-Path $checkout $path) /grant "*$($userSid.Value):RX" /Q 2>&1
        Assert-OwnerResearch ($LASTEXITCODE -eq 0)
    }
    $adminSid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $researchStage = 'protect-parent'
    Assert-OwnerResearch ($adminSid.Value -ne 'S-1-5-18' -and $adminSid.Value -ne $userSid.Value)
    $null = & icacls $directory /grant "*$($adminSid.Value):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' "*$($userSid.Value):(OI)(CI)M" /Q 2>&1
    Assert-OwnerResearch ($LASTEXITCODE -eq 0)
    $null = & icacls $directory /inheritance:r /Q 2>&1
    Assert-OwnerResearch ($LASTEXITCODE -eq 0)
    $acl = Get-Acl -LiteralPath $directory
    $researchStage = 'check-parent-rules'
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    # A newly created directory can contain an explicit administrator-group ACE.
    # Remove only extra principals from this owned synthetic parent, then re-read.
    $researchStage = 'remove-extra-parent-ace'
    foreach ($rule in $rules) {
        if ($rule.IdentityReference.Value -notin @($adminSid.Value, $userSid.Value, 'S-1-5-18')) {
            $null = & icacls $directory /remove "*$($rule.IdentityReference.Value)" /Q 2>&1
            Assert-OwnerResearch ($LASTEXITCODE -eq 0)
        }
    }
    $acl = Get-Acl -LiteralPath $directory
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    $researchStage = 'check-parent-rules'
    $safeRules = @()
    foreach ($rule in $rules) {
        $role = if ($rule.IdentityReference.Value -eq $userSid.Value) { 'ordinary' }
            elseif ($rule.IdentityReference.Value -eq $adminSid.Value) { 'admin' }
            elseif ($rule.IdentityReference.Value -eq 'S-1-5-18') { 'system' }
            elseif ($rule.IdentityReference.Value -eq 'S-1-3-0') { 'creator-owner' } else { 'other' }
        $safeRules += [ordered]@{ role = $role; inherited = [bool]$rule.IsInherited;
            allow = $rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow;
            rights = [long]$rule.FileSystemRights; inheritance = [int]$rule.InheritanceFlags; propagation = [int]$rule.PropagationFlags }
    }
    Write-Output ('DRMR_ACL_PARENT_PROFILE ' + ([ordered]@{ protected = [bool]$acl.AreAccessRulesProtected; count = $rules.Count; rules = $safeRules } | ConvertTo-Json -Depth 4 -Compress))
    Assert-OwnerResearch ($acl.AreAccessRulesProtected -and $rules.Count -eq 3)
    foreach ($rule in $rules) {
        Assert-OwnerResearch (-not $rule.IsInherited -and $rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow)
        Assert-OwnerResearch ($rule.IdentityReference.Value -in @($adminSid.Value, $userSid.Value, 'S-1-5-18'))
    }
    $ordinary = @($rules | Where-Object { $_.IdentityReference.Value -eq $userSid.Value })
    Assert-OwnerResearch ($ordinary.Count -eq 1)
    $rights = $ordinary[0].FileSystemRights
    $researchStage = 'check-modify-only'
    $modify = [Security.AccessControl.FileSystemRights]::Modify
    $allowed = $modify -bor [Security.AccessControl.FileSystemRights]::Synchronize
    Assert-OwnerResearch (($rights -band $modify) -eq $modify -and ($rights -band (-bnot $allowed)) -eq 0)
    Assert-OwnerResearch (($rights -band ([Security.AccessControl.FileSystemRights]::TakeOwnership -bor [Security.AccessControl.FileSystemRights]::ChangePermissions)) -eq 0)
    $researchStage = 'prepare-native-regression'
    $nativeFixture = Join-Path $PSScriptRoot 'windows-cli-private-acl-tests.ps1'
    . $nativeFixture -Directory $directory -Checkout $checkout -Python $python
    Initialize-PrivateAclRegression $directory $userSid $parent $directoryIdentity $python $checkout
    $temporaryIdentity = Get-DrmrArchiveDirectoryIdentity (Join-Path $directory 'temp') $python
    $context = @{
        python = $python; helper = (Join-Path $checkout 'tools/qualify_cli_archive.py')
        worker = (Join-Path $PSScriptRoot 'windows_archive_acl_owner_worker.py')
        research = (Join-Path $PSScriptRoot 'windows_archive_acl_research.py')
        native_fixture = $nativeFixture; checkout = $checkout; parent = $parent
        identity = $directoryIdentity; temporary_identity = $temporaryIdentity
        collector_sid = $adminSid.Value
    }
    $researchStage = 'write-fixture'
    $context | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $directory 'context.json')
    $fixture = Join-Path $directory 'fixture.ps1'
    @'
$ErrorActionPreference = 'Stop'
try {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { exit 4 }
    $context = Get-Content -Raw (Join-Path $PSScriptRoot 'context.json') | ConvertFrom-Json
    & powershell.exe -NoLogo -NoProfile -NonInteractive -File $context.native_fixture -Ordinary -Directory $PSScriptRoot -Checkout $context.checkout -Python $context.python -ExpectedParent $context.parent -ExpectedIdentity $context.identity -ExpectedTempIdentity $context.temporary_identity -ExpectedCollectorSid $context.collector_sid
    if ($LASTEXITCODE -ne 0) { exit 7 }
    [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'qualification.json'), '{}')
    New-Item -ItemType Directory -Path (Join-Path $PSScriptRoot 'temp') | Out-Null
    foreach ($name in @('qualification.json', 'temp')) {
        $acl = Get-Acl -LiteralPath (Join-Path $PSScriptRoot $name)
        if (-not $acl.GetOwner([Security.Principal.SecurityIdentifier]).Equals($identity.User)) { exit 5 }
    }
    [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'preconditions.json'),
        '{"schema_version":1,"ordinary_user":true,"file_owner_matches":true,"directory_owner_matches":true}', [Text.UTF8Encoding]::new($false))
    & $context.python -B $context.worker $PSScriptRoot $context.helper $context.research
    exit $LASTEXITCODE
} catch { exit 6 }
'@ | Set-Content -Encoding utf8 $fixture
    $arguments = @('-NoLogo', '-NoProfile', '-NonInteractive', '-File', "`"$fixture`"")
    $researchStage = 'launch-child'
    $executionAttempted = $true
    $process = Start-Process -FilePath $shell -Credential $credential -LoadUserProfile -WorkingDirectory $directory -ArgumentList $arguments -Wait -PassThru
    $executionExited = $true
    Write-Output ('DRMR_ACL_OWNER_CHILD_EXIT ' + $process.ExitCode)
    if (Test-Path -LiteralPath (Join-Path $directory 'private-acl-failure.json')) {
        $nativeFailure = Read-OwnedResearchJson 'private-acl-failure.json'
        Assert-ExactKeys $nativeFailure @('schema_version', 'stage', 'category', 'hresult')
        Assert-ResearchInteger $nativeFailure.schema_version 1 1
        Assert-ResearchInteger $nativeFailure.hresult -2147483648 4294967295
        Assert-OwnerResearch ($nativeFailure.stage -cin @('entered', 'root-premise', 'root-identity', 'ordinary-user', 'parent-profile', 'temporary-identity', 'temporary-profile', 'foreign-permission', 'helper-run', 'helper-positive', 'helper-negative-snapshot', 'document-extraction', 'document-positive', 'document-negative', 'owned-cleanup'))
        Assert-OwnerResearch ($nativeFailure.category -cin @('RuntimeException', 'MethodInvocationException', 'UnauthorizedAccessException', 'ItemNotFoundException', 'Win32Exception', 'UnclassifiedException'))
        Write-Output ('DRMR_PRIVATE_ACL_FAILURE ' + ($nativeFailure | ConvertTo-Json -Compress))
    }
    $researchStage = 'check-child-exit'
    Assert-OwnerResearch ($process.ExitCode -eq 0)
    $nativeProof = Read-OwnedResearchJson 'private-acl-proof.json'
    Assert-ExactKeys $nativeProof @('schema_version', 'ordinary_user', 'modify_parent', 'helper_positive', 'helper_foreign_owner_refusals',
        'document_positive', 'document_foreign_owner_refusals', 'foreign_owner_full_control', 'foreign_owner_descriptor_identity_payload_unchanged', 'documents')
    Assert-ResearchInteger $nativeProof.schema_version 1 1
    Assert-ResearchInteger $nativeProof.helper_positive 2 2
    Assert-ResearchInteger $nativeProof.helper_foreign_owner_refusals 2 2
    Assert-ResearchInteger $nativeProof.document_positive 6 6
    Assert-ResearchInteger $nativeProof.document_foreign_owner_refusals 6 6
    foreach ($key in @('ordinary_user', 'modify_parent', 'foreign_owner_full_control', 'foreign_owner_descriptor_identity_payload_unchanged')) {
        Assert-OwnerResearch ($nativeProof[$key] -is [bool] -and $nativeProof[$key])
    }
    Assert-OwnerResearch ($nativeProof.documents -is [array] -and $nativeProof.documents.Count -eq 3)
    foreach ($document in $nativeProof.documents) {
        Assert-ExactKeys $document @('document', 'source_sha256', 'function_sha256')
        Assert-OwnerResearch ($document.document -cin @('docs/windows.md', 'site-content/guide/en.md', 'site-content/guide/ja.md'))
        Assert-OwnerResearch ($document.source_sha256 -cmatch '^[0-9a-f]{64}$' -and $document.function_sha256 -cmatch '^[0-9a-f]{64}$')
    }
    Write-Output ('DRMR_PRIVATE_ACL_PROOF ' + ($nativeProof | ConvertTo-Json -Depth 6 -Compress))
    $premises = Read-OwnedResearchJson 'preconditions.json'
    $researchStage = 'check-preconditions'
    Assert-ExactKeys $premises @('schema_version', 'ordinary_user', 'file_owner_matches', 'directory_owner_matches')
    Assert-ResearchInteger $premises.schema_version 1 1
    foreach ($key in @('ordinary_user', 'file_owner_matches', 'directory_owner_matches')) {
        Assert-OwnerResearch ($premises[$key] -is [bool] -and $premises[$key])
    }
    Write-Output 'DRMR_ACL_MODIFY_ONLY_PRECONDITIONS_CONFIRMED'
    $record = Read-OwnedResearchJson 'research.json'
    $researchStage = 'check-research-record'
    Assert-ExactKeys $record @('schema_version', 'results', 'calls')
    Assert-ResearchInteger $record.schema_version 1 1
    Assert-OwnerResearch ($record.results -is [array] -and $record.results.Count -eq 2 -and $record.calls -is [array] -and $record.calls.Count -eq 2)
    $categories = @('MethodInvocationException', 'UnauthorizedAccessException', 'SecurityException', 'IOException',
        'ArgumentException', 'ArgumentNullException', 'InvalidOperationException', 'PlatformNotSupportedException',
        'NotSupportedException', 'DirectoryNotFoundException', 'FileNotFoundException', 'Win32Exception',
        'ParameterBindingException', 'ParserError', 'CommandNotFoundException', 'RuntimeException',
        'IdentityNotMappedException', 'PrivilegeNotHeldException', 'InvalidAclException', 'SetAccessControl',
        'GetAccessControl', 'GetAccessRules', 'SetOwner', 'SetAccessRuleProtection', 'AccessDenied',
        'IdentityTranslationFailure', 'RequiredPrivilegeMissing', 'OwnerAssignmentRefused',
        'PrivateAclVerificationFailed', 'InvalidHandle', 'AssemblyLoadFailure', 'NoStderr', 'UnclassifiedStderr')
    for ($index = 0; $index -lt 2; $index++) {
        $stage = @('report-file', 'temp-directory')[$index]
        $result = $record.results[$index]
        Assert-ExactKeys $result @('stage', 'result')
        Assert-OwnerResearch ($result.stage -ceq $stage -and $result.result -cin @('refused', 'secured'))
        $call = $record.calls[$index]
        Assert-ExactKeys $call @('stage', 'status', 'elapsed_ms', 'exit_code', 'stderr_state', 'stderr_bytes',
            'stderr_retained_bytes', 'stderr_truncated', 'categories', 'hresults', 'win32_codes')
        Assert-OwnerResearch ($call.stage -ceq $stage -and $call.status -cin @('ok', 'nonzero', 'timeout', 'os-error'))
        Assert-ResearchInteger $call.elapsed_ms 0 120000
        if ($null -ne $call.exit_code) { Assert-ResearchInteger $call.exit_code -2147483648 4294967295 }
        Assert-OwnerResearch ($call.stderr_state -cin @('complete', 'incomplete', 'unavailable', 'read-error'))
        Assert-ResearchInteger $call.stderr_bytes 0 2147483647
        Assert-ResearchInteger $call.stderr_retained_bytes 0 8192
        Assert-OwnerResearch ($call.stderr_truncated -is [bool] -and $call.stderr_truncated -eq ($call.stderr_bytes -gt 8192))
        Assert-OwnerResearch ($call.categories -is [array] -and $call.categories.Count -ge 1 -and $call.categories.Count -le 16)
        foreach ($category in $call.categories) { Assert-OwnerResearch ($category -is [string] -and $category -cin $categories) }
        foreach ($pair in @(@('hresults', [long]-2147483648), @('win32_codes', [long]0))) {
            $values = $call[$pair[0]]
            Assert-OwnerResearch ($values -is [array] -and $values.Count -le 4)
            foreach ($value in $values) { Assert-ResearchInteger $value $pair[1] 4294967295 }
        }
        Assert-OwnerResearch (($result.result -ceq 'secured') -eq ($call.status -ceq 'ok'))
        $safe = [ordered]@{ event = 'modify-only-acl-research'; schema_version = 1; stage = $stage; result = $result.result }
        foreach ($key in @('status', 'elapsed_ms', 'exit_code', 'stderr_state', 'stderr_bytes',
            'stderr_retained_bytes', 'stderr_truncated', 'categories', 'hresults', 'win32_codes')) { $safe[$key] = $call[$key] }
        Write-Output ('DRMR_ACL_RESEARCH ' + ($safe | ConvertTo-Json -Depth 8 -Compress))
        if ($call.status -ceq 'nonzero' -and $call.stderr_state -ceq 'complete' -and 'AccessDenied' -cin $call.categories -and 'SetAccessControl' -cin $call.categories) {
            $classifiedFailure = $true
        }
    }
} catch {
    $category = $_.Exception.GetType().Name
    if ($category -cnotin @('MemberExistsException', 'InvalidOperationException', 'UnauthorizedAccessException', 'ItemNotFoundException', 'Win32Exception', 'RuntimeException', 'MethodInvocationException')) { $category = 'UnclassifiedException' }
    Write-Output ('DRMR_ACL_OWNER_SETUP ' + ([ordered]@{ stage = $researchStage; category = $category; hresult = [long]$_.Exception.HResult } | ConvertTo-Json -Compress))
    Write-Output 'DRMR_ACL_OWNER_RESEARCH_FAILURE'
    throw 'The controlled owner diagnostic failed'
} finally {
    Complete-DrmrArchiveOwner -Directory $directory -ExpectedParent $parent -ExpectedIdentity $directoryIdentity -Python $python -PowerShell $shell -WorkingDirectory $checkout -Credential $credential -UserSid $userSid -Process $process -DirectoryCreated $directoryCreated -ExecutionAttempted $executionAttempted -ExecutionExited $executionExited
    if (-not (Test-Path -LiteralPath $directory)) { Write-Output 'DRMR_ACL_OWNER_OWNED_CLEANUP_CONFIRMED' }
}
Assert-OwnerResearch (-not (Test-Path -LiteralPath $directory))
if ($classifiedFailure) {
    Write-Output 'DRMR_ACL_MODIFY_ONLY_ACCESS_DENIED_CONFIRMED'
    if ($env:GITHUB_OUTPUT) { Add-Content -LiteralPath $env:GITHUB_OUTPUT 'classified_failure=true' }
} elseif ($env:GITHUB_OUTPUT) { Add-Content -LiteralPath $env:GITHUB_OUTPUT 'classified_failure=false' }
