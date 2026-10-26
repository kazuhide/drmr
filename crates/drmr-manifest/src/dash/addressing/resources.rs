//! Immutable literal-resource indexes are shared by inherited List contexts.
use super::super::{NS, check, not_checked, numeric::Parsed, span};
use crate::{DiagnosticSink, NotCheckedReason};
use roxmltree::Node;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) struct Range {
    start: u64,
    end: u64,
}

pub(super) fn range(value: &str) -> Parsed<Range> {
    let Some((start, end)) = value.split_once('-') else {
        return Parsed::Invalid;
    };
    if start.is_empty()
        || end.is_empty()
        || !start.bytes().all(|byte| byte.is_ascii_digit())
        || !end.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Parsed::Invalid;
    }
    let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
        return Parsed::Unsupported;
    };
    if end < start {
        return Parsed::Invalid;
    }
    if end
        .checked_sub(start)
        .and_then(|length| length.checked_add(1))
        .is_none()
    {
        return Parsed::Unsupported;
    }
    Parsed::Value(Range { start, end })
}

#[derive(Default)]
enum Claims {
    #[default]
    Empty,
    Whole,
    Ranges(BTreeMap<u64, u64>),
}
impl Claims {
    fn overlaps(&self, range: Option<Range>) -> bool {
        match (self, range) {
            (Self::Empty, _) => false,
            (Self::Whole, _) | (Self::Ranges(_), None) => true,
            (Self::Ranges(ranges), Some(range)) => {
                ranges
                    .range(..=range.start)
                    .next_back()
                    .is_some_and(|(_, end)| *end >= range.start)
                    || ranges
                        .range(range.start..)
                        .next()
                        .is_some_and(|(start, _)| *start <= range.end)
            }
        }
    }
    fn add(&mut self, range: Option<Range>) {
        match range {
            None => *self = Self::Whole,
            Some(range) => {
                if matches!(self, Self::Empty) {
                    *self = Self::Ranges(BTreeMap::new());
                }
                if let Self::Ranges(ranges) = self {
                    ranges.insert(range.start, range.end);
                }
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Facts<'a> {
    pub count: u128,
    pub requires_base: bool,
    resources: BTreeMap<&'a str, Claims>,
}
impl<'a> Facts<'a> {
    pub fn check(node: Node<'a, '_>, sink: &mut DiagnosticSink<'_>) -> Self {
        let mut facts = Self::default();
        for item in node
            .children()
            .filter(|node| node.has_tag_name((NS, "SegmentURL")))
        {
            facts.count += 1;
            facts.requires_base |= item.attribute("media").is_none_or(str::is_empty);
            let Ok(range) = optional_range(item, "mediaRange") else {
                continue;
            };
            let claims = facts
                .resources
                .entry(item.attribute("media").unwrap_or(""))
                .or_default();
            check(
                sink,
                "DASH-ADDRESSING-RANGES",
                !claims.overlaps(range),
                span(item),
            );
            claims.add(range);
        }
        check(sink, "DASH-LIST-SEGMENTS", facts.count > 0, span(node));
        facts
    }
    pub fn initialization(&self, node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
        let Ok(range) = optional_range(node, "range") else {
            return;
        };
        let overlaps = self
            .resources
            .get(node.attribute("sourceURL").unwrap_or(""))
            .is_some_and(|claims| claims.overlaps(range));
        check(sink, "DASH-ADDRESSING-RANGES", !overlaps, span(node));
    }
}

pub(super) fn optional_range(
    node: Node<'_, '_>,
    name: &str,
) -> Result<Option<Range>, NotCheckedReason> {
    match node.attribute(name).map(range) {
        None => Ok(None),
        Some(Parsed::Value(range)) => Ok(Some(range)),
        Some(Parsed::Invalid) => Err(NotCheckedReason::InvalidPrerequisite),
        Some(Parsed::Unsupported) => Err(NotCheckedReason::ResourceLimit),
    }
}

pub(super) fn element(node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    for name in ["range", "mediaRange", "indexRange"] {
        if let Some(value) = node.attribute(name) {
            match range(value) {
                Parsed::Value(_) => check(sink, "DASH-ADDRESSING-RANGES", true, span(node)),
                Parsed::Invalid => check(sink, "DASH-ADDRESSING-RANGES", false, span(node)),
                Parsed::Unsupported => not_checked(
                    sink,
                    "DASH-ADDRESSING-RANGES",
                    NotCheckedReason::ResourceLimit,
                    span(node),
                ),
            }
        }
    }
}

pub(super) fn base_ranges(index: &str, init: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    let (Parsed::Value(index), Ok(init_range)) = (range(index), optional_range(init, "range"))
    else {
        return;
    };
    // An explicit sourceURL names a separate literal resource. Alias and actual
    // file-extent checks require the caller's confined media resolver.
    if init.attribute("sourceURL").is_none() {
        let overlaps =
            init_range.is_none_or(|init| init.start <= index.end && index.start <= init.end);
        check(sink, "DASH-ADDRESSING-RANGES", !overlaps, span(init));
    }
}
