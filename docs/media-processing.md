# Media processing and verification

drmr preserves the existing video and audio encoding. Repackaging can change container metadata, encryption, and segmentation without changing the encoded picture or sound. A successful conversion requires native checks of all selected media content, codec configuration and presentation clocks/windows. `--decode` additionally checks complete decoding without replacing those requirements. See [verification depth and report schemas](verification.md).

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

A typical AAC-LC audio frame contains 1,024 samples. At 48,000 samples per second, one frame lasts about 21.333 milliseconds. Two seconds therefore contain 93.75 frames. At 44,100 samples per second, one frame lasts about 23.220 milliseconds and two seconds contain about 86.133 frames. A packager must place the boundary on a complete frame, so audio segment durations can alternate around a two-second video target.

The segment target and the integer `EXT-X-TARGETDURATION` declaration have different
roles. [RFC 8216 section 4.3.3.1](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.3.1)
compares each `EXTINF` duration after rounding it to the nearest integer.
For original-segment processing, and when stitching with the source HLS
target, drmr checks every final segment against that bound and retains it
in every generated media playlist. For
example, 6.013977778 seconds fits a target of 6; 6.5 seconds does not. The
check compares decimal digits exactly and leaves the actual durations and
encoded media unchanged. If a new segment cannot fit the inherited bound,
conversion stops before publication. Other sources and older plans with a
different packaging target retain the generated conservative bound. Common
conversion applies the largest such audio/video bound to each media playlist.
Independent verification reads the final declarations and rejects inconsistent
bounds. This follows [RFC 8216 section 6.2.4](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.4),
which separately allows target-duration exceptions for VOD subtitles and I-frame
playlists.

This arithmetic explains a boundary difference; it does not justify missing or duplicated frames, gaps, overlaps, or growing audio/video drift. Check the actual sample counts, timestamps, and complete presentation. An arbitrary tolerance of one audio frame could hide lost content.

Audio can also carry encoder priming: samples decoded at the beginning that should not be presented. MP4 edit-list metadata can describe this trimming. Identical compressed audio packets alone do not establish identical presented audio if that metadata changes during repackaging.

## Generic conversion and timing limits

`--processing` selects `auto`, `segments`, or `stitch` when converting directly or saving a plan. `auto` selects a qualified route and is the default when omitted. The saved plan records the requested and resolved modes plus the boundary guarantee; execution cannot override them.

`stitch` prepares complete tracks before packaging. Local DASH initialization data and fragments are assembled in timeline order. HLS AES-128 resources are decrypted separately before assembly; their padding and cipher state cannot be treated as one encrypted stream. SAMPLE-AES TS is decrypted in Rust. Shaka Packager packages the prepared tracks into the requested output: a supported MP4 file, or HLS/DASH segmented again at its resolved target. HLS AES-128 is applied to each resulting resource with a fresh initialization vector and padding. This route uses whole-track temporary disk files and does not retain original segment boundaries.

For a newly created plan or direct conversion from HLS, the common admitted
`EXT-X-TARGETDURATION` is retained as the selected bound. This declaration is
separate from the usual video segment length. For example, video segments can
last 2.000 seconds while fractional AAC durations make the conservative common
declaration 3. Asking a packager for three-second segments can then move a
video cut to the next four-second access point and exceed that source bound.
For HLS and DASH output, drmr measures resource intervals for each selected video
stream using admitted sample clocks. For fragmented MP4, it uses the longest
interior fragment interval. Reordered pictures and playback edges can make the
first and final fragments shorter or longer than the regular cadence: a sequence
of 1.916666667, 2, 2, 2 and 2.083333334 seconds still uses a two-second request.
With only one or two fragments, no interior cadence is established, so the
smaller observed interval supplies a conservative request. Transport streams
retain their measured maximum video interval. drmr selects a
packaging request no larger than those intervals or the inherited bound. Two-second video under
a bound of 3 therefore uses a two-second request; six-second video under a bound
of 6 keeps a six-second request. Audio-only input uses the selected bound.
HLS output retains the original declaration and must pass its final duration
proof. DASH declarations describe the resulting sample intervals.
All selected audio/video media playlists must agree, as required by
[RFC 8216 section 6.2.4](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.4).
Missing, invalid or overflowing values, and conflicting declarations, are
refused. MP4 and DASH sources retain a two-second fallback because this route
does not infer a nominal packaging target from their sample or timeline lengths.
New saved plans, versions 9/10, store the exact packaging request in
`segment_duration_ms`. Execution passes that value unchanged and derives the
separate HLS bound from the same byte-bound source. Versions 1–6 retain their
original behavior: two seconds for implicit selections, or the exact explicitly
saved request, with generated HLS headers. They do not adopt the
new bound-inheritance policy, even when a saved request happens to equal the
source declaration. Create a new plan to use the current selection policy.
Milliseconds are stored as a positive 32-bit integer; a measured video interval
is rounded downward so the request cannot move just beyond an access point.

The target does not promise identical segment boundaries. Shaka's
[consistent chunking algorithm](https://github.com/shaka-project/shaka-packager/blob/v3.8.0/packager/media/chunking/chunking_handler.h)
uses the absolute sample clock and the next permitted access point. A nonzero
source origin can leave a partial first segment, and the final segment can be
shorter. To avoid an unintended short first audio segment, `stitch` temporarily
projects the first coded sample in the private audio packaging input to time
zero. After packaging, drmr restores the input byte for byte and restores the
original absolute clocks in the output, before applying playback windows,
writing final manifests and independently comparing the complete media. The
retained source is unchanged. `segments` keeps the original sample membership
and does not perform this projection. Audio cuts retain complete frames.
Matching targets therefore do not
establish identical audio/video cut positions; every original sample, timestamp
and relative audio/video offset must still pass the independent media checks.
Use a qualified `segments` route when retaining sample membership and original
boundaries is required.

`segments` converts supported matching media containers at the original segment boundaries. Rust preserves each segment's sample membership, raw presentation/decode clocks and duration while updating encryption metadata, data locations and manifest declarations. Custom keyframes and unequal segment durations are retained; encrypted resource byte sizes can change. Native processing and boundary verification read the original resources without assembling a whole-track intermediate. Segment boundaries do not make media state independent: TS continuity, timestamps and frame assembly remain ordered checks across resources. An unsupported container change or source profile is refused, with no silent switch to `stitch`.

Native `segments` processing also checks the qualified two-edit terminal-AAC presentation layout. It binds the declared playback range to every coded frame and clock while retaining the original sample membership. Mismatched ranges and unsupported edits are refused. Matching-container and track-layout conditions still apply; this does not enable container changes that require `stitch`.

Optional `--decode` still adds complete decoding and may prepare whole-track temporary files. Its existing TS preparation also uses Shaka Packager, in addition to FFmpeg/ffprobe decoding. Segments mode does not imply zero temporary storage, constant memory, or a particular speed improvement. Original resources remain private and unchanged, and output is published only after all required checks pass. See [processing modes](usage.md#select-a-processing-mode) for commands.

Rust independently reads the complete original and final tracks. Conversion requires matching stream metadata, every selected encoded sample, presentation/decode time and duration, and supported playback windows. `--decode` additionally requires complete decoded video/audio equality. Metadata checks include codec configuration, sample rate, channel count/layout, video dimensions and pixel format, aspect ratio, color characteristics, display rotation, and language. Encryption initialization data is expected to change and is checked separately.

Silent audio has the same sample bytes at different sample rates, so decoded-byte identity alone cannot verify its presentation. Native checks read stream metadata from the actual initialization data and compare complete source and output sample evidence. Original playlist declarations remain independent timing references.

Presentation times determine when content is displayed; decode times determine when a decoder must process it. Comparing absolute times across all tracks also detects changes to relative audio/video timing. Manifest timeline declarations are checked separately from media bytes. DASH compares times exactly in their declared clock units. HLS segment durations allow the greater of one media-clock tick or about one millisecond for decimal playlist rounding; this does not relax the exact packet timing comparison.

MP4 edit lists map encoded sample clocks onto movie presentation time. A
normal-rate single edit with a duration of zero applies to all subsequent
fragmented media; it does not declare an empty presentation. drmr applies the
admitted audio and video edits when checking DASH intervals, total duration
and single-file indexes, while comparing original packet clocks separately.
Ignoring a video edit can otherwise offset every manifest interval even when
the compressed samples are unchanged. See the
[W3C initialization-segment rules](https://www.w3.org/TR/mse-byte-stream-format-isobmff/#initialization-segments).

For generated static DASH, the common presentation starts at the earliest presented sample and ends at the latest presented sample across all tracks. The final period and presentation durations round that exact interval upward to nine decimal places. This changes only its decimal representation; compressed payloads and packet clocks remain unchanged. An open-ended fragmented source can therefore produce a period slightly longer than its nominal ten-second duration. A source with a finite audio playback window has different presentation bounds, as described below.

The [joint audio/video regression](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/conversion_signaling.rs) independently decrypts the complete output tracks and checks all 240 video packets, 470 AAC packets, their clocks, and the complete decoded media, including 481,280 audio samples per channel. Fresh verification rejects shortening the manifest to ten seconds even when its media resources are unchanged.

Video with reordered pictures can start with a composition offset: the difference between a picture's decode and presentation times. Shaka Packager 3.8.0 normally resets the initial offset. That changes presentation times even when packet content matches. drmr explicitly disables this reset and tests a source with reordered pictures against its original timestamps. See the [pinned packager source](https://github.com/shaka-project/shaka-packager/tree/v3.8.0/packager/media/formats/mp4).

New downloads and conversions retain the source's relative media directories,
independently of the processing mode. Older saved plans keep their recorded
placement. See
[retaining source directories](source-layout.md) for retained downloads,
generated filenames and the supported source-relative output layout.

### Private track identifiers

Each conversion job prepares its tracks in a separate owner-only working directory. This prevents one job from removing temporary files while another job checks that directory. A track identifier names a track within an MP4 file. Before packaging fragmented MP4 with nonsequential identifiers, drmr creates a private compatibility copy and assigns identifiers in the original track order: 1 for a single track, or 1, 2 and so on for multiple tracks.

The copy changes identifier fields and recognized matching index references without rebuilding the fragments. It preserves sample bytes, byte positions, timestamps, encryption metadata and playback ranges. This avoids a verified crash in the tested Linux x86-64 Shaka Packager 3.8.0 binary when a single-track fragmented file retains identifier 2. Native analysis and independent verification retain their existing input rules; this tool-specific copy is prepared only when packaging.

### Absolute fragment positions

Some fragmented MP4 files locate media using absolute byte positions within the file. Native analysis and verification accept this layout when its samples and clocks pass the existing checks. In the tested AAC fixture, the Linux x86-64 Shaka Packager 3.8.0 binary changed encoded sample bytes during normalization for DASH and TS output, including with a single track already numbered 1.

Conversion detects that mismatch and returns `source-normalization-verification-failed` before publishing a result. Native validation of the original remains available. This fixture is outside the qualified conversion cases for that tool build; see the [release qualification scope](releases.md). A successful tool exit alone does not establish matching media.

## Finite audio presentation windows

A progressive MP4 stores sample locations and playback edits together. An audio edit can select a finite range of decoded samples, excluding encoder warm-up at the start and padding at the end. Copying every compressed packet is insufficient: losing that edit can add audible samples without changing any packet payload or timestamp.

The supported source contains one AAC-LC (AAC Low Complexity) audio track stored as `mp4a`, optionally with one H.264 (`avc1`), qualified HEVC Main/Main10 (`hvc1`/`hev1`) or VP9 (`vp09`) video track, in the same complete progressive file. The audio edit must be a single normal-rate window with a nonnegative media start and an exactly representable duration. Empty edits that introduce a delay, multiple windows, rate changes, additional tracks, other codecs, and combining different source files are refused.

The VP9/AAC profile supports clear or CENC sample protection; the qualified AVC/HEVC/AAC profiles also retain CBCS support. VP9/CBCS is refused with `vp9-protection-scheme-not-supported`.

`convert` preserves that range for clear, `cenc`, and `cbcs` MP4 or DASH output, and clear, `cenc`, `cbcs`, or AES-128 fMP4 HLS output. Clear MP4 copies the original encoded tracks into a progressive file. The other outputs prepare complete tracks, package fragments, and retain each track's playback window in the resulting metadata. Joint encrypted MP4 combines only the qualified tracks from that original file. It keeps fragment-relative data offsets and assigns distinct track identifiers; it does not combine unrelated clips. Modified fragments do not advertise unqualified Common Media Application Format (CMAF) brands.

Progressive copying and input decryption run in Rust. The writer copies the
original into a private file and decrypts each sample at its original byte
position. It restores clear sample types and replaces protection boxes with
equal-sized padding boxes whose contents are zeroed. Sample and chunk locations,
timestamps, playback edits and display settings therefore remain unchanged;
this copy is not joined, resegmented or decoded. Complete media is streamed
through bounded buffers. The original-to-result checks independently read every
selected sample and its timing and playback range before publication.

DASH and HLS are segmented again with a two-second target. Encoded audio frames remain complete even when the playback range ends inside the final frame. The final output's own edit, each segment's declared presentation interval, and the manifest's end must agree. DASH retains an explicit final period duration. Finite-window DASH and HLS durations use the exact presented sample bounds, rounded upward to nine decimal places only when decimal text is needed. A changed declaration must equal that calculated value; a one-sample edit is not accepted as playlist rounding. The ordinary nonfinite HLS profile retains its separate decimal-rounding rule described above. HLS initialization segments keep the zero movie and track durations required by [RFC 8216](https://www.rfc-editor.org/rfc/rfc8216.html#section-3.3); those headers are not used to invent the audio end.

### Additional decoding: every sample and each file's own window

FFmpeg 9.0.2 does not apply the finite end trim on every fragmented MP4 demuxing path. For the ten-second fixture, ordinary progressive decoding presents 480,000 samples per channel, while a fragmented demux can expose padding even with the same compressed samples and edit. This decoder behavior is a reason to check presentation metadata explicitly; it is not permission to shorten a result until it matches the source.

With `--decode`, drmr independently decodes all coded audio samples from each side without automatic edit or discard trimming. It compares the complete raw decoded content and sample count, then applies each file's own validated playback window once and compares the presented content and count. The original progressive source's ordinary decode must also agree with its own window. Both the raw content and the exact native windows must match. A changed start, end, payload, packet duration, or missing window fails; the reference is never trimmed to fit the result.

This finite AAC proof uses the decoder's native 32-bit floating-point output. Converting to 16-bit integer audio first can introduce a one-bit rounding difference when the final decode frame has a different length. Comparing native decoded values avoids that extra quantization step. Hashing and window selection stream through a bounded buffer; complete PCM audio is not retained in memory. The [FFmpeg demuxer](https://github.com/FFmpeg/FFmpeg/blob/n9.0.2/libavformat/mov.c) and [decoder](https://github.com/FFmpeg/FFmpeg/blob/n9.0.2/libavcodec/decode.c) implement the edit and discard behavior. The [W3C MP4 byte-stream specification](https://www.w3.org/TR/mse-byte-stream-format-isobmff/#initialization-segments) describes finite, normal-rate edits in initialization segments. These checks qualify drmr's interpretation of the declared range; they do not prove that every player applies it correctly.

### Playback and encryption checks

For this profile, `packet_timing_equal` also requires matching integer presentation windows, with exact conversions between movie and media clocks. Equivalent movie timescales can describe the same window. Packet time bases and all signed absolute presentation times, decode times, and durations must still match. Video with reordered pictures and negative decode timestamps receives the same checks; an edit ending inside the final picture is refused.

This comparison also preserves an explicitly finite video range. Matching packet clocks and decoded pictures alone do not establish that the original playback declaration survived: a finite source range and a fragmented candidate without that range fail the strict comparison. Input preparation must retain the original start and duration before comparing or converting the prepared presentation.

Movie and track playback settings must describe ordinary enabled playback at normal rate and gain, without unsupported alternate-track selection. These native checks apply to progressive and fragmented results. The original video display width and height must be preserved exactly, including nonsquare pixels. The composed display transform remains part of stream metadata comparison. Changing a track's enabled flag, volume, rate, selection group, or display extent can fail even when compressed bytes and decoded media remain unchanged. Fragmented HLS duration headers remain zero while these playback settings are checked.

Encrypted progressive sources must use one protection scheme across all tracks and one default key identifier per track. Every source tool receives all declared role keys by identifier, since stream discovery may read unselected tracks. Clear results do not receive those keys. The admitted profiles are:

- `cenc`: eight- or sixteen-byte per-sample initialization vectors, whole-sample audio encryption, and video subsample encryption with validated auxiliary sizes and offsets.
- `cbcs`: sixteen-byte constant vectors, whole-sample audio encryption, and video with a one-encrypted/nine-clear block pattern and validated subsample records. The audio-only and joint public fixtures exercise these layouts.

Ordinary AAC roll recovery metadata is accepted only with its supported exact descriptor and complete coverage. Key rotation, mixed source protection, alternate auxiliary layouts, and malformed or misplaced encryption metadata are refused before packaging. Unsupported layouts report `progressive-cenc-profile-not-supported` or `progressive-cbcs-profile-not-supported`. Missing keys report `input-key-required`; unsupported presentation windows report `presentation-window-not-supported`. Complete payload and timing mismatches against an independent reference are rejected; requested complete decoding must pass too.

The supported encryption modes are unauthenticated. Without an independent known clear reference, native correspondence alone does not establish key correctness or complete decodability. Successful `--decode` adds decoder evidence, without cryptographically authenticating a key or plaintext or proving license acquisition or device playback.

For this finite progressive conversion profile, drmr also refuses any source Protection System Specific Header (PSSH), including unknown systems or malformed contents, rather than silently removing DRM initialization data. A clear candidate must contain no sample protection, encryption auxiliary data, or PSSH. Independent known-key verification has a different purpose: it compares existing media without rewriting it and can retain the existing CENC/CBCS verification profiles with PSSH or supported mixed track protection. Verification does not establish DRM initialization correctness, license acquisition, or device behavior; `license_playback_verified` remains false. Keys and external-tool arguments are redacted from retained records; operating-system process inspection can expose arguments while a tool runs.

The [finite-output regression](https://github.com/kazuhide/drmr/blob/main/crates/drmr-core/tests/conversion_finite_outputs.rs) covers all 54 protection/layout/output combinations with conversion and fresh original-source verification. Its negative cases alter windows by one sample, playback headers, display extents, encryption metadata, role keys, encoded media, and manifest declarations. A progressive remux is not automatically applied to arbitrary fragmented sources, whose originally presented padding may differ.

A complete joint fragmented MP4 can be reimported when it has exactly one supported video and one AAC-LC audio track, the same supported protection on both, and one positive normal-rate finite edit per track. Rust first validates all source media and keys. Protected fragmented input uses private single-track views of the original configuration and fragments. Each view must independently match its original track before Shaka Packager processes it. Every original presentation window is restored before the fragments are combined. The final clear, CENC or CBCS MP4 is independently compared with the original. Clear output stays fragmented. This does not combine unrelated files or admit arbitrary multi-track layouts.

This separation avoids an observed selected-track decryption problem in the tested Shaka Packager 3.8.0: reading the combined CBCS fixture changed all 470 AAC packet payloads while its 240 video packets matched. Reading the validated single-track views preserved both video and AAC. A successful tool exit never replaces the per-view or final media comparison.

## Generic resource limits

Generic conversion is deliberately bounded: at most 16 GiB per staged track, 64 tracks, and 100,000 referenced DASH segments. Exceeding a limit fails rather than reporting partial verification. Local DASH accepts one static Period with finite nominal-duration templates, explicit timelines, lists, or supported flat single-file indexes. Resources must remain confined to the manifest directory; declared timing and the shared presentation origin must agree with actual media. Lists can use separate objects or disjoint byte ranges in one object. Live/dynamic input, multiple periods, hierarchical or external indexes, event declarations and presentation metadata that cannot be preserved are refused. The fragmented MP4 HLS profile uses completed, local playlists; byte ranges, unsupported rendition metadata and discontinuities are refused. An external audio group must apply consistently to every video variant; mixed associations are refused. Percent-encoded local resource paths are decoded once before confinement checks, so they select the URI target rather than a similarly named literal file. In-band MP4 event messages are also refused until their preservation is supported. See [DASH references](dash-addressing.md) for the addressing and range conditions. These limits describe drmr's implementation, not limits of the formats.

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

HLS written from DASH can add a mono or stereo `CHANNELS` attribute from the checked AAC configuration. An explicit source channel declaration must be retained and match the actual media; when the source omitted it, verification allows this checked addition.

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
associations. DASH localization preserves protection declarations, PSSH, finite
resource timing and supported byte ranges while pointing at the downloaded
initialization and media resources. Original manifest bytes are retained
separately. MP4 bytes are
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

The acquisition graph, preserved paths and ancestor directories, file-ownership
records, and overlapping verification inventories share a 128 MiB metadata
ceiling. Admission accounts for retained metadata before creating files and
keeps that memory charged while the downloaded source is in use. Deep or long
paths can reach this ceiling before the resource-count limit. Worker buffers
and transient acquisition state have a separate bounded reservation; raising a
fetch-policy resource limit does not remove these memory limits.

## MPEG transport stream input

The fragmented MP4 output route accepts a completed local HLS media playlist or a supported master with separate video qualities and one shared audio rendition. Segments in each playlist must use 188-byte packets and one fixed program. Each playlist accepts one progressive H.264 stream, one AAC-LC audio stream at 44,100 or 48,000 samples per second, or both; multiplexed audio/video is limited to the supported single-variant topology. The route supports a fixed single SPS/PPS video configuration (the sequence and picture parameter sets required by a decoder) and ordinary AAC channel configurations. AAC uses seven-byte transport headers without a checksum and one raw data block per frame. DASH and fragmented MP4 HLS support `clear`, `cenc`, or `cbcs`; fMP4 HLS also supports whole-resource AES-128 protection. Input key URI mappings are supplied by the operator; drmr never retrieves a playlist key URI.

Standalone fragmented MP4 output supports `clear`, `cenc`, or `cbcs` with one video, one audio, or one of each. For this output, restrict a multi-quality or multi-audio master to the chosen video and associated audio before analysis. It never silently selects or drops tracks. A supported multiplexed TS playlist or the selected video and separate audio playlist can supply those tracks. `auto` and `stitch` prepare complete tracks; `segments` refuses the container change. See [TS to one MP4](conversion-support.md#mpeg-transport-streams-to-one-mp4) for representable language metadata and HLS selection attributes that MP4 does not carry.

Without `--save`, `plan` describes compatibility from metadata and reports `source-profile-validation-required`; it does not qualify the underlying HLS media container. Normal `analyze` and `plan --save` check complete source media, while execution independently admits the actual source again. `analyze --metadata-only` reads declarations without that media admission. Conversion can refuse a profile that metadata alone could not distinguish.

Before demuxing, drmr checks program tables and their cyclic redundancy checks, packet continuity, declared streams, packet and elementary-stream lengths, and clock progression across every segment. Multiple programs, unknown streams, configuration changes, corrupted or scrambled packets, clock wrap, discontinuities, byte ranges, wall-clock metadata, interlaced or mixed video, and unsupported descriptors are refused. Segment durations must match the primary video's measured duration rounded only to the decimal precision written in each `EXTINF` declaration. Every absolute packet timestamp is still compared exactly.

Moving media into MP4 changes codec framing: H.264 start codes become lengths, and AAC transport headers become decoder configuration. The verification therefore compares the complete decoder configuration and canonical access units (the encoded bytes for each picture or audio frame). Only validated access-unit delimiters and unchanged H.264 parameter sets are removed from that comparison; every remaining NAL unit, including supplemental information, and every raw AAC byte must retain its order and count. Native SPS/PPS/VUI and every slice header must establish the supported progressive picture structure; absent metadata is not permission to assume it. Existing declared metadata remains part of the comparison. `--decode` additionally requires complete decoded-frame evidence.

The source for the final checks is the original complete HLS playlist, read from a confined, byte-preserving private snapshot with its supplied keys. The result is the actual standalone MP4, or the generated HLS playlist or DASH manifest and its referenced media, independently decrypted when required. Both must match complete encoded sample evidence, codec configuration and every absolute presentation and decode time, including relative audio/video timing. `--decode` additionally compares complete decoded media. Packet durations must preserve those clocks and the exact final end; the 44.1 kHz representation difference is explained below. No normalized MP4 is substituted as the source reference. Output remains private until these checks pass; no key file is published automatically.

Reading an audio-only HLS playlist through FFprobe can omit the first packet duration and repeat its presentation timestamp even though the transport packets have distinct clocks. The TS routes therefore read original packetized elementary stream (PES) clocks and Audio Data Transport Stream (ADTS) headers directly, before normalization. They require AAC-LC at 44.1 or 48 kHz, one 1,024-sample audio frame per ADTS unit, and a stable configuration. Every payload, count, presentation time, and decode time must match an independent read of the complete ordered transport bytes before those bytes can serve as a reference. When requested, complete decoding must also produce every original sample per channel. Missing or inconsistent evidence is refused; a repeated timestamp is not repaired by guessing.

Audio-only input uses those native checks for standalone MP4, DASH, fragmented MP4 HLS, and TS output. The same checks apply to AAC accompanying video. fMP4 HLS previously required video because the FFprobe-only path lacked that timing evidence; this was an implementation restriction, not a requirement of HLS. The original TS evidence now anchors conversion and fresh verification of audio-only fMP4 HLS.

### AAC sample rates and transport clocks

The 44.1 kHz handling below is included in the 1.0.1 source. The immutable v1.0.0 archives retain their 48 kHz-only TS audio profile; see [release status](releases.md).

The sample rate describes decoded audio samples per second. The container's clock describes timestamp units; the two need not be equal. MPEG-TS uses 90,000 ticks per second. A 1,024-sample AAC frame occupies exactly 1,920 ticks at 48 kHz, but about 2,089.796 ticks at 44.1 kHz. Integer timestamps therefore need rounding at 44.1 kHz. Adding 2,089 ticks for every frame would accumulate drift.

The source reader retains each packetized elementary stream (PES) timestamp and calculates the clocks of its enclosed AAC frames at the actual sample rate. Source admission accounts for rounding a PES timestamp and then a frame timestamp to integer ticks. It checks one consistent clock progression over the entire track, rather than accepting a new error allowance for every packet. Once admitted, every original frame timestamp is fixed. Conversion and independent verification require exact equality with those timestamps; there is no output timing tolerance.

Static HLS and DASH can retain these transport clocks in fragmented MP4. After resolving the manifest, drmr recognizes the supported single-track AAC-LC profile with 44.1 kHz audio and a 90 kHz media clock, and applies the same bounded rounding model across the complete track. The clock state continues across all resources. This checks whether an input's timing is supported; source-to-output comparison still requires every original frame, timestamp and playback bound to match exactly. Complete single-track fragmented MP4 input can use this checked clock family too. Input admission alone does not qualify MP4 output: non-TS input without a separately verified final-AAC playback declaration must also pass the stricter MP4 clock checks. TS-to-MP4 output instead retains and checks the complete original transport evidence. This is a drmr output-validation limit; MP4 can represent the retained timestamps.

For 44.1 kHz input, a Rust AAC reader copies the validated frames into a private fragmented MP4 before Shaka Packager creates the final HLS or DASH segments, or TS output. The MP4 media clock remains 90 kHz, while its audio sample entry and AudioSpecificConfig decoder settings retain 44,100 samples per second and the original channel configuration. Keeping these separate preserves every original presentation time (PTS) and decode time (DTS). AAC payloads, frame count, and decoded samples are unchanged. Only the transport headers are replaced; no audio is re-encoded. The writer holds at most 128 frames, with at most approximately 1 MiB of encoded audio, before writing a fragment.

FFprobe can report a nominal TS packet duration of 2,089 ticks even when the next packet starts 2,090 ticks later. In fragmented MP4, each sample duration instead gives the exact distance to the next decode timestamp. drmr stores that distance and uses the nominal integer duration for the final frame, which has no following timestamp. The verification compares every original PTS and DTS and the exact final end, while requiring valid consecutive MP4 sample durations. It does not require the two containers' rounded duration labels to be identical. Native comparison retains the exact audio-sample span separately, including the final frame; requested decoder evidence cannot replace that rational-duration proof. A changed timestamp, growing drift, missing frame, changed payload, or changed final end still fails.

For a 44.1 kHz source playlist, a nonfinal `EXTINF` can describe the distance from this segment's first frame to the next segment's first frame. The final segment has no following boundary: its declaration must match the exact audio-sample span, or the measured last-frame clock plus a nominal final duration of 2,089 or 2,090 ticks, at the declaration's decimal precision. These are specific representations of the original segment duration, not a general error allowance. They do not change any original frame timestamp or relax the output comparison.

### Playback ending inside the final AAC frame

An AAC frame still contains 1,024 samples when the final `EXTINF` declares a shorter playback interval. For an AAC-only rendition at 44.1 or 48 kHz, drmr accepts an end inside the final frame only when the declaration identifies one exact sample count. It does not infer that the excluded samples are silence or disposable padding. Ambiguous decimal precision, a partial nonfinal segment, and a declaration that removes a whole frame are refused. A separate audio rendition can accompany video; a multiplexed video/audio playlist uses the primary video duration and does not declare an independent audio trim.

The complete encoded track, every raw PTS/DTS, all decoded samples and the coded end are preserved. The playback interval starts at decoded sample zero and ends at the declared sample count. The original nonzero transport timestamp is a timeline origin, not a number of leading PCM samples to discard. Actual MP4 playback controls are checked separately: nonstandard playback rates or gains, disabled tracks and unsupported alternate groups are refused even when the encoded media and playback range match.

For fMP4 HLS, DASH and standalone fragmented MP4, the final initialization metadata records this range in a standard MP4 edit list: an empty edit retains the original timeline origin, followed by a normal-speed edit for the presented samples. Its movie clock can express both 90 kHz transport ticks and individual audio samples exactly; the media clock remains 90 kHz. Movie and track duration headers remain zero. drmr writes these edits after packaging. Before checking all coded frames or repackaging, it removes only this recognized declaration from a private copy. It then binds the declared playback range to the checked frame counts and clocks, and rechecks the original declaration. This also avoids the packager rounding its timestamp adjustment. The input and final output retain their edits. Unsupported edit lists still fail.

HLS declarations are calculated from the output's own playback bounds. DASH keeps the complete coded timing and writes the selected reference form; its common period covers the maximum presented end across all tracks, including when audio ends before video. A nominal template must satisfy the additional timing proof in [DASH references](dash-addressing.md). TS output records the shortened range in the final playlist declaration. Fresh verification independently reconstructs the output range and compares all coded media and the independently established native playback bounds. `--decode` also compares complete raw and presented PCM samples. This does not establish DRM-device playback or general support for arbitrary MP4 edits.

A complete standalone fragmented MP4 with this qualified canonical edit layout supports analysis, independent verification, and conversion to qualified DASH, fMP4 HLS or TS outputs. It can contain one audio track and optional video; audio-only input can also be re-encrypted as MP4. Each accepted edit must match all original coded frames, absolute clocks, exact playback ends and supported language metadata. This is not admission of arbitrary edit lists. Progressive MP4 and edits that exclude initial decoder warm-up samples remain outside the TS output profile; see [TS output conditions](conversion-support.md#mpeg-transport-stream-output).

Creating a transport stream from a progressive MP4 can itself change audio trimming or padding. This route proves identity to the supplied transport-stream presentation, not to an earlier file from which it was created. All existing staged-size, resource-budget, timeout, cancellation, and confirmed-process-cleanup limits still apply. Access-unit evidence is streamed with an 8 MiB per-packet limit and a charged 64 MiB reader bound; a malformed, oversized, incomplete, or unsuccessful read fails rather than returning a partial proof.

### Audio-only fMP4 HLS from transport streams

HLS can carry audio in fragmented MP4; see [Apple's audio authoring requirements](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html) and [the fMP4 segment rules in RFC 8216](https://www.rfc-editor.org/rfc/rfc8216.html#section-3.3). drmr checks and decrypts the ordered TS resources, separates the complete audio track into private MP4 media, then packages new fMP4 segments and playlists. It does not encrypt each source segment in place. The actual final playlists, initialization media, and segments must pass all required original-source native comparisons and any additional requested complete decoding before publication.

AAC frames contain 1,024 samples in this profile. A frame lasts about 21.333 milliseconds at 48 kHz or 23.220 milliseconds at 44.1 kHz, so a two-second segment target cannot always be an exact audio-frame boundary. drmr retains complete frames and their original clocks, and checks the playlist durations against the resulting media. It neither cuts a frame to reach two seconds nor drops the last frame to make the whole presentation exactly ten seconds. The 48 kHz, 470-frame TS fixture therefore presents 481,280 samples per channel; a progressive MP4 that declares a shorter finite playback range is a different source presentation.

### DASH output from transport streams

MPEG-TS timestamps often start above zero. DASH uses a presentation-time offset to map media clocks onto the manifest's playback timeline. Audio can begin before the first video picture; choosing that picture as the common start would hide the earlier audio. drmr chooses the earliest original presentation time across all tracks as the shared start. It preserves the original media timestamps and relative audio/video timing, checks each fragment against the selected manifest reference form, and reads the final manifest again before publication. The final output does not independently reset each track's timeline to zero.

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
Video durations are derived in Rust from validated SPS/PPS/VUI and every slice
header, then checked against original PTS/DTS and playlist durations. Missing
clocks or unsupported picture timing cause refusal; durations are never guessed
from a nominal frame rate. With `--decode`, decoded picture counts must also
match the original encoded picture count, and AAC must decode all required
samples per frame and channel. Final output passes the same complete native
content and timing checks at either depth. This covers
the last buffered packet as well as earlier segments.

The profile requires complete PES packets within each segment, a fixed
SAMPLE-AES method, and the supported Apple stream descriptors. AAC is 44.1 or 48 kHz
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
Both tracks use the resolved segment target. Audio boundaries follow complete frames and
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

Native validation reads every selected sample and its complete container or original access-unit clock evidence. It does not require FFprobe timing output. The additional `--decode` path retains the independent tool timing checks described here.

Tool-based packet timing verification reads every packet in the selected track. It hashes the ordered presentation time, decode time and duration as fixed-width signed integers with SHA-256, then compares both the complete digest and packet count. Clock units must match; times are never rounded, shifted or sampled. The native sample table or original access-unit inventory supplies the complete expected count. The additional decoder path uses its independent whole-track probe. Stream metadata, every encoded payload and playback windows are checked separately; `--decode` also checks complete decoded content.

The timing reader retains one record of at most 256 bytes and an 8 KiB read buffer rather than a list of packets. There is no longer a 100,000-packet or 32 MiB total-output limit on this proof. Malformed or unfinished records, missing clock values, wrong stream selectors, count changes and unsuccessful tool exits fail verification. Standard error remains independently bounded, and cancellation, deadlines and process cleanup use the same resource admission and supervision as other media commands. Unconfirmed process cleanup takes precedence over a normal cancellation or completed proof.

Other limits remain: staged track size, track and segment counts, MP4 initialization metadata, fragment metadata and samples per fragment are bounded separately. Removing the packet-proof limit does not make total memory, temporary disk space or runtime unlimited.

### Additional complete decoded-content hashes

`--decode` decodes the complete selected track. It hashes
the same raw picture bytes and signed 16-bit little-endian audio samples as
FFmpeg's SHA-256 output format, without selecting a different pixel format,
sample rate or channel layout. Rust receives the bytes through a fixed-size
buffer and computes SHA-256 with processor acceleration where available. The
decoded presentation is not retained in memory or written to a temporary file.

The same decoder feeds a second FFmpeg output using the original hash format
and its timestamp checks. That output produces a small Adler-32 completion
marker; the marker is never used to establish content equality. The SHA-256
result is accepted only after the whole subprocess succeeds and its cleanup is
confirmed. Decreasing timestamps still fail, while equal timestamps retain the
hash format's existing handling. Cancellation, deadlines and the independently
bounded error stream remain active. Each FFmpeg output queue is limited to one
packet. Subprocess admission retains the previous memory reservation alongside
the picture-size and input-buffer estimates; decoder and codec allocations
remain estimates rather than an operating-system memory limit.

Encoded-payload hashes and the higher-precision checks for native audio and
finite audio playback ranges keep their existing formats and processing.

The long-presentation regression derives a 32 × 18 video from the attributed [Big Buck Bunny clip](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md) and repeats all ten seconds 417 times. It converts and independently verifies all 100,080 packets over 4,170 seconds. Generate and check it locally with `cargo test -p drmr --test long_conversion --locked`; media files are temporary. This small resolution tests complete long-duration verification, not representative encoding throughput or device playback. A separate generated 700,001-packet test passes more than 32 MiB of valid timing records through the same reader.

## Interpret a result

Metadata inspection is evidence about declarations or initialization information. File hashing establishes byte identity. Decryption and decoded-content comparisons establish different properties. Neither a structural check nor a successful local decode substitutes for license-server or device playback testing.

An incorrect HLS AES-128 key does not always produce a padding error. Its decrypted bytes can accidentally match PKCS#7, the required byte pattern at the end of a resource. The result can instead be rejected while parsing MP4/TS data or comparing media. Successful padding removal does not establish that a key is correct. Use independent `verify` against the original source for content and timing checks.

The additional-decoding regressions use FFmpeg 9.0.2 and Shaka Packager 3.8.0. In that path, FFmpeg 6.1.1 on Ubuntu 24.04 omits the first clear AAC packet's duration and all encrypted AAC packet durations on this fixture. drmr reports `unsupported-packet-timing` rather than guessing missing values or skipping timing checks. Use a build that provides complete packet timing. Linux CI pins a checksum-verified FFmpeg 9.0 build from the [BtbN build project](https://github.com/BtbN/FFmpeg-Builds); media tools are not bundled in the CLI archive.

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
timestamps remain the native comparison reference. Full decoded content is additionally compared with `--decode`. This view
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

## Complete input analysis

Complete `analyze` checks every prepared source track directly. It does not match
the input against a second copy of the same track set. MPEG-TS source checks read
every encoded access unit and timestamp, compare the encoded content with
evidence collected directly from the original transport packets, inspect every
video frame, and decode the complete content. AAC checks also count the complete
decoded samples. Removing repeated work does not turn complete analysis into a
metadata check.

Analysis still reads all referenced renditions. Its duration depends on their
combined length and resolution, as well as retrieval and storage speed. The
separate `verify` operation independently compares source and output media;
source analysis does not replace that comparison or authorize a later command
without fresh checks.
