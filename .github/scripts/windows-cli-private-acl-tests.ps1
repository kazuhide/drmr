param([switch]$Ordinary, [string]$Directory, [string]$Checkout, [string]$Python,
    [string]$ExpectedParent, [string]$ExpectedIdentity, [string]$ExpectedTempIdentity,
    [string]$ExpectedCollectorSid)

$ErrorActionPreference = 'Stop'
$script:PrivateAclRegressionStage = 'entered'

function Assert-PrivateAclRegression {
    param([bool]$Condition)
    if (-not $Condition) { throw 'Private ACL regression assertion failed' }
}

function Get-PrivateAclSha256 {
    param([byte[]]$Bytes)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($algorithm.ComputeHash($Bytes)).Replace('-', '').ToLowerInvariant() }
    finally { $algorithm.Dispose() }
}

function Initialize-PrivateAclRegression {
    param([string]$Root, [Security.Principal.SecurityIdentifier]$UserSid, [string]$ExpectedParent,
        [string]$ExpectedRootIdentity, [string]$Python, [string]$Checkout)
    Assert-DrmrArchiveDirectory $Root $ExpectedParent $ExpectedRootIdentity $Python
    $collector = [Security.Principal.WindowsIdentity]::GetCurrent().User
    Assert-PrivateAclRegression ($collector.Value -ne $UserSid.Value -and $collector.Value -ne 'S-1-5-18')
    $null = & icacls $Root /grant:r "*$($collector.Value):(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' "*$($UserSid.Value):(OI)(CI)M" /Q 2>&1
    Assert-PrivateAclRegression ($LASTEXITCODE -eq 0)
    $null = & icacls $Root /inheritance:r /Q 2>&1
    Assert-PrivateAclRegression ($LASTEXITCODE -eq 0)
    $acl = Get-Acl -LiteralPath $Root
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.IdentityReference.Value -notin @($collector.Value, $UserSid.Value, 'S-1-5-18')) {
            $null = & icacls $Root /remove "*$($rule.IdentityReference.Value)" /Q 2>&1
            Assert-PrivateAclRegression ($LASTEXITCODE -eq 0)
        }
    }
    Assert-ControlledPrivateAclParent $Root $UserSid $collector.Value
    foreach ($relative in @('docs', 'site-content', 'site-content/guide', 'docs/windows.md', 'site-content/guide/en.md', 'site-content/guide/ja.md')) {
        $null = & icacls (Join-Path $Checkout $relative) /grant "*$($UserSid.Value):RX" /Q 2>&1
        Assert-PrivateAclRegression ($LASTEXITCODE -eq 0)
    }
    Assert-DrmrArchiveDirectory $Root $ExpectedParent $ExpectedRootIdentity $Python
    $temporary = Join-Path $Root 'temp'
    Assert-PrivateAclRegression (-not (Test-Path -LiteralPath $temporary))
    New-Item -ItemType Directory -Path $temporary | Out-Null
    foreach ($label in @('helper', 'windows', 'en', 'ja')) {
        foreach ($kind in @('file', 'directory')) {
            $path = Join-Path $temporary ($label + '-foreign-' + $kind)
            if ($kind -eq 'directory') {
                New-Item -ItemType Directory -Path $path | Out-Null
                [IO.File]::WriteAllText((Join-Path $path 'keep.txt'), 'unchanged synthetic payload')
                $permission = "*$($UserSid.Value):(OI)(CI)F"
            } else {
                [IO.File]::WriteAllText($path, 'unchanged synthetic payload')
                $permission = "*$($UserSid.Value):F"
            }
            $null = & icacls $path /grant:r $permission /T /Q 2>&1
            Assert-PrivateAclRegression ($LASTEXITCODE -eq 0)
        }
    }
}

function Assert-ControlledPrivateAclParent {
    param([string]$Root, [Security.Principal.SecurityIdentifier]$UserSid, [string]$CollectorSid)
    Assert-PrivateAclRegression ($CollectorSid -ne $UserSid.Value -and $CollectorSid -ne 'S-1-5-18')
    $acl = Get-Acl -LiteralPath $Root
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    Assert-PrivateAclRegression ($acl.AreAccessRulesProtected -and $rules.Count -eq 3)
    foreach ($rule in $rules) {
        Assert-PrivateAclRegression (-not $rule.IsInherited -and
            $rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
            $rule.IdentityReference.Value -in @($UserSid.Value, $CollectorSid, 'S-1-5-18') -and
            [int]$rule.InheritanceFlags -eq 3 -and [int]$rule.PropagationFlags -eq 0)
        if ($rule.IdentityReference.Equals($UserSid)) {
            $modify = [Security.AccessControl.FileSystemRights]::Modify
            $allowed = $modify -bor [Security.AccessControl.FileSystemRights]::Synchronize
            Assert-PrivateAclRegression (($rule.FileSystemRights -band $modify) -eq $modify -and
                ($rule.FileSystemRights -band (-bnot $allowed)) -eq 0)
        } else {
            Assert-PrivateAclRegression ($rule.FileSystemRights -eq [Security.AccessControl.FileSystemRights]::FullControl)
        }
    }
    Assert-PrivateAclRegression (@($rules | Where-Object { $_.IdentityReference.Equals($UserSid) }).Count -eq 1)
}

function Get-PrivateAclSnapshot {
    param([string]$Path, [bool]$IsDirectory, [string]$Python)
    $sections = [Security.AccessControl.AccessControlSections]'Owner, Access'
    $acl = if ($IsDirectory) { [IO.Directory]::GetAccessControl($Path, $sections) }
        else { [IO.File]::GetAccessControl($Path, $sections) }
    $code = @'
import os, sys
s = os.lstat(sys.argv[1])
assert not getattr(s, 'st_file_attributes', 0) & 0x400
print(str(s.st_dev) + ':' + str(s.st_ino))
'@
    $identity = & $Python -B -c $code $Path
    Assert-PrivateAclRegression ($LASTEXITCODE -eq 0 -and $identity -is [string] -and $identity -match '^[0-9]+:[0-9]+$')
    $payload = if ($IsDirectory) { Join-Path $Path 'keep.txt' } else { $Path }
    return [ordered]@{ descriptor = $acl.GetSecurityDescriptorSddlForm($sections); identity = $identity;
        payload = Get-PrivateAclSha256 ([IO.File]::ReadAllBytes($payload)) }
}

function Assert-PrivateAclSnapshot {
    param($Before, [string]$Path, [bool]$IsDirectory, [string]$Python)
    $after = Get-PrivateAclSnapshot $Path $IsDirectory $Python
    Assert-PrivateAclRegression ($Before.descriptor -ceq $after.descriptor -and
        $Before.identity -ceq $after.identity -and $Before.payload -ceq $after.payload)
}

function Assert-PrivateAclProfile {
    param([string]$Path, [bool]$IsDirectory, [Security.Principal.SecurityIdentifier]$Sid, [int]$Inheritance)
    $sections = [Security.AccessControl.AccessControlSections]'Owner, Access'
    $acl = if ($IsDirectory) { [IO.Directory]::GetAccessControl($Path, $sections) }
        else { [IO.File]::GetAccessControl($Path, $sections) }
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    Assert-PrivateAclRegression ($acl.AreAccessRulesProtected -and $acl.GetOwner([Security.Principal.SecurityIdentifier]).Equals($Sid) -and $rules.Count -eq 1)
    $rule = $rules[0]
    Assert-PrivateAclRegression ($rule.IdentityReference.Equals($Sid) -and -not $rule.IsInherited -and
        $rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
        $rule.FileSystemRights -eq [Security.AccessControl.FileSystemRights]::FullControl -and
        [int]$rule.InheritanceFlags -eq $Inheritance -and
        $rule.PropagationFlags -eq [Security.AccessControl.PropagationFlags]::None)
}

function Test-PrivateAclRegression {
    param([string]$Root, [string]$Checkout, [string]$Python,
        [string]$ExpectedParent, [string]$ExpectedRootIdentity, [string]$ExpectedTempIdentity, [string]$ExpectedCollectorSid)
    . (Join-Path $PSScriptRoot 'windows-cli-archive-cleanup.ps1') -Directory $Root -ExpectedParent $ExpectedParent -ExpectedIdentity $ExpectedRootIdentity -Python $Python
    $script:PrivateAclRegressionStage = 'root-identity'
    Assert-DrmrArchiveDirectory $Root $ExpectedParent $ExpectedRootIdentity $Python
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $script:PrivateAclRegressionStage = 'ordinary-user'
    Assert-PrivateAclRegression (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))
    $sid = $identity.User
    $script:PrivateAclRegressionStage = 'parent-profile'
    Assert-ControlledPrivateAclParent $Root $sid $ExpectedCollectorSid
    $temporary = Join-Path $Root 'temp'
    $script:PrivateAclRegressionStage = 'temporary-identity'
    Assert-PrivateAclRegression ((Get-DrmrArchiveDirectoryIdentity $temporary $Python) -ceq $ExpectedTempIdentity)
    $acl = [IO.Directory]::GetAccessControl($temporary, [Security.AccessControl.AccessControlSections]'Owner, Access')
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    $ownRules = @($rules | Where-Object { $_.IdentityReference.Equals($sid) })
    $script:PrivateAclRegressionStage = 'temporary-profile'
    Assert-PrivateAclRegression ($ownRules.Count -eq 1 -and
        ($ownRules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::Modify) -eq [Security.AccessControl.FileSystemRights]::Modify -and
        ($ownRules[0].FileSystemRights -band ([Security.AccessControl.FileSystemRights]::TakeOwnership -bor [Security.AccessControl.FileSystemRights]::ChangePermissions)) -eq 0 -and
        @($rules | Where-Object { $_.IdentityReference.Value -eq 'S-1-3-0' }).Count -eq 0)
    $before = @{}
    $script:PrivateAclRegressionStage = 'foreign-permission'
    foreach ($label in @('helper', 'windows', 'en', 'ja')) {
        foreach ($kind in @('file', 'directory')) {
            $name = $label + '-foreign-' + $kind
            $path = Join-Path $temporary $name
            $foreignAcl = Get-Acl -LiteralPath $path
            $foreignRules = @($foreignAcl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
            Assert-PrivateAclRegression (-not $foreignAcl.GetOwner([Security.Principal.SecurityIdentifier]).Equals($sid))
            Assert-PrivateAclRegression (@($foreignRules | Where-Object { $_.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow }).Count -eq 0)
            Assert-PrivateAclRegression (@($foreignRules | Where-Object {
                $_.IdentityReference.Equals($sid) -and -not $_.IsInherited -and
                $_.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
                $_.FileSystemRights -eq [Security.AccessControl.FileSystemRights]::FullControl
            }).Count -eq 1)
            $before[$name] = Get-PrivateAclSnapshot $path ($kind -eq 'directory') $Python
        }
    }
    foreach ($kind in @('file', 'directory')) {
        $path = Join-Path $temporary ('helper-current-' + $kind)
        if ($kind -eq 'directory') { New-Item -ItemType Directory -Path $path | Out-Null }
        else { [IO.File]::WriteAllText($path, 'unchanged synthetic payload') }
        Assert-PrivateAclRegression ((Get-Acl -LiteralPath $path).GetOwner([Security.Principal.SecurityIdentifier]).Equals($sid))
    }
    $helper = Join-Path $Checkout 'tools/qualify_cli_archive.py'
    $code = @'
import importlib.util, json, pathlib, sys
sys.dont_write_bytecode = True
try:
    root, helper = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    spec = importlib.util.spec_from_file_location('qualify_cli_archive', helper)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    for directory in (False, True):
        kind = 'directory' if directory else 'file'
        path = root / ('helper-current-' + kind)
        module.secure_windows(path, directory)
        try:
            module.secure_windows(root / ('helper-foreign-' + kind), directory)
        except module.Refusal as error:
            if str(error) != 'private-acl-unavailable':
                sys.exit(2)
        else:
            sys.exit(3)
    sys.exit(0)
except Exception:
    sys.exit(4)
'@
    $script:PrivateAclRegressionStage = 'helper-run'
    & $Python -B -c $code $temporary $helper
    Assert-PrivateAclRegression ($LASTEXITCODE -eq 0)
    foreach ($kind in @('file', 'directory')) {
        $isDirectory = $kind -eq 'directory'
        $script:PrivateAclRegressionStage = 'helper-positive'
        Assert-PrivateAclProfile (Join-Path $temporary ('helper-current-' + $kind)) $isDirectory $sid $(if ($isDirectory) { 3 } else { 0 })
        $script:PrivateAclRegressionStage = 'helper-negative-snapshot'
        Assert-PrivateAclSnapshot $before['helper-foreign-' + $kind] (Join-Path $temporary ('helper-foreign-' + $kind)) $isDirectory $Python
    }
    $documents = @(
        @('windows', 'docs/windows.md'),
        @('en', 'site-content/guide/en.md'),
        @('ja', 'site-content/guide/ja.md'))
    $documentProofs = @()
    foreach ($document in $documents) {
        $script:PrivateAclRegressionStage = 'document-extraction'
        $label, $relative = $document
        $bytes = [IO.File]::ReadAllBytes((Join-Path $Checkout $relative))
        $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
        $functions = @()
        foreach ($block in [regex]::Matches($text, '(?ms)^```powershell\r?\n(.*?)^```\s*$')) {
            $tokens = $null
            $errors = $null
            $ast = [Management.Automation.Language.Parser]::ParseInput($block.Groups[1].Value, [ref]$tokens, [ref]$errors)
            Assert-PrivateAclRegression ($errors.Count -eq 0)
            $functions += @($ast.FindAll({ param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Set-OwnerOnlyAcl'
            }, $true))
        }
        Assert-PrivateAclRegression ($functions.Count -eq 1)
        $functionText = $functions[0].Extent.Text
        . ([ScriptBlock]::Create($functionText))
        foreach ($kind in @('file', 'directory')) {
            $isDirectory = $kind -eq 'directory'
            $path = Join-Path $temporary ($label + '-current-' + $kind)
            if ($isDirectory) { New-Item -ItemType Directory -Path $path | Out-Null }
            else { [IO.File]::WriteAllText($path, 'unchanged synthetic payload') }
            Assert-PrivateAclRegression ((Get-Acl -LiteralPath $path).GetOwner([Security.Principal.SecurityIdentifier]).Equals($sid))
            $security = if ($isDirectory) { [Security.AccessControl.DirectorySecurity]::new() }
                else { [Security.AccessControl.FileSecurity]::new() }
            $script:PrivateAclRegressionStage = 'document-positive'
            Set-OwnerOnlyAcl $path $security
            Assert-PrivateAclProfile $path $isDirectory $sid 0
            $name = $label + '-foreign-' + $kind
            $path = Join-Path $temporary $name
            $security = if ($isDirectory) { [Security.AccessControl.DirectorySecurity]::new() }
                else { [Security.AccessControl.FileSecurity]::new() }
            $refused = $false
            $script:PrivateAclRegressionStage = 'document-negative'
            try { Set-OwnerOnlyAcl $path $security }
            catch { Assert-PrivateAclRegression ($_.Exception.Message -ceq 'The current user must own the path'); $refused = $true }
            Assert-PrivateAclRegression $refused
            Assert-PrivateAclSnapshot $before[$name] $path $isDirectory $Python
        }
        $documentProofs += [ordered]@{ document = $relative;
            source_sha256 = Get-PrivateAclSha256 ([Text.UTF8Encoding]::new($false).GetBytes($text.Replace("`r`n", "`n")));
            function_sha256 = Get-PrivateAclSha256 ([Text.UTF8Encoding]::new($false).GetBytes($functionText.Replace("`r`n", "`n"))) }
    }
    $proof = [ordered]@{ schema_version = 1; ordinary_user = $true; modify_parent = $true;
        helper_positive = 2; helper_foreign_owner_refusals = 2; document_positive = 6;
        document_foreign_owner_refusals = 6; foreign_owner_full_control = $true;
        foreign_owner_descriptor_identity_payload_unchanged = $true; documents = $documentProofs }
    [IO.File]::WriteAllText((Join-Path $Root 'private-acl-proof.json'), ($proof | ConvertTo-Json -Depth 6 -Compress), [Text.UTF8Encoding]::new($false))
    $script:PrivateAclRegressionStage = 'owned-cleanup'
    Assert-DrmrArchiveDirectory $Root $ExpectedParent $ExpectedRootIdentity $Python
    Assert-PrivateAclRegression ((Get-DrmrArchiveDirectoryIdentity $temporary $Python) -ceq $ExpectedTempIdentity)
    Assert-NoDrmrArchiveReparsePoint $temporary
    Remove-Item -LiteralPath $temporary -Recurse -Force
    Assert-PrivateAclRegression (-not (Test-Path -LiteralPath $temporary))
}

if ($Ordinary) {
    try { Test-PrivateAclRegression $Directory $Checkout $Python $ExpectedParent $ExpectedIdentity $ExpectedTempIdentity $ExpectedCollectorSid; exit 0 }
    catch {
        $category = $_.Exception.GetType().Name
        if ($category -cnotin @('RuntimeException', 'MethodInvocationException', 'UnauthorizedAccessException', 'ItemNotFoundException', 'Win32Exception')) { $category = 'UnclassifiedException' }
        $failure = [ordered]@{ schema_version = 1; stage = $script:PrivateAclRegressionStage;
            category = $category; hresult = [long]$_.Exception.HResult }
        try { [IO.File]::WriteAllText((Join-Path $Directory 'private-acl-failure.json'),
            ($failure | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false)) } catch { }
        exit 8
    }
}
