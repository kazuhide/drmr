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
$cargo = (& rustup which cargo).Trim()
if ($LASTEXITCODE -ne 0) { throw 'The selected native Cargo is unavailable' }
$rustc = (& rustup which rustc).Trim()
if ($LASTEXITCODE -ne 0) { throw 'The selected native Rust compiler is unavailable' }

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
$rustToolDirectories = @((Split-Path $cargo), (Split-Path $rustc)) | Sort-Object -Unique
foreach ($path in $rustToolDirectories) {
    & icacls $path /grant "${username}:(OI)(CI)RX" /T /Q
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant read-only Rust tool access' }
}
$env:DRMR_WINDOWS_MEDIA_TOOLS | Set-Content (Join-Path $directory 'media-tools.txt')
(Get-Command python).Source | Set-Content (Join-Path $directory 'python.txt')
$cargo | Set-Content (Join-Path $directory 'cargo.txt')
$rustc | Set-Content (Join-Path $directory 'rustc.txt')
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
$cargo = (Get-Content (Join-Path $PSScriptRoot 'cargo.txt')).Trim()
$rustc = (Get-Content (Join-Path $PSScriptRoot 'rustc.txt')).Trim()
# Use the selected binaries directly: the ordinary user has no rustup installation.
$env:CARGO_HOME = Join-Path $PSScriptRoot 'cargo-home'
$env:RUSTC = $rustc
$env:PATH = "$(Split-Path $python);$(Split-Path $cargo);$(Split-Path $rustc);$tools;$env:PATH"
$env:DRMR_MEDIA_REPORT = Join-Path $PSScriptRoot 'bbb-media-report.json'
$env:DRMR_CONVERSION_REPORT = Join-Path $PSScriptRoot 'conversion-report.json'
foreach ($tool in @('ffmpeg', 'ffprobe', 'mp4dump', 'mp4encrypt', 'packager', 'python', 'cargo', 'rustc')) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "Required native test tool unavailable: $tool" }
}
& ffmpeg -hide_banner -version
if ($LASTEXITCODE -ne 0) { throw 'FFmpeg could not run as a regular user' }
& packager --version
if ($LASTEXITCODE -ne 0) { throw 'Packager could not run as a regular user' }
& python --version
if ($LASTEXITCODE -ne 0) { throw 'Python could not run as a regular user' }
& $cargo --version
if ($LASTEXITCODE -ne 0) { throw 'The selected Cargo could not run as a regular user' }
& $rustc --version
if ($LASTEXITCODE -ne 0) { throw 'The selected Rust compiler could not run as a regular user' }
New-Item -ItemType Directory -Path $env:TEMP | Out-Null
New-Item -ItemType Directory -Path $env:CARGO_HOME | Out-Null
# Check release identity and real private-file ACLs before long native media runs.
foreach ($pattern in @('test_check_release.py', 'test_prepare_release.py', 'test_qualify_cli_archive.py')) {
    & python -W error::ResourceWarning -m unittest discover -s tools/tests -p $pattern -v
    if ($LASTEXITCODE -ne 0) { throw 'Ordinary-user release helper checks failed' }
}
$executables = Get-Content (Join-Path $PSScriptRoot 'executables.json') | ConvertFrom-Json
# Catch directory-layout regressions before the longer real-media matrix. These
# run with the same ordinary user, native binary and ACL policy as the full suite.
$coreExecutables = @($executables | Where-Object {
    [IO.Path]::GetFileName($_) -match '^drmr_core-[0-9a-f]+\.exe$'
})
if ($coreExecutables.Count -ne 1) { throw 'The native core unit-test binary is ambiguous' }
$coreExecutable = $coreExecutables[0]
$layoutChecks = @(
    'native::workspace::windows::tests::exclusive_media_child_creation_preserves_guards_and_existing_entries',
    'conversion::layout::stage::tests::re_layout_moves_media_without_copying_and_rewrites_actual_hls_paths',
    'conversion::download::layout_tests::source_download_keeps_nested_paths_and_child_relative_uris',
    'conversion::download::owned::tests::entrypoint_publication_is_prepaid_before_moving_the_pending_original'
)
foreach ($check in $layoutChecks) {
    $listing = & $coreExecutable --list --exact $check
    if ($LASTEXITCODE -ne 0 -or $listing -notcontains "${check}: test") {
        throw 'A required early layout test is missing'
    }
    & $coreExecutable --exact $check --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'An early ordinary-user layout test failed' }
}
$failed = $false
foreach ($executable in $executables) {
    Write-Output "Running $executable"
    & $executable --nocapture
    if ($LASTEXITCODE -ne 0) { $failed = $true }
}
& python -W error::ResourceWarning -m unittest discover -s tools/tests -v
if ($LASTEXITCODE -ne 0) { $failed = $true }
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
        if ($match.Value -notmatch '^skipped: ffmpeg [0-6] omits packet timing on this fixture(?: \(.*\))?$') {
            throw "Unexpected test skip: $($match.Value)"
        }
    }
    if ([regex]::IsMatch($text, 'test [^\r\n]* \.\.\. ignored\b')) {
        throw 'Native tests must not contain ignored tests'
    }
    if ($process.ExitCode -ne 0) { throw "Native tests failed with exit code $($process.ExitCode)" }
} finally {
    Remove-LocalUser -Name $username
}
