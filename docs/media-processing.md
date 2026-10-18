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

The clear-HLS path preserves a single open-ended audio trim at normal playback rate and compensates decode timestamps to retain presentation timing. Complex or finite edits and inexact timescale conversions are rejected. The [ten-second Big Buck Bunny regression](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md) verifies both individual segments and the continuous presentation, including 240 video frames and the source's audio sample count. Its JSON results include tool versions, elapsed time, and sampled memory; CI uploads them with each platform's test artifacts.

## Generic conversion and timing limits

`convert` stages a private snapshot of each supported input track. For local DASH it appends the initialization data and referenced fragments in timeline order; for local HLS it first decrypts complete AES-128 resources when required, then assembles each fragmented MP4 track. Shaka Packager decrypts sample-encrypted input and packages the resulting tracks together into the selected output format. DASH and HLS output are segmented again at a two-second target. HLS AES-128 is applied separately to each resulting resource, with a fresh initialization vector and padding. This route uses whole-track temporary disk files; it does not re-encrypt each original segment in place.

The complete original and final tracks are independently read by FFmpeg. Conversion requires matching stream metadata, encoded packet content, decoded video/audio, and every packet's presentation time, decode time, and duration. Metadata checks include codec configuration, sample rate, channel count/layout, video dimensions and pixel format, aspect ratio, color characteristics, display rotation, and language. Encryption initialization data is expected to change and is checked separately.

Silent audio has the same sample bytes at different sample rates, so decoded-byte identity alone cannot verify its presentation. HLS demuxing can omit some stream metadata; these fields are read from a byte-preserving snapshot of the actual initialization data. Decryption, content, and packet timing still use an independent read of the original playlist.

Presentation times determine when content is displayed; decode times determine when a decoder must process it. Comparing absolute times across all tracks also detects changes to relative audio/video timing. Manifest timeline declarations are checked separately from media bytes. DASH compares times exactly in their declared clock units. HLS segment durations allow the greater of one media-clock tick or about one millisecond for decimal playlist rounding; this does not relax the exact packet timing comparison.

Video with reordered pictures can start with a composition offset: the difference between a picture's decode and presentation times. Shaka Packager 3.8.0 normally resets the initial offset. That changes presentation times even when packet content matches. drmr explicitly disables this reset and tests a source with reordered pictures against its original timestamps. See the [pinned packager source](https://github.com/shaka-project/shaka-packager/tree/v3.8.0/packager/media/formats/mp4).

An ordinary MP4 can use a finite audio edit to exclude encoder padding at the end. For clear progressive MP4 input with one AAC-LC (AAC Low Complexity) audio track stored as `mp4a` and optionally one H.264 video track stored as `avc1`, `convert --to clear --format mp4` automatically writes progressive MP4. Progressive MP4 stores sample locations and presentation edits together; fragmented MP4 describes media in successive fragments. Fragmenting a finite audio presentation first can lose its end boundary and add decoded padding even when every compressed packet and packet timestamp matches.

This route copies the original encoded tracks directly into one file. It multiplexes audio and video from the same source presentation; it does not concatenate different clips or resegment the source. It preserves the original movie/video clock scales and checks every signed absolute presentation time, decode time, and duration, the encoded packet content, complete decoded media, and stream metadata against the original private snapshot. A changed audio edit can fail full decoding even when packet content and timestamps match. For this finite progressive source profile, `packet_timing_equal` also requires matching presentation windows from the edit lists and consistent movie/track durations. This catches a video end edit inside the final picture, which can leave packet timestamps and decoded pixels unchanged. Separate verification compares exact integer windows rather than box bytes, so equivalent movie clock representations remain equivalent while media time bases and packet timestamps must still match exactly. An unsupported result window is refused with `presentation-window-not-supported`. The reference is never trimmed to make verification pass.

Independent verification applies these same window, playback-header, and display-size checks to qualified progressive references with known CENC or CBCS keys, as well as clear references. The same checks apply when audio and video use different supported protection types. Finite-edit conversion continues to require clear input and clear output. Metadata and packet-count probes use the selected track's key; encrypted packet bytes must not be interpreted as clear codec data.

The progressive profile requires a single normal-rate audio edit with a finite duration, nonnegative media start, and an exact conversion between movie and media clock units. Empty edits that add an initial delay, multiple edit windows, rate changes, additional tracks, combining different source files, and other codecs are refused. Movie and track playback headers must describe ordinary enabled playback: normal rate, default gain, and no alternate track selection beyond the single audio track. Nondefault gain is refused because stream copying can reset it without changing decoded sample bytes. Independent verification also refuses unsupported playback headers on either side of this profile. Video track display width and height must be preserved exactly, including a correctly retained size for nonsquare pixels. The composed movie and track display transform remains part of the stream metadata comparison; an equivalent transform can move from the movie matrix into the track matrix. A result with unchanged decoded pixels can still fail when its declared display size changes. Video with reordered pictures and negative decode timestamps remains subject to the same exact checks. Output is published only after every original track passes verification. Existing destinations are refused.

Finite audio edits remain unsupported for encrypted MP4 output and DASH/HLS repackaging; these routes report `finite-audio-tail-not-supported` without falling back to clear output. Multitrack encrypted MP4 output is also refused. The existing fragmented route continues to verify open-ended initial priming trims and the final padding actually presented by a fragmented source. A progressive remux is not applied automatically to arbitrary fragmented input, because doing so could change which decoded padding is presented.

Generic conversion is deliberately bounded: at most 16 GiB per staged track, 64 tracks, and 100,000 referenced DASH segments. Exceeding a limit fails rather than reporting partial verification. The first local DASH profile is a static, single-period presentation with finite, nonnegative, continuous `SegmentTimeline` entries, zero presentation offset, and relative resources confined to the manifest directory. Live/dynamic input, multiple periods, byte ranges, alternate addressing, event declarations and presentation metadata that cannot be preserved are refused. The HLS profile uses completed, local fragmented MP4 playlists; unsupported rendition metadata and discontinuities are refused. An external audio group must apply consistently to every video variant; mixed associations are refused. Percent-encoded local resource paths are decoded once before confinement checks, so they select the URI target rather than a similarly named literal file. In-band MP4 event messages are also refused until their preservation is supported. These limits describe drmr's implementation, not limits of the formats.

## Packet verification for long presentations

Packet timing verification reads every packet in the selected track. It hashes the ordered presentation time, decode time and duration as fixed-width signed integers with SHA-256, then compares both the complete digest and packet count. Clock units must match; times are never rounded, shifted or sampled. An independent whole-track probe supplies the expected count. Stream metadata, encoded payload and complete decoded content are still checked separately.

The timing reader retains one record of at most 256 bytes and an 8 KiB read buffer rather than a list of packets. There is no longer a 100,000-packet or 32 MiB total-output limit on this proof. Malformed or unfinished records, missing clock values, wrong stream selectors, count changes and unsuccessful tool exits fail verification. Standard error remains independently bounded, and cancellation, deadlines and process cleanup use the same resource admission and supervision as other media commands. Unconfirmed process cleanup takes precedence over a normal cancellation or completed proof.

Other limits remain: staged track size, track and segment counts, MP4 initialization metadata, fragment metadata and samples per fragment are bounded separately. Removing the packet-proof limit does not make total memory, temporary disk space or runtime unlimited.

The long-presentation regression derives a 32 × 18 video from the attributed [Big Buck Bunny clip](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md) and repeats all ten seconds 417 times. It converts and independently verifies all 100,080 packets over 4,170 seconds. Generate and check it locally with `cargo test -p drmr --test long_conversion --locked`; media files are temporary. This small resolution tests complete long-duration verification, not representative encoding throughput or device playback. A separate generated 700,001-packet test passes more than 32 MiB of valid timing records through the same reader.

## How the existing processing paths handle segments

The prepared-DASH `run` path concatenates a rendition's initialization segment and media segments, then decrypts a complete clear reference. Normally, Shaka Packager encrypts and segments that reference again. For supported AAC audio whose source segment duration does not divide into 1,024-sample frames, drmr instead decrypts and encrypts each original segment with its initialization data, preserving the original boundary and checking decode timestamps. The complete clear reference is still produced for validation; this path does not eliminate whole-rendition temporary files.

The clear-HLS `package-hls` path processes each downloaded media segment with its initialization data, normalizes the supported container and timeline, and encrypts that segment. It checks the resulting structure, timing, and decoded content. Validation can join media when checking the presentation, so per-segment encryption does not imply that every verification operation is per segment.

These paths use temporary disk space and external tools. They do not yet promise a constant total memory footprint or a universal conversion route between every format and protection scheme. The command's reported checks describe what was actually verified.

## Interpret a result

Metadata inspection is evidence about declarations or initialization information. File hashing establishes byte identity. Decryption and decoded-content comparisons establish different properties. Neither a structural check nor a successful local decode substitutes for license-server or device playback testing.

The regression is qualified with FFmpeg 9.0.2 and Shaka Packager 3.8.0. FFmpeg 6.1.1 on Ubuntu 24.04 omits the first clear AAC packet's duration and all encrypted AAC packet durations on this fixture. drmr reports `unsupported-packet-timing` rather than guessing missing values or skipping timing checks. Use a build that provides complete packet timing. Linux CI pins a checksum-verified FFmpeg 9.0 build from the [BtbN build project](https://github.com/BtbN/FFmpeg-Builds); media tools are not bundled in the CLI archive.

Use the same source media and tool versions when comparing processing time or memory. A short regression clip is useful for correctness and startup overhead; it cannot establish throughput or memory scaling for a long presentation.
