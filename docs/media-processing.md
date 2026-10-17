# Media processing and verification

drmr preserves the existing video and audio encoding. Repackaging can change container metadata, encryption, and segmentation without changing the encoded picture or sound. A successful conversion requires checks of both media content and its presentation timeline.

## Describe protection separately from delivery

MP4 is a media container. DASH and HLS describe how a player obtains media segments. `clear` means media is unencrypted; it is not a container or streaming format.

| Protection | Cipher and scope |
| --- | --- |
| `cenc` | AES counter mode applied to MP4 media samples |
| `cbcs` | AES cipher block chaining with an encryption/skip pattern inside MP4 media samples |
| HLS `AES-128` | AES cipher block chaining over an entire HLS segment, with padding |

Subsample encryption leaves selected regions of a sample clear, such as codec headers. This is a separate property from the cipher mode and encryption pattern. A `cenc` label does not mean every byte of the file is encrypted. A `cbcs` label does not describe an entire segment encrypted as HLS AES-128.

DRM systems such as Widevine, PlayReady, and FairPlay add key and license signaling. Signaling, the media encryption scheme, and device support are separate facts. A recognized DRM label does not prove that a license can be acquired or that a particular device can play the result.

The [W3C MP4 encryption description](https://www.w3.org/TR/eme-stream-mp4/) explains MP4 scheme signaling and partially encrypted samples. [HLS encryption declarations](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.2.4) define the distinct segment encryption method.

## Why audio boundaries differ from video boundaries

A typical AAC-LC audio frame contains 1,024 samples. At 48,000 samples per second, one frame lasts about 21.333 milliseconds. Two seconds therefore contain 93.75 frames. A packager must place the boundary on a complete frame, so audio segment durations can alternate around a two-second video target.

This arithmetic explains a boundary difference; it does not justify missing or duplicated frames, gaps, overlaps, or growing audio/video drift. Check the actual sample counts, timestamps, and complete presentation. An arbitrary tolerance of one audio frame could hide lost content.

Audio can also carry encoder priming: samples decoded at the beginning that should not be presented. MP4 edit-list metadata can describe this trimming. Identical compressed audio packets alone do not establish identical presented audio if that metadata changes during repackaging.

## How the existing processing paths handle segments

The prepared-DASH `run` path concatenates a rendition's initialization segment and media segments, then decrypts a complete clear reference. Normally, Shaka Packager encrypts and segments that reference again. For supported AAC audio whose source segment duration does not divide into 1,024-sample frames, drmr instead decrypts and encrypts each original segment with its initialization data, preserving the original boundary and checking decode timestamps. The complete clear reference is still produced for validation; this path does not eliminate whole-rendition temporary files.

The clear-HLS `package-hls` path processes each downloaded media segment with its initialization data, normalizes the supported container and timeline, and encrypts that segment. It checks the resulting structure, timing, and decoded content. Validation can join media when checking the presentation, so per-segment encryption does not imply that every verification operation is per segment.

These paths use temporary disk space and external tools. They do not yet promise a constant total memory footprint or a universal conversion route between every format and protection scheme. The command's reported checks describe what was actually verified.

## Interpret a result

Metadata inspection is evidence about declarations or initialization information. File hashing establishes byte identity. Decryption and decoded-content comparisons establish different properties. Neither a structural check nor a successful local decode substitutes for license-server or device playback testing.

Use the same source media and tool versions when comparing processing time or memory. A short regression clip is useful for correctness and startup overhead; it cannot establish throughput or memory scaling for a long presentation.
