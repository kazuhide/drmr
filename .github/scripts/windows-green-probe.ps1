$ErrorActionPreference = 'Stop'
$username = 'drmr_native_test'
$password = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(36)) + '!aA1'
Write-Output "::add-mask::$password"
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$user = New-LocalUser -Name $username -Password $secure -Description 'Ephemeral frozen product qualification user'
$users = Get-LocalGroup -SID 'S-1-5-32-545'
if (-not (Get-LocalGroupMember -Group $users | Where-Object SID -eq $user.SID)) { Add-LocalGroupMember -Group $users -Member $user }
$credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $secure)
$directory = Join-Path $env:RUNNER_TEMP 'drmr-native-user'
New-Item -ItemType Directory -Path $directory | Out-Null
& icacls $directory /grant "${username}:(OI)(CI)M" /T /Q | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Workspace grant failed' }
& icacls (Get-Location).Path /grant "${username}:(OI)(CI)RX" /T /Q | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Source grant failed' }
& icacls $env:DRMR_WINDOWS_MEDIA_TOOLS /grant "${username}:(OI)(CI)RX" /T /Q | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Tool grant failed' }
$config = @{
    python=(Get-Command python).Source
    tools=$env:DRMR_WINDOWS_MEDIA_TOOLS
    source=(Get-Location).Path
    workflow_head=$env:GITHUB_SHA
    product_head=$env:DRMR_PRODUCT_SOURCE_HEAD
    product_tree=$env:DRMR_PRODUCT_SOURCE_TREE
    source_fingerprint=$env:DRMR_PRODUCT_SOURCE_FINGERPRINT
    runtime_fingerprint=$env:DRMR_PRODUCT_RUNTIME_FINGERPRINT
    source_count=[int]$env:DRMR_PRODUCT_SOURCE_FILE_COUNT
    runtime_count=[int]$env:DRMR_PRODUCT_RUNTIME_FILE_COUNT
    expected_encrypted=[int]$env:DRMR_EXPECT_ENCRYPTED_TESTS
    expected_manifest=[int]$env:DRMR_EXPECT_MANIFEST_TESTS
    executables=Get-Content (Join-Path $env:RUNNER_TEMP 'drmr-green-executables.json') | ConvertFrom-Json
    rustc=Get-Content (Join-Path $env:RUNNER_TEMP 'drmr-green-rustc.txt') -Raw
    cargo=Get-Content (Join-Path $env:RUNNER_TEMP 'drmr-green-cargo.txt') -Raw
}
$config | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $directory 'config.json')
@'
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Qualification must be unprivileged' }
$config = Get-Content (Join-Path $PSScriptRoot 'config.json') | ConvertFrom-Json
$env:TEMP = Join-Path $PSScriptRoot 'temp'
$env:TMP = $env:TEMP
$env:PATH = "$(Split-Path $config.python);$($config.tools);$env:PATH"
New-Item -ItemType Directory -Path $env:TEMP | Out-Null
& $config.python (Join-Path $config.source '.github/scripts/windows-green-probe.py') (Join-Path $PSScriptRoot 'config.json')
exit $LASTEXITCODE
'@ | Set-Content (Join-Path $directory 'run.ps1')
try {
    $process = Start-Process -FilePath (Get-Command pwsh).Source -Credential $credential -LoadUserProfile -WorkingDirectory (Get-Location).Path -ArgumentList @('-NoLogo','-NoProfile','-File',(Join-Path $directory 'run.ps1')) -RedirectStandardOutput (Join-Path $directory 'stdout.log') -RedirectStandardError (Join-Path $directory 'stderr.log') -Wait -PassThru
    Get-Content (Join-Path $directory 'stdout.log')
    $stderr = Get-Content (Join-Path $directory 'stderr.log') -Raw
    if ($stderr) { Write-Output '{"qualification":"launcher","unexpected_stderr":true}'; throw 'Qualification emitted unexpected launcher diagnostics' }
    if ($process.ExitCode -ne 0) { throw "Frozen product qualification failed: $($process.ExitCode)" }
} finally { Remove-LocalUser -Name $username }
