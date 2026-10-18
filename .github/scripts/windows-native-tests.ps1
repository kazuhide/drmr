$ErrorActionPreference = 'Stop'

# Build with the hosted runner's toolchain, then run the same native binaries as
# an ordinary user. Elevated tokens can assign Administrators as a new file's
# owner, which is deliberately outside the media workspace security profile.
$builds = @(
    @('-p', 'drmr-core', '--lib', '--tests'),
    @('-p', 'drmr', '--tests'),
    @('-p', 'drmr-server', '--lib', '--tests'),
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
& icacls $env:DRMR_WINDOWS_MEDIA_TOOLS /grant "${username}:(OI)(CI)RX" /T /Q
if ($LASTEXITCODE -ne 0) { throw 'Could not grant media tool access' }
$env:DRMR_WINDOWS_MEDIA_TOOLS | Set-Content (Join-Path $directory 'media-tools.txt')
(Get-Command python).Source | Set-Content (Join-Path $directory 'python.txt')
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
$tools = (Get-Content (Join-Path $PSScriptRoot 'media-tools.txt')).Trim()
$python = (Get-Content (Join-Path $PSScriptRoot 'python.txt')).Trim()
$env:PATH = "$(Split-Path $python);$tools;$env:PATH"
$env:DRMR_MEDIA_REPORT = Join-Path $PSScriptRoot 'bbb-media-report.json'
$env:DRMR_CONVERSION_REPORT = Join-Path $PSScriptRoot 'conversion-report.json'
foreach ($tool in @('ffmpeg', 'ffprobe', 'mp4dump', 'mp4encrypt', 'packager', 'python')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "Required media tool unavailable: $tool" }
}
& ffmpeg -hide_banner -version
if ($LASTEXITCODE -ne 0) { throw 'FFmpeg could not run as a regular user' }
& packager --version
if ($LASTEXITCODE -ne 0) { throw 'Packager could not run as a regular user' }
& python --version
if ($LASTEXITCODE -ne 0) { throw 'Python could not run as a regular user' }
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
    $text = (Get-Content $stdout -Raw) + (Get-Content $stderr -Raw)
    foreach ($match in [regex]::Matches($text, 'skipped: [^\r\n]*')) {
        if ($match.Value -ne 'skipped: host FFmpeg lacks TTML-in-MP4 muxing') {
            throw "Unexpected test skip: $($match.Value)"
        }
    }
    $expectedHelpers = @(
        'child_reads_the_process_environment',
        'child_transcrypts_with_relative_paths'
    )
    $ignored = @([regex]::Matches($text, 'test (\S+) \.\.\. ignored(?:, [^\r\n]*)?') | ForEach-Object { $_.Groups[1].Value })
    if (@(Compare-Object ($expectedHelpers | Sort-Object) ($ignored | Sort-Object)).Count -ne 0) {
        throw 'Ignored tests must be exactly the two isolated child-process helpers'
    }
    if ($process.ExitCode -ne 0) { throw "Native tests failed with exit code $($process.ExitCode)" }
} finally {
    Remove-LocalUser -Name $username
}
