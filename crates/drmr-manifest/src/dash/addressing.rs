mod resources;

use super::{
    NS, check,
    inheritance::{EffectiveAddressing, Kind},
    not_checked,
    numeric::{Parsed, signed, unsigned},
    span, terminal, timeline,
};
use crate::{DiagnosticSink, NotCheckedReason};
use roxmltree::Node;
use std::sync::OnceLock;

pub(super) struct List<'a, 'input> {
    pub node: Node<'a, 'input>,
    facts: OnceLock<resources::Facts<'a>>,
}
impl<'a, 'input> List<'a, 'input> {
    pub fn new(node: Node<'a, 'input>) -> Self {
        Self {
            node,
            facts: OnceLock::new(),
        }
    }
    fn facts(&self, sink: &mut DiagnosticSink<'_>) -> &resources::Facts<'a> {
        self.facts
            .get_or_init(|| resources::Facts::check(self.node, sink))
    }
    pub fn check(&self, sink: &mut DiagnosticSink<'_>) {
        self.facts(sink);
    }
}

/// `BaseURL` scope facts are calculated once per owner, avoiding an ancestor's
/// full sibling scan for every representation in a large `AdaptationSet`.
#[derive(Clone, Copy, Default)]
pub(super) struct BaseContext {
    object: bool,
    ambiguous: bool,
}
impl BaseContext {
    pub fn inherit(node: Node<'_, '_>, mut parent: Self) -> Self {
        let mut bases = node
            .children()
            .filter(|node| node.has_tag_name((NS, "BaseURL")));
        if let Some(base) = bases.next() {
            if let Some(text) = base.text().map(str::trim).filter(|text| !text.is_empty()) {
                let path = text.split(['?', '#']).next().unwrap_or("");
                parent.object = !path.is_empty() && !path.ends_with('/');
            }
            parent.ambiguous |= bases.next().is_some();
        }
        parent
    }
}

pub(super) fn element(node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    if matches!(
        node.tag_name().name(),
        "SegmentBase"
            | "SegmentList"
            | "SegmentTemplate"
            | "SegmentURL"
            | "Initialization"
            | "RepresentationIndex"
    ) {
        resources::element(node, sink);
    }
}

pub(super) fn representation(
    selected: &EffectiveAddressing<'_, '_>,
    node: Node<'_, '_>,
    duration: terminal::PeriodDuration,
    timeline: Option<timeline::Facts>,
    list: Option<&List<'_, '_>>,
    base_context: BaseContext,
    sink: &mut DiagnosticSink<'_>,
) {
    own_bound(selected, duration, node, sink);
    number_order(selected, sink);
    match selected.kind {
        Kind::Base => base(selected, node, base_context, sink),
        Kind::List => {
            if let Some(list) = list {
                let facts = list.facts(sink);
                let requires_base = facts.requires_base
                    || selected
                        .initialization
                        .is_some_and(|node| node.attribute("sourceURL").is_none_or(str::is_empty));
                if requires_base && (base_context.ambiguous || !base_context.object) {
                    not_checked(
                        sink,
                        "DASH-LIST-SEGMENTS",
                        if base_context.ambiguous {
                            NotCheckedReason::UnsupportedFeature
                        } else {
                            NotCheckedReason::InvalidPrerequisite
                        },
                        span(node),
                    );
                }
                if let Some(initialization) = selected.initialization {
                    facts.initialization(initialization, sink);
                }
                let count = count(selected, duration, timeline);
                if selected.timeline.is_none() {
                    nominal_bounds(selected, duration, count, node, sink);
                }
                record(
                    sink,
                    "DASH-LIST-SEGMENTS",
                    count.map(|count| count == facts.count),
                    node,
                );
                numbers(selected, count, sink);
            }
        }
        Kind::Template => {
            if selected.timeline.is_none() && selected.value("duration").is_some() {
                let count = count(selected, duration, timeline);
                nominal_bounds(selected, duration, count, node, sink);
                numbers(selected, count, sink);
            } else if selected.value("endNumber").is_some() {
                numbers(selected, count(selected, duration, timeline), sink);
            }
        }
    }
}

fn count(
    selected: &EffectiveAddressing<'_, '_>,
    duration: terminal::PeriodDuration,
    facts: Option<timeline::Facts>,
) -> Result<u128, NotCheckedReason> {
    if let Some(facts) = facts {
        let count = facts
            .segments
            .ok_or(NotCheckedReason::InvalidPrerequisite)?;
        return if let Some(run) = facts.terminal {
            terminal::calculate(
                run.start.ok_or(NotCheckedReason::InvalidPrerequisite)?,
                run.length,
                duration?,
                selected
                    .timescale
                    .ok_or(NotCheckedReason::InvalidPrerequisite)?,
                selected
                    .offset
                    .ok_or(NotCheckedReason::InvalidPrerequisite)?,
            )
            .and_then(|(tail, _)| {
                count
                    .checked_add(tail)
                    .ok_or(NotCheckedReason::ResourceLimit)
            })
        } else {
            Ok(count)
        };
    }
    nominal(selected, own_duration(selected, duration?)?)
}

fn nominal(
    selected: &EffectiveAddressing<'_, '_>,
    period: super::numeric::Duration,
) -> Result<u128, NotCheckedReason> {
    if let Some(count) = explicit_count(selected)? {
        return Ok(count);
    }
    let (distance, divisor) = nominal_span(selected, period)?;
    if distance == 0 {
        return Err(NotCheckedReason::InvalidPrerequisite);
    }
    (distance / divisor)
        .checked_add(u128::from(distance % divisor != 0))
        .ok_or(NotCheckedReason::ResourceLimit)
}

fn nominal_span(
    selected: &EffectiveAddressing<'_, '_>,
    period: super::numeric::Duration,
) -> Result<(u128, u128), NotCheckedReason> {
    let invalid = NotCheckedReason::InvalidPrerequisite;
    let limit = NotCheckedReason::ResourceLimit;
    let scale = selected.timescale.ok_or(invalid)?;
    let length = selected
        .value("duration")
        .and_then(|value| unsigned(value, u64::from(u32::MAX)))
        .filter(|value| *value > 0)
        .ok_or(invalid)?;
    let delta = match selected.value("eptDelta").map_or(Parsed::Value(0), signed) {
        Parsed::Value(delta) => delta,
        Parsed::Invalid => return Err(invalid),
        Parsed::Unsupported => return Err(limit),
    };
    if period.denominator == 0 || scale == 0 {
        return Err(invalid);
    }
    let divisor = terminal::gcd(period.numerator, period.denominator);
    let numerator = period.numerator / divisor;
    let denominator = period.denominator / divisor;
    let divisor = terminal::gcd(u128::from(scale), denominator);
    let scale = u128::from(scale) / divisor;
    let denominator = denominator / divisor;
    let period_numerator = numerator.checked_mul(scale).ok_or(limit)?;
    let adjustment = delta.unsigned_abs().checked_mul(denominator).ok_or(limit)?;
    let distance = if delta < 0 {
        period_numerator.checked_add(adjustment).ok_or(limit)?
    } else {
        // No nonnegative segment ordinal can start before this own endpoint.
        period_numerator.saturating_sub(adjustment)
    };
    let divisor = denominator
        .checked_mul(u128::from(length))
        .filter(|value| *value > 0)
        .ok_or(limit)?;
    Ok((distance, divisor))
}

fn explicit_count(
    selected: &EffectiveAddressing<'_, '_>,
) -> Result<Option<u128>, NotCheckedReason> {
    let Some(end) = selected.value("endNumber") else {
        return Ok(None);
    };
    let invalid = NotCheckedReason::InvalidPrerequisite;
    let start = selected
        .value("startNumber")
        .map_or(Some(1), |value| unsigned(value, u64::MAX))
        .ok_or(invalid)?;
    let end = unsigned(end, u64::MAX)
        .filter(|end| *end >= start)
        .ok_or(invalid)?;
    Ok(Some(u128::from(end) - u128::from(start) + 1))
}

fn number_order(selected: &EffectiveAddressing<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    if let Some(end) = selected
        .value("endNumber")
        .and_then(|value| unsigned(value, u64::MAX))
        && let Some(start) = selected
            .value("startNumber")
            .map_or(Some(1), |value| unsigned(value, u64::MAX))
    {
        check(
            sink,
            "DASH-SEGMENT-NUMBERS",
            end >= start,
            span(selected.node),
        );
    }
}

fn own_duration(
    selected: &EffectiveAddressing<'_, '_>,
    global: super::numeric::Duration,
) -> Result<super::numeric::Duration, NotCheckedReason> {
    let Some(value) = selected.value("presentationDuration") else {
        return Ok(global);
    };
    let invalid = NotCheckedReason::InvalidPrerequisite;
    let duration = unsigned(value, u64::MAX)
        .filter(|value| *value > 0)
        .ok_or(invalid)?;
    let scale = selected
        .timescale
        .filter(|value| *value > 0)
        .ok_or(invalid)?;
    Ok(super::numeric::Duration {
        numerator: u128::from(duration),
        denominator: u128::from(scale),
    })
}

fn own_bound(
    selected: &EffectiveAddressing<'_, '_>,
    period: terminal::PeriodDuration,
    node: Node<'_, '_>,
    sink: &mut DiagnosticSink<'_>,
) {
    if selected.value("presentationDuration").is_some() {
        let within = period.and_then(|period| {
            own_duration(selected, period).and_then(|own| within_period(own, period))
        });
        record(sink, "DASH-NOMINAL-TIMING", within, node);
    }
}

fn within_period(
    own: super::numeric::Duration,
    period: super::numeric::Duration,
) -> Result<bool, NotCheckedReason> {
    if own.denominator == 0 || period.denominator == 0 {
        return Err(NotCheckedReason::InvalidPrerequisite);
    }
    let common = terminal::gcd(own.denominator, period.denominator);
    let left = own.numerator.checked_mul(period.denominator / common);
    let right = period.numerator.checked_mul(own.denominator / common);
    // For nonnegative values, one overflowed product is strictly larger than
    // the representable product; no floating-point comparison is necessary.
    match (left, right) {
        (Some(left), Some(right)) => Ok(left <= right),
        (Some(_), None) => Ok(true),
        (None, Some(_)) => Ok(false),
        (None, None) => Err(NotCheckedReason::ResourceLimit),
    }
}

fn nominal_bounds(
    selected: &EffectiveAddressing<'_, '_>,
    duration: terminal::PeriodDuration,
    count: Result<u128, NotCheckedReason>,
    node: Node<'_, '_>,
    sink: &mut DiagnosticSink<'_>,
) {
    record(
        sink,
        "DASH-NOMINAL-TIMING",
        count.map(|count| count > 0),
        node,
    );
    let before_end = duration
        .and_then(|period| own_duration(selected, period))
        .and_then(|own| nominal_span(selected, own))
        .and_then(|(distance, divisor)| {
            count?
                .checked_sub(1)
                .and_then(|tail| tail.checked_mul(divisor))
                .map(|last| last < distance)
                .ok_or(NotCheckedReason::ResourceLimit)
        });
    record(sink, "DASH-SEGMENT-NUMBERS", before_end, node);
}

fn numbers(
    selected: &EffectiveAddressing<'_, '_>,
    count: Result<u128, NotCheckedReason>,
    sink: &mut DiagnosticSink<'_>,
) {
    let result = count.and_then(|count| {
        let start = selected
            .value("startNumber")
            .map_or(Some(1), |value| unsigned(value, u64::MAX))
            .ok_or(NotCheckedReason::InvalidPrerequisite)?;
        let final_number = count
            .checked_sub(1)
            .and_then(|tail| u64::try_from(tail).ok())
            .and_then(|tail| start.checked_add(tail))
            .ok_or(NotCheckedReason::ResourceLimit)?;
        Ok(selected
            .value("endNumber")
            .is_none_or(|value| unsigned(value, u64::MAX) == Some(final_number)))
    });
    record(sink, "DASH-SEGMENT-NUMBERS", result, selected.node);
}

fn base(
    selected: &EffectiveAddressing<'_, '_>,
    node: Node<'_, '_>,
    context: BaseContext,
    sink: &mut DiagnosticSink<'_>,
) {
    if context.ambiguous || selected.value("eptDelta").is_some() {
        not_checked(
            sink,
            "DASH-BASE-ADDRESSING",
            NotCheckedReason::UnsupportedFeature,
            span(node),
        );
    } else if context.object && selected.complete() {
        check(sink, "DASH-BASE-ADDRESSING", true, span(node));
    } else {
        not_checked(
            sink,
            "DASH-BASE-ADDRESSING",
            NotCheckedReason::InvalidPrerequisite,
            span(node),
        );
    }
    if let Some(index) = selected.value("indexRange")
        && let Some(init) = selected.initialization
    {
        resources::base_ranges(index, init, sink);
    }
}

fn record(
    sink: &mut DiagnosticSink<'_>,
    rule: &'static str,
    result: Result<bool, NotCheckedReason>,
    node: Node<'_, '_>,
) {
    match result {
        Ok(valid) => check(sink, rule, valid, span(node)),
        Err(reason) => not_checked(sink, rule, reason, span(node)),
    }
}
