param(
    [Parameter(Mandatory = $true)][string]$Archive,
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Report,
    [string]$RoundtripInputs = ''
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-cli-archive-cleanup.ps1')
if (Test-Path -LiteralPath $Report) { throw 'The qualification report already exists' }
$checkout = (Get-Location).Path
$python = (Get-Command python).Source
$shell = (Get-Command pwsh).Source
$username = 'drmr_archive_test'
# Retain the full nonce and ownership checks without a long scratch label.
$directory = Join-Path $env:RUNNER_TEMP ('q-' + [guid]::NewGuid().ToString('N'))
$directoryCreated = $false
$directoryIdentity = $null
$user = $null
$userSid = $null
$credential = $null
$process = $null
$executionAttempted = $false
$executionExited = $false
try {
    New-Item -ItemType Directory -Path $directory | Out-Null
    $directoryCreated = $true
    $directoryIdentity = Get-DrmrArchiveDirectoryIdentity $directory $python
    $password = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(36)) + '!aA1'
    Write-Output "::add-mask::$password"
    $securePassword = ConvertTo-SecureString $password -AsPlainText -Force
    $user = New-LocalUser -Name $username -Password $securePassword -Description 'Ephemeral drmr archive qualification user'
    $userSid = $user.SID
    $users = Get-LocalGroup -SID 'S-1-5-32-545'
    if (-not (Get-LocalGroupMember -Group $users | Where-Object SID -eq $user.SID)) {
        Add-LocalGroupMember -Group $users -Member $user
    }
    $credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $securePassword)
    $cargo = (& rustup which cargo).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'The selected native Cargo is unavailable' }
    $rustc = (& rustup which rustc).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'The selected native Rust compiler is unavailable' }
    $cargoHome = Join-Path $directory 'cargo-home'
    New-Item -ItemType Directory -Path $cargoHome | Out-Null
    $sourceCargoHome = $env:CARGO_HOME
    if (-not $sourceCargoHome) { $sourceCargoHome = Join-Path $env:USERPROFILE '.cargo' }
    # Copy registry data only: no authentication or operator configuration.
    if (-not $RoundtripInputs) {
        Copy-Item -LiteralPath (Join-Path $sourceCargoHome 'registry') -Destination $cargoHome -Recurse
    }
    $retainedArchive = Join-Path $directory ([IO.Path]::GetFileName($Archive))
    Copy-Item -LiteralPath $Archive -Destination $retainedArchive
    $context = @{
        archive = $retainedArchive
        version = $Version
        checkout = $checkout
        python = $python
        cargo = $cargo
        rustc = $rustc
        tools = $env:DRMR_WINDOWS_MEDIA_TOOLS
        roundtrip_inputs = $RoundtripInputs
    }
    $context | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $directory 'context.json')
    foreach ($path in @($checkout, (Split-Path $cargo), (Split-Path $rustc), $env:DRMR_WINDOWS_MEDIA_TOOLS)) {
        & icacls $path /grant "${username}:(OI)(CI)RX" /T /Q
        if ($LASTEXITCODE -ne 0) { throw 'Could not grant read-only qualification inputs' }
    }
    if ($RoundtripInputs) {
        & icacls $RoundtripInputs /grant "${username}:(OI)(CI)RX" /T /Q
        if ($LASTEXITCODE -ne 0) { throw 'Could not grant checked round-trip inputs' }
    }
    & icacls $directory /grant "${username}:(OI)(CI)M" /T /Q
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant the owned qualification workspace' }
    $script = Join-Path $directory 'run.ps1'
    @'
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Archive media qualification requires an ordinary user'
}
$context = Get-Content -Raw (Join-Path $PSScriptRoot 'context.json') | ConvertFrom-Json
$env:TEMP = Join-Path $PSScriptRoot 'temp'
$env:TMP = $env:TEMP
New-Item -ItemType Directory -Path $env:TEMP | Out-Null
$env:CARGO_HOME = Join-Path $PSScriptRoot 'cargo-home'
$env:PATH = "$(Split-Path $context.python);$(Split-Path $context.cargo);$(Split-Path $context.rustc);$($context.tools);$env:PATH"
$env:GIT_CONFIG_COUNT = '1'
$env:GIT_CONFIG_KEY_0 = 'safe.directory'
$env:GIT_CONFIG_VALUE_0 = $context.checkout
Set-Location -LiteralPath $context.checkout
$privateReport = Join-Path $PSScriptRoot 'qualification.json'
if ($context.roundtrip_inputs) {
    & $context.python tools/release_roundtrip.py --inputs $context.roundtrip_inputs --target x86_64-pc-windows-msvc --report $privateReport
} else {
    & $context.python tools/qualify_cli_archive.py --archive $context.archive --target x86_64-pc-windows-msvc --expected-version $context.version --report $privateReport
}
$status = $LASTEXITCODE
if (Test-Path -LiteralPath $privateReport) {
    # Only allowlisted qualification evidence leaves the ordinary user's ACL.
    # Creating a new file inherits the parent ACL; copying its private ACL would
    # prevent the elevated collector from reading the result.
    [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'public-report.json'), [IO.File]::ReadAllText($privateReport), [Text.UTF8Encoding]::new($false))
}
exit $status
'@ | Set-Content -Encoding utf8 $script
    $stdout = Join-Path $directory 'stdout.log'
    $stderr = Join-Path $directory 'stderr.log'
    $executionAttempted = $true
    $process = Start-Process -FilePath $shell -Credential $credential -LoadUserProfile -WorkingDirectory $checkout -ArgumentList @('-NoLogo', '-NoProfile', '-File', "`"$script`"") -RedirectStandardOutput $stdout -RedirectStandardError $stderr -Wait -PassThru
    $executionExited = $true
    Get-Content -LiteralPath $stdout
    Get-Content -LiteralPath $stderr
    $userReport = Join-Path $directory 'public-report.json'
    if (Test-Path -LiteralPath $userReport) { Copy-Item -LiteralPath $userReport -Destination $Report }
    if ($process.ExitCode -ne 0) { throw 'Extracted CLI media qualification failed' }
    if (-not (Test-Path -LiteralPath $Report)) { throw 'The qualification report is missing' }
    $result = Get-Content -Raw -LiteralPath $Report | ConvertFrom-Json
    if (-not $result.success -or -not $result.native_ordinary_user -or $result.owned_workspace_cleanup -ne 'confirmed') {
        throw 'Extracted CLI qualification was incomplete'
    }
} finally {
    Complete-DrmrArchiveOwner -Directory $directory -ExpectedParent $env:RUNNER_TEMP -ExpectedIdentity $directoryIdentity -Python $python -PowerShell $shell -WorkingDirectory $checkout -Credential $credential -UserSid $userSid -Process $process -DirectoryCreated $directoryCreated -ExecutionAttempted $executionAttempted -ExecutionExited $executionExited
}
