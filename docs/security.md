# Keys and local security

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

Do not place confidential tokens in an output license URL or key URI unless publishing that address is intended. Keep signed input playlists and retained downloads private: their original contents can contain credentials or signed URLs.

## Input and diagnostic handling

For `convert` and `verify`, supply an owner-only JSON file with `--keys-file`. Input decryption keys and output encryption keys have separate roles. The file must be a regular, singly linked file with the required owner-only permissions; symbolic links and insecure files are refused. See [key configuration](usage.md#separate-input-and-output-keys) and [Windows file protection](windows.md#protect-configuration-files).

The older `run` and `package-hls` commands read `DRM_KID`, `DRM_KEY`, and `DRM_IV` from the environment. Batch processing can use a protected per-asset key file. drmr has no public command-line option for entering a content-key value. Avoid shell-history entries containing keys; use your local secret-management process to prepare the environment or file.

Diagnostic protection has several layers:

- Key-holding types provide redacted debug output.
- Tool records mask registered secrets and their supported hexadecimal, byte-separated, Base64, escaped, and encoded representations. Secret values become `***REDACTED***`; some commands omit tool arguments and raw output entirely.
- The command-line JSON Lines writer scrubs serialized records before writing them. Generic conversion failures use fixed error codes and safe progress labels.
- The local web console excludes keys from ordinary job, history, and result fields. Its tool-log safety also depends on the command-line producer's masking.

These controls cover recognized secret material. They do not classify every filename, label, media tag, or arbitrary value as confidential. Public Rust APIs that return captured raw tool output return data for processing; their raw buffers are not a safe log format. Review exported reports and third-party diagnostics before sharing them.

## External tools and memory

FFmpeg and Shaka Packager require plaintext keys for some supported operations. drmr passes those keys to child processes, including through command arguments. The operating system, sufficiently privileged processes, and process-inspection tools may observe those arguments or the process environment while the tool runs. Diagnostic masking does not hide live operating-system process information.

Keys also exist in ordinary process memory. Sensitive Rust key buffers and several temporary representations are wiped when their owners are dropped. This is a best-effort lifetime control, not a guarantee that every copy has been erased. Child processes, browser buffers, allocator copies, swap, crash dumps, and forced termination are outside that guarantee. drmr does not provide Secure Enclave storage or control the host's dump and swap policies.

## Temporary files and cleanup

Some verification paths write decrypted media and plaintext key files into private temporary directories. Processing is therefore not memory-only. Raw key-file writes refuse unsafe existing destinations instead of following a symbolic link or repairing public permissions after opening a file.

Confirmed normal cleanup removes owned temporary files. An interrupted process or unconfirmed child cleanup can leave private working files behind; inspect the reported job and workspace before reuse. Deleting a file does not guarantee secure erasure from the storage device, snapshots, or backups. Keep working directories, retained source material, and backups under the same access controls as the input keys.

## Local web and Desktop playback

The web console has no authentication. Keep it on loopback on a trusted machine; do not publish it through a proxy or port forward. Host and Origin checks restrict browser-origin requests, but they do not authenticate other local processes. The Desktop host uses the same local server and security boundary.

Key fields are visually masked by default and can be explicitly revealed. The application clears submitted form values and does not save keys to its own browser local storage or session storage. A visual mask or cleared form does not erase browser memory or prevent inspection of the page and network requests.

Local ClearKey playback intentionally supplies the actual content key to the browser in a JSON Web Key (JWK) response or player configuration. The response is marked `no-store`, but the key remains readable to browser developer tools, network-recording exports such as HAR files, page code, and sufficiently privileged extensions. This preview is not a production DRM license service or a hardware-protected playback path.

Successful HLS packaging can retain an in-memory preview credential context for 30 minutes of inactivity. Expiry is checked by a 30-second sweep and on relevant accesses; clearing the form does not immediately revoke that context. Explicit session revocation and server shutdown release their owned contexts. Do not share a browser profile or capture playback diagnostics when its keys must remain private.
