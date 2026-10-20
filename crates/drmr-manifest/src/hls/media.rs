use super::{
    attribute_list, check, rule,
    syntax::{self, Attribute, Decimal, Line},
    unknown_attributes,
};
use crate::{CheckStatus, DiagnosticSink, Document, LintContext, NotCheckedReason, Profile, Span};

type ByteRange = (u64, Option<u64>);

#[derive(Default)]
struct Media<'a> {
    target: Option<String>,
    pending: Option<(Decimal<'a>, Span)>,
    pending_range: Option<(ByteRange, Span)>,
    // Length and offset are u64; their exclusive sum has no separate u64 limit.
    previous: Option<(&'a str, u128)>,
    keys: super::keys::State<'a>,
    segments: usize,
    saw_discontinuity: bool,
    endlist: bool,
    vod: bool,
    references: usize,
}

pub(super) fn lint(
    document: &Document<'_>,
    lines: &[Line<'_>],
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
    minimum: &mut u64,
    facts: &mut super::HlsFacts,
) {
    let target = lines
        .iter()
        .find_map(|line| line.text.strip_prefix("#EXT-X-TARGETDURATION:"))
        .and_then(syntax::integer);
    check(
        sink,
        rule(
            "HLS-TARGET-DURATION",
            "hls-target-duration-invalid",
            "4.3.3.1",
        ),
        target.is_some(),
        lines[0].span,
    );
    let mut state = Media {
        target: target.map(|value| value.to_string()),
        ..Media::default()
    };
    for &line in &lines[1..] {
        state.line(document, line, sink, minimum, facts);
        if state.references > context.options.limits.max_references {
            super::resource_limit(sink, line.span);
            sink.check(
                rule(
                    "HLS-STATE-COVERAGE",
                    "hls-state-rules-resource-limited",
                    "4.3",
                ),
                CheckStatus::NotChecked(NotCheckedReason::ResourceLimit),
                Some(line.span),
            );
            return;
        }
    }
    state.finish(lines[0].span, context, sink);
}

impl<'a> Media<'a> {
    fn line(
        &mut self,
        document: &Document<'_>,
        line: Line<'a>,
        sink: &mut DiagnosticSink<'_>,
        minimum: &mut u64,
        facts: &mut super::HlsFacts,
    ) {
        let (name, value) = line.tag();
        match name {
            "#EXTINF" => self.extinf(line, minimum, sink),
            "#EXT-X-BYTERANGE" => {
                *minimum = (*minimum).max(4);
                let parsed = value.and_then(range);
                check(
                    sink,
                    rule("HLS-BYTE-RANGE", "hls-byte-range-invalid", "4.3.2.2"),
                    parsed.is_some() && self.pending_range.is_none(),
                    line.span,
                );
                self.pending_range = parsed.map(|range| (range, line.span));
            }
            "#EXT-X-KEY" => {
                if let Some(attributes) = attribute_list(line, sink) {
                    self.references += usize::from(syntax::get(&attributes, "URI").is_some());
                    self.keys.declare(&attributes, line, minimum, sink);
                    if let Some(fact) = super::keys::fact(&attributes, line.span, document.base_uri)
                    {
                        facts.keys.push(fact);
                    }
                }
            }
            "#EXT-X-MAP" => self.map(line, sink),
            "#EXT-X-MEDIA-SEQUENCE" | "#EXT-X-DISCONTINUITY-SEQUENCE" => {
                let valid = value.and_then(syntax::integer).is_some()
                    && self.segments == 0
                    && (name != "#EXT-X-DISCONTINUITY-SEQUENCE" || !self.saw_discontinuity);
                check(
                    sink,
                    rule(
                        "HLS-SEQUENCE",
                        "hls-sequence-declaration-invalid",
                        "4.3.3.2-4.3.3.3",
                    ),
                    valid,
                    line.span,
                );
            }
            "#EXT-X-DISCONTINUITY" => self.saw_discontinuity = true,
            "#EXT-X-ENDLIST" => self.endlist = true,
            "#EXT-X-PLAYLIST-TYPE" => {
                self.vod = value == Some("VOD");
                check(
                    sink,
                    rule("HLS-PLAYLIST-TYPE", "hls-playlist-type-invalid", "4.3.3.5"),
                    matches!(value, Some("VOD" | "EVENT")),
                    line.span,
                );
            }
            _ if !line.text.is_empty() && !line.text.starts_with('#') => self.segment(line, sink),
            _ => {}
        }
    }

    fn extinf(&mut self, line: Line<'a>, minimum: &mut u64, sink: &mut DiagnosticSink<'_>) {
        check(
            sink,
            rule(
                "HLS-SEGMENT-ASSOCIATION",
                "hls-segment-duration-association-invalid",
                "4.3.2.1",
            ),
            self.pending.is_none(),
            line.span,
        );
        let parsed = line
            .tag()
            .1
            .and_then(|value| value.split_once(','))
            .and_then(|(duration, _)| {
                Decimal::parse(duration)
                    .filter(|parsed| parsed.floating || syntax::integer(duration).is_some())
            });
        check(
            sink,
            rule("HLS-EXTINF", "hls-segment-duration-invalid", "4.3.2.1"),
            parsed.is_some(),
            line.span,
        );
        self.pending = parsed.map(|duration| (duration, line.span));
        if let Some(duration) = parsed {
            if duration.floating {
                *minimum = (*minimum).max(3);
            }
            if let Some(target) = &self.target {
                check(
                    sink,
                    rule(
                        "HLS-TARGET-DURATION",
                        "hls-target-duration-invalid",
                        "4.3.3.1",
                    ),
                    !duration.rounds_above(target),
                    line.span,
                );
            }
        }
    }

    fn map(&mut self, line: Line<'_>, sink: &mut DiagnosticSink<'_>) {
        let Some(attributes) = attribute_list(line, sink) else {
            return;
        };
        self.references += 1;
        unknown_attributes(&attributes, &["URI", "BYTERANGE"], sink);
        super::attribute_types(&attributes, &["URI", "BYTERANGE"], &[], sink);
        check(
            sink,
            rule("HLS-MAP", "hls-map-declaration-invalid", "4.3.2.5"),
            syntax::get(&attributes, "URI").is_some_and(|uri| !uri.value.is_empty()),
            line.span,
        );
        if let Some(uri) = syntax::get(&attributes, "URI") {
            super::uri_check(uri, sink);
        }
        if let Some(attribute) = syntax::get(&attributes, "BYTERANGE") {
            map_range(attribute, sink);
        }
        self.keys.check_map(line.span, sink);
    }

    fn segment(&mut self, line: Line<'a>, sink: &mut DiagnosticSink<'_>) {
        self.references += 1;
        self.segments += 1;
        self.keys.check_methods(line.span, sink);
        check(
            sink,
            rule(
                "HLS-SEGMENT-ASSOCIATION",
                "hls-segment-duration-association-invalid",
                "4.3.2.1",
            ),
            self.pending.take().is_some(),
            line.span,
        );
        super::uri_check(
            Attribute {
                name: "URI",
                value: line.text,
                quoted: true,
                span: line.span,
            },
            sink,
        );
        if let Some(((length, offset), span)) = self.pending_range.take() {
            let offset = offset.map(u128::from).or_else(|| {
                self.previous
                    .filter(|(uri, _)| *uri == line.text)
                    .map(|(_, end)| end)
            });
            let end = offset.and_then(|offset| offset.checked_add(u128::from(length)));
            check(
                sink,
                rule("HLS-BYTE-RANGE", "hls-byte-range-invalid", "4.3.2.2"),
                end.is_some(),
                span,
            );
            self.previous = end.map(|end| (line.text, end));
        } else {
            self.previous = None;
        }
    }

    fn finish(&self, span: Span, context: &LintContext<'_>, sink: &mut DiagnosticSink<'_>) {
        check(
            sink,
            rule(
                "HLS-SEGMENT-ASSOCIATION",
                "hls-segment-duration-association-invalid",
                "4.3.2.1",
            ),
            self.pending.is_none() && self.pending_range.is_none(),
            span,
        );
        if context.options.profile == Profile::AppleVodManifest {
            check(
                sink,
                super::apple_rule("APPLE-HLS-VOD", "apple-hls-finite-vod-required", "8.6"),
                self.vod && self.endlist && self.segments > 0,
                span,
            );
        }
    }
}

fn map_range(attribute: Attribute<'_>, sink: &mut DiagnosticSink<'_>) {
    let parsed = range(attribute.value);
    if parsed.is_some_and(|(_, offset)| offset.is_none()) {
        sink.check(
            rule(
                "HLS-MAP-RANGE",
                "hls-map-byte-range-requires-context",
                "4.3.2.5",
            ),
            CheckStatus::NotChecked(NotCheckedReason::RequiresMediaEvidence),
            Some(attribute.span),
        );
    } else {
        let valid = parsed.is_some_and(|(_, offset)| offset.is_some());
        check(
            sink,
            rule("HLS-MAP-RANGE", "hls-map-byte-range-invalid", "4.3.2.5"),
            valid,
            attribute.span,
        );
    }
}

fn range(value: &str) -> Option<(u64, Option<u64>)> {
    let (length, offset) = value
        .split_once('@')
        .map_or((value, None), |(length, offset)| (length, Some(offset)));
    let offset = match offset {
        Some(value) => Some(syntax::integer(value)?),
        None => None,
    };
    Some((syntax::integer(length)?, offset))
}
