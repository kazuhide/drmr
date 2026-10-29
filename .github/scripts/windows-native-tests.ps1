$ErrorActionPreference = 'Stop'

# Build with the hosted runner's toolchain, then run the same native binaries as
# an ordinary user. Elevated tokens can assign Administrators as a new file's
# owner, which is deliberately outside the media workspace security profile.
# Select the same library, binary and integration test targets in one Cargo graph.
$messages = & cargo test -p drmr-core -p drmr -p drmr-server -p drmr-test-support --tests --locked --no-run --message-format=json
if ($LASTEXITCODE -ne 0) { throw 'Native test compilation failed' }
$executables = @()
foreach ($line in $messages) {
    $message = $line | ConvertFrom-Json
    if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and $message.executable) {
        $executables += $message.executable
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
$executables = Get-Content (Join-Path $PSScriptRoot 'executables.json') | ConvertFrom-Json
# Require critical protection cases to be present, then run each complete binary once.
$coreExecutables = @($executables | Where-Object {
    [IO.Path]::GetFileName($_) -match '^drmr_core-[0-9a-f]+\.exe$'
})
if ($coreExecutables.Count -ne 1) { throw 'The native core unit-test binary is ambiguous' }
$coreExecutable = $coreExecutables[0]
$layoutChecks = @(
    'native::workspace::windows::tests::exclusive_media_child_creation_preserves_guards_and_existing_entries',
    'conversion::files::relocation::tests::late_replacements_are_not_moved_or_deleted_by_source_mutations',
    'conversion::layout::stage::tests::re_layout_moves_media_without_copying_and_rewrites_actual_hls_paths',
    'conversion::download::layout_tests::source_download_keeps_nested_paths_and_child_relative_uris',
    'conversion::download::owned::tests::entrypoint_publication_is_prepaid_before_moving_the_pending_original'
)
foreach ($check in $layoutChecks) {
    $listing = & $coreExecutable --list --exact $check
    if ($LASTEXITCODE -ne 0 -or $listing -notcontains "${check}: test") {
        throw 'A required early layout test is missing'
    }
}
$policyExecutables = @($executables | Where-Object {
    [IO.Path]::GetFileName($_) -match '^conversion_saved_hls_policy-[0-9a-f]+\.exe$'
})
if ($policyExecutables.Count -ne 1) { throw 'The saved HLS policy test binary is ambiguous' }
$policyExecutable = $policyExecutables[0]
$policyChecks = @(
    'new_single_plan_saves_the_actual_request_and_replays_the_source_bound_and_layout',
    'single_schema_one_keeps_two_seconds',
    'source_policy::legacy_segment_plans_keep_generated_seven_while_new_plans_inherit_six'
)
foreach ($check in $policyChecks) {
    $listing = & $policyExecutable --list --exact $check
    if ($LASTEXITCODE -ne 0 -or $listing -notcontains "${check}: test") {
        throw 'A required saved HLS policy test is missing'
    }
}
. (Join-Path (Get-Location).Path '.github/scripts/windows-native-parallel.ps1')
$budget = Get-DrmrNativeTestBudget
# Start the two longest presentations first so shorter groups can fill free slots.
$ordered = @($executables | Sort-Object @{ Expression = {
    if ([IO.Path]::GetFileName($_) -match '^conversion_ts_output-') { 0 }
    elseif ([IO.Path]::GetFileName($_) -match '^conversion_abr_hls-') { 1 }
    else { 2 }
} }, @{ Expression = { $_ } })
$groups = @($ordered | ForEach-Object {
    @{ executable = $_; arguments = @('--test-threads', [string]$budget.threads, '--nocapture') }
})
$results = @(Invoke-DrmrNativeTestGroups -Groups $groups -Directory (Join-Path $PSScriptRoot 'groups') -Workers $budget.workers)
if ($results.Count -ne $executables.Count) { throw 'A native test group result is missing' }
$failed = $false
foreach ($result in @($results | Sort-Object index)) {
    Write-Output ('Native group {0}/{1} output' -f ($result.index + 1), $executables.Count)
    Get-Content -LiteralPath (Join-Path $result.directory 'stdout.log')
    Get-Content -LiteralPath (Join-Path $result.directory 'stderr.log')
    if ($result.exit_code -ne 0) { $failed = $true }
}
foreach ($name in @('bbb-media-report.json', 'conversion-report.json')) {
    $reports = @($results | ForEach-Object { Join-Path $_.directory $name } | Where-Object { Test-Path -LiteralPath $_ })
    if ($reports.Count -ne 1) { throw 'A native media report is missing or ambiguous' }
    [IO.File]::WriteAllText((Join-Path $PSScriptRoot $name), [IO.File]::ReadAllText($reports[0]), [Text.UTF8Encoding]::new($false))
}
& python -W error::ResourceWarning -m unittest discover -s tools/tests -v
if ($LASTEXITCODE -ne 0) { $failed = $true }
if ($failed) { exit 1 }
exit 0
'@ | Set-Content $script
$stdout = Join-Path $directory 'stdout.log'
$stderr = Join-Path $directory 'stderr.log'
$progressJob = $null
$process = $null
$started = @()
$executionExited = $false
try {
    # Only numeric progress crosses into the live Actions log. The ordinary
    # owner's complete output stays in separate files until its tree exits.
    $progressJob = Start-ThreadJob -StreamingHost $Host -ArgumentList (Join-Path $directory 'groups/progress.log') -ScriptBlock {
        param($Path)
        $shown = 0
        while ($true) {
            try {
                if (Test-Path -LiteralPath $Path) {
                    $lines = [IO.File]::ReadAllText($Path).Split("`n")
                    while ($shown -lt $lines.Count - 1) {
                        $line = $lines[$shown++].TrimEnd("`r")
                        if ($line -notmatch '^Native group [1-9][0-9]*/[1-9][0-9]* (started|finished; exit -?[0-9]+; elapsed [0-9]+s)$') {
                            throw 'Invalid native group progress'
                        }
                        Write-Host $line
                    }
                }
            } catch [IO.IOException] { }
            Start-Sleep -Milliseconds 200
        }
    }
    $process = Start-Process -FilePath (Get-Command pwsh).Source -Credential $credential -LoadUserProfile -WorkingDirectory (Get-Location).Path -ArgumentList @('-NoLogo', '-NoProfile', '-File', "`"$script`"") -RedirectStandardOutput $stdout -RedirectStandardError $stderr -Wait -PassThru -OutVariable started
    if (-not $process.HasExited) { throw 'The native test process tree exit is unconfirmed' }
    $executionExited = $true
    if ($progressJob.State -eq 'Failed') { throw 'Native group progress failed' }
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
    try {
        if ($null -ne $progressJob) {
            try { Stop-Job -Job $progressJob }
            finally { Remove-Job -Job $progressJob -Force }
        }
    } finally {
        try {
            # PassThru publishes the process before Start-Process waits. Keep that
            # owned handle available if pipeline cancellation interrupts the wait.
            if ($null -eq $process -and $started.Count -gt 0) { $process = $started[0] }
            if ($null -ne $process) {
                if (-not $executionExited) {
                    if (-not $process.HasExited) { $process.Kill($true) }
                    if (-not $process.WaitForExit(10000)) { throw 'Native test cancellation cleanup is unconfirmed' }
                }
                $process.Dispose()
            }
        } finally { Remove-LocalUser -Name $username }
    }
}
