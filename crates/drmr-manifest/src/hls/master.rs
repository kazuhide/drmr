use super::{
    ChildFact, ChildRole, HlsFacts, attribute_list, attribute_types, check, rule,
    syntax::{self, Attribute, Line},
    unknown_attributes,
};
use crate::{CheckStatus, DiagnosticSink, Document, LintContext, NotCheckedReason, Profile, Span};
use std::collections::BTreeSet;

type SelectedGroups<'a> = Vec<(&'a str, &'a str)>;

#[derive(Default)]
struct Master<'a> {
    groups: super::renditions::Groups<'a>,
    variants: Vec<(SelectedGroups<'a>, Span)>,
    pending: bool,
    actual: BTreeSet<&'a str>,
    children: Vec<ChildFact>,
    closed_none: usize,
    variant_count: usize,
    video_variants: super::apple::VideoVariants,
    references: usize,
    session_keys: BTreeSet<[u8; 32]>,
    session_data: BTreeSet<(&'a str, Option<&'a str>)>,
}

pub(super) fn lint(
    document: &Document<'_>,
    lines: &[Line<'_>],
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
    minimum: &mut u64,
    facts: &mut HlsFacts,
) {
    let mut state = Master::default();
    for &line in &lines[1..] {
        state.line(document, line, context, sink, minimum, facts);
        if state.references > context.options.limits.max_references {
            super::resource_limit(sink, line.span);
            sink.check(
                rule(
                    "HLS-GRAPH-COVERAGE",
                    "hls-playlist-reference-limit",
                    "4.3.4",
                ),
                CheckStatus::NotChecked(NotCheckedReason::ResourceLimit),
                Some(line.span),
            );
            break;
        }
    }
    state.finish(document, lines[0].span, context, sink, facts);
}

fn association(passed: bool, span: Span, sink: &mut DiagnosticSink<'_>) {
    check(
        sink,
        rule(
            "HLS-VARIANT-ASSOCIATION",
            "hls-variant-playlist-association-invalid",
            "4.3.4.2",
        ),
        passed,
        span,
    );
}

impl<'a> Master<'a> {
    fn line(
        &mut self,
        document: &Document<'_>,
        line: Line<'a>,
        context: &LintContext<'_>,
        sink: &mut DiagnosticSink<'_>,
        minimum: &mut u64,
        facts: &mut HlsFacts,
    ) {
        match line.tag().0 {
            "#EXT-X-MEDIA" => {
                if let Some(attributes) = attribute_list(line, sink) {
                    self.groups
                        .declare(&attributes, line, context, sink, minimum);
                    if let Some(uri) = syntax::get(&attributes, "URI") {
                        let role = if syntax::get(&attributes, "TYPE")
                            .is_some_and(|kind| kind.value == "SUBTITLES")
                        {
                            ChildRole::Subtitles
                        } else {
                            ChildRole::Regular
                        };
                        self.child(document, uri, role, context, sink);
                    }
                }
            }
            "#EXT-X-STREAM-INF" | "#EXT-X-I-FRAME-STREAM-INF" => {
                self.stream(document, line, context, sink);
            }
            "#EXT-X-SESSION-KEY" => {
                self.references += usize::from(session_key(
                    document,
                    line,
                    context.options.profile,
                    sink,
                    facts,
                    &mut self.session_keys,
                ));
            }
            "#EXT-X-SESSION-DATA" => session_data_entry(line, sink, &mut self.session_data),
            _ if !line.text.is_empty() && !line.text.starts_with('#') => {
                association(self.pending, line.span, sink);
                if self.pending {
                    self.child(
                        document,
                        Attribute {
                            name: "URI",
                            value: line.text,
                            quoted: true,
                            span: line.span,
                        },
                        ChildRole::Regular,
                        context,
                        sink,
                    );
                    self.pending = false;
                }
            }
            name if self.pending && super::references::is_media_tag(name) => {
                association(false, line.span, sink);
                self.pending = false;
            }
            _ => {}
        }
    }

    fn stream(
        &mut self,
        document: &Document<'_>,
        line: Line<'a>,
        context: &LintContext<'_>,
        sink: &mut DiagnosticSink<'_>,
    ) {
        association(!self.pending, line.span, sink);
        let iframe = line.tag().0 == "#EXT-X-I-FRAME-STREAM-INF";
        self.pending = !iframe;
        let Some(attributes) = attribute_list(line, sink) else {
            return;
        };
        let video = variant(&attributes, line, iframe, context, sink);
        if !iframe {
            self.variant_count += 1;
            if video {
                self.video_variants.record(&attributes);
            }
            self.closed_none += usize::from(
                syntax::get(&attributes, "CLOSED-CAPTIONS")
                    .is_some_and(|value| value.value == "NONE" && !value.quoted),
            );
        } else if let Some(uri) = syntax::get(&attributes, "URI") {
            self.child(document, uri, ChildRole::IFrame, context, sink);
        }
        let selected = ["AUDIO", "VIDEO", "SUBTITLES", "CLOSED-CAPTIONS"]
            .into_iter()
            .filter_map(|kind| {
                syntax::get(&attributes, kind)
                    .filter(|attribute| attribute.quoted)
                    .map(|attribute| (kind, attribute.value))
            })
            .collect();
        self.variants.push((selected, line.span));
    }

    fn child(
        &mut self,
        document: &Document<'_>,
        uri: Attribute<'a>,
        role: ChildRole,
        context: &LintContext<'_>,
        sink: &mut DiagnosticSink<'_>,
    ) {
        super::uri_check(uri, sink);
        self.actual.insert(uri.value);
        self.references += 1;
        let target = context
            .linked(document.id, uri.value)
            .map(|document| document.id);
        let requirement = rule(
            "HLS-GRAPH-REFERENCES",
            "hls-child-playlist-not-supplied",
            "4.3.4",
        );
        sink.check(
            requirement,
            if target.is_some() {
                CheckStatus::Passed
            } else {
                CheckStatus::NotChecked(NotCheckedReason::MissingLinkedManifest)
            },
            Some(uri.span),
        );
        self.children.push(ChildFact {
            target,
            span: uri.span,
            role,
        });
    }

    fn finish(
        self,
        document: &Document<'_>,
        span: Span,
        context: &LintContext<'_>,
        sink: &mut DiagnosticSink<'_>,
        facts: &mut HlsFacts,
    ) {
        association(!self.pending, span, sink);
        for (selected, span) in self.variants {
            check(
                sink,
                rule(
                    "HLS-RENDITION-REFERENCES",
                    "hls-rendition-group-reference-invalid",
                    "4.3.4.2",
                ),
                selected.iter().all(|key| self.groups.contains(key)),
                span,
            );
        }
        check(
            sink,
            rule(
                "HLS-CLOSED-CAPTIONS",
                "hls-closed-captions-none-inconsistent",
                "4.3.4.2",
            ),
            self.closed_none == 0 || self.closed_none == self.variant_count,
            span,
        );
        self.groups.finish(span, sink);
        for (reference, _) in context.outgoing(document.id) {
            check(
                sink,
                rule("HLS-GRAPH-BINDINGS", "hls-foreign-playlist-binding", "4.1"),
                self.actual.contains(reference),
                span,
            );
        }
        if context.options.profile == Profile::AppleVodManifest {
            self.video_variants.finish(span, sink);
        }
        facts.children = self.children;
    }
}

fn session_key(
    document: &Document<'_>,
    line: Line<'_>,
    profile: Profile,
    sink: &mut DiagnosticSink<'_>,
    facts: &mut HlsFacts,
    seen: &mut BTreeSet<[u8; 32]>,
) -> bool {
    let Some(attributes) = attribute_list(line, sink) else {
        return false;
    };
    super::keys::validate(&attributes, line, profile, sink);
    let valid = syntax::get(&attributes, "METHOD").is_some_and(|method| method.value != "NONE")
        && syntax::get(&attributes, "URI").is_some();
    check(
        sink,
        rule(
            "HLS-SESSION-KEY",
            "hls-session-key-declaration-invalid",
            "4.3.4.5",
        ),
        valid,
        line.span,
    );
    if let Some(fact) = super::keys::fact(&attributes, line.span, document.base_uri) {
        check(
            sink,
            rule(
                "HLS-SESSION-KEY-DUPLICATES",
                "hls-session-key-duplicated",
                "4.3.4.5",
            ),
            seen.insert(fact.duplicate),
            line.span,
        );
        facts.session_keys.push(fact);
        true
    } else {
        false
    }
}

fn variant(
    attributes: &[Attribute<'_>],
    line: Line<'_>,
    iframe: bool,
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
) -> bool {
    unknown_attributes(
        attributes,
        &[
            "BANDWIDTH",
            "AVERAGE-BANDWIDTH",
            "CODECS",
            "RESOLUTION",
            "FRAME-RATE",
            "HDCP-LEVEL",
            "AUDIO",
            "VIDEO",
            "SUBTITLES",
            "CLOSED-CAPTIONS",
            "URI",
        ],
        sink,
    );
    attribute_types(
        attributes,
        &["CODECS", "AUDIO", "VIDEO", "SUBTITLES", "URI"],
        &[
            "BANDWIDTH",
            "AVERAGE-BANDWIDTH",
            "RESOLUTION",
            "FRAME-RATE",
            "HDCP-LEVEL",
        ],
        sink,
    );
    let value = |name| syntax::get(attributes, name).map(|attribute| attribute.value);
    let bandwidth = value("BANDWIDTH").and_then(syntax::integer);
    let valid = bandwidth.is_some()
        && value("AVERAGE-BANDWIDTH").is_none_or(|value| syntax::integer(value).is_some())
        && value("RESOLUTION").is_none_or(|value| {
            value.split_once('x').is_some_and(|(width, height)| {
                [width, height]
                    .into_iter()
                    .all(|value| syntax::integer(value).is_some_and(|value| value > 0))
            })
        })
        && value("FRAME-RATE").is_none_or(|value| syntax::Decimal::parse(value).is_some())
        && value("CODECS").is_none_or(|value| {
            value
                .split(',')
                .all(|codec| !codec.is_empty() && !codec.chars().any(char::is_whitespace))
        })
        && (!iframe || value("URI").is_some())
        && (iframe || value("URI").is_none());
    check(
        sink,
        rule(
            "HLS-VARIANT",
            "hls-variant-declaration-invalid",
            "4.3.4.2-4.3.4.3",
        ),
        valid,
        line.span,
    );
    if let Some(closed) = syntax::get(attributes, "CLOSED-CAPTIONS") {
        check(
            sink,
            rule(
                "HLS-ATTRIBUTE-TYPES",
                "hls-attribute-value-type-invalid",
                "4.2",
            ),
            closed.quoted || closed.value == "NONE",
            closed.span,
        );
    }
    variant_restrictions(attributes, line, iframe, sink);
    let video = super::apple::is_video(attributes);
    if context.options.profile == Profile::AppleVodManifest {
        super::apple::variant(attributes, line, iframe, sink);
    }
    video
}

fn variant_restrictions(
    attributes: &[Attribute<'_>],
    line: Line<'_>,
    iframe: bool,
    sink: &mut DiagnosticSink<'_>,
) {
    if let Some(level) = syntax::get(attributes, "HDCP-LEVEL") {
        let status = match level.value {
            "NONE" | "TYPE-0" => CheckStatus::Passed,
            // TYPE-1 is defined by HLS2, outside the selected RFC 8216 rules.
            "TYPE-1" => CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
            _ => CheckStatus::Failed,
        };
        sink.check(
            rule("HLS-HDCP-LEVEL", "hls-hdcp-level-invalid", "4.3.4.2"),
            status,
            Some(level.span),
        );
    }
    if iframe {
        check(
            sink,
            rule(
                "HLS-I-FRAME-ATTRIBUTES",
                "hls-iframe-attribute-not-permitted",
                "4.3.4.3",
            ),
            attributes.iter().all(|attribute| {
                !matches!(
                    attribute.name,
                    "FRAME-RATE" | "AUDIO" | "SUBTITLES" | "CLOSED-CAPTIONS"
                )
            }),
            line.span,
        );
    }
}

fn session_data_entry<'a>(
    line: Line<'a>,
    sink: &mut DiagnosticSink<'_>,
    seen: &mut BTreeSet<(&'a str, Option<&'a str>)>,
) {
    let Some(attributes) = attribute_list(line, sink) else {
        return;
    };
    unknown_attributes(&attributes, &["DATA-ID", "VALUE", "URI", "LANGUAGE"], sink);
    attribute_types(
        &attributes,
        &["DATA-ID", "VALUE", "URI", "LANGUAGE"],
        &[],
        sink,
    );
    let value = |name| syntax::get(&attributes, name);
    check(
        sink,
        rule(
            "HLS-SESSION-DATA",
            "hls-session-data-declaration-invalid",
            "4.3.4.4",
        ),
        value("DATA-ID").is_some() && value("VALUE").is_some() != value("URI").is_some(),
        line.span,
    );
    if let Some(uri) = value("URI") {
        super::uri_check(uri, sink);
    }
    if let Some(id) = value("DATA-ID") {
        check(
            sink,
            rule(
                "HLS-SESSION-DATA-DUPLICATES",
                "hls-session-data-duplicated",
                "4.3.4.4",
            ),
            seen.insert((id.value, value("LANGUAGE").map(|attribute| attribute.value))),
            line.span,
        );
    }
}
