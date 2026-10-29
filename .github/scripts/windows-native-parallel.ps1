$ErrorActionPreference = 'Stop'

function Get-DrmrNativeTestBudget {
    param([int]$Processors = [Environment]::ProcessorCount, [long]$AvailableBytes = -1)
    if ($AvailableBytes -lt 0) {
        try { $AvailableBytes = [long]((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory) * 1024 }
        catch { $AvailableBytes = 0 }
    }
    $workers = [Math]::Max(1, [Math]::Min(2, [Math]::Min(
        [Math]::Floor($Processors / 2), [Math]::Floor($AvailableBytes / 4GB))))
    return @{ workers = [int]$workers; threads = [Math]::Max(1, [Math]::Min(2, [Math]::Floor($Processors / $workers))) }
}

function Invoke-DrmrNativeTestGroups {
    param(
        [Parameter(Mandatory)][object[]]$Groups,
        [Parameter(Mandatory)][string]$Directory,
        [ValidateRange(1, 2)][int]$Workers = 1,
        [Threading.CancellationToken]$CancellationToken = [Threading.CancellationToken]::None
    )
    if (Test-Path -LiteralPath $Directory) { throw 'The native group directory already exists' }
    New-Item -ItemType Directory -Path $Directory | Out-Null
    $active = [Collections.Generic.List[object]]::new()
    $results = [Collections.Generic.List[object]]::new()
    $next = 0
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $progress = Join-Path $Directory 'progress.log'
    $utf8 = [Text.UTF8Encoding]::new($false)
    try {
        while ($next -lt $Groups.Count -or $active.Count -gt 0) {
            $CancellationToken.ThrowIfCancellationRequested()
            while ($next -lt $Groups.Count -and $active.Count -lt $Workers) {
                $CancellationToken.ThrowIfCancellationRequested()
                $group = $Groups[$next]
                $index = $next++
                $path = Join-Path $Directory ('group-{0:D4}' -f $index)
                $temp = Join-Path $path 'temp'
                New-Item -ItemType Directory -Path $temp | Out-Null
                $info = [Diagnostics.ProcessStartInfo]::new([string]$group.executable)
                $info.UseShellExecute = $false
                $info.CreateNoWindow = $true
                $info.WorkingDirectory = (Get-Location).Path
                $info.RedirectStandardOutput = $true
                $info.RedirectStandardError = $true
                foreach ($argument in $group.arguments) { $info.ArgumentList.Add([string]$argument) }
                $info.Environment['TEMP'] = $temp
                $info.Environment['TMP'] = $temp
                $info.Environment['DRMR_MEDIA_REPORT'] = Join-Path $path 'bbb-media-report.json'
                $info.Environment['DRMR_CONVERSION_REPORT'] = Join-Path $path 'conversion-report.json'
                $process = [Diagnostics.Process]::new()
                $process.StartInfo = $info
                $output = $null
                try {
                    $output = [IO.File]::Create((Join-Path $path 'stdout.log'))
                    $errorOutput = [IO.File]::Create((Join-Path $path 'stderr.log'))
                } catch {
                    if ($null -ne $output) { $output.Dispose() }
                    $process.Dispose()
                    throw
                }
                $item = @{ index = $index; directory = $path; process = $process; output = $output;
                    error_output = $errorOutput; output_copy = $null; error_copy = $null; started = $false }
                $active.Add($item)
                if (-not $process.Start()) { throw 'A native test group could not start' }
                $item.started = $true
                $item.output_copy = $process.StandardOutput.BaseStream.CopyToAsync($output)
                $item.error_copy = $process.StandardError.BaseStream.CopyToAsync($errorOutput)
                [IO.File]::AppendAllText($progress, "Native group $($index + 1)/$($Groups.Count) started`n", $utf8)
            }
            foreach ($item in @($active.ToArray())) {
                if (-not $item.process.HasExited) { continue }
                $item.process.WaitForExit()
                $copies = [Threading.Tasks.Task[]]@($item.output_copy, $item.error_copy)
                if (-not [Threading.Tasks.Task]::WaitAll($copies, 10000)) { throw 'Native test output did not close' }
                $item.output.Dispose()
                $item.error_output.Dispose()
                $result = @{ index = $item.index; directory = $item.directory; exit_code = $item.process.ExitCode }
                $results.Add($result)
                $item.process.Dispose()
                $active.Remove($item) | Out-Null
                [IO.File]::AppendAllText($progress,
                    "Native group $($item.index + 1)/$($Groups.Count) finished; exit $($result.exit_code); elapsed $([int]$clock.Elapsed.TotalSeconds)s`n", $utf8)
            }
            if ($active.Count -gt 0) { Start-Sleep -Milliseconds 100 }
        }
    } finally {
        $cleanupFailed = $false
        foreach ($item in $active) {
            try {
                if ($item.started) {
                    if (-not $item.process.HasExited) { $item.process.Kill($true) }
                    if (-not $item.process.WaitForExit(10000)) { throw 'Native test termination is unconfirmed' }
                }
                foreach ($copy in @($item.output_copy, $item.error_copy)) {
                    if ($null -ne $copy -and -not $copy.Wait(10000)) { throw 'Native test output cleanup is unconfirmed' }
                }
            } catch { $cleanupFailed = $true }
            finally {
                $item.output.Dispose()
                $item.error_output.Dispose()
                $item.process.Dispose()
            }
        }
        if ($cleanupFailed) { throw 'Native test process cleanup is unconfirmed' }
    }
    return $results.ToArray()
}
