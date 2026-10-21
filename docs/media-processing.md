# Media processing and verification

drmr preserves the existing video and audio encoding. Repackaging can change container metadata, encryption, and segmentation without changing the encoded picture or sound. A successful conversion requires checks of both media content and its presentation timeline.

## Describe protection separately from delivery

MP4 is a media container. DASH and HLS describe how a player obtains media segments. `clear` means media is unencrypted; it is not a container or streaming format.

| Protection | Cipher and scope |
| --- | --- |
| `cenc` | AES counter mode applied to MP4 media samples |
| `cbcs` | AES cipher block chaining with an encryption/skip pattern inside MP4 media samples |
| HLS `AES-128` | AES cipher block chaining over an entire HLS segment, with padding |
| HLS `SAMPLE-AES` in TS | AES cipher block chaining over specified regions of H.264 pictures or AAC frames |

Subsample encryption leaves selected regions of a sample clear, such as codec headers. This is a separate property from the cipher mode and encryption pattern. A `cenc` label does not mean every byte of the file is encrypted. A `cbcs` label does not describe an entire segment encrypted as HLS AES-128.

DRM systems such as Widevine, PlayReady, and FairPlay add key and license signaling. Signaling, the media encryption scheme, and device support are separate facts. A recognized DRM label does not prove that a license can be acquired or that a particular device can play the result.

The [W3C MP4 encryption description](https://www.w3.org/TR/eme-stream-mp4/) explains MP4 scheme signaling and partially encrypted samples. [HLS encryption declarations](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.2.4) define the distinct segment encryption method.

## Why audio boundaries differ from video boundaries

A typical AAC-LC audio frame contains 1,024 samples. At 48,000 samples per second, one frame lasts about 21.333 milliseconds. Two seconds therefore contain 93.75 frames. A packager must place the boundary on a complete frame, so audio segment durations can alternate around a two-second video target.

The segment target and the integer `EXT-X-TARGETDURATION` declaration have different
roles. RFC 8216 compares each `EXTINF` duration after rounding it to the nearest
integer. drmr derives a conservative track bound by rounding final segment
durations upward. Common conversion applies the largest generated audio/video
bound to each media playlist. For example, separate bounds of 2 and 3 become 3.
The encoded segments and their `EXTINF` durations are preserved. Independent
verification reads the final declarations and rejects inconsistent bounds. This
follows [RFC 8216 section 6.2.4](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.4),
which separately allows target-duration exceptions for VOD subtitles and I-frame
playlists.

This arithmetic explains a boundary difference; it does not justify missing or duplicated frames, gaps, overlaps, or growing audio/video drift. Check the actual sample counts, timestamps, and complete presentation. An arbitrary tolerance of one audio frame could hide lost content.

Audio can also carry encoder priming: samples decoded at the beginning that should not be presented. MP4 edit-list metadata can describe this trimming. Identical compressed audio packets alone do not establish identical presented audio if that metadata changes during repackaging.

The clear-HLS path preserves a single open-ended audio trim at normal playback rate and compensates decode timestamps to retain presentation timing. Complex or finite edits and inexact timescale conversions are rejected. The [ten-second Big Buck Bunny regression](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md) verifies both individual segments and the continuous presentation, including 240 video frames and the source's audio sample count. Its JSON results include tool versions, elapsed time, and sampled memory; CI uploads them with each platform's test artifacts.

## Generic conversion and timing limits

`convert` stages a private snapshot of each supported input track. For local DASH it appends the initialization data and referenced fragments in timeline order; for local HLS it first decrypts complete AES-128 resources when required, then assembles each fragmented MP4 track. Shaka Packager decrypts supported MP4 sample encryption and packages the resulting tracks into the selected output format. SAMPLE-AES TS is decrypted in Rust before packaging. DASH and HLS output are segmented again at a two-second target. HLS AES-128 is applied separately to each resulting resource, with a fresh initialization vector and padding. This route uses whole-track temporary disk files; it does not re-encrypt each original segment in place.

The complete original and final tracks are independently read by FFmpeg. Conversion requires matching stream metadata, encoded packet content, decoded video/audio, and every packet's presentation time, decode time, and duration. Metadata checks include codec configuration, sample rate, channel count/layout, video dimensions and pixel format, aspect ratio, color characteristics, display rotation, and language. Encryption initialization data is expected to change and is checked separately.

Silent audio has the same sample bytes at different sample rates, so decoded-byte identity alone cannot verify its presentation. HLS demuxing can omit some stream metadata; these fields are read from a byte-preserving snapshot of the actual initialization data. Decryption, content, and packet timing still use an independent read of the original playlist.

Presentation times determine when content is displayed; decode times determine when a decoder must process it. Comparing absolute times across all tracks also detects changes to relative audio/video timing. Manifest timeline declarations are checked separately from media bytes. DASH compares times exactly in their declared clock units. HLS segment durations allow the greater of one media-clock tick or about one millisecond for decimal playlist rounding; this does not relax the exact packet timing comparison.

For generated static DASH, the common presentation starts at the earliest presented sample and ends at the latest presented sample across all tracks. The final period and presentation durations round that exact interval upward to nine decimal places. This changes only its decimal representation; compressed payloads and packet clocks remain unchanged. An open-ended fragmented source can therefore produce a period slightly longer than its nominal ten-second duration. A source with a finite audio playback window has different presentation bounds, as described below.

The [joint audio/video regression](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/conversion_signaling.rs) independently decrypts the complete output tracks and checks all 240 video packets, 470 AAC packets, their clocks, and the complete decoded media, including 481,280 audio samples per channel. Fresh verification rejects shortening the manifest to ten seconds even when its media resources are unchanged.

Video with reordered pictures can start with a composition offset: the difference between a picture's decode and presentation times. Shaka Packager 3.8.0 normally resets the initial offset. That changes presentation times even when packet content matches. drmr explicitly disables this reset and tests a source with reordered pictures against its original timestamps. See the [pinned packager source](https://github.com/shaka-project/shaka-packager/tree/v3.8.0/packager/media/formats/mp4).

## Finite audio presentation windows

A progressive MP4 stores sample locations and playback edits together. An audio edit can select a finite range of decoded samples, excluding encoder warm-up at the start and padding at the end. Copying every compressed packet is insufficient: losing that edit can add audible samples without changing any packet payload or timestamp.

The supported source contains one AAC-LC (AAC Low Complexity) audio track stored as `mp4a`, optionally with one H.264 video track stored as `avc1`, in the same complete progressive file. The audio edit must be a single normal-rate window with a nonnegative media start and an exactly representable duration. Empty edits that introduce a delay, multiple windows, rate changes, additional tracks, other codecs, and combining different source files are refused.

`convert` preserves that range for clear, `cenc`, and `cbcs` MP4 or DASH output, and clear, `cbcs`, or AES-128 fMP4 HLS output. Clear MP4 copies the original encoded tracks into a progressive file. The other outputs prepare complete tracks, package fragments, and retain each track's playback window in the resulting metadata. Joint encrypted MP4 combines only the qualified tracks from that original file. It keeps fragment-relative data offsets and assigns distinct track identifiers; it does not combine unrelated clips. Modified fragments do not advertise unqualified Common Media Application Format (CMAF) brands.

DASH and HLS are segmented again with a two-second target. Encoded audio frames remain complete even when the playback range ends inside the final frame. The final output's own edit, each segment's declared presentation interval, and the manifest's end must agree. DASH retains an explicit final period duration. Finite-window DASH and HLS durations use the exact presented sample bounds, rounded upward to nine decimal places only when decimal text is needed. A changed declaration must equal that calculated value; a one-sample edit is not accepted as playlist rounding. The ordinary nonfinite HLS profile retains its separate decimal-rounding rule described above. HLS initialization segments keep the zero movie and track durations required by [RFC 8216](https://www.rfc-editor.org/rfc/rfc8216.html#section-3.3); those headers are not used to invent the audio end.

### Decode every sample, then apply each file's own window

FFmpeg 9.0.2 does not apply the finite end trim on every fragmented MP4 demuxing path. For the ten-second fixture, ordinary progressive decoding presents 480,000 samples per channel, while a fragmented demux can expose padding even with the same compressed samples and edit. This decoder behavior is a reason to check presentation metadata explicitly; it is not permission to shorten a result until it matches the source.

drmr independently decodes all coded audio samples from each side without automatic edit or discard trimming. It compares the complete raw decoded content and sample count, then applies each file's own validated playback window once and compares the presented content and count. The original progressive source's ordinary decode must also agree with its own window. Both the raw content and the exact native windows must match. A changed start, end, payload, packet duration, or missing window fails; the reference is never trimmed to fit the result.

This finite AAC proof uses the decoder's native 32-bit floating-point output. Converting to 16-bit integer audio first can introduce a one-bit rounding difference when the final decode frame has a different length. Comparing native decoded values avoids that extra quantization step. Hashing and window selection stream through a bounded buffer; complete PCM audio is not retained in memory. The [FFmpeg demuxer](https://github.com/FFmpeg/FFmpeg/blob/n9.0.2/libavformat/mov.c) and [decoder](https://github.com/FFmpeg/FFmpeg/blob/n9.0.2/libavcodec/decode.c) implement the edit and discard behavior. The [W3C MP4 byte-stream specification](https://www.w3.org/TR/mse-byte-stream-format-isobmff/#initialization-segments) describes finite, normal-rate edits in initialization segments. These checks qualify drmr's interpretation of the declared range; they do not prove that every player applies it correctly.

### Playback and encryption checks

For this profile, `packet_timing_equal` also requires matching integer presentation windows, with exact conversions between movie and media clocks. Equivalent movie timescales can describe the same window. Packet time bases and all signed absolute presentation times, decode times, and durations must still match. Video with reordered pictures and negative decode timestamps receives the same checks; an edit ending inside the final picture is refused.

This comparison also preserves an explicitly finite video range. Matching packet clocks and decoded pictures alone do not establish that the original playback declaration survived: a finite source range and a fragmented candidate without that range fail the strict comparison. Input preparation must retain the original start and duration before comparing or converting the prepared presentation.

Movie and track playback settings must describe ordinary enabled playback at normal rate and gain, without unsupported alternate-track selection. These native checks apply to progressive and fragmented results. The original video display width and height must be preserved exactly, including nonsquare pixels. The composed display transform remains part of stream metadata comparison. Changing a track's enabled flag, volume, rate, selection group, or display extent can fail even when compressed bytes and decoded media remain unchanged. Fragmented HLS duration headers remain zero while these playback settings are checked.

Encrypted progressive sources must use one protection scheme across all tracks and one default key identifier per track. Every source tool receives all declared role keys by identifier, since stream discovery may read unselected tracks. Clear results do not receive those keys. The admitted profiles are:

- `cenc`: eight- or sixteen-byte per-sample initialization vectors, whole-sample audio encryption, and video subsample encryption with validated auxiliary sizes and offsets.
- `cbcs`: sixteen-byte constant vectors, whole-sample audio encryption, and video with a one-encrypted/nine-clear block pattern and validated subsample records. The audio-only and joint public fixtures exercise these layouts.

Ordinary AAC roll recovery metadata is accepted only with its supported exact descriptor and complete coverage. Key rotation, mixed source protection, alternate auxiliary layouts, and malformed or misplaced encryption metadata are refused before packaging. Unsupported layouts report `progressive-cenc-profile-not-supported` or `progressive-cbcs-profile-not-supported`. Missing keys report `input-key-required`; unsupported presentation windows report `presentation-window-not-supported`. Wrong keys cannot pass complete packet and decode checks.

For this finite progressive conversion profile, drmr also refuses any source Protection System Specific Header (PSSH), including unknown systems or malformed contents, rather than silently removing DRM initialization data. A clear candidate must contain no sample protection, encryption auxiliary data, or PSSH. Independent known-key verification has a different purpose: it compares existing media without rewriting it and can retain the existing CENC/CBCS verification profiles with PSSH or supported mixed track protection. Verification does not establish DRM initialization correctness, license acquisition, or device behavior; `license_playback_verified` remains false. Keys and external-tool arguments are redacted from retained records; operating-system process inspection can expose arguments while a tool runs.

The [finite-output regression](https://github.com/kazuhide/drmr/blob/main/crates/drmr-core/tests/conversion_finite_outputs.rs) covers all 54 protection/layout/output combinations with conversion and fresh original-source verification. Its negative cases alter windows by one sample, playback headers, display extents, encryption metadata, role keys, encoded media, and manifest declarations. A progressive remux is not automatically applied to arbitrary fragmented sources, whose originally presented padding may differ.

## Generic resource limits

Generic conversion is deliberately bounded: at most 16 GiB per staged track, 64 tracks, and 100,000 referenced DASH segments. Exceeding a limit fails rather than reporting partial verification. The first local DASH profile is a static, single-period presentation with finite, nonnegative, continuous `SegmentTimeline` entries, a shared presentation offset matching the earliest presented sample, and relative resources confined to the manifest directory. Live/dynamic input, multiple periods, byte ranges, alternate addressing, event declarations and presentation metadata that cannot be preserved are refused. The fragmented MP4 HLS profile uses completed, local playlists; unsupported rendition metadata and discontinuities are refused. An external audio group must apply consistently to every video variant; mixed associations are refused. Percent-encoded local resource paths are decoded once before confinement checks, so they select the URI target rather than a similarly named literal file. In-band MP4 event messages are also refused until their preservation is supported. These limits describe drmr's implementation, not limits of the formats.

### Multiple qualities and audio declarations

A supported HLS master can reference several video-only playlists and one
separate audio playlist used by every video variant. The TS media restrictions
below apply independently to each playlist. Conversion and fresh verification
match all actual tracks by their media evidence, rather than assuming that the
first source variant corresponds to the first result variant. Missing, extra,
or changed tracks and conflicting alias declarations cannot pass simply by
reordering the master playlist.

Within HLS, the supported audio name, language, group association, channel
declaration, `DEFAULT`, and `AUTOSELECT` behavior are retained and checked. An
omitted selection flag is interpreted according to
[RFC 8216 section 4.3.4.1](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.4.1).
Profiles with alternative audio groups, inconsistent video/audio associations,
or unsupported selection declarations are refused.

For a supported presentation with one audio rendition, DASH output maps its
language to `lang`, its name to `Label`, and its default preference to the
author's `selectionPriority` (2 for preferred, 1 otherwise). Every video quality
shares that sole audio adaptation. HLS `AUTOSELECT` has no identical DASH flag
in this mapping: DASH clients apply their own selection rules. For example,
[dash.js track selection](https://dashif.org/dash.js/pages/usage/track-selection.html)
can consider language, roles, author priority, and player settings. Thus an HLS
audio rendition with omitted `DEFAULT` and `AUTOSELECT` can be converted to DASH,
while identical automatic player selection is not claimed. Cross-format media
verification checks the documented mapping, complete media, and original clocks;
it does not establish equivalent player preference handling. HLS-to-HLS
verification also checks its native selection declarations.

This selection mapping applies to audio. DASH video labels, a language other than
`und` (undetermined), and explicit author selection priorities are refused with
`presentation-selection-not-representable`, including inherited declarations.
They are not silently removed during conversion or ignored by verification.
The neutral `und` language and the standard `main` role remain supported.

### Saved source identities

The [four-stage CLI workflow](executable-plans.md) captures only resources reached
from the local entrypoint. A saved source inventory is limited to 16 GiB total,
100,130 files, 16 MiB of path text, and 128 path components. Every file is copied
into a private working area and its actual copied bytes are hashed before use.
This avoids treating a pathname or an earlier successful analysis as evidence
that the same bytes will be processed later. The retained originals are checked
again before publication. Referenced files are not adopted as temporary cleanup
targets; unrelated files in the source directory are not included in the graph.

## Acquiring URL input

For a supported HLS, static DASH or MP4 URL, `analyze` and direct `convert` retain
the complete source in the chosen download directory. Content detection selects
`input.m3u8`, `input.mpd` or `input.mp4` as the local entrypoint. HLS localization
preserves playlist declarations, duration text, sequence numbers and rendition
associations. DASH localization preserves protection declarations, PSSH and the
explicit finite timeline while pointing at the downloaded initialization and
media resources. Original manifest bytes are retained separately. MP4 bytes are
saved unchanged. The source is not reconstructed from a probe summary, which
could lose those facts. See the [URL input profile](conversion-support.md#url-input)
for the accepted manifest addressing and media conditions.

Conversion and verification then use those same local bytes. They do not fetch
the URL again for comparison: a server can change between requests. The saved
originals and localized references are checked again at final publication, after
output copying and sealing. A changed or missing source prevents publication.
A completed download remains available after conversion failure; an incomplete
acquisition is removed only after active work has stopped.

Cancellation interrupts active response reads. Name resolution and connection
setup may wait for the operating system or request timeout before stopping.

Acquisition caps each manifest at 4 MiB, the combined manifest and media resource
count at 100,000, and planned text and URL metadata at 16 MiB. URI expansion is
checked before constructing the reference graph. Fetch settings can impose
additional lower limits; increasing a setting does not bypass the profile. Resource
bodies stream to private files rather than being retained together in memory.

## MPEG transport stream input

The fragmented MP4 output route accepts a completed local HLS media playlist or a supported master with separate video qualities and one shared audio rendition. Segments in each playlist must use 188-byte packets and one fixed program. Each playlist accepts one progressive H.264 stream, one AAC-LC audio stream at 48,000 samples per second, or both; multiplexed audio/video is limited to the supported single-variant topology. The route supports a fixed single SPS/PPS video configuration (the sequence and picture parameter sets required by a decoder) and ordinary AAC channel configurations. AAC uses seven-byte transport headers without a checksum and one raw data block per frame. Output is DASH with `clear`, `cenc`, or `cbcs`, or fragmented MP4 HLS with `clear`, `cbcs`, or whole-resource AES-128 protection. Input key URI mappings are supplied by the operator; drmr never retrieves a playlist key URI.

Without `--save`, `plan` describes compatibility from metadata and reports `source-profile-validation-required`; it does not qualify the underlying HLS media container. `analyze` and `plan --save` check complete source media, while execution independently admits the actual source again. Conversion can refuse a profile that metadata alone could not distinguish.

Before demuxing, drmr checks program tables and their cyclic redundancy checks, packet continuity, declared streams, packet and elementary-stream lengths, and clock progression across every segment. Multiple programs, unknown streams, configuration changes, corrupted or scrambled packets, clock wrap, discontinuities, byte ranges, wall-clock metadata, interlaced or mixed video, and unsupported descriptors are refused. Segment durations must match the primary video's measured duration rounded only to the decimal precision written in each `EXTINF` declaration. Every absolute packet timestamp is still compared exactly.

Moving media into MP4 changes codec framing: H.264 start codes become lengths, and AAC transport headers become decoder configuration. The verification therefore compares the complete decoder configuration and canonical access units (the encoded bytes for each picture or audio frame). Only validated access-unit delimiters and unchanged H.264 parameter sets are removed from that comparison; every remaining NAL unit, including supplemental information, and every raw AAC byte must retain its order and count. Full decoded-frame evidence from each complete playlist must establish progressive video before either omitted field-order label can be resolved; an existing label remains exact. The existing same-container packet-hash checks are unchanged.

The source for the final checks is the original complete HLS playlist, read from a confined, byte-preserving private snapshot with its supplied keys. The result is the actual generated HLS playlist or DASH manifest and its referenced media, independently decrypted when required. Both must match complete decoded media and every absolute presentation time, decode time, and packet duration, including relative audio/video timing. No normalized MP4 is substituted as the source reference. Output remains private until these checks pass; no key file is published automatically.

Reading an audio-only HLS playlist through FFprobe can omit the first packet duration and repeat its presentation timestamp even though the transport packets have distinct clocks. The TS routes therefore read original packetized elementary stream (PES) clocks and Audio Data Transport Stream (ADTS) headers directly, before normalization. They require 48 kHz AAC-LC, one 1,024-sample audio frame per ADTS unit, a stable configuration, and exactly 1,920 ticks per frame on the 90 kHz transport clock. Every payload, count, presentation time, and decode time must match an independent read of the complete ordered transport bytes before those bytes can serve as a reference. Full decoding must also produce every original sample per channel. Missing or inconsistent evidence is refused; a repeated timestamp is not repaired by guessing.

Audio-only input uses those native checks for DASH, fragmented MP4 HLS, and TS output. fMP4 HLS previously required video because the FFprobe-only path lacked that timing evidence; this was an implementation restriction, not a requirement of HLS. The original TS evidence now also anchors conversion and fresh verification of audio-only fMP4 HLS. Separately, 44.1 kHz AAC is refused because the tested remux changes individual packet clock values.

Creating a transport stream from a progressive MP4 can itself change audio trimming or padding. This route proves identity to the supplied transport-stream presentation, not to an earlier file from which it was created. All existing staged-size, resource-budget, timeout, cancellation, and confirmed-process-cleanup limits still apply. Access-unit evidence is streamed with an 8 MiB per-packet limit and a charged 64 MiB reader bound; a malformed, oversized, incomplete, or unsuccessful read fails rather than returning a partial proof.

### Audio-only fMP4 HLS from transport streams

HLS can carry audio in fragmented MP4; see [Apple's audio authoring requirements](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html) and [the fMP4 segment rules in RFC 8216](https://www.rfc-editor.org/rfc/rfc8216.html#section-3.3). drmr checks and decrypts the ordered TS resources, separates the complete audio track into private MP4 media, then packages new fMP4 segments and playlists. It does not encrypt each source segment in place. The actual final playlists, initialization media, and segments must pass the four original-source comparisons before publication.

AAC frames contain 1,024 samples in this profile. At 48 kHz a frame lasts about 21.333 milliseconds, so a two-second segment target cannot always be an exact audio-frame boundary. drmr retains complete frames and their original clocks, and checks the playlist durations against the resulting media. It neither cuts a frame to reach two seconds nor drops the last frame to make the whole presentation exactly ten seconds. The 470-frame TS fixture therefore presents 481,280 samples per channel; a progressive MP4 that declares a shorter finite playback range is a different source presentation.

### DASH output from transport streams

MPEG-TS timestamps often start above zero. DASH uses a presentation-time offset to map media clocks onto the manifest's playback timeline. Audio can begin before the first video picture; choosing that picture as the common start would hide the earlier audio. drmr chooses the earliest original presentation time across all tracks as the shared start. It preserves the original media timestamps and relative audio/video timing, checks each fragment against the manifest's integer segment timeline, and reads the final manifest again before publication. It does not move each track independently to zero.

An AAC track can end between two decimal fractions that a manifest can express. For example, transport clocks count 90,000 ticks per second, so some exact durations contain a repeating decimal. drmr keeps segment and packet times as exact integers or rational values. It represents the final DASH period duration by rounding its exact latest track end upward to nine decimal places, a difference of less than one nanosecond. The declared duration must equal that calculated value; this is an XML representation rule, not a packet-timing tolerance. Audio and video keep their own complete durations.

The manifest contains an explicit final period duration. Its presentation-time offset, segment start times, durations and resource bytes are checked independently. A modified declaration is not accepted merely because its packets still decode. This bounded static DASH profile does not establish conformance to every DASH interoperability profile or playback device. The [DASH-IF timing model](https://dashif.org/Guidelines-TimingModel/#sample-timeline) explains the distinct media and presentation timelines.

### SAMPLE-AES transport streams

SAMPLE-AES TS uses the [Apple sample-encryption format](https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/HLS_Sample_Encryption/Encryption/Encryption.html).
H.264 keeps codec headers clear and encrypts designated 16-byte blocks in a
one-encrypted/nine-clear pattern. AAC keeps its transport header and an initial
clear prefix, then encrypts complete blocks within each audio frame. The IV is
reset for each protected H.264 network abstraction layer (NAL) unit or audio
frame. These rules require
sample-level decryption rather than whole-segment AES-128 decryption.

drmr checks the original encrypted program tables, packet continuity, clocks,
and packetized elementary streams (PES) before external demultiplexing. It then
decrypts encoded frames in Rust and records every original frame's content,
configuration, count, and presentation/decode times. A private clear TS view may
change encryption signaling and payload stuffing, but must retain those original
facts and clock references. H.264 escape bytes introduced by encryption are
removed once; the original encoded escape bytes remain intact.

The private view is checked against that original evidence before packaging.
Video packet durations are derived from the same unchanged encoded source by
FFprobe, since PES headers do not store them. Every duration is still required
and compared exactly. Decoded video-frame counts must match the original encoded
picture count; AAC must decode all 1,024 samples per frame and channel. Final
output also passes the existing complete content and timing checks. This covers
the last buffered packet as well as earlier segments.

The initial profile requires complete PES packets within each segment, a fixed
SAMPLE-AES method, and the supported Apple stream descriptors. AAC is 48 kHz
AAC-LC with seven-byte headers, no checksum, and zero declared priming. Unsupported
descriptors or configuration changes are refused. PES payloads are limited to
8 MiB each, with bounded simultaneous payloads and packet-position records.
The native decryption phase shares the command's CPU/memory admission and
cancellation; its permit is released before external tools run.

## MPEG transport stream output

`--format hls-ts` reads admitted original TS resources in playlist order and
prepares complete tracks before creating new segments. It does not independently
re-encrypt each old segment. For SAMPLE-AES input, the clear view remains bound
to native evidence taken from the original encrypted frames. For clear or
AES-128 input, native evidence is taken from the original clear transport bytes
after whole-resource decryption where required. Joining resources creates a
private byte-preserving reference; it is not permission to rewrite their clocks
or lose frames at a boundary.

Shaka Packager produces separate video and audio TS playlists when both roles
are present. drmr writes a master playlist that preserves their association.
Video segments target two seconds. Audio boundaries follow complete frames and
can differ in both duration and number of segments. The full presentation and
the original relative audio/video timing must still match exactly.

For SAMPLE-AES output, sample encryption starts with the first media frame:
there is no initial clear interval. Only the regions defined by the codec
encryption rules are encrypted. drmr disables the packager's default 100 millisecond
transport timestamp offset, because adding the offset would change the source
timeline. See the [Shaka Packager 3.8.0 timestamp option](https://github.com/shaka-project/shaka-packager/blob/v3.8.0/packager/app/muxer_flags.cc#L56-L63). The output uses the FairPlay key-format declaration and the explicit
key URI, key, and associated IV supplied by the operator. AAC and H.264 use
different frame-encryption rules, so no common encryption/skip pattern is
claimed for the presentation. AES-128 output instead encrypts each complete
new `.ts` resource, with padding and its own fresh IV.

The actual final playlists are read again, with output keys independent from
source keys. Every native frame, codec configuration, count, absolute presentation/decode
time, packet duration, and decoded picture or audio sample must match the original.
No frame tolerance, clock realignment, missing-value substitution, or final
padding removal is used to make a result pass. In particular, 470 AAC frames
represent 481,280 samples per channel; the original TS presentation may include
padding absent from a finite ten-second MP4 edit. Verification preserves the
supplied TS presentation rather than trimming it to that earlier file.

The current TS output profile excludes MP4/fMP4 input, live playlists,
unsupported codecs or audio rates, nonzero audio priming, and a single output
TS multiplex containing both audio and video. These are implementation limits;
HLS and MPEG-TS are capable of additional profiles. Existing source confinement,
shared CPU/memory admission, bounded parsing, cancellation, confirmed subprocess
cleanup, and publication checks apply throughout.

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

An incorrect HLS AES-128 key does not always produce a padding error. Its decrypted bytes can accidentally match PKCS#7, the required byte pattern at the end of a resource. The result can instead be rejected while parsing MP4/TS data or comparing media. Successful padding removal does not establish that a key is correct. Use independent `verify` against the original source for content and timing checks.

The regression is qualified with FFmpeg 9.0.2 and Shaka Packager 3.8.0. FFmpeg 6.1.1 on Ubuntu 24.04 omits the first clear AAC packet's duration and all encrypted AAC packet durations on this fixture. drmr reports `unsupported-packet-timing` rather than guessing missing values or skipping timing checks. Use a build that provides complete packet timing. Linux CI pins a checksum-verified FFmpeg 9.0 build from the [BtbN build project](https://github.com/BtbN/FFmpeg-Builds); media tools are not bundled in the CLI archive.

Use the same source media and tool versions when comparing processing time or memory. A short regression clip is useful for correctness and startup overhead; it cannot establish throughput or memory scaling for a long presentation.

## Transport-stream compatibility

`analyze` reports the following source facts in `compatibility_notes`. They
describe limited compatibility handling, not general repair or a guarantee of
transport-stream standards conformance. The retained original and the source
fingerprints always refer to the original bytes.

A packet identifier (PID) selects a TS stream. Packetized elementary stream
(PES) packets carry its encoded media.

| Code | Accepted source condition | Processing |
| --- | --- | --- |
| `ts-empty-payload-padding` | An established audio/video PID has a zero-body packet with adaptation-plus-payload control, adaptation length 183, no payload-unit start, zero flags and only `0xFF` stuffing; the native PES and continuity checks must still pass | Omit only these empty packets from a private demultiplexer input and adjust subsequent continuity counters for the same PID by the exact number removed |
| `hls-empty-cue-pair` | A completed segment is followed by a zero-duration `EXT-X-CUE-OUT`, immediately paired with `EXT-X-CUE-IN` and then the next `EXTINF` | Accept the empty boundary; keep the original playlist, but do not carry an active advertisement interval into the result |

The 183-byte adaptation field with the payload flag set is nonconforming:
[ITU-T H.222.0, section 2.4.3.5](https://www.itu.int/rec/dologin_pub.asp?id=T-REC-H.222.0-201410-S%21%21PDF-E&lang=e&type=items)
limits that form to 182 bytes. The
[Shaka Packager 3.8 parser](https://github.com/shaka-project/shaka-packager/blob/v3.8.0/packager/media/formats/mp2t/ts_packet.cc#L110-L115)
also rejects it. The private view lets the demultiplexer read an otherwise
admitted source without inventing media bytes or timestamps. Original native
packet validation, complete encoded payloads, original presentation/decode
timestamps, and full decoded content remain the comparison reference. This view
also feeds TS repackaging; it does not modify the retained source.

Active or unbalanced cue markers, missing payload, invalid stuffing, continuity
errors, unsupported packet flags, and incomplete PES data remain failures.

### Decimal segment durations

`EXTINF` is a decimal declaration of a segment's duration. The
[HLS specification](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.2.1)
requires sufficient precision to avoid accumulated perceptible error; it does
not impose a nine-digit fractional limit. Native TS timestamps use a 90 kHz
clock: one tick is 1/90,000 second, which often has no finite decimal expansion.

For zero through nine fractional digits, drmr compares the declared value at its
stated decimal precision. For ten through seventeen digits, it also accepts a
bounded decimal whose IEEE 754 double-precision value is exactly the same as
the native tick duration divided by 90,000. This is an equality check, with no
additional timing tolerance. Native duration must fit an unsigned 32-bit tick
count for each segment, and the declaration is limited to 64 characters.
Nonfinite numbers, exponents, excessive precision and trailing data fail this
rule. A duration changed by one clock tick does not pass as decimal rounding.
All media frame and absolute clock checks still apply; decimal acceptance
cannot realign audio or video.
