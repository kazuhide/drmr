param([ValidateSet('direct','focused')][string] $Mode)
$ErrorActionPreference = 'Stop'
$username = 'drmr_path_probe'
$password = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(36)) + '!aA1'
Write-Output "::add-mask::$password"
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$user = New-LocalUser -Name $username -Password $secure -Description 'Ephemeral isolated diagnostic user'
$users = Get-LocalGroup -SID 'S-1-5-32-545'
if (-not (Get-LocalGroupMember -Group $users | Where-Object SID -eq $user.SID)) { Add-LocalGroupMember -Group $users -Member $user }
$credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $secure)
$directory = Join-Path $env:RUNNER_TEMP "drmr-path-probe-$Mode"
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
    mode=$Mode
    sha=$env:GITHUB_SHA
}
if ($Mode -eq 'focused') {
    $config.executables=Get-Content (Join-Path $env:RUNNER_TEMP 'drmr-probe-executables.json') | ConvertFrom-Json
}
$config | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $directory 'config.json')
@'
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Probe must be unprivileged' }
$config = Get-Content (Join-Path $PSScriptRoot 'config.json') | ConvertFrom-Json
$env:TEMP = Join-Path $PSScriptRoot 'temp'
$env:TMP = $env:TEMP
$env:PATH = "$(Split-Path $config.python);$($config.tools);$env:PATH"
New-Item -ItemType Directory -Path $env:TEMP | Out-Null
& $config.python (Join-Path $config.source '.github/scripts/windows-path-probe.py') (Join-Path $PSScriptRoot 'config.json')
exit $LASTEXITCODE
'@ | Set-Content (Join-Path $directory 'run.ps1')
try {
    $process = Start-Process -FilePath (Get-Command pwsh).Source -Credential $credential -LoadUserProfile -WorkingDirectory (Get-Location).Path -ArgumentList @('-NoLogo','-NoProfile','-File',(Join-Path $directory 'run.ps1')) -RedirectStandardOutput (Join-Path $directory 'stdout.log') -RedirectStandardError (Join-Path $directory 'stderr.log') -Wait -PassThru
    Get-Content (Join-Path $directory 'stdout.log')
    $stderr = Get-Content (Join-Path $directory 'stderr.log') -Raw
    if ($stderr) { Write-Output '{"probe":"launcher","unexpected_stderr":true}' }
    if ($process.ExitCode -ne 0) { throw "Categorical probe process failed: $($process.ExitCode)" }
} finally { Remove-LocalUser -Name $username }
