use super::{
    NS,
    numeric::{Duration, Parsed, duration},
    terminal::PeriodDuration,
};
use crate::NotCheckedReason;
use roxmltree::Node;
use std::collections::BTreeMap;

struct Timing<'a, 'input> {
    node: Node<'a, 'input>,
    start: PeriodDuration,
    duration: Option<PeriodDuration>,
}

/// Resolve each Period's own duration without borrowing an unrelated endpoint or
/// imposing the DASH-IF restricted model's last-duration requirement.
pub(super) fn durations(root: Node<'_, '_>) -> BTreeMap<u32, PeriodDuration> {
    let mut previous_end = match root.attribute("type") {
        None | Some("static") => Ok(Duration {
            numerator: 0,
            denominator: 1,
        }),
        _ => Err(NotCheckedReason::InvalidPrerequisite),
    };
    let periods: Vec<_> = root
        .children()
        .filter(|node| node.has_tag_name((NS, "Period")))
        .map(|node| {
            let start = node.attribute("start").map_or(previous_end, value);
            let duration = node.attribute("duration").map(value);
            previous_end = start.and_then(|start| {
                duration
                    .unwrap_or(Err(NotCheckedReason::InvalidPrerequisite))
                    .and_then(|duration| {
                        start
                            .checked_add(duration)
                            .ok_or(NotCheckedReason::ResourceLimit)
                    })
            });
            Timing {
                node,
                start,
                duration,
            }
        })
        .collect();
    let presentation_end = root
        .attribute("mediaPresentationDuration")
        .map_or(Err(NotCheckedReason::InvalidPrerequisite), value);
    periods
        .iter()
        .enumerate()
        .map(|(index, period)| {
            let end = periods
                .get(index + 1)
                .map_or(presentation_end, |next| next.start);
            let duration = period.duration.unwrap_or_else(|| {
                period.start.and_then(|start| {
                    end.and_then(|end| {
                        let order = end.compare(start).ok_or(NotCheckedReason::ResourceLimit)?;
                        if order.is_lt() {
                            return Err(NotCheckedReason::InvalidPrerequisite);
                        }
                        end.checked_sub(start)
                            .ok_or(NotCheckedReason::ResourceLimit)
                    })
                })
            });
            (period.node.id().get(), duration)
        })
        .collect()
}

fn value(text: &str) -> PeriodDuration {
    match duration(text) {
        Parsed::Value(value) => Ok(value),
        Parsed::Unsupported => Err(NotCheckedReason::UnsupportedFeature),
        Parsed::Invalid => Err(NotCheckedReason::InvalidPrerequisite),
    }
}
