# Windows CLI

Use a regular, non-administrator PowerShell terminal on Windows x64 with a local NTFS volume. Media processing deliberately refuses an elevated account whose new files would be owned by the Administrators group. Network shares, directory junctions in private paths, alternate data streams, and insecure existing working directories are refused.

## Install and check tools

Build from the repository with Rust 1.95 or newer. See [requirements and tested versions](requirements.md):

```powershell
cargo build --release --locked -p drmr
.\target\release\drmr.exe --help
.\target\release\drmr.exe doctor
```

The Windows CI archive contains `drmr.exe`, the license, and user documentation. Extract it to a local directory and add that directory to your user `PATH`, or invoke the executable by its full path. Native analysis and verification need no external media tools. Packaging routes use separately installed Shaka Packager (`packager.exe`); complete `--decode` checks additionally use FFmpeg (`ffmpeg.exe` and `ffprobe.exe`). Progressive MP4 copying and decryption run in Rust. See [requirements](requirements.md) and the [installation links](../README.md#install). `doctor` discovers executables; it does not validate their versions.

The CI-built executable imports `VCRUNTIME140.dll`. Install the [Microsoft Visual C++ x64 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist) if that runtime is not already available. CI prints the release executable's imported libraries before creating the archive.

## Protect configuration files

Windows uses an access control list (ACL) instead of `chmod`. Secret configuration must have a protected ACL granting only the current user full control, with the current user as owner. Inherited permissions alone are insufficient. drmr refuses an insecure existing file or directory; it does not silently change its permissions.

In a regular Windows PowerShell 5.1 terminal (`powershell.exe`), create a new private directory and an empty protected key configuration before entering any keys:

```powershell
$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$private = Join-Path $env:LOCALAPPDATA 'drmr-keys'
if (Test-Path -LiteralPath $private) { throw 'Choose a new private directory' }
New-Item -ItemType Directory -Path $private | Out-Null

function Set-OwnerOnlyAcl($path, $security) {
    $directory = $security -is
        [System.Security.AccessControl.DirectorySecurity]
    $sections =
        [System.Security.AccessControl.AccessControlSections]'Owner, Access'
    $current = if ($directory) {
        [System.IO.Directory]::GetAccessControl($path, $sections)
    } else {
        [System.IO.File]::GetAccessControl($path, $sections)
    }
    if (-not $current.GetOwner(
        [System.Security.Principal.SecurityIdentifier]).Equals($sid)) {
        throw 'The current user must own the path'
    }
    $security.SetAccessRuleProtection($true, $false)
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $sid, 'FullControl', 'Allow')
    $security.AddAccessRule($rule)
    $checked = if ($directory) {
        [System.IO.Directory]::SetAccessControl($path, $security)
        [System.IO.Directory]::GetAccessControl($path, $sections)
    } else {
        [System.IO.File]::SetAccessControl($path, $security)
        [System.IO.File]::GetAccessControl($path, $sections)
    }
    $rules = @($checked.GetAccessRules($true, $true,
        [System.Security.Principal.SecurityIdentifier]))
    if (
        -not $checked.AreAccessRulesProtected -or
        $rules.Count -ne 1 -or
        -not $checked.GetOwner(
            [System.Security.Principal.SecurityIdentifier]).Equals($sid)
    ) {
        throw 'Private ACL verification failed'
    }
    $actual = $rules[0]
    if (
        -not $actual.IdentityReference.Equals($sid) -or
        $actual.IsInherited -or
        $actual.AccessControlType -ne
            [System.Security.AccessControl.AccessControlType]::Allow -or
        $actual.FileSystemRights -ne
            [System.Security.AccessControl.FileSystemRights]::FullControl -or
        $actual.InheritanceFlags -ne
            [System.Security.AccessControl.InheritanceFlags]::None -or
        $actual.PropagationFlags -ne
            [System.Security.AccessControl.PropagationFlags]::None
    ) {
        throw 'Private ACL verification failed'
    }
}

Set-OwnerOnlyAcl $private ([System.Security.AccessControl.DirectorySecurity]::new())
$keyFile = Join-Path $private 'keys.json'
New-Item -ItemType File -Path $keyFile | Out-Null
Set-OwnerOnlyAcl $keyFile ([System.Security.AccessControl.FileSecurity]::new())
notepad $keyFile
```

Save the [key configuration structure](usage.md#separate-input-and-output-keys) in that file using your editor or secret-management process. Keep the directory outside the output tree and version control. Never put key values in command arguments or shell history. If an editor replaces the file and changes its ACL, apply the owner-only file ACL again while it remains inside the private directory. The example uses the .NET Framework [Directory.SetAccessControl](https://learn.microsoft.com/en-us/dotnet/api/system.io.directory.setaccesscontrol?view=netframework-4.8.1) and [File.SetAccessControl](https://learn.microsoft.com/en-us/dotnet/api/system.io.file.setaccesscontrol?view=netframework-4.8.1) methods available in Windows PowerShell 5.1. It verifies that you already own each path and changes only its access rules, then checks the resulting owner and ACL. Assigning an owner requires a separate permission and is unnecessary for these new paths. Unrelated audit and group settings are not copied.

While external tools run, drmr keeps handles to the working directories and their ancestors. These handles allow child files to be created and updated while refusing rename or deletion of the held directories. Private configuration is checked using the path you supplied, so resolving a path cannot hide a junction or alternate data stream. A completed conversion is published in one filesystem operation that refuses an existing destination. Tools can exit while descendants still have files open. drmr confirms termination of the supervised tree before reporting success or a completed cancellation; unconfirmed cleanup is an error.

A new private directory can become visible while drmr is still validating it through its creation handle. drmr completes that short setup phase before another transfer uses the directory, avoiding sharing violations during publication. Copying the media bytes and publishing completed transfers remain parallel.

## Run commands

PowerShell accepts quoted native paths. This example uses a supported single-track MP4 input and a new output file in an existing directory:

```powershell
$outputDirectory = Join-Path $HOME 'drmr-output'
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
.\drmr.exe analyze 'C:\media\source.mp4' --metadata-only --json
.\drmr.exe plan 'C:\media\source.mp4' --to cbcs --format mp4
.\drmr.exe convert 'C:\media\source.mp4' --to cbcs --format mp4 --output "$outputDirectory\protected.mp4" --keys-file $keyFile
.\drmr.exe verify "$outputDirectory\protected.mp4" --source 'C:\media\source.mp4' --keys-file $keyFile --json
```

Use the [usage guide](usage.md) for URL input, saved multi-output plans, protected key files, and exit codes. Windows paths belong in command arguments and local configuration paths; manifest URLs and object keys continue to use `/`.

Keep output and temporary-directory paths reasonably shallow. Media tools can create longer temporary names than the final media or manifest names: Shaka Packager, for example, [writes manifests through a temporary file in the same directory](https://github.com/shaka-project/shaka-packager/blob/v3.8.0/packager/file/file.cc#L75). A segment can therefore be written successfully while a later manifest write reaches a tool's path-length limit. Windows long-path settings do not make every external tool support long paths; see Microsoft's [path-length conditions](https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation).

For HLS and DASH children of a multi-output conversion, drmr creates media-output stages beside the bundle stage under the original output parent, avoiding another level of temporary-directory nesting. A verified HLS or DASH child is moved into the private bundle on the same filesystem. MP4 retains its separate file-copy publication stage. The complete bundle is published only after all requested outputs pass, with no cross-volume copy fallback for that final atomic move.

The Windows qualification workflow runs the native CLI, supervision, access-control and descendant-process tests as a regular user with pinned media tools. Its artifacts retain real-media JSON reports and test logs. These checks establish only the tested media profiles; license acquisition and device playback require separate validation.

## Cancel a media operation

Press Ctrl+C or Ctrl+Break to cancel `convert` or `verify`. The CLI stops scheduling
work, terminates its owned media processes, and removes temporary workspaces. If
cancellation is observed before final publication, the destination is not published.
Exit code `130` means cancellation completed with confirmed process cleanup; an
unconfirmed cleanup is an error instead.

The native Windows tests deliver both console events for both commands and check
immediate descendant termination, released file locks, unchanged input, and no
published output. These tests use a regular user on a local NTFS filesystem.

## Unconfirmed process cleanup

`cleanup-unconfirmed` or `media-cleanup-unconfirmed` means drmr could not confirm that its child processes finished. The operation must not be treated as successful or as a completed cancellation.

The 30-second cleanup limit covers waiting for the Windows job termination notification.
Captured output and streamed digest readers exclusively own the standard library’s
overlapped child-output handles. They check available bytes before each read and
read no more than that amount. They check their stop flag between reads and join before the operation returns,
even if a writer remains open. Empty pipes mean “try again”; only a closed pipe means EOF.
No reader is detached, and process cleanup must still be confirmed before a successful
result or cancellation is reported. This does not turn operating-system scheduling,
synchronous filesystem operations, or caller-supplied callbacks into hard real-time operations.
