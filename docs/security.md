# Keys and local security

Multiple-output plans fix the output IDs, formats, protection schemes, DRM system
selections and external manifest-check policies. Their runtime schema-version-2
key file keeps each output's keys and DRM information separate. PSSH, license
addresses, FairPlay key addresses and initialization vectors are not stored in
the plan or printed in result summaries. Generated manifests intentionally
publish their configured DRM declarations. See [multiple outputs](multiple-outputs.md).

Native manifest linting accepts captured bytes without keys, network access,
or external processes. Its diagnostics identify rules and source positions;
they do not echo input text, key addresses, or protection payloads.

External manifest checks inspect byte-identical private copies of final output
manifests. They do not contact license or key services, decrypt media, or upload
an asset to a hosted validation service. Their result scope is `manifest-only`;
successful media correspondence is a separate internal check. Executable paths
and raw validator diagnostics are excluded from retained result summaries.

drmr processes media with keys supplied by the operator. It does not obtain DRM licenses or isolate keys in secure hardware. Use a trusted operating-system account, trusted media tools, and a trusted browser for local playback.

## Where supplied information goes

| Information | How drmr uses it |
| --- | --- |
| Content encryption keys | Decrypt input, encrypt output, and verify media. They are excluded from normal reports and masked in diagnostic records. |
| Key identifier (KID) | Identifies which key applies to a track. It is public media metadata, rather than the content key itself. |
| Initialization vector (IV) | Supplies the starting value for an encryption operation. It can be present in the media or playlist; drmr also masks configured IV values in tool diagnostics. |
| License URLs and HLS key URIs | Describe where a player should obtain a license or key. Configured output addresses are deliberately included in the resulting media metadata or playlists. |
| HTTP request headers | Used for the configured download origin. They are excluded from download reports; keep the fetch-policy file private. |
| Paths and labels | Identify local input, output, tools, and assets. Some successful results and diagnostics display them; they are not all treated as secrets. |

A license URL can also be encoded inside a Protection System Specific Header (PSSH), which is DRM initialization metadata stored in a media file or manifest. Hexadecimal and Base64 are reversible encodings, not encryption. Masking a URL only in its readable form is insufficient. Known signaling values are removed from diagnostic records, while the intended signaling remains in the media output.

Do not place confidential tokens in an output license URL or key URI unless publishing that address is intended. Keep signed input manifests and retained downloads private: original HLS playlists and DASH MPDs can contain credentials or signed URLs.

HLS, DASH and MP4 URL input use the same fetch policy. Request headers are sent
only to their configured origin, and redirects and media references must pass
the origin and resource checks. Acquisition does not fetch advertised keys or
licenses and does not decrypt media. It streams resources into private files;
decryption and media admission use separately supplied keys afterward. The
download summary includes local paths, not the source URL or request headers.

## Input and diagnostic handling

For `analyze`, saved-plan creation, `convert`, and `verify`, supply an owner-only JSON file with `--keys-file` when keys are needed. Input decryption keys and output encryption keys have separate roles. The file must be a regular, singly linked file with the required owner-only permissions; symbolic links and insecure files are refused. See [key configuration](usage.md#separate-input-and-output-keys) and [Windows file protection](windows.md#protect-configuration-files).

The older `run` and `package-hls` commands read `DRM_KID`, `DRM_KEY`, and `DRM_IV` from the environment. Batch processing can use a protected per-asset key file. drmr has no public command-line option for entering a content-key value. Avoid shell-history entries containing keys; use your local secret-management process to prepare the environment or file.

Diagnostic protection has several layers:

- Key-holding types provide redacted debug output.
- Tool records mask registered secrets and their supported hexadecimal, byte-separated, Base64, escaped, and encoded representations. Secret values become `***REDACTED***`; some commands omit tool arguments and raw output entirely.
- The command-line JSON Lines writer scrubs serialized records before writing them. Generic conversion failures use fixed error codes and safe progress labels.
- The local web console excludes keys from ordinary job, history, and result fields. Its tool-log safety also depends on the command-line producer's masking.

These controls cover recognized secret material. They do not classify every filename, label, media tag, or arbitrary value as confidential. Public Rust APIs that return captured raw tool output return data for processing; their raw buffers are not a safe log format. Review exported reports and third-party diagnostics before sharing them.

## External tools and memory

### Saved plans and retained originals

An [execution plan](executable-plans.md) stores local source filenames, sizes,
SHA-256 fingerprints, and one output location and media intent. It contains no
keys, initialization vectors, key or license addresses, request headers, source
URLs, or executable commands. Keys and optional output signaling are read
separately at runtime. Paths and fingerprints can identify an asset, so saved
plans are private files too. The original downloaded manifests may contain
signed URLs even though the plan does not.

The plan digest detects edits, not a malicious author who can calculate another
digest. Execute only plans you trust. Every use validates the strict schema,
source references, actual copied bytes, and media profile again. A saved file
inventory grants no permission to delete the original files. Conversion
publishes only to a new destination after its independent checks, and verification
reads the original and current result rather than trusting saved success claims.

### Process arguments and plaintext memory

FFmpeg and Shaka Packager require plaintext keys for some supported operations. drmr passes those keys to child processes, including through command arguments. The operating system, sufficiently privileged processes, and process-inspection tools may observe those arguments or the process environment while the tool runs. Diagnostic masking does not hide live operating-system process information.

Explicit DRM initialization metadata is passed to Shaka Packager through a
private flag file instead of a command-line argument. The file can contain
configured license addresses and remains private working data. This transport
does not remove content keys from other tool arguments or protect them from
process-memory inspection.

Keys also exist in ordinary process memory. Sensitive Rust key buffers and several temporary representations are wiped when their owners are dropped. This is a best-effort lifetime control, not a guarantee that every copy has been erased. Child processes, browser buffers, allocator copies, swap, crash dumps, and forced termination are outside that guarantee. drmr does not provide Secure Enclave storage or control the host's dump and swap policies.

## Temporary files and cleanup

Some verification paths write decrypted media and plaintext key files into private temporary directories. Processing is therefore not memory-only. Raw key-file writes refuse unsafe existing destinations instead of following a symbolic link or repairing public permissions after opening a file.

Temporary DRM flag files are created exclusively with private permissions.
Their directory, file identity and contents are checked around tool execution.
If child-process cleanup cannot be confirmed, the owning workspace remains
alive until the child is reaped. A replaced workspace is preserved instead of
recursively deleting the replacement through its pathname.

On Unix, use trusted existing parent directories for output, plans and temporary
work. A conversion staging parent must belong to the current user or the administrator
(`root`), and its Unix permission bits must deny group and other users write access. A shared temporary
directory with the sticky permission bit, such as a root-owned `/tmp`, is also
accepted: that bit protects each user's entries from other unprivileged users.
A writable shared parent without that protection is refused with
`temporary-workspace-parent-unsafe`. Saving a plan rejects an unsafe output
parent with `execution-plan-destination-unsafe` before media admission.

Conversion staging cleanup follows retained directory handles rather than adopting a new tree
through the original pathname. If ownership or the namespace cannot be
confirmed, drmr leaves uncertain entries alone. Explicit cleanup returns
`temporary-cleanup-unconfirmed`; automatic fallback cleanup cannot always report
this error.
These controls do not protect against an administrator or a malicious process
running as the same operating-system user.

For a partial Desktop plan copy, cleanup first moves the selected entry into a
new owner-only `.drmr-plan-cleanup-*` folder beside the selected file. It checks
the moved entry's identity before deleting it, so a concurrent replacement at
the original filename is not deleted. An unfamiliar entry is restored only if
its original name is free; an occupied name is never overwritten. Unconfirmed
cleanup retains its file owner and recovery state. Inspect the selected private
folder, including hidden cleanup folders, and preserve unfamiliar files. Restore
the original private folder before selecting **Quit drmr** to retry cleanup.
These checks have the same operating-system account boundary described above.

Confirmed normal cleanup removes owned temporary files. An interrupted process or unconfirmed child cleanup can leave private working files behind; inspect the reported job and workspace before reuse. Deleting a file does not guarantee secure erasure from the storage device, snapshots, or backups. Keep working directories, retained source material, and backups under the same access controls as the input keys.

## Local web and Desktop playback

The web console has no authentication. Keep it on loopback on a trusted machine; do not publish it through a proxy or port forward. Host and Origin checks restrict browser-origin requests, but they do not authenticate other local processes. The Desktop host uses the same local server and security boundary.

Key fields are visually masked by default and can be explicitly revealed. The application clears submitted form values and does not save keys to its own browser local storage or session storage. A visual mask or cleared form does not erase browser memory or prevent inspection of the page and network requests.

Local ClearKey playback intentionally supplies the actual content key to the browser in a JSON Web Key (JWK) response or player configuration. The response is marked `no-store`, but the key remains readable to browser developer tools, network-recording exports such as HAR files, page code, and sufficiently privileged extensions. This preview is not a production DRM license service or a hardware-protected playback path.

Successful HLS packaging can retain an in-memory preview credential context for 30 minutes of inactivity. Expiry is checked by a 30-second sweep and on relevant accesses; clearing the form does not immediately revoke that context. Revoking the last playback session for a result removes that result's credential context; server shutdown clears its owned playback sessions and contexts. Do not share a browser profile or capture playback diagnostics when its keys must remain private.
