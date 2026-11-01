$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-native-parallel.ps1')
$root = Join-Path $env:RUNNER_TEMP ('drmr-native-parallel-control-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
$python = (Get-Command python).Source
$fixture = Join-Path $root 'child.py'
@'
import os, pathlib, subprocess, sys, time
root, mode, index = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
(root / (index + '.started')).write_text(str(os.getpid()))
print('stdout-' + index, flush=True)
print('stderr-' + index, file=sys.stderr, flush=True)
if mode == 'overlap':
    deadline = time.monotonic() + 10
    while len(list(root.glob('*.started'))) < 2:
        if time.monotonic() > deadline: sys.exit(9)
        time.sleep(0.01)
elif mode == 'cancel':
    child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
    (root / 'descendant.pid').write_text(str(child.pid))
    time.sleep(60)
elif mode == 'failure':
    sys.exit(7)
elif mode == 'after-priority':
    if not (root / 'priority.finished').is_file(): sys.exit(8)
print('temp-' + os.environ['TEMP'], flush=True)
(root / (index + '.finished')).write_text('finished')
'@ | Set-Content -Encoding utf8 $fixture
try {
    $progressPath = Join-Path $root 'shared-progress.log'
    $progressLine = "Native group 1/1 started`n"
    $bytes = [Text.Encoding]::UTF8.GetBytes($progressLine)
    $writer = [IO.FileStream]::new($progressPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    try {
        $writer.Write($bytes, 0, $bytes.Length)
        $writer.Flush()
        if ((Read-DrmrNativeProgress -Path $progressPath) -ne $progressLine) { throw 'Native progress could not be read alongside its writer' }
    } finally { $writer.Dispose() }
    $exclusive = [IO.FileStream]::new($progressPath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        if ($exclusive.Length -ne $bytes.Length) { throw 'Native progress content changed while reading' }
    } finally { $exclusive.Dispose() }
    foreach ($processors in @(1, 2, 4, 16)) {
        $budget = Get-DrmrNativeTestBudget -Processors $processors -AvailableBytes 16GB
        if ($budget.workers -gt 2 -or $budget.workers * $budget.threads -gt $processors) { throw 'Native CPU budget was exceeded' }
    }
    if ((Get-DrmrNativeTestBudget -Processors 8 -AvailableBytes 2GB).workers -ne 1) { throw 'Native memory budget was exceeded' }
    $groups = @(0, 1, 2 | ForEach-Object {
        @{ executable = $python; arguments = @($fixture, $root, 'overlap', [string]$_) }
    })
    $results = @(Invoke-DrmrNativeTestGroups -Groups $groups -Directory (Join-Path $root 'parallel') -Workers 2)
    if ($results.Count -ne 3 -or @($results | Where-Object exit_code -ne 0).Count -ne 0) { throw 'Native parallel overlap failed' }
    $active = 0
    foreach ($line in Get-Content (Join-Path $root 'parallel/progress.log')) {
        if ($line.EndsWith('started')) { $active++ } else { $active-- }
        if ($active -lt 0 -or $active -gt 2) { throw 'Native worker bound was exceeded' }
    }
    if ($active -ne 0) { throw 'Native group completion was not paired' }
    foreach ($result in $results) {
        $i = $result.index
        $text = Get-Content -Raw (Join-Path $result.directory 'stdout.log')
        $errorText = Get-Content -Raw (Join-Path $result.directory 'stderr.log')
        if (-not $text.Contains("stdout-$i") -or -not $errorText.Contains("stderr-$i") -or
            -not $text.Contains((Join-Path $result.directory 'temp'))) { throw 'Native logs or temporary files were mixed' }
        foreach ($other in @(0, 1, 2 | Where-Object { $_ -ne $i })) {
            if ($text.Contains("stdout-$other") -or $errorText.Contains("stderr-$other")) { throw 'Native output crossed group boundaries' }
        }
    }
    $failure = @(
        @{ executable = $python; arguments = @($fixture, $root, 'failure', 'failed') },
        @{ executable = $python; arguments = @($fixture, $root, 'ok', 'continued') }
    )
    $failed = @(Invoke-DrmrNativeTestGroups -Groups $failure -Directory (Join-Path $root 'failure') -Workers 1)
    if ($failed.Count -ne 2 -or $failed[0].exit_code -ne 7 -or $failed[1].exit_code -ne 0) { throw 'Native failure aggregation lost a group' }
    $earlyFailure = @(
        @{ executable = $python; arguments = @($fixture, $root, 'failure', 'early-failed') },
        @{ executable = $python; arguments = @($fixture, $root, 'ok', 'must-not-start') }
    )
    $early = @(Invoke-DrmrNativeTestGroups -Groups $earlyFailure -Directory (Join-Path $root 'early-failure') -Workers 2 -PriorityCount 1)
    if ($early.Count -ne 1 -or $early[0].exit_code -ne 7 -or
        (Test-Path -LiteralPath (Join-Path $root 'must-not-start.started'))) { throw 'An early failure started expensive tests' }
    $barrier = @(
        @{ executable = $python; arguments = @($fixture, $root, 'ok', 'priority') },
        @{ executable = $python; arguments = @($fixture, $root, 'after-priority', 'after') }
    )
    $after = @(Invoke-DrmrNativeTestGroups -Groups $barrier -Directory (Join-Path $root 'barrier') -Workers 2 -PriorityCount 1)
    if ($after.Count -ne 2 -or @($after | Where-Object exit_code -ne 0).Count -ne 0) { throw 'Native priority barrier was bypassed' }
    $source = [Threading.CancellationTokenSource]::new()
    $source.CancelAfter(15000)
    $canceller = Start-ThreadJob -ArgumentList $source, $root -ScriptBlock {
        param($Source, $Root)
        while (-not (Test-Path -LiteralPath (Join-Path $Root 'descendant.pid'))) { Start-Sleep -Milliseconds 20 }
        $Source.Cancel()
    }
    $cancelled = $false
    try {
        Invoke-DrmrNativeTestGroups -Groups @(@{ executable = $python; arguments = @($fixture, $root, 'cancel', 'cancelled') }) -Directory (Join-Path $root 'cancel') -CancellationToken $source.Token | Out-Null
    } catch [OperationCanceledException] { $cancelled = $true }
    finally {
        try { Stop-Job -Job $canceller }
        finally {
            Remove-Job -Job $canceller -Force
            $source.Dispose()
        }
    }
    if (-not $cancelled) { throw 'Native cancellation was not propagated' }
    foreach ($name in @('cancelled.started', 'descendant.pid')) {
        $path = Join-Path $root $name
        if (-not (Test-Path -LiteralPath $path)) { throw 'Native cancellation did not exercise a real process tree' }
        $childId = [int](Get-Content -LiteralPath $path)
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        while (Get-Process -Id $childId -ErrorAction SilentlyContinue) {
            if ([DateTime]::UtcNow -gt $deadline) { throw 'Native cancellation left a process alive' }
            Start-Sleep -Milliseconds 20
        }
    }
    Write-Output 'Native parallel controls passed: progress sharing, overlap, budgets, isolation, failure and cancellation'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force
}
