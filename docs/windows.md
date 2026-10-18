# Windows CLI

Use a regular, non-administrator PowerShell terminal on Windows x64 with a local NTFS volume. Media processing deliberately refuses an elevated account whose new files would be owned by the Administrators group. Network shares, directory junctions in private paths, alternate data streams, and insecure existing working directories are refused.

## Install and check tools

Build from the repository with Rust 1.95 or newer. See [requirements and tested versions](requirements.md):

```powershell
cargo build --release --locked -p drmr
.\target\release\drmr.exe --help
.\target\release\drmr.exe doctor
```

The Windows CI archive contains `drmr.exe`, the license, and user documentation. Extract it to a local directory and add that directory to your user `PATH`, or invoke the executable by its full path. External tools are installed separately: FFmpeg (`ffmpeg.exe` and `ffprobe.exe`), Bento4 (`mp4dump.exe`), and Shaka Packager (`packager.exe`). See the [installation links](../README.md#install). `doctor` discovers executables; it does not validate their versions.

The CI-built executable imports `VCRUNTIME140.dll`. Install the [Microsoft Visual C++ x64 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist) if that runtime is not already available. CI prints the release executable's imported libraries before creating the archive.

## Protect configuration files

Windows uses an access control list (ACL) instead of `chmod`. Secret configuration must have a protected ACL granting only the current user full control, with the current user as owner. Inherited permissions alone are insufficient. drmr refuses an insecure existing file or directory; it does not silently change its permissions.

Create a new private directory and an empty protected key configuration before entering any keys:

```powershell
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$private = Join-Path $env:LOCALAPPDATA 'drmr-keys'
if (Test-Path -LiteralPath $private) { throw 'Choose a new private directory' }
New-Item -ItemType Directory -Path $private | Out-Null

function Set-OwnerOnlyAcl($path, $security) {
    $security.SetOwner($sid)
    $security.SetAccessRuleProtection($true, $false)
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $sid, 'FullControl', 'Allow')
    $security.AddAccessRule($rule)
    Set-Acl -LiteralPath $path -AclObject $security
}

Set-OwnerOnlyAcl $private ([System.Security.AccessControl.DirectorySecurity]::new())
$keyFile = Join-Path $private 'keys.json'
New-Item -ItemType File -Path $keyFile | Out-Null
Set-OwnerOnlyAcl $keyFile ([System.Security.AccessControl.FileSecurity]::new())
notepad $keyFile
```

Save the [key configuration structure](usage.md#separate-input-and-output-keys) in that file using your editor or secret-management process. Keep the directory outside the output tree and version control. Never put key values in command arguments or shell history. If an editor replaces the file and changes its ACL, apply the owner-only file ACL again while it remains inside the private directory. See Microsoft's [Set-Acl documentation](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.security/set-acl) for ACL commands.

While external tools run, drmr keeps handles to the working directories and their ancestors. These handles allow child files to be created and updated while refusing rename or deletion of the held directories. Private configuration is checked using the path you supplied, so resolving a path cannot hide a junction or alternate data stream. A completed conversion is published in one filesystem operation that refuses an existing destination. Tools can exit while descendants still have files open. drmr confirms termination of the supervised tree before reporting success or a completed cancellation; unconfirmed cleanup is an error.

A new private directory can become visible while drmr is still validating it through its creation handle. drmr completes that short setup phase before another transfer uses the directory, avoiding sharing violations during publication. Copying the media bytes and publishing completed transfers remain parallel.

## Run commands

PowerShell accepts quoted native paths. This example uses a supported single-track MP4 input and a new output file in an existing directory:

```powershell
$outputDirectory = Join-Path $HOME 'drmr-output'
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
.\drmr.exe inspect 'C:\media\source.mp4' --json
.\drmr.exe plan 'C:\media\source.mp4' --to cbcs --format mp4
.\drmr.exe convert 'C:\media\source.mp4' --to cbcs --format mp4 --output "$outputDirectory\protected.mp4" --keys-file $keyFile
.\drmr.exe verify "$outputDirectory\protected.mp4" --source 'C:\media\source.mp4' --keys-file $keyFile --json
.\drmr.exe batch --help
```

Use the [usage guide](usage.md) for prepared DASH, clear-HLS snapshots, batch work lists, and exit codes. Windows paths belong in command arguments and local configuration paths; manifest URLs and object keys continue to use `/`.

The Windows qualification workflow runs the native CLI, batch supervision, access-control and descendant-process tests as a regular user with pinned media tools. Its artifacts retain real-media JSON reports and test logs. These checks establish only the tested media profiles; license acquisition and device playback require separate validation.

## Unconfirmed process cleanup

`cleanup-unconfirmed` or `media-cleanup-unconfirmed` means drmr could not confirm that its child processes finished. The operation must not be treated as successful or as a completed cancellation.

The 30-second cleanup limit covers waiting for the Windows job termination notification. If Windows input or output remains blocked, joining the pipe readers can take longer; that limit does not bound the whole failure path. drmr retains ownership while joining those readers.
