mod addressing;
mod attributes;
mod facts;
mod inheritance;
mod numeric;
mod parallel;
mod period;
mod preflight;
mod protection;
mod template;
mod terminal;
mod timeline;

use crate::{
    CheckStatus, DiagnosticSink, Document, LintContext, NotCheckedReason, Rule, RuleReference,
    RuleScope, Severity, Span,
};
use numeric::{Parsed, duration};
use roxmltree::Node;

const NS: &str = "urn:mpeg:dash:schema:mpd:2011";
const SOURCE: RuleReference = RuleReference {
    url: "https://github.com/Dash-Industry-Forum/DASH-IF-Conformance/blob/v3.1.0/resources/xsd/DASH5th.xsd",
    section: "MPDtype, RepresentationBaseType, RepresentationType, SegmentTemplateType, SegmentTimelineType",
    revision: "DASH-IF v3.1.0 fifth-edition schema",
};
const ADDRESSING_SOURCE: RuleReference = RuleReference {
    url: "https://github.com/MPEGGroup/DASHSchema/blob/6th-Ed/DASH-MPD.xsd",
    section: "SegmentBaseType, MultipleSegmentBaseType, SegmentListType, SegmentURLType",
    revision: "MPEG DASH sixth-edition schema",
};
fn rule(id: &'static str) -> Rule {
    let reference = match id {
        "DASH-XML-LIMIT" | "DASH-EXECUTION-LIMIT" => crate::model::RESOURCE_REFERENCE,
        "DASH-XML-SYNTAX" => RuleReference {
            url: "https://www.w3.org/TR/REC-xml/",
            section: "2 Documents",
            revision: "XML 1.0 fifth edition",
        },
        "DASH-ATTRIBUTE-LEXICAL" => RuleReference {
            url: SOURCE.url,
            section: "Supported attribute declarations and simple types",
            revision: SOURCE.revision,
        },
        "DASH-TEMPLATE-INTEGERS"
        | "DASH-ADDRESSING-TIMESCALE"
        | "DASH-ADDRESSING-INTEGERS"
        | "DASH-SEGMENT-NUMBERS"
        | "DASH-LIST-SEGMENTS"
        | "DASH-ADDRESSING-RANGES"
        | "DASH-BASE-ADDRESSING" => ADDRESSING_SOURCE,
        "DASH-NOMINAL-TIMING" => RuleReference {
            url: "https://dashif.org/Guidelines-TimingModel/",
            section: "18.4 Simple addressing; finite reference counts and nominal timing",
            revision: "Commit Snapshot 2024-10-24",
        },
        "DASH-PERIOD-TIMING" | "DASH-TIMELINE-ARITHMETIC" => RuleReference {
            url: "https://dashif.org/Guidelines-TimingModel/",
            section: "8 Period timing; 9.1 Sample timeline; 18.3 Explicit addressing (ordinary timing descriptions)",
            revision: "Commit Snapshot 2024-10-24",
        },
        "DASH-REPRESENTATION-IDENTITY" => RuleReference {
            url: "https://github.com/kazuhide/drmr/tree/main/crates/drmr-manifest#evidence-and-profiles",
            section: "Native unambiguous representation identity policy",
            revision: "2026-10-21.1",
        },
        "DASH-CENC-DEFAULT-KID" => RuleReference {
            url: "https://learn.microsoft.com/en-us/playready/specifications/mpeg-dash-playready",
            section: "2.1.3 Including Track Encryption Box Fields in the MPD; 2.2.5 KID Byte Order",
            revision: "Microsoft specification v1.3 2018-02-15",
        },
        "DASH-CENC-PSSH-BOX" => RuleReference {
            url: "https://www.w3.org/TR/2026/NOTE-eme-initdata-cenc-20260604/",
            section: "1 Format; 4 Common SystemID and PSSH Box Format",
            revision: "W3C Group Note 2026-06-04",
        },
        "DASH-PROTECTION-BASE64" => RuleReference {
            url: "https://www.w3.org/TR/xmlschema11-2/#base64Binary",
            section: "3.3.17 base64Binary",
            revision: "W3C Recommendation 2012-04-05",
        },
        "DASH-FULL-XSD-SCHEMATRON" => RuleReference {
            url: "https://conformance.dashif.org/faq",
            section: "Validation steps",
            revision: "retrieved 2026-10-21",
        },
        _ => SOURCE,
    };
    Rule {
        id,
        severity: Severity::Error,
        scope: RuleScope::InScope,
        message: id,
        reference,
    }
}
fn check(
    sink: &mut DiagnosticSink<'_>,
    id: &'static str,
    valid: bool,
    span: impl Into<Option<Span>>,
) {
    sink.check(
        rule(id),
        if valid {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        span.into(),
    );
}
fn not_checked(
    sink: &mut DiagnosticSink<'_>,
    id: &'static str,
    reason: NotCheckedReason,
    span: impl Into<Option<Span>>,
) {
    sink.check(rule(id), CheckStatus::NotChecked(reason), span.into());
}
fn span(node: Node<'_, '_>) -> Span {
    let range = node.range();
    Span {
        start: range.start,
        end: range.end,
    }
}
fn duration_check(
    sink: &mut DiagnosticSink<'_>,
    id: &'static str,
    value: Option<&str>,
    node: Node<'_, '_>,
) {
    match value.map(duration) {
        Some(Parsed::Value(value)) => check(
            sink,
            id,
            value
                .compare(numeric::Duration {
                    numerator: 0,
                    denominator: 1,
                })
                .is_some_and(|order| !order.is_lt()),
            span(node),
        ),
        Some(Parsed::Unsupported) => {
            not_checked(sink, id, NotCheckedReason::UnsupportedFeature, span(node));
        }
        Some(Parsed::Invalid) | None => check(sink, id, false, span(node)),
    }
}

pub(crate) fn lint(
    document: &Document<'_>,
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
) {
    if preflight::unsupported_encoding(document.bytes) {
        not_checked(
            sink,
            "DASH-XML-ENCODING",
            NotCheckedReason::UnsupportedFeature,
            None,
        );
        return;
    }
    let Ok(text) = std::str::from_utf8(document.bytes) else {
        check(sink, "DASH-XML-SYNTAX", false, None);
        return;
    };
    if !preflight::within_limits(text, &context.options.limits) {
        not_checked(
            sink,
            "DASH-XML-LIMIT",
            NotCheckedReason::ResourceLimit,
            None,
        );
        return;
    }
    let options = roxmltree::ParsingOptions {
        nodes_limit: context.options.limits.max_xml_nodes,
        ..Default::default()
    };
    let parsed = match roxmltree::Document::parse_with_options(text, options) {
        Ok(parsed) => parsed,
        Err(roxmltree::Error::DtdDetected) => {
            not_checked(
                sink,
                "DASH-XML-EXTERNAL",
                NotCheckedReason::UnsupportedFeature,
                None,
            );
            return;
        }
        Err(
            roxmltree::Error::NodesLimitReached
            | roxmltree::Error::AttributesLimitReached
            | roxmltree::Error::NamespacesLimitReached,
        ) => {
            not_checked(
                sink,
                "DASH-XML-LIMIT",
                NotCheckedReason::ResourceLimit,
                None,
            );
            return;
        }
        Err(_) => {
            check(sink, "DASH-XML-SYNTAX", false, None);
            return;
        }
    };
    check(sink, "DASH-XML-SYNTAX", true, None);
    for node in parsed.root().children() {
        if node
            .pi()
            .is_some_and(|instruction| instruction.target == "xml-stylesheet")
        {
            not_checked(
                sink,
                "DASH-XML-EXTERNAL",
                NotCheckedReason::UnsupportedFeature,
                span(node),
            );
        }
    }
    let root = parsed.root_element();
    let valid_root = root.has_tag_name((NS, "MPD"));
    check(sink, "DASH-MPD-ROOT", valid_root, span(root));
    if !valid_root {
        return;
    }
    root_checks(root, sink);
    if let Some(inventory) = facts::Inventory::build(root, context, sink) {
        parallel::run(
            &inventory.jobs,
            context.semantic_workers,
            sink,
            &|job, sink| inventory.check(*job, sink),
        );
    }
    for (id, reason) in [
        (
            "DASH-MEDIA-EVIDENCE",
            NotCheckedReason::RequiresMediaEvidence,
        ),
        (
            "DASH-FULL-XSD-SCHEMATRON",
            NotCheckedReason::UnsupportedFeature,
        ),
    ] {
        let mut rule = rule(id);
        rule.scope = RuleScope::OutsideScope;
        sink.check(rule, CheckStatus::NotChecked(reason), None);
    }
}

fn root_checks(root: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    check(
        sink,
        "DASH-MPD-PROFILES",
        root.attribute("profiles")
            .is_some_and(|value| !value.trim().is_empty()),
        span(root),
    );
    duration_check(
        sink,
        "DASH-MPD-MIN-BUFFER-TIME",
        root.attribute("minBufferTime"),
        root,
    );
    check(
        sink,
        "DASH-MPD-PERIOD",
        root.children()
            .any(|node| node.has_tag_name((NS, "Period"))),
        span(root),
    );
    match root.attribute("type") {
        None | Some("static") => check(sink, "DASH-PROFILE-STATIC", true, span(root)),
        Some("dynamic") => not_checked(
            sink,
            "DASH-PROFILE-STATIC",
            NotCheckedReason::UnsupportedFeature,
            span(root),
        ),
        Some(_) => check(sink, "DASH-PROFILE-STATIC", false, span(root)),
    }
}
