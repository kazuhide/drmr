//! Independent RFC 8216 and explicitly selected Apple VOD manifest checks.

mod apple;
mod grammar;
mod graph;
mod keys;
mod master;
mod media;
mod metadata;
mod references;
mod renditions;
mod syntax;
mod time;
mod timed;
mod timed_values;
mod uri;
pub(crate) use graph::GraphFacts;

pub(crate) fn valid_uri_reference(value: &str) -> bool {
    uri::valid(value)
}

use crate::{
    CheckStatus, DiagnosticSink, Document, LintContext, NotCheckedReason, Rule, RuleReference,
    RuleScope, Severity, Span,
};
use syntax::{Attribute, AttributeError, Line, TextError};

const RFC: &str = "https://www.rfc-editor.org/rfc/rfc8216.html";

const fn resource_rule() -> Rule {
    Rule {
        id: "HLS-LIMITS",
        message: "hls-resource-limit",
        severity: Severity::Error,
        scope: RuleScope::InScope,
        reference: RuleReference {
            url: "https://github.com/kazuhide/drmr/tree/main/crates/drmr-manifest#resource-limits",
            section: "resource-limits",
            revision: "2026-10-21.1",
        },
    }
}

fn resource_limit(sink: &mut DiagnosticSink<'_>, span: Span) {
    sink.check(
        resource_rule(),
        CheckStatus::NotChecked(NotCheckedReason::ResourceLimit),
        Some(span),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsKind {
    Invalid,
    Media,
    Master,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaylistType {
    Vod,
    Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildRole {
    Regular,
    Subtitles,
    IFrame,
}

#[derive(Clone, Debug)]
pub(crate) struct ChildFact {
    pub target: Option<crate::DocumentId>,
    pub span: Span,
    pub role: ChildRole,
}

#[derive(Clone, Debug)]
pub(crate) struct HlsFacts {
    pub kind: HlsKind,
    pub target_duration: Option<u64>,
    pub iframe_only: bool,
    pub playlist_type: Option<PlaylistType>,
    pub start: Option<[u8; 32]>,
    pub independent: bool,
    pub program_date_time: bool,
    pub date_ranges: Result<[u8; 32], NotCheckedReason>,
    pub children: Vec<ChildFact>,
    pub keys: Vec<keys::KeyFact>,
    pub session_keys: Vec<keys::KeyFact>,
}

impl HlsFacts {
    fn invalid() -> Self {
        Self {
            kind: HlsKind::Invalid,
            target_duration: None,
            iframe_only: false,
            playlist_type: None,
            start: None,
            independent: false,
            program_date_time: false,
            date_ranges: Err(NotCheckedReason::InvalidPrerequisite),
            children: Vec::new(),
            keys: Vec::new(),
            session_keys: Vec::new(),
        }
    }
}

pub(crate) fn lint_graph(
    document: &Document<'_>,
    facts: &HlsFacts,
    all: &std::collections::BTreeMap<crate::DocumentId, HlsFacts>,
    graph: &GraphFacts,
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
) {
    graph::lint(document, facts, all, graph, context, sink);
}

/// Extract only explicitly declared child playlists without opening any resource.
/// Semantic checks, including protocol versions, remain the linter's responsibility.
pub fn child_manifest_references<'a>(
    bytes: &'a [u8],
    limits: &crate::Limits,
) -> Result<Vec<crate::ManifestReference<'a>>, crate::ReferenceError> {
    references::collect(bytes, limits)
}

const fn rule(id: &'static str, message: &'static str, section: &'static str) -> Rule {
    Rule {
        id,
        message,
        severity: Severity::Error,
        scope: RuleScope::InScope,
        reference: RuleReference {
            url: RFC,
            section,
            revision: "RFC 8216 (2017-08)",
        },
    }
}

const fn apple_rule(id: &'static str, message: &'static str, section: &'static str) -> Rule {
    Rule {
        id,
        message,
        severity: Severity::Error,
        scope: RuleScope::InScope,
        reference: RuleReference {
            url: "https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html",
            section,
            revision: "retrieved 2026-10-21",
        },
    }
}

fn unknown_attributes(attributes: &[Attribute<'_>], known: &[&str], sink: &mut DiagnosticSink<'_>) {
    for attribute in attributes {
        if !known.contains(&attribute.name) {
            sink.check(
                rule(
                    "HLS-UNKNOWN-ATTRIBUTES",
                    "hls-extension-attribute-not-checked",
                    "4.2",
                ),
                CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
                Some(attribute.span),
            );
        }
    }
}

fn attribute_types(
    attributes: &[Attribute<'_>],
    quoted: &[&str],
    unquoted: &[&str],
    sink: &mut DiagnosticSink<'_>,
) {
    for attribute in attributes {
        if quoted.contains(&attribute.name) || unquoted.contains(&attribute.name) {
            check(
                sink,
                rule(
                    "HLS-ATTRIBUTE-TYPES",
                    "hls-attribute-value-type-invalid",
                    "4.2",
                ),
                attribute.quoted == quoted.contains(&attribute.name),
                attribute.span,
            );
        }
    }
}

fn uri_check(attribute: Attribute<'_>, sink: &mut DiagnosticSink<'_>) {
    let requirement = rule("HLS-URI", "hls-uri-invalid", "4.1");
    if attribute.value.contains("{$") {
        sink.check(
            requirement,
            CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
            Some(attribute.span),
        );
    } else {
        check(
            sink,
            requirement,
            syntax::uri(attribute.value),
            attribute.span,
        );
    }
}

fn start(line: Line<'_>, sink: &mut DiagnosticSink<'_>) -> Option<[u8; 32]> {
    let attributes = attribute_list(line, sink)?;
    unknown_attributes(&attributes, &["TIME-OFFSET", "PRECISE"], sink);
    attribute_types(&attributes, &[], &["TIME-OFFSET", "PRECISE"], sink);
    let offset = syntax::get(&attributes, "TIME-OFFSET");
    let precise = syntax::get(&attributes, "PRECISE");
    let valid = offset.is_some_and(|attribute| {
        syntax::Decimal::parse(attribute.value.strip_prefix('-').unwrap_or(attribute.value))
            .is_some()
    }) && precise.is_none_or(|attribute| matches!(attribute.value, "YES" | "NO"));
    check(
        sink,
        rule("HLS-START", "hls-start-declaration-invalid", "4.3.5.2"),
        valid,
        line.span,
    );
    if !valid {
        return None;
    }
    let offset = offset?;
    let negative = offset.value.starts_with('-');
    let decimal = syntax::Decimal::parse(offset.value.strip_prefix('-').unwrap_or(offset.value))?;
    let fraction = decimal.fraction.trim_end_matches('0');
    let sign = if negative && (!decimal.whole.is_empty() || !fraction.is_empty()) {
        "negative"
    } else {
        "nonnegative"
    };
    Some(keys::fingerprint(&[
        sign,
        decimal.whole,
        fraction,
        precise.map_or("NO", |attribute| attribute.value),
    ]))
}

fn check(sink: &mut DiagnosticSink<'_>, rule: Rule, passed: bool, span: Span) {
    sink.check(
        rule,
        if passed {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        Some(span),
    );
}

fn attribute_list<'a>(line: Line<'a>, sink: &mut DiagnosticSink<'_>) -> Option<Vec<Attribute<'a>>> {
    match line.tag().1.map_or_else(
        || Err(AttributeError::Syntax(line.span)),
        |value| syntax::attributes(value, line.value_start()),
    ) {
        Ok(parsed) => {
            sink.check(
                rule("HLS-ATTRIBUTES", "hls-attribute-list-invalid", "4.2"),
                CheckStatus::Passed,
                Some(line.span),
            );
            sink.check(
                rule(
                    "HLS-ATTRIBUTES-UNIQUE",
                    "hls-attribute-name-duplicated",
                    "4.2",
                ),
                CheckStatus::Passed,
                Some(line.span),
            );
            return Some(parsed);
        }
        Err(AttributeError::Syntax(span)) => sink.check(
            rule("HLS-ATTRIBUTES", "hls-attribute-list-invalid", "4.2"),
            CheckStatus::Failed,
            Some(span),
        ),
        Err(AttributeError::Duplicate(span)) => sink.check(
            rule(
                "HLS-ATTRIBUTES-UNIQUE",
                "hls-attribute-name-duplicated",
                "4.2",
            ),
            CheckStatus::Failed,
            Some(span),
        ),
    }
    None
}

pub(crate) fn lint(
    document: &Document<'_>,
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
) -> HlsFacts {
    let Some(lines) = read(document, context, sink) else {
        return HlsFacts::invalid();
    };
    let mut facts = classify(&lines, sink);
    let master = facts.kind == HlsKind::Master;
    let (mut minimum, version, start) = grammar::lint(&lines, sink);
    facts.start = start;
    if master {
        master::lint(document, &lines, context, sink, &mut minimum, &mut facts);
    } else {
        media::lint(document, &lines, context, sink, &mut minimum, &mut facts);
        let timed = timed::lint(&lines, context, sink);
        facts.program_date_time = timed.program_date_time;
        facts.date_ranges = timed.date_ranges;
        for _ in context.outgoing(document.id) {
            check(
                sink,
                rule("HLS-GRAPH-BINDINGS", "hls-foreign-playlist-binding", "4.1"),
                false,
                lines[0].span,
            );
        }
    }

    check(
        sink,
        rule("HLS-VERSION-MINIMUM", "hls-protocol-version-too-low", "7"),
        version.unwrap_or(1) >= minimum,
        lines[0].span,
    );

    outside_scope(sink);
    if sink.errors > 0 {
        facts.kind = HlsKind::Invalid;
    }
    facts
}

fn read<'a>(
    document: &Document<'a>,
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
) -> Option<Vec<Line<'a>>> {
    let lines = match syntax::lines(document.bytes, &context.options.limits) {
        Ok(lines) => lines,
        Err(error) => {
            let (id, message, section, span) = match error {
                TextError::Encoding(span) => ("HLS-UTF8", "hls-utf8-invalid", "4.1", span),
                TextError::Layout(span) => ("HLS-TEXT", "hls-text-layout-invalid", "4.1", span),
                TextError::Normalization(span) => {
                    ("HLS-NFC", "hls-text-not-normalized", "4.1", span)
                }
                TextError::Header(span) => ("HLS-HEADER", "hls-header-invalid", "4.3.1.1", span),
                TextError::Limit(span) => {
                    resource_limit(sink, span);
                    return None;
                }
            };
            sink.check(rule(id, message, section), CheckStatus::Failed, Some(span));
            sink.check(
                rule("HLS-PARSE-DEPENDENT", "hls-rules-require-valid-text", "4.1"),
                CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite),
                None,
            );
            return None;
        }
    };
    for (id, message, section) in [
        ("HLS-UTF8", "hls-utf8-invalid", "4.1"),
        ("HLS-TEXT", "hls-text-layout-invalid", "4.1"),
        ("HLS-NFC", "hls-text-not-normalized", "4.1"),
        ("HLS-HEADER", "hls-header-invalid", "4.3.1.1"),
    ] {
        sink.check(rule(id, message, section), CheckStatus::Passed, None);
    }
    sink.check(resource_rule(), CheckStatus::Passed, None);

    Some(lines)
}

fn classify(lines: &[Line<'_>], sink: &mut DiagnosticSink<'_>) -> HlsFacts {
    let iframe = lines.iter().any(|line| line.text == "#EXT-X-I-FRAMES-ONLY");
    let master = lines
        .iter()
        .any(|line| references::is_master_tag(line.tag().0));
    let media = lines
        .iter()
        .any(|line| references::is_media_tag(line.tag().0));
    check(
        sink,
        rule("HLS-PLAYLIST-KIND", "hls-master-media-tags-mixed", "4.1"),
        !(master && media),
        lines[0].span,
    );
    HlsFacts {
        kind: if master {
            HlsKind::Master
        } else {
            HlsKind::Media
        },
        target_duration: lines
            .iter()
            .find_map(|line| line.text.strip_prefix("#EXT-X-TARGETDURATION:"))
            .and_then(syntax::integer),
        iframe_only: iframe,
        playlist_type: lines.iter().find_map(|line| match line.text {
            "#EXT-X-PLAYLIST-TYPE:VOD" => Some(PlaylistType::Vod),
            "#EXT-X-PLAYLIST-TYPE:EVENT" => Some(PlaylistType::Event),
            _ => None,
        }),
        start: None,
        independent: lines
            .iter()
            .any(|line| line.text == "#EXT-X-INDEPENDENT-SEGMENTS"),
        program_date_time: false,
        date_ranges: Err(NotCheckedReason::InvalidPrerequisite),
        children: Vec::new(),
        keys: Vec::new(),
        session_keys: Vec::new(),
    }
}

fn outside_scope(sink: &mut DiagnosticSink<'_>) {
    for (id, message, section, reason) in [
        (
            "HLS-MEDIA-EVIDENCE",
            "hls-media-bytes-not-inspected",
            "3",
            NotCheckedReason::RequiresMediaEvidence,
        ),
        (
            "HLS-DELIVERY-EVIDENCE",
            "hls-delivery-not-inspected",
            "6",
            NotCheckedReason::RequiresDeliveryEvidence,
        ),
    ] {
        let mut requirement = rule(id, message, section);
        requirement.scope = RuleScope::OutsideScope;
        sink.check(requirement, CheckStatus::NotChecked(reason), None);
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_graph;
#[cfg(test)]
mod tests_timed;
