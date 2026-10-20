use super::{
    NS, attributes, not_checked,
    numeric::{Parsed, repeat, unsigned},
    span,
    terminal::Run,
};
use crate::{DiagnosticSink, NotCheckedReason};
use roxmltree::Node;

#[derive(Clone, Copy, Default)]
pub(super) struct Facts {
    pub terminal: Option<Run>,
}

/// Parse and check structural runs once. Terminal runs retain only their scalar
/// start and length for checks in each active effective addressing context.
pub(super) fn check(timeline: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) -> Facts {
    let mut facts = Facts::default();
    let mut cursor = Some(0_u128);
    let mut count = 0;
    let mut entries = timeline
        .children()
        .filter(|node| node.has_tag_name((NS, "S")))
        .peekable();
    while let Some(entry) = entries.next() {
        count += 1;
        attributes::check(entry, sink);
        let length = entry
            .attribute("d")
            .and_then(|value| unsigned(value, u64::MAX))
            .filter(|value| *value > 0);
        let time = entry.attribute("t").map(|value| unsigned(value, u64::MAX));
        let repeated = entry.attribute("r").map_or(Parsed::Value(Some(0)), repeat);
        let valid = length.is_some()
            && time.is_none_or(|value| value.is_some())
            && !matches!(repeated, Parsed::Invalid);
        check_entry(sink, valid, entry);
        if !valid {
            cursor = None;
            continue;
        }
        let Some(length) = length else {
            continue;
        };
        let start = time.flatten().map(u128::from).or(cursor);
        match repeated {
            Parsed::Value(Some(repeats)) => {
                cursor = start.and_then(|start| {
                    repeats
                        .checked_add(1)
                        .and_then(|count| count.checked_mul(u128::from(length)))
                        .and_then(|length| start.checked_add(length))
                });
                if cursor.is_none() {
                    not_checked(
                        sink,
                        "DASH-TIMELINE-ARITHMETIC",
                        NotCheckedReason::ResourceLimit,
                        span(entry),
                    );
                } else {
                    super::check(sink, "DASH-TIMELINE-ARITHMETIC", true, span(entry));
                }
            }
            Parsed::Value(None) => {
                if entries.peek().is_none() {
                    facts.terminal = Some(Run {
                        start,
                        length,
                        span: span(entry),
                    });
                    continue;
                }
                cursor = entries
                    .peek()
                    .and_then(|next| next.attribute("t"))
                    .and_then(|value| unsigned(value, u64::MAX))
                    .map(u128::from);
                if start.zip(cursor).is_none_or(|(start, end)| end <= start) {
                    not_checked(
                        sink,
                        "DASH-TIMELINE-ARITHMETIC",
                        NotCheckedReason::InvalidPrerequisite,
                        span(entry),
                    );
                } else {
                    super::check(sink, "DASH-TIMELINE-ARITHMETIC", true, span(entry));
                }
            }
            Parsed::Unsupported => not_checked(
                sink,
                "DASH-TIMELINE-ARITHMETIC",
                NotCheckedReason::ResourceLimit,
                span(entry),
            ),
            Parsed::Invalid => {}
        }
    }
    if count == 0 {
        not_checked(
            sink,
            "DASH-TIMELINE-ENTRIES",
            NotCheckedReason::InvalidPrerequisite,
            span(timeline),
        );
    } else {
        super::check(sink, "DASH-TIMELINE-ENTRIES", true, span(timeline));
    }
    facts
}

fn check_entry(sink: &mut DiagnosticSink<'_>, valid: bool, entry: Node<'_, '_>) {
    super::check(sink, "DASH-TIMELINE-ENTRIES", valid, span(entry));
}
