use super::timed_values::{self, hex};
use super::{
    attribute_list, attribute_types, check, rule,
    syntax::{self, Attribute, Decimal, Line},
    time::{self, DateError, Point},
    unknown_attributes,
};
use crate::{CheckStatus, DiagnosticSink, LintContext, NotCheckedReason, RuleScope, Span};
use std::collections::BTreeMap;

type Attributes<'a> = BTreeMap<&'a str, Attribute<'a>>;
pub(super) type Ranges<'a> = BTreeMap<&'a str, (Attributes<'a>, Span)>;

pub(super) struct Facts {
    pub program_date_time: bool,
    pub date_ranges: Result<[u8; 32], NotCheckedReason>,
}

pub(super) fn lint(
    lines: &[Line<'_>],
    context: &LintContext<'_>,
    sink: &mut DiagnosticSink<'_>,
) -> Facts {
    let mut ranges = Ranges::new();
    let mut dated = false;
    let mut count = 0;
    let mut limited = false;
    for &line in lines {
        match line.tag().0 {
            "#EXT-X-PROGRAM-DATE-TIME" => {
                dated = true;
                parse_date(line.tag().1.unwrap_or(""), line.span, sink);
            }
            "#EXT-X-DATERANGE" => {
                count += 1;
                if count > context.options.limits.max_references {
                    limited = true;
                    super::resource_limit(sink, line.span);
                    sink.check(
                        rule("HLS-DATE-RANGE-COVERAGE", "hls-date-range-limit", "4.3.2.7"),
                        CheckStatus::NotChecked(NotCheckedReason::ResourceLimit),
                        Some(line.span),
                    );
                    break;
                }
                declaration(line, &mut ranges, sink);
            }
            _ => {}
        }
    }
    if !ranges.is_empty() {
        check(
            sink,
            rule(
                "HLS-DATE-RANGE-PDT",
                "hls-date-range-program-date-time-required",
                "4.3.2.7",
            ),
            dated,
            lines[0].span,
        );
    }
    finish(&ranges, sink);
    Facts {
        program_date_time: dated,
        date_ranges: if limited {
            Err(NotCheckedReason::ResourceLimit)
        } else {
            timed_values::fingerprint(&ranges)
        },
    }
}

fn declaration<'a>(line: Line<'a>, ranges: &mut Ranges<'a>, sink: &mut DiagnosticSink<'_>) {
    let Some(attributes) = attribute_list(line, sink) else {
        return;
    };
    schema(&attributes, sink);
    let value = |name| syntax::get(&attributes, name);
    let has_start = value("START-DATE").is_some()
        || value("ID").is_some_and(|id| {
            ranges
                .get(id.value)
                .is_some_and(|entry| entry.0.contains_key("START-DATE"))
        });
    let valid = value("ID").is_some()
        && has_start
        && ["DURATION", "PLANNED-DURATION"].iter().all(|name| {
            value(name).is_none_or(|attribute| Decimal::parse(attribute.value).is_some())
        })
        && value("END-ON-NEXT").is_none_or(|attribute| attribute.value == "YES");
    check(
        sink,
        rule(
            "HLS-DATE-RANGE",
            "hls-date-range-declaration-invalid",
            "4.3.2.7",
        ),
        valid,
        line.span,
    );
    client_attributes(&attributes, sink);
    if let Some(id) = value("ID") {
        let entry = ranges
            .entry(id.value)
            .or_insert_with(|| (BTreeMap::new(), line.span));
        for attribute in attributes {
            let previous = entry.0.get(attribute.name);
            let requirement = rule(
                "HLS-DATE-RANGE-IDENTITY",
                "hls-date-range-attribute-conflict",
                "4.3.2.7",
            );
            match previous.map_or(Ok(true), |previous| {
                timed_values::equivalent(*previous, attribute)
            }) {
                Ok(equal) => check(sink, requirement, equal, attribute.span),
                Err(reason) => sink.check(
                    requirement,
                    CheckStatus::NotChecked(reason),
                    Some(attribute.span),
                ),
            }
            entry.0.entry(attribute.name).or_insert(attribute);
        }
    }
}

fn schema(attributes: &[Attribute<'_>], sink: &mut DiagnosticSink<'_>) {
    let standard = attributes
        .iter()
        .filter(|attribute| !attribute.name.starts_with("X-"))
        .copied()
        .collect::<Vec<_>>();
    unknown_attributes(
        &standard,
        &[
            "ID",
            "CLASS",
            "START-DATE",
            "END-DATE",
            "DURATION",
            "PLANNED-DURATION",
            "END-ON-NEXT",
            "SCTE35-CMD",
            "SCTE35-OUT",
            "SCTE35-IN",
        ],
        sink,
    );
    attribute_types(
        attributes,
        &["ID", "CLASS", "START-DATE", "END-DATE"],
        &[
            "DURATION",
            "PLANNED-DURATION",
            "END-ON-NEXT",
            "SCTE35-CMD",
            "SCTE35-OUT",
            "SCTE35-IN",
        ],
        sink,
    );
}

fn client_attributes(attributes: &[Attribute<'_>], sink: &mut DiagnosticSink<'_>) {
    for attribute in attributes {
        if attribute.name.starts_with("X-") {
            let valid = attribute.name.len() > 2
                && (attribute.quoted
                    || hex(attribute.value)
                    || Decimal::parse(attribute.value).is_some());
            check(
                sink,
                rule(
                    "HLS-DATE-RANGE-CLIENT",
                    "hls-client-attribute-type-invalid",
                    "4.3.2.7",
                ),
                valid,
                attribute.span,
            );
        }
        if attribute.name.starts_with("SCTE35-") {
            check(
                sink,
                rule(
                    "HLS-DATE-RANGE-SCTE",
                    "hls-scte35-hexadecimal-invalid",
                    "4.3.2.7.1",
                ),
                hex(attribute.value),
                attribute.span,
            );
            let mut requirement = rule(
                "HLS-SCTE-EVIDENCE",
                "hls-scte35-payload-not-inspected",
                "4.3.2.7.1",
            );
            requirement.scope = RuleScope::OutsideScope;
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::RequiresMediaEvidence),
                Some(attribute.span),
            );
        }
    }
}

fn finish(ranges: &Ranges<'_>, sink: &mut DiagnosticSink<'_>) {
    let mut classes = BTreeMap::<&str, Vec<Interval<'_>>>::new();
    for (attributes, span) in ranges.values() {
        let get = |name| attributes.get(name).copied();
        let start = get("START-DATE")
            .and_then(|attribute| parse_date(attribute.value, attribute.span, sink));
        let end =
            get("END-DATE").and_then(|attribute| parse_date(attribute.value, attribute.span, sink));
        if let (Some(start), Some(end)) = (&start, &end) {
            let requirement = rule(
                "HLS-DATE-RANGE-TIME",
                "hls-date-range-end-time-invalid",
                "4.3.2.7",
            );
            if start.zoned && end.zoned {
                let valid = !end.compare(start).is_lt()
                    && get("DURATION").is_none_or(|duration| {
                        Decimal::parse(duration.value)
                            .is_some_and(|duration| start.plus_equals(duration, end))
                    });
                check(sink, requirement, valid, *span);
            } else {
                sink.check(
                    requirement,
                    CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
                    Some(*span),
                );
            }
        }
        let next = get("END-ON-NEXT").is_some();
        check(
            sink,
            rule(
                "HLS-DATE-RANGE",
                "hls-date-range-declaration-invalid",
                "4.3.2.7",
            ),
            !next
                || get("CLASS").is_some() && get("DURATION").is_none() && get("END-DATE").is_none(),
            *span,
        );
        if let Some(class) = get("CLASS") {
            classes.entry(class.value).or_default().push(Interval {
                start,
                end,
                duration: get("DURATION").and_then(|attribute| Decimal::parse(attribute.value)),
                next,
                span: *span,
            });
        }
    }
    for ranges in classes.values_mut() {
        if ranges.iter().any(|range| range.next) {
            overlaps(ranges, sink);
        }
    }
}

struct Interval<'a> {
    start: Option<Point<'a>>,
    end: Option<Point<'a>>,
    duration: Option<Decimal<'a>>,
    next: bool,
    span: Span,
}

fn overlaps(ranges: &mut [Interval<'_>], sink: &mut DiagnosticSink<'_>) {
    let requirement = rule(
        "HLS-DATE-RANGE-OVERLAP",
        "hls-following-date-range-overlap-invalid",
        "4.3.2.7",
    );
    if ranges.iter().any(|range| {
        range.start.as_ref().is_none_or(|start| !start.zoned)
            || range.end.as_ref().is_some_and(|end| !end.zoned)
    }) {
        sink.check(
            requirement,
            CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite),
            Some(ranges[0].span),
        );
        return;
    }
    ranges.sort_unstable_by(|left, right| {
        left.start
            .as_ref()
            .unwrap()
            .compare(right.start.as_ref().unwrap())
    });
    let mut next_later = ranges.len();
    for at in (0..ranges.len()).rev() {
        let range = &ranges[at];
        let start = range.start.as_ref().unwrap();
        if at + 1 < ranges.len()
            && start
                .compare(ranges[at + 1].start.as_ref().unwrap())
                .is_lt()
        {
            next_later = at + 1;
        }
        let following = ranges.get(at + 1).and_then(|range| range.start.as_ref());
        let no_overlap = if let Some(end) = &range.end {
            following.is_none_or(|following| !end.compare(following).is_gt())
        } else if let Some(duration) = range.duration {
            following.is_none_or(|following| !start.plus_compare(duration, following).is_gt())
        } else if range.next {
            if following.is_some() && next_later == ranges.len() {
                sink.check(
                    requirement,
                    CheckStatus::NotChecked(NotCheckedReason::RequiresDeliveryEvidence),
                    Some(range.span),
                );
                continue;
            }
            following.is_none_or(|following| {
                ranges
                    .get(next_later)
                    .is_none_or(|next| !next.start.as_ref().unwrap().compare(following).is_gt())
            })
        } else if following.is_some() {
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::RequiresDeliveryEvidence),
                Some(range.span),
            );
            continue;
        } else {
            true
        };
        check(sink, requirement, no_overlap, range.span);
    }
}

fn parse_date<'a>(value: &'a str, span: Span, sink: &mut DiagnosticSink<'_>) -> Option<Point<'a>> {
    let requirement = rule("HLS-DATE-TIME", "hls-date-time-invalid", "4.3.2.6-4.3.2.7");
    match time::date(value) {
        Ok(date) => {
            sink.check(requirement, CheckStatus::Passed, Some(span));
            Some(date)
        }
        Err(DateError::Invalid) => {
            sink.check(requirement, CheckStatus::Failed, Some(span));
            None
        }
        Err(DateError::Unsupported) => {
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
                Some(span),
            );
            None
        }
    }
}
