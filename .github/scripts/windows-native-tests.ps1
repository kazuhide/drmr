$ErrorActionPreference = 'Stop'

# Build with the hosted runner's toolchain, then run the same native binaries as
# an ordinary user. Elevated tokens can assign Administrators as a new file's
# owner, which is deliberately outside the media workspace security profile.
$builds = @(
    @('-p', 'drmr-core', '--lib', '--tests'),
    @('-p', 'drmr', '--tests'),
    @('-p', 'drmr-test-support', '--tests')
)
$executables = @()
foreach ($build in $builds) {
    $messages = & cargo test @build --locked --no-run --message-format=json
    if ($LASTEXITCODE -ne 0) { throw 'Native test compilation failed' }
    foreach ($line in $messages) {
        $message = $line | ConvertFrom-Json
        if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and $message.executable) {
            $executables += $message.executable
        }
    }
}
if ($executables.Count -lt 4) { throw 'Native test binaries are missing' }
$executables = @($executables | Sort-Object -Unique)

$username = 'drmr_native_test'
$password = [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(36)) + '!aA1'
Write-Output "::add-mask::$password"
$securePassword = ConvertTo-SecureString $password -AsPlainText -Force
$user = New-LocalUser -Name $username -Password $securePassword -Description 'Ephemeral drmr CI qualification user'
$users = Get-LocalGroup -SID 'S-1-5-32-545'
if (-not (Get-LocalGroupMember -Group $users | Where-Object SID -eq $user.SID)) {
    Add-LocalGroupMember -Group $users -Member $user
}
$credential = [PSCredential]::new("$env:COMPUTERNAME\$username", $securePassword)
$directory = Join-Path $env:RUNNER_TEMP 'drmr-native-user'
New-Item -ItemType Directory -Path $directory | Out-Null
& icacls $directory /grant "${username}:(OI)(CI)M" /T /Q
if ($LASTEXITCODE -ne 0) { throw 'Could not grant the test workspace' }
& icacls (Get-Location).Path /grant "${username}:(OI)(CI)RX" /T /Q
if ($LASTEXITCODE -ne 0) { throw 'Could not grant public test artifact access' }
$executables | ConvertTo-Json | Set-Content (Join-Path $directory 'executables.json')
$script = Join-Path $directory 'run.ps1'
@'
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Native media qualification must run without administrator privileges'
}
$env:TEMP = Join-Path $PSScriptRoot 'temp'
$env:TMP = $env:TEMP
New-Item -ItemType Directory -Path $env:TEMP | Out-Null
$executables = Get-Content (Join-Path $PSScriptRoot 'executables.json') | ConvertFrom-Json
$failed = $false
foreach ($executable in $executables) {
    Write-Output "Running $executable"
    & $executable --nocapture
    if ($LASTEXITCODE -ne 0) { $failed = $true }
}
if ($failed) { exit 1 }
exit 0
'@ | Set-Content $script
$stdout = Join-Path $directory 'stdout.log'
$stderr = Join-Path $directory 'stderr.log'
try {
    $process = Start-Process -FilePath (Get-Command pwsh).Source -Credential $credential -LoadUserProfile -WorkingDirectory (Get-Location).Path -ArgumentList @('-NoLogo', '-NoProfile', '-File', $script) -RedirectStandardOutput $stdout -RedirectStandardError $stderr -Wait -PassThru
    Get-Content $stdout
    Get-Content $stderr
    if ($process.ExitCode -ne 0) { throw "Native tests failed with exit code $($process.ExitCode)" }
} finally {
    Remove-LocalUser -Name $username
}
