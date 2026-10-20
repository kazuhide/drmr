mod lexical;

use super::{
    NS, check as record, not_checked,
    numeric::{boolean, unsigned},
    span,
};
use crate::{DiagnosticSink, NotCheckedReason, Span};
use roxmltree::Node;

enum Kind {
    String,
    UInt,
    ULong,
    Boolean,
    FrameRate,
    Ratio,
    Sap,
    ScanType,
    Language,
    SamplingRates,
    ContentType,
    Codecs,
    Profiles,
    ByteRange,
    Unknown,
}

pub(super) fn check(node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    let name = node.tag_name().name();
    if !known_element(name) {
        not_checked(
            sink,
            "DASH-FEATURE-COVERAGE",
            NotCheckedReason::UnsupportedFeature,
            span(node),
        );
        return;
    }
    if let Some(parent) = node.parent().filter(Node::is_element)
        && parent.tag_name().namespace() == Some(NS)
    {
        record(
            sink,
            "DASH-ELEMENT-PLACEMENT",
            valid_parent(name, parent.tag_name().name()),
            span(node),
        );
    }
    for attribute in node.attributes() {
        let range = attribute.range();
        let location = Span {
            start: range.start,
            end: range.end,
        };
        if let Some(namespace) = attribute.namespace() {
            if namespace == "http://www.w3.org/XML/1998/namespace"
                && matches!(attribute.name(), "lang" | "base" | "space")
                || namespace == "http://www.w3.org/2001/XMLSchema-instance"
                    && attribute.name() == "schemaLocation"
            {
                continue;
            }
            if name == "ContentProtection"
                && namespace == "urn:mpeg:cenc:2013"
                && attribute.name() == "default_KID"
            {
                continue;
            }
            not_checked(
                sink,
                "DASH-ATTRIBUTE-COVERAGE",
                NotCheckedReason::UnsupportedFeature,
                location,
            );
            continue;
        }
        // The corresponding semantic handlers own these checks and rule IDs.
        if name == "Representation" && matches!(attribute.name(), "id" | "bandwidth")
            || name == "SegmentTemplate"
                && matches!(
                    attribute.name(),
                    "timescale"
                        | "presentationTimeOffset"
                        | "duration"
                        | "startNumber"
                        | "endNumber"
                        | "media"
                        | "initialization"
                        | "index"
                        | "bitstreamSwitching"
                )
            || name == "S" && matches!(attribute.name(), "t" | "d" | "r")
        {
            continue;
        }
        let value = attribute.value();
        let valid = match kind(name, attribute.name()) {
            Kind::String => continue,
            Kind::UInt => unsigned(value, u64::from(u32::MAX)).is_some(),
            Kind::ULong => unsigned(value, u64::MAX).is_some(),
            Kind::Boolean => boolean(value).is_some(),
            Kind::FrameRate => frame_rate(value),
            Kind::Ratio => ratio(value),
            Kind::Sap => unsigned(value, 6).is_some(),
            Kind::ScanType => matches!(value, "progressive" | "interlaced" | "unknown"),
            Kind::Language => lexical::language(value),
            Kind::SamplingRates => lexical::sampling_rates(value),
            Kind::ContentType => lexical::content_type(value),
            Kind::Codecs => lexical::codecs(value),
            Kind::Profiles => lexical::profiles(value),
            Kind::ByteRange => lexical::byte_range(value),
            Kind::Unknown => {
                not_checked(
                    sink,
                    "DASH-ATTRIBUTE-COVERAGE",
                    NotCheckedReason::UnsupportedFeature,
                    location,
                );
                continue;
            }
        };
        record(sink, "DASH-ATTRIBUTE-LEXICAL", valid, location);
    }
    descriptor(node, sink);
}

fn kind(element: &str, name: &str) -> Kind {
    if matches!(element, "Representation" | "AdaptationSet") {
        return representation_kind(element, name);
    }
    match (element, name) {
        ("MPD", "id" | "type" | "minBufferTime" | "mediaPresentationDuration")
        | ("Period", "id" | "start" | "duration")
        | ("BaseURL", "serviceLocation" | "byteRange")
        | (
            "ContentProtection"
            | "Role"
            | "Accessibility"
            | "AudioChannelConfiguration"
            | "SupplementalProperty"
            | "EssentialProperty"
            | "Rating"
            | "Viewpoint"
            | "AssetIdentifier",
            "id" | "schemeIdUri" | "value",
        )
        | ("ProgramInformation", "moreInformationURL")
        | ("Initialization" | "RepresentationIndex" | "BitstreamSwitching", "sourceURL") => {
            Kind::String
        }
        ("MPD", "profiles") => Kind::Profiles,
        ("Label" | "ProgramInformation", "lang") => Kind::Language,
        ("Initialization" | "RepresentationIndex" | "BitstreamSwitching", "range")
        | ("SegmentTemplate" | "SegmentList" | "SegmentBase", "indexRange") => Kind::ByteRange,
        ("Period", "bitstreamSwitching")
        | (
            "BaseURL" | "SegmentTemplate" | "SegmentList" | "SegmentBase",
            "availabilityTimeComplete",
        )
        | ("SegmentTemplate" | "SegmentList" | "SegmentBase", "indexRangeExact") => Kind::Boolean,
        ("SegmentTemplate" | "SegmentList" | "SegmentBase", "timescale")
        | ("SegmentTemplate" | "SegmentList", "duration" | "startNumber" | "endNumber") => {
            Kind::UInt
        }
        ("SegmentTemplate" | "SegmentList" | "SegmentBase", "presentationTimeOffset")
        | ("S", "n" | "k") => Kind::ULong,
        _ => Kind::Unknown,
    }
}

fn representation_kind(element: &str, name: &str) -> Kind {
    match name {
        "width" | "height" | "selectionPriority" => Kind::UInt,
        "codingDependency" => Kind::Boolean,
        "frameRate" => Kind::FrameRate,
        "sar" => Kind::Ratio,
        "startWithSAP" => Kind::Sap,
        "scanType" => Kind::ScanType,
        "mimeType" | "segmentProfiles" | "containerProfiles" | "tag" => Kind::String,
        "codecs" => Kind::Codecs,
        "audioSamplingRate" => Kind::SamplingRates,
        "profiles" => Kind::Profiles,
        _ => match (element, name) {
            (
                "AdaptationSet",
                "id" | "group" | "minBandwidth" | "maxBandwidth" | "minWidth" | "maxWidth"
                | "minHeight" | "maxHeight",
            )
            | ("Representation", "qualityRanking") => Kind::UInt,
            (
                "AdaptationSet",
                "segmentAlignment" | "subsegmentAlignment" | "bitstreamSwitching",
            ) => Kind::Boolean,
            ("AdaptationSet", "minFrameRate" | "maxFrameRate") => Kind::FrameRate,
            ("AdaptationSet", "par") => Kind::Ratio,
            ("AdaptationSet", "subsegmentStartsWithSAP") => Kind::Sap,
            ("AdaptationSet", "lang") => Kind::Language,
            ("AdaptationSet", "contentType") => Kind::ContentType,
            ("AdaptationSet", "initializationPrincipal")
            | (
                "Representation",
                "dependencyId" | "mediaStreamStructureId" | "associationId" | "associationType",
            ) => Kind::String,
            _ => Kind::Unknown,
        },
    }
}

/// The pinned schema's `StringNoWhitespaceType` permits an empty string. Its
/// category Z exclusion uses Unicode 17.0 properties, not Rust's broader `White_Space`.
/// <https://www.unicode.org/Public/17.0.0/ucd/extracted/DerivedGeneralCategory.txt>
pub(super) fn representation_identifier(value: &str) -> bool {
    !value.chars().any(|character| {
        matches!(
            character,
            '\t' | '\r' | '\n' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
        )
    })
}

fn known_element(name: &str) -> bool {
    matches!(
        name,
        "MPD"
            | "Period"
            | "AdaptationSet"
            | "Representation"
            | "SegmentTemplate"
            | "SegmentTimeline"
            | "S"
            | "BaseURL"
            | "ContentProtection"
            | "Role"
            | "Accessibility"
            | "AudioChannelConfiguration"
            | "SupplementalProperty"
            | "EssentialProperty"
            | "Rating"
            | "Viewpoint"
            | "AssetIdentifier"
            | "Label"
            | "ProgramInformation"
            | "Title"
            | "Source"
            | "Copyright"
            | "Initialization"
            | "RepresentationIndex"
            | "BitstreamSwitching"
            | "SegmentBase"
            | "SegmentList"
            | "SegmentURL"
    )
}

fn valid_parent(name: &str, parent: &str) -> bool {
    match name {
        "MPD" => false,
        "Period" | "ProgramInformation" => parent == "MPD",
        "AdaptationSet" | "AssetIdentifier" => parent == "Period",
        "Representation" => parent == "AdaptationSet",
        "SegmentTemplate" | "SegmentBase" | "SegmentList" => {
            matches!(parent, "Period" | "AdaptationSet" | "Representation")
        }
        "SegmentTimeline" | "BitstreamSwitching" => {
            matches!(parent, "SegmentTemplate" | "SegmentList")
        }
        "S" => parent == "SegmentTimeline",
        "SegmentURL" => parent == "SegmentList",
        "Initialization" | "RepresentationIndex" => {
            matches!(parent, "SegmentBase" | "SegmentList" | "SegmentTemplate")
        }
        "Title" | "Source" | "Copyright" => parent == "ProgramInformation",
        "BaseURL" => matches!(
            parent,
            "MPD" | "Period" | "AdaptationSet" | "Representation"
        ),
        "ContentProtection" => matches!(
            parent,
            "MPD" | "Period" | "AdaptationSet" | "Representation"
        ),
        // Other recognized descriptors have additional schema contexts beyond
        // this manifest profile; their full context belongs to the external oracle.
        _ => true,
    }
}

fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}
fn frame_rate(value: &str) -> bool {
    value.split_once('/').map_or_else(
        || digits(value),
        |(numerator, denominator)| {
            digits(numerator) && digits(denominator) && denominator.as_bytes()[0] != b'0'
        },
    )
}
fn ratio(value: &str) -> bool {
    value.split_once(':').is_some_and(|(left, right)| {
        left.bytes().all(|byte| byte.is_ascii_digit())
            && right.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn descriptor(node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    let name = node.tag_name().name();
    if matches!(
        name,
        "ContentProtection"
            | "Role"
            | "Accessibility"
            | "AudioChannelConfiguration"
            | "SupplementalProperty"
            | "EssentialProperty"
            | "Rating"
            | "Viewpoint"
            | "AssetIdentifier"
    ) {
        record(
            sink,
            "DASH-DESCRIPTOR-SCHEME",
            node.attribute("schemeIdUri").is_some(),
            span(node),
        );
    }
}
