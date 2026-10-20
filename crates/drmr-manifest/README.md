# drmr-manifest

An independent Rust library for bounded, deterministic, in-memory HLS and DASH
manifest checks. The caller supplies bytes, opaque document IDs and exact playlist
bindings. The library does not open manifest, media or key files, fetch resources
or launch external tools. Automatic worker selection queries available parallelism.

```rust
use drmr_manifest::{Document, DocumentId, Format, Options, Profile, lint_document};

let document = Document {
    id: DocumentId(1), format: Format::Hls,
    bytes: b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nsegment.ts\n",
    base_uri: None,
};
let report = lint_document(document, &Options::new(Profile::HlsBaseManifest))?;
assert_eq!(report.errors, 0);
# Ok::<(), drmr_manifest::ConfigurationError>(())
```

## Evidence and profiles

`HlsBaseManifest` uses RFC 8216 manifest rules. `AppleVodManifest` adds separately
identified Apple VOD authoring requirements. `DashStaticManifest` checks static
MPD structure, supported typed attributes, representation identities, inherited
`SegmentTemplate` addressing and checked `SegmentTimeline` arithmetic. Its schema
facts are pinned to the fifth-edition schema shipped with DASH-IF Conformance
v3.1.0; it does not claim full XSD or Schematron equivalence. The DASH-IF restricted
timing model's additional constraints are not silently imposed on base DASH.
Finite runs use checked integer arithmetic. Negative runs require a positive
forward next-entry boundary or an exactly resolved Period duration. Terminal
counts use rational duration, effective timescale and presentation time offset;
fractional endpoints use a ceiling count without enumerating segments. Missing,
unsupported or unrepresentable context remains unresolved in scope. These checks
do not assert full media-to-Period timing alignment.
The native profile requires unambiguous representation identities within a Period.
Repeated IDs leave identity-dependent conclusions unresolved: the manifest alone
cannot prove the functional equivalence exception. They are not classified as a
general schema violation. ID lexical and numeric attribute errors remain separate.
ID lexical checks permit the schema's empty string and use Unicode 17.0 category Z
separators, rather than the broader Unicode whitespace property.

Rules have stable IDs, severity, source URL, section and revision. Coverage is
aggregated once per document and rule. Failed checks dominate unresolved checks,
which dominate passed checks; all counts and unresolved reasons remain visible.
Reports distinguish in-scope unresolved work from outside-scope evidence. A
resource limit, unsupported feature, missing child, foreign binding, graph cycle
or diagnostic truncation cannot produce a clean result for the declared scope.

Manifest bytes cannot prove media decodability, actual bandwidth, IDR placement,
sample encryption, delivery behavior or full presentation timing alignment.
Reports identify these evidence boundaries explicitly. Optional Apple and DASH-IF
external engines remain useful qualification and differential oracles. Existing
saved-plan `conformance: required` promises external validation; this library
does not change that contract.

## Graphs and privacy

`child_manifest_references` extracts only HLS child playlist references. It excludes
segments, keys and initialization maps. `lint_presentation` checks caller-supplied
bindings without discovering or fetching resources. Every supplied document must
be reachable from the entrypoint, and the graph must be acyclic and within the
configured maximum path length. DASH `XLink` expansion is explicitly unsupported.

`Document::base_uri` optionally supplies an absolute retrieval identity for relative
HLS key comparison. Relative references use RFC 3986 section 5.2; percent-encoded
separators remain encoded. Absolute key identifiers preserve their exact token
spelling, including opaque schemes. Missing or unsuitable bases leave dependent
checks unresolved. The caller may supply synthetic absolute bases representing
its captured manifest paths; the library never opens those identities.

Input bytes, base URIs and reference values are omitted from public Debug output.
Diagnostics use fixed codes and original byte spans with one-based line and
Unicode scalar column positions. They contain no source text, URL, key or payload.

## Resource limits

Limits bound bytes per document and presentation, document and reference counts,
physical lines and line bytes, XML nodes and depth, attributes per element,
distinct lexical namespace bindings per document, diagnostics, workers and a
conservative allocation budget. Limits are execution policy, not specification
violations. `NotChecked(ResourceLimit)` and `incomplete` record exhausted work.
XML depth, attribute and namespace limits are scanned linearly before DOM parsing,
bounding the parser's duplicate-attribute and namespace processing. Comments,
CDATA, processing instructions and quoted values do not count as tag attributes.
Repeated identical namespace declarations share one binding budget entry. Prefix
or raw URI changes count separately, bounding the parser's global namespace table.
Caller retrieval bases are capped at 8 KiB each. Exact link reference bytes in
total cannot exceed the supplied manifest bytes; foreign oversized bindings are
refused before reference indexes or parsers are constructed. These bounds are
included conservatively even in metadata-only resource estimates.
The borrowed DASH parser decodes UTF-8. Recognized alternative XML encodings are
explicitly unresolved; they are not reported as malformed UTF-8 documents.

`estimate_document_sizes` and `estimate_resources` use the same checked formula.
Metadata-only admission lets an I/O adapter reserve resources before retaining
all input bytes. The adapter must verify loaded bytes against its inventory.
Input bytes, retained facts, graph indexes, coverage and the final bounded report
are reserved first. Each concurrent parser reserves `64 * largest_input + 64 KiB`;
each semantic worker additionally reserves two bounded diagnostic sinks, rule
coverage and 64 KiB fixed overhead. Retained accounting reserves `18 * total_input`
plus 8 KiB per document and the configured graph, coverage and diagnostic capacities.
Overflow admits no worker. This is conservative allocation accounting, not an
operating-system RSS cap.

`Options::workers = None` uses available parallelism. An explicit count remains an
upper bound; task count, configured worker maximum and memory admission also apply.
Small calls and graphs with at least 95% of bytes in one document run inline.
Balanced multiple documents share one bounded parser pool. A large
single DASH MPD is parsed once and independent semantic jobs share its immutable
DOM in the same admitted CPU budget. Pools never nest. Inherited template facts
and timeline structure are checked once. Terminal arithmetic is reused for each
unique active timeline, Period, timescale and presentation time offset. HLS graph
checks reuse compact child facts and key indexes without reparsing child documents.

Diagnostics are bounded before position computation. Worker sinks share a lazy
line/scalar index and merge deterministically; reports preserve caller input order
regardless of scheduling. Only `workers_used` describes execution scheduling.

## Sources and verification

Rules are independently authored from [RFC 8216](https://www.rfc-editor.org/rfc/rfc8216),
[Apple's authoring specification](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html),
[W3C XML](https://www.w3.org/TR/REC-xml/) and
[XML Schema 1.0 datatypes](https://www.w3.org/TR/2004/REC-xmlschema-2-20041028/),
[pinned DASH schema facts](https://github.com/Dash-Industry-Forum/DASH-IF-Conformance/blob/v3.1.0/resources/xsd/DASH5th.xsd),
and [DASH-IF timing guidance](https://dashif.org/Guidelines-TimingModel/).
No DASH-IF engine code or schema assets are copied into this MIT crate.

Tests include independent positive, negative and boundary fixtures, graph and URI
counterexamples, privacy and limit controls, actual parallel worker execution,
and serial/parallel report equality with failures and diagnostic truncation.
External-engine results and media qualification remain separate evidence.
